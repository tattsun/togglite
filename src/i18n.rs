//! UI strings. The language follows the Windows display language unless the user
//! picks one on the settings page (stored as `language` in config.json).
//! `TOGGLITE_LANG` overrides the detected system language, for demos.

use std::sync::atomic::{AtomicU8, Ordering};
use winapi::um::winnls::GetUserDefaultUILanguage;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Lang {
    En,
    Ja,
}

impl Lang {
    /// Every supported language, in the order the settings page lists them.
    pub const ALL: [Lang; 2] = [Lang::En, Lang::Ja];

    /// Accepts BCP 47 style tags such as `ja`, `ja-JP`, `en_US`; returns None for
    /// anything unsupported so the caller can fall back to auto-detection.
    pub fn from_tag(tag: &str) -> Option<Lang> {
        let primary = tag.trim().split(['-', '_']).next().unwrap_or("");
        match primary.to_ascii_lowercase().as_str() {
            "en" => Some(Lang::En),
            "ja" => Some(Lang::Ja),
            _ => None,
        }
    }

    pub fn tag(self) -> &'static str {
        match self {
            Lang::En => "en",
            Lang::Ja => "ja",
        }
    }

    /// The language's name in itself, as shown in the language picker.
    pub fn native_name(self) -> &'static str {
        match self {
            Lang::En => "English",
            Lang::Ja => "日本語",
        }
    }

    fn strings(self) -> &'static Strings {
        match self {
            Lang::En => &EN,
            Lang::Ja => &JA,
        }
    }

    fn from_u8(v: u8) -> Option<Lang> {
        Lang::ALL.iter().copied().find(|l| *l as u8 == v)
    }
}

/// Windows LANGID primary language for Japanese.
const LANG_JAPANESE: u16 = 0x11;

/// The Windows display language of the current user (`TOGGLITE_LANG` overrides it).
pub fn system_lang() -> Lang {
    if let Some(l) = std::env::var("TOGGLITE_LANG").ok().and_then(|v| Lang::from_tag(&v)) {
        return l;
    }
    let id = unsafe { GetUserDefaultUILanguage() };
    match id & 0x3FF {
        LANG_JAPANESE => Lang::Ja,
        _ => Lang::En,
    }
}

/// Sentinel for "no explicit preference, follow the system".
const AUTO: u8 = u8::MAX;

/// The user's explicit choice (or AUTO) and the language actually in effect.
static PREFERENCE: AtomicU8 = AtomicU8::new(AUTO);
static CURRENT: AtomicU8 = AtomicU8::new(Lang::En as u8);

/// Applies the saved preference (a language tag, or None for automatic) at startup.
pub fn init(config_lang: Option<&str>) {
    set_preference(config_lang.and_then(Lang::from_tag));
}

/// Switches the UI language. `None` follows the Windows display language.
pub fn set_preference(pref: Option<Lang>) {
    PREFERENCE.store(pref.map_or(AUTO, |l| l as u8), Ordering::Relaxed);
    CURRENT.store(pref.unwrap_or_else(system_lang) as u8, Ordering::Relaxed);
}

pub fn preference() -> Option<Lang> {
    Lang::from_u8(PREFERENCE.load(Ordering::Relaxed))
}

pub fn current() -> Lang {
    Lang::from_u8(CURRENT.load(Ordering::Relaxed)).unwrap_or(Lang::En)
}

/// Strings for the current language. Safe to call from worker threads.
pub fn t() -> &'static Strings {
    current().strings()
}

/// A short calendar date for day headers and the editor, e.g. "Wed, Sep 17" / "9月17日 (水)".
/// `weekday` is 0 for Sunday.
pub fn date_label(month: u32, day: u32, weekday: u32) -> String {
    date_label_in(current(), month, day, weekday)
}

fn date_label_in(lang: Lang, month: u32, day: u32, weekday: u32) -> String {
    let s = lang.strings();
    let wd = s.weekdays[weekday as usize % 7];
    match lang {
        Lang::En => format!("{wd}, {} {day}", s.months[(month as usize).clamp(1, 12) - 1]),
        Lang::Ja => format!("{month}月{day}日 ({wd})"),
    }
}

pub struct Strings {
    // tray menu
    pub menu_start: &'static str,
    pub menu_open: &'static str,
    pub menu_reload: &'static str,
    pub menu_settings: &'static str,
    pub menu_quit: &'static str,
    /// Menu label while a timer runs: "{stop}: {description} ({elapsed})".
    pub menu_stop: &'static str,
    pub tip_stopped: &'static str,

    // main page
    pub cue_description: &'static str,
    pub status_syncing: &'static str,
    pub status_tracking: &'static str,
    pub status_stopped: &'static str,
    pub no_project: &'static str,
    pub no_description: &'static str,
    pub start: &'static str,
    pub stop: &'static str,
    pub recent_entries: &'static str,
    pub no_entries: &'static str,
    pub need_token: &'static str,

