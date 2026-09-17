use crate::i18n::t;
use crate::util::{base64, now_unix, parse_rfc3339, rfc3339_utc};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;

const BASE: &str = "https://api.track.toggl.com/api/v9";

#[derive(Deserialize, Clone, Debug)]
pub struct Me {
    pub default_workspace_id: i64,
}

#[derive(Deserialize, Serialize, Clone, Debug)]
pub struct TimeEntry {
    pub id: i64,
    pub workspace_id: i64,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub project_id: Option<i64>,
    #[serde(default)]
    pub start: String,
    #[serde(default)]
    pub stop: Option<String>,
    #[serde(default)]
    pub duration: i64,
    /// Set on entries that were deleted; `since=` listings include them.
    #[serde(default)]
    pub server_deleted_at: Option<String>,
}

impl TimeEntry {
    pub fn is_running(&self) -> bool {
        self.duration < 0
    }

    /// Seconds elapsed. API v9 reports running entries with `duration: -1`, so the
    /// elapsed time comes from `start`; `-start_unix` (v8 style) is accepted as a fallback.
    pub fn elapsed(&self, now: i64) -> i64 {
        if !self.is_running() {
            return self.duration;
        }
        match parse_rfc3339(&self.start) {
            Some(start) => (now - start).max(0),
            None if self.duration < -1 => (now + self.duration).max(0),
            None => 0,
        }
    }

    pub fn desc(&self) -> &str {
        self.description.as_deref().unwrap_or("")
    }
}

#[derive(Deserialize, Clone, Debug)]
pub struct Project {
    pub id: i64,
    pub name: String,
    #[serde(default)]
    pub color: Option<String>,
    #[serde(default)]
    pub active: Option<bool>,
}

#[derive(Clone)]
pub struct Client {
    agent: ureq::Agent,
    auth: String,
}

impl Client {
    pub fn new(token: &str) -> Result<Self, String> {
        let tls = native_tls::TlsConnector::new().map_err(|e| e.to_string())?;
        let agent = ureq::AgentBuilder::new()
            .tls_connector(Arc::new(tls))
            .timeout(Duration::from_secs(20))
            .user_agent(concat!("togglite/", env!("CARGO_PKG_VERSION")))
            .build();
        let auth = format!(
            "Basic {}",
            base64(format!("{}:api_token", token.trim()).as_bytes())
        );
        Ok(Client { agent, auth })
    }

    fn req(&self, method: &str, path: &str) -> ureq::Request {
        self.agent
            .request(method, &format!("{BASE}{path}"))
            .set("Authorization", &self.auth)
            .set("Content-Type", "application/json")
    }

    fn handle<T: DeserializeOwned>(r: Result<ureq::Response, ureq::Error>) -> Result<T, String> {
        Self::check(r)?
            .into_json::<T>()
            .map_err(|e| format!("{}: {e}", t().err_parse))
    }

    /// Maps transport and HTTP errors to user-facing messages; the body is left to the caller.
    fn check(r: Result<ureq::Response, ureq::Error>) -> Result<ureq::Response, String> {
        match r {
            Ok(resp) => Ok(resp),
            Err(ureq::Error::Status(401, _)) | Err(ureq::Error::Status(403, _)) => {
                Err(t().err_auth.to_string())
            }
            // 402 is Toggl's "hourly quota of your plan is used up" (30/h on the free plan).
            Err(ureq::Error::Status(402, _)) | Err(ureq::Error::Status(429, _)) => {
                Err(t().err_rate_limit.to_string())
            }
            Err(ureq::Error::Status(code, resp)) => {
                let body = resp.into_string().unwrap_or_default();
                let body: String = body.chars().take(200).collect();
                Err(format!("HTTP {code}: {body}"))
            }
            Err(ureq::Error::Transport(e)) => Err(format!("{}: {e}", t().err_network)),
        }
    }

    pub fn me(&self) -> Result<Me, String> {
        Self::handle(self.req("GET", "/me").call())
    }

