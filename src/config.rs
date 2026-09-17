//! Settings file at `%APPDATA%\togglite\config.json`.
//!
//! The API token is stored encrypted with Windows DPAPI (bound to the current user
//! account), base64-encoded in `api_token_dpapi`. A legacy plaintext `api_token`
//! field is still read and migrated to the encrypted form on the next save.
//!
//! `language` is an optional tag (`en`, `ja`); when absent the UI follows the
//! Windows display language.

use crate::util::{base64, base64_decode, wide};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::ptr::null_mut;
use std::{env, fs};
use winapi::um::dpapi::{CryptProtectData, CryptUnprotectData, CRYPTPROTECT_UI_FORBIDDEN};
use winapi::um::winbase::LocalFree;
use winapi::um::wincrypt::DATA_BLOB;

#[derive(Default, Clone)]
pub struct Config {
    pub api_token: String,
    pub popup_x: Option<i32>,
    pub popup_y: Option<i32>,
    pub language: Option<String>,
    /// True when the token was read from the legacy plaintext field.
    pub legacy_plaintext: bool,
}

#[derive(Serialize, Deserialize, Default)]
struct Stored {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    api_token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    api_token_dpapi: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    popup_x: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    popup_y: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    language: Option<String>,
}

pub fn path() -> PathBuf {
    let base = env::var_os("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("togglite").join("config.json")
}

pub fn load() -> Config {
    load_from(&path())
}

pub fn save(cfg: &Config) -> Result<(), String> {
    save_to(&path(), cfg)
}

fn load_from(p: &Path) -> Config {
    let stored: Stored = fs::read_to_string(p)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    let (api_token, legacy_plaintext) = match stored.api_token_dpapi.as_deref() {
        Some(enc) => (
            base64_decode(enc)
                .and_then(|blob| dpapi_unprotect(&blob))
                .and_then(|bytes| String::from_utf8(bytes).ok())
                .unwrap_or_default(),
            false,
        ),
        None => (stored.api_token.clone(), !stored.api_token.is_empty()),
    };
    Config {
        api_token,
        popup_x: stored.popup_x,
        popup_y: stored.popup_y,
        language: stored.language.filter(|l| !l.trim().is_empty()),
        legacy_plaintext,
    }
}

fn save_to(p: &Path, cfg: &Config) -> Result<(), String> {
    if let Some(dir) = p.parent() {
        fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let token = cfg.api_token.trim();
    let mut stored = Stored {
        popup_x: cfg.popup_x,
        popup_y: cfg.popup_y,
        language: cfg.language.clone(),
        ..Stored::default()
    };
    if !token.is_empty() {
        match dpapi_protect(token.as_bytes()) {
            Some(blob) => stored.api_token_dpapi = Some(base64(&blob)),
            None => stored.api_token = token.to_string(),
        }
    }
    let body = serde_json::to_string_pretty(&stored).map_err(|e| e.to_string())?;
    fs::write(p, body).map_err(|e| e.to_string())
}

fn dpapi_protect(data: &[u8]) -> Option<Vec<u8>> {
    let descr = wide("Togglite API token");
    let mut input = DATA_BLOB {
        cbData: data.len() as u32,
        pbData: data.as_ptr() as *mut u8,
    };
    let mut output = DATA_BLOB {
        cbData: 0,
        pbData: null_mut(),
    };
    let ok = unsafe {
        CryptProtectData(
            &mut input,
            descr.as_ptr(),
            null_mut(),
            null_mut(),
            null_mut(),
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        )
    };
    take_blob(ok, output)
}

fn dpapi_unprotect(data: &[u8]) -> Option<Vec<u8>> {
    let mut input = DATA_BLOB {
        cbData: data.len() as u32,
        pbData: data.as_ptr() as *mut u8,
    };
    let mut output = DATA_BLOB {
        cbData: 0,
        pbData: null_mut(),
    };
    let ok = unsafe {
        CryptUnprotectData(
            &mut input,
            null_mut(),
            null_mut(),
            null_mut(),
            null_mut(),
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        )
    };
    take_blob(ok, output)
}

fn take_blob(ok: i32, blob: DATA_BLOB) -> Option<Vec<u8>> {
    if ok == 0 || blob.pbData.is_null() {
        return None;
    }
    unsafe {
        let v = std::slice::from_raw_parts(blob.pbData, blob.cbData as usize).to_vec();
        LocalFree(blob.pbData as *mut _);
        Some(v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_file(name: &str) -> PathBuf {
        let dir = env::temp_dir().join(format!("togglite-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        dir.join(name)
    }

    #[test]
    fn token_is_encrypted_on_disk_and_round_trips() {
        let p = temp_file("enc.json");
        let cfg = Config {
            api_token: "s3cret-token".into(),
            popup_x: Some(10),
            popup_y: Some(20),
            language: Some("ja".into()),
            legacy_plaintext: false,
        };
        save_to(&p, &cfg).unwrap();
        let raw = fs::read_to_string(&p).unwrap();
        assert!(!raw.contains("s3cret-token"), "token must not be stored in plaintext: {raw}");
        assert!(raw.contains("api_token_dpapi"));
        let back = load_from(&p);
        assert_eq!(back.api_token, "s3cret-token");
        assert_eq!((back.popup_x, back.popup_y), (Some(10), Some(20)));
        assert_eq!(back.language.as_deref(), Some("ja"));
        assert!(!back.legacy_plaintext);
        let _ = fs::remove_file(&p);
    }

    #[test]
    fn legacy_plaintext_is_read_and_flagged() {
        let p = temp_file("legacy.json");
        fs::write(&p, r#"{ "api_token": "old-plain", "popup_x": 1 }"#).unwrap();
        let cfg = load_from(&p);
        assert_eq!(cfg.api_token, "old-plain");
        assert!(cfg.legacy_plaintext);
        // saving migrates it
        save_to(&p, &cfg).unwrap();
        let raw = fs::read_to_string(&p).unwrap();
        assert!(!raw.contains("old-plain"));
        assert_eq!(load_from(&p).api_token, "old-plain");
        let _ = fs::remove_file(&p);
    }

    #[test]
    fn missing_file_is_default() {
        let cfg = load_from(Path::new("Z:/definitely/missing/config.json"));
        assert!(cfg.api_token.is_empty());
        assert!(cfg.language.is_none());
        assert!(!cfg.legacy_plaintext);
    }

    #[test]
    fn language_is_optional_and_omitted_when_unset() {
        let p = temp_file("lang.json");
        save_to(&p, &Config::default()).unwrap();
        assert!(!fs::read_to_string(&p).unwrap().contains("language"));
        fs::write(&p, r#"{ "language": "" }"#).unwrap();
        assert!(load_from(&p).language.is_none());
        let _ = fs::remove_file(&p);
    }
}