    // history and editor pages
    pub history: &'static str,
    pub today: &'static str,
    pub yesterday: &'static str,
    pub edit_entry: &'static str,
    pub delete: &'static str,
    /// Label of the delete button once armed; the next click deletes.
    pub delete_confirm: &'static str,
    pub saving: &'static str,
    pub weekdays: [&'static str; 7],
    pub months: [&'static str; 12],

    // settings page
    pub settings: &'static str,
    pub cue_token: &'static str,
    pub token_label: &'static str,
    pub token_hint: &'static str,
    pub open_profile: &'static str,
    pub language: &'static str,
    /// Picker entry that follows the Windows display language.
    pub language_auto: &'static str,
    pub checking: &'static str,
    pub save: &'static str,

    // errors (the ": detail" suffix is appended by the caller)
    pub err_token_empty: &'static str,
    pub err_save_failed: &'static str,
    pub err_init_failed: &'static str,
    pub err_not_synced: &'static str,
    pub err_rate_limit: &'static str,
    pub err_bad_time: &'static str,
    pub err_parse: &'static str,
    pub err_auth: &'static str,
    pub err_network: &'static str,
}

static EN: Strings = Strings {
    menu_start: "Start",
    menu_open: "Open",
    menu_reload: "Reload",
    menu_settings: "Settings...",
    menu_quit: "Quit",
    menu_stop: "Stop",
    tip_stopped: "Togglite (stopped)",

    cue_description: "What are you working on?",
    status_syncing: "Syncing…",
    status_tracking: "Tracking",
    status_stopped: "Stopped",
    no_project: "No project",
    no_description: "(no description)",
    start: "Start",
    stop: "Stop",
    recent_entries: "Recent entries",
    no_entries: "No entries yet",
    need_token: "Add your API token in Settings",

    history: "History",
    today: "Today",
    yesterday: "Yesterday",
    edit_entry: "Edit entry",
    delete: "Delete entry",
    delete_confirm: "Click again to delete",
    saving: "Saving…",
    weekdays: ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"],
    months: ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"],

    settings: "Settings",
    cue_token: "Paste your API token",
    token_label: "Toggl API token",
    token_hint: "Copy the API token from the bottom of your Toggl Profile settings page and paste it here.",
    open_profile: "Open track.toggl.com/profile ↗",
    language: "Language",
    language_auto: "Automatic",
    checking: "Checking…",
    save: "Save",

    err_token_empty: "Enter your API token",
    err_save_failed: "Failed to save settings",
    err_init_failed: "Failed to initialize",
    err_not_synced: "Not synced yet",
    err_rate_limit: "Toggl API hourly limit reached; try again later",
    err_bad_time: "Enter times as HH:MM",
    err_parse: "Failed to parse the response",
    err_auth: "Authentication failed: check your API token",
    err_network: "Network error",
};

static JA: Strings = Strings {
    menu_start: "開始",
    menu_open: "開く",
    menu_reload: "再読み込み",
    menu_settings: "設定...",
    menu_quit: "終了",
    menu_stop: "停止",
    tip_stopped: "Togglite (停止中)",

    cue_description: "何をしていますか？",
    status_syncing: "同期中…",
    status_tracking: "計測中",
    status_stopped: "停止中",
    no_project: "プロジェクトなし",
    no_description: "(説明なし)",
    start: "開始",
    stop: "停止",
    recent_entries: "最近のエントリ",
    no_entries: "エントリがありません",
    need_token: "設定から API トークンを登録してください",

    history: "履歴",
    today: "今日",
    yesterday: "昨日",
    edit_entry: "エントリを編集",
    delete: "エントリを削除",
    delete_confirm: "もう一度クリックで削除",
    saving: "保存中…",
    weekdays: ["日", "月", "火", "水", "木", "金", "土"],
    months: ["1月", "2月", "3月", "4月", "5月", "6月", "7月", "8月", "9月", "10月", "11月", "12月"],

    settings: "設定",
    cue_token: "API トークンを貼り付け",
    token_label: "Toggl API トークン",
    token_hint: "Toggl の Profile settings ページ下部にある API Token をコピーして貼り付けてください。",
    open_profile: "track.toggl.com/profile を開く ↗",
    language: "言語",
    language_auto: "自動",
    checking: "確認中…",
    save: "保存",

    err_token_empty: "API トークンを入力してください",
    err_save_failed: "設定の保存に失敗",
    err_init_failed: "初期化に失敗",
    err_not_synced: "まだ同期されていません",
    err_rate_limit: "Toggl API の 1 時間あたりの上限に達しました。しばらく待ってください",
    err_bad_time: "時刻は HH:MM 形式で入力してください",
    err_parse: "応答の解析に失敗",
    err_auth: "認証エラー: API トークンを確認してください",
    err_network: "通信エラー",
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tags_parse_leniently() {
        assert_eq!(Lang::from_tag("ja"), Some(Lang::Ja));
        assert_eq!(Lang::from_tag("ja-JP"), Some(Lang::Ja));
        assert_eq!(Lang::from_tag("JA_jp"), Some(Lang::Ja));
        assert_eq!(Lang::from_tag(" en-US "), Some(Lang::En));
        assert_eq!(Lang::from_tag("fr"), None);
        assert_eq!(Lang::from_tag(""), None);
        for l in Lang::ALL {
            assert_eq!(Lang::from_tag(l.tag()), Some(l));
        }
    }

    #[test]
    fn preference_drives_current_language() {
        // Explicit choice wins regardless of the system language.
        set_preference(Some(Lang::Ja));
        assert_eq!(preference(), Some(Lang::Ja));
        assert_eq!(current(), Lang::Ja);
        assert_eq!(t().save, "保存");
        set_preference(Some(Lang::En));
        assert_eq!(current(), Lang::En);
        assert_eq!(t().save, "Save");
        // Automatic falls back to the system language.
        set_preference(None);
        assert_eq!(preference(), None);
        assert_eq!(current(), system_lang());
    }

    #[test]
    fn date_labels() {
        assert_eq!(date_label_in(Lang::En, 9, 17, 3), "Wed, Sep 17");
        assert_eq!(date_label_in(Lang::Ja, 9, 17, 3), "9月17日 (水)");
        assert_eq!(date_label_in(Lang::En, 1, 1, 0), "Sun, Jan 1");
    }
}