    /// Entries of the last 30 days, the running one included (so `/me/time_entries/current`
    /// is never needed). `since=` means "modified since", which includes entries deleted in
    /// that window (flagged with `server_deleted_at`); those are dropped here.
    pub fn recent(&self) -> Result<Vec<TimeEntry>, String> {
        let since = now_unix() - 30 * 86400;
        let entries: Vec<TimeEntry> = Self::handle(
            self.req("GET", &format!("/me/time_entries?since={since}"))
                .call(),
        )?;
        Ok(entries.into_iter().filter(|e| e.server_deleted_at.is_none()).collect())
    }

    pub fn projects(&self) -> Result<Vec<Project>, String> {
        Self::handle(self.req("GET", "/me/projects").call())
    }

    pub fn start(
        &self,
        wid: i64,
        description: &str,
        project_id: Option<i64>,
    ) -> Result<TimeEntry, String> {
        let body = serde_json::json!({
            "created_with": "togglite",
            "workspace_id": wid,
            "description": description,
            "project_id": project_id,
            "start": rfc3339_utc(now_unix()),
            "duration": -1,
        });
        Self::handle(
            self.req("POST", &format!("/workspaces/{wid}/time_entries"))
                .send_json(body),
        )
    }

    pub fn stop(&self, wid: i64, id: i64) -> Result<TimeEntry, String> {
        Self::handle(
            self.req("PATCH", &format!("/workspaces/{wid}/time_entries/{id}/stop"))
                .call(),
        )
    }

    /// Rewrites an entry. `stop: None` keeps it running (duration -1) with the new start.
    pub fn update(
        &self,
        wid: i64,
        id: i64,
        description: &str,
        project_id: Option<i64>,
        start: i64,
        stop: Option<i64>,
    ) -> Result<TimeEntry, String> {
        let mut body = serde_json::json!({
            "created_with": "togglite",
            "workspace_id": wid,
            "description": description,
            "project_id": project_id,
            "start": rfc3339_utc(start),
            "duration": stop.map_or(-1, |s| s - start),
        });
        if let Some(s) = stop {
            body["stop"] = serde_json::Value::String(rfc3339_utc(s));
        }
        Self::handle(
            self.req("PUT", &format!("/workspaces/{wid}/time_entries/{id}"))
                .send_json(body),
        )
    }

    /// An entry that is already gone (404) counts as deleted so the local copy goes away too.
    pub fn delete(&self, wid: i64, id: i64) -> Result<(), String> {
        match self.req("DELETE", &format!("/workspaces/{wid}/time_entries/{id}")).call() {
            Err(ureq::Error::Status(404, _)) => Ok(()),
            r => Self::check(r).map(|_| ()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(start: &str, duration: i64) -> TimeEntry {
        TimeEntry {
            id: 1,
            workspace_id: 1,
            description: None,
            project_id: None,
            start: start.to_string(),
            stop: None,
            duration,
            server_deleted_at: None,
        }
    }

    #[test]
    fn deleted_entries_are_flagged_by_the_api() {
        let live: TimeEntry = serde_json::from_str(
            r#"{"id":1,"workspace_id":2,"start":"2026-09-16T12:00:00Z","stop":"2026-09-16T12:10:00Z","duration":600}"#,
        )
        .unwrap();
        let gone: TimeEntry = serde_json::from_str(
            r#"{"id":1,"workspace_id":2,"start":"2026-09-16T12:00:00Z","duration":600,"server_deleted_at":"2026-09-17T01:02:03Z"}"#,
        )
        .unwrap();
        assert!(live.server_deleted_at.is_none());
        assert!(gone.server_deleted_at.is_some());
    }

    #[test]
    fn elapsed_uses_start_for_running_entries() {
        let now = 1_789_000_000;
        assert_eq!(entry(&rfc3339_utc(now - 754), -1).elapsed(now), 754);
        assert_eq!(entry("2026-09-16T12:00:00+09:00", -1).elapsed(parse_rfc3339("2026-09-16T12:10:00+09:00").unwrap()), 600);
        // v8-style negative start as fallback when start is unparsable
        assert_eq!(entry("", -(now - 42)).elapsed(now), 42);
        // stopped entries report their stored duration
        assert_eq!(entry(&rfc3339_utc(now - 754), 300).elapsed(now), 300);
    }
}
