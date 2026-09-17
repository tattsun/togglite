#![windows_subsystem = "windows"]

mod api;
mod config;
mod i18n;
mod ui;
mod util;

use api::{Client, Project, TimeEntry};
use i18n::t;
use native_windows_gui as nwg;
use std::cell::RefCell;
use winapi::ctypes::c_void;
use std::rc::Rc;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::thread;
use std::time::Duration;
use ui::{
    Action, EditorView, HistoryRow, HistoryView, Host, Page, Popup, PopupBinding, ProjectView,
    RecentView, Rgb, RunningView, View,
};
use util::{fmt_hms, from_local, now_unix, parse_hm, parse_rfc3339, to_local, truncate};
use winapi::shared::minwindef::{LPARAM, LRESULT, UINT, WPARAM};
use winapi::shared::windef::HWND;
use winapi::um::winuser::*;

const HOTKEY_ID: i32 = 1;

enum Msg {
    Synced {
        /// Freshly fetched from `/me`; None when the cached one was used.
        workspace_id: Option<i64>,
        entries: Vec<TimeEntry>,
        projects: Vec<Project>,
    },
    Started(TimeEntry),
    Stopped(TimeEntry),
    Updated(TimeEntry),
    Deleted(i64),
    Error(String),
}

#[derive(Clone, PartialEq)]
struct RecentItem {
    description: String,
    project_id: Option<i64>,
}

struct State {
    client: Option<Client>,
    workspace_id: i64,
    current: Option<TimeEntry>,
    /// Every entry of the last 30 days (the running one included), newest first.
    entries: Vec<TimeEntry>,
    /// Quick-restart list derived from `entries`.
    recent: Vec<RecentItem>,
    projects: Vec<Project>,
    selected_project: Option<i64>,
    /// Snapshot of the entry open in the editor; the fields live in the popup's EDIT controls.
    editing: Option<TimeEntry>,
    edit_project: Option<i64>,
    /// Page the editor was opened from, to return to.
    edit_from: Page,
    /// The delete button was clicked once; the next click deletes.
    delete_armed: bool,
    busy: bool,
    last_error: Option<String>,
}

impl Default for State {
    fn default() -> Self {
        State {
            client: None,
            workspace_id: 0,
            current: None,
            entries: Vec::new(),
            recent: Vec::new(),
            projects: Vec::new(),
            selected_project: None,
            editing: None,
            edit_project: None,
            edit_from: Page::Main,
            delete_armed: false,
            busy: false,
            last_error: None,
        }
    }
}

impl State {
    fn project(&self, id: Option<i64>) -> Option<&Project> {
        let id = id?;
        self.projects.iter().find(|p| p.id == id)
    }

    fn project_view(&self, id: Option<i64>) -> Option<ProjectView> {
        self.project(id).map(|p| ProjectView {
            name: p.name.clone(),
            color: p.color.as_deref().and_then(Rgb::from_hex),
        })
    }

    /// Keeps `entries` newest first and re-derives the quick-restart list.
    fn entries_changed(&mut self) {
        self.entries
            .sort_by_key(|e| std::cmp::Reverse(parse_rfc3339(&e.start).unwrap_or(0)));
        self.recent = dedupe_recent(&self.entries);
    }

    fn upsert(&mut self, entry: TimeEntry) {
        match self.entries.iter_mut().find(|e| e.id == entry.id) {
            Some(e) => *e = entry,
            None => self.entries.push(entry),
        }
        self.entries_changed();
    }

    fn entry(&self, id: i64) -> Option<&TimeEntry> {
        self.entries
            .iter()
            .find(|e| e.id == id)
            .or(self.current.as_ref().filter(|c| c.id == id))
    }
}

struct App {
    icon_idle: nwg::Icon,
    icon_run: nwg::Icon,

    // Hidden message-only window that owns the tray icon, menu, timer and notice.
    msg_window: nwg::MessageWindow,
    tray: nwg::TrayNotification,
    menu: nwg::Menu,
    mi_toggle: nwg::MenuItem,
    mi_open: nwg::MenuItem,
    mi_sync: nwg::MenuItem,
    mi_settings: nwg::MenuItem,
    mi_quit: nwg::MenuItem,
    timer: nwg::AnimationTimer,
    notice: nwg::Notice,

    popup: Popup,

    state: RefCell<State>,
    /// Description the user was typing before the settings page or the editor took over the field.
    draft: RefCell<String>,
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
}

impl App {
    fn build() -> Result<Rc<App>, nwg::NwgError> {
        let embed = nwg::EmbedResource::load(None)?;
        let icon_idle = embed.icon_str("IDI_IDLE", None).expect("IDI_IDLE icon");
        let icon_run = embed.icon_str("IDI_RUN", None).expect("IDI_RUN icon");

        let mut msg_window = nwg::MessageWindow::default();
        nwg::MessageWindow::builder().build(&mut msg_window)?;

        let mut tray = nwg::TrayNotification::default();
        nwg::TrayNotification::builder()
            .parent(&msg_window)
            .icon(Some(&icon_idle))
            .tip(Some("Togglite"))
            .build(&mut tray)?;

        let mut menu = nwg::Menu::default();
        nwg::Menu::builder()
            .popup(true)
            .parent(&msg_window)
            .build(&mut menu)?;

        let mut mi_toggle = nwg::MenuItem::default();
        let mut mi_open = nwg::MenuItem::default();
        let mut mi_sync = nwg::MenuItem::default();
        let mut mi_settings = nwg::MenuItem::default();
        let mut mi_quit = nwg::MenuItem::default();
        let mut sep1 = nwg::MenuSeparator::default();
        let mut sep2 = nwg::MenuSeparator::default();
        nwg::MenuItem::builder().text(t().menu_start).parent(&menu).build(&mut mi_toggle)?;
        let open_label = format!("{}\tCtrl+Alt+T", t().menu_open);
        nwg::MenuItem::builder().text(&open_label).parent(&menu).build(&mut mi_open)?;
        nwg::MenuSeparator::builder().parent(&menu).build(&mut sep1)?;
        nwg::MenuItem::builder().text(t().menu_reload).parent(&menu).build(&mut mi_sync)?;
        nwg::MenuItem::builder().text(t().menu_settings).parent(&menu).build(&mut mi_settings)?;
        nwg::MenuSeparator::builder().parent(&menu).build(&mut sep2)?;
        nwg::MenuItem::builder().text(t().menu_quit).parent(&menu).build(&mut mi_quit)?;

        let mut timer = nwg::AnimationTimer::default();
        nwg::AnimationTimer::builder()
            .parent(&msg_window)
            .interval(Duration::from_millis(1000))
            .active(true)
            .build(&mut timer)?;

        let mut notice = nwg::Notice::default();
        nwg::Notice::builder().parent(&msg_window).build(&mut notice)?;

        let popup = Popup::new();
        let (tx, rx) = channel();

        Ok(Rc::new(App {
            icon_idle,
            icon_run,
            msg_window,
            tray,
            menu,
            mi_toggle,
            mi_open,
            mi_sync,
            mi_settings,
            mi_quit,
            timer,
            notice,
            popup,
            state: RefCell::new(State::default()),
            draft: RefCell::new(String::new()),
            tx,
            rx,
        }))
    }

    fn dispatch(host: *const c_void, hwnd: HWND, msg: UINT, w: WPARAM, l: LPARAM) -> LRESULT {
        let app = unsafe { &*(host as *const App) };
        app.popup.handle(app, hwnd, msg, w, l)
    }

    fn bind(app: &Rc<App>) -> nwg::EventHandler {
        let weak = Rc::downgrade(app);
        nwg::full_bind_event_handler(&app.msg_window.handle, move |evt, _data, h| {
            if let Some(app) = weak.upgrade() {
                app.on_event(evt, h);
            }
        })
    }

    fn bind_hotkey(app: &Rc<App>) -> nwg::RawEventHandler {
        // WM_HOTKEY is delivered to the window that registered it.
        let weak = Rc::downgrade(app);
        nwg::bind_raw_event_handler(&app.msg_window.handle, 0x10000, move |_, msg, _, _| {
            if msg == WM_HOTKEY {
                if let Some(app) = weak.upgrade() {
                    app.toggle_popup();
                }
            }
            None
        })
        .expect("bind hotkey handler")
    }

    fn on_event(&self, evt: nwg::Event, handle: nwg::ControlHandle) {
        use nwg::Event as E;
        match evt {
            E::OnContextMenu if handle == self.tray => self.show_menu(),
            E::OnMousePress(nwg::MousePressEvent::MousePressLeftUp) if handle == self.tray => {
                self.toggle_popup()
            }
            E::OnMenuItemSelected => {
                if handle == self.mi_toggle {
                    self.toggle_timer();
                } else if handle == self.mi_open {
                    self.show_popup();
                } else if handle == self.mi_sync {
                    self.sync();
                } else if handle == self.mi_settings {
                    self.open_settings();
                    self.show_popup();
                } else if handle == self.mi_quit {
                    nwg::stop_thread_dispatch();
                }
            }
            E::OnTimerTick if handle == self.timer => self.tick(),
            E::OnNotice if handle == self.notice => self.drain(),
            _ => {}
        }
    }

    // ---- UI actions ----

    fn show_menu(&self) {
        let mut pt = winapi::shared::windef::POINT { x: 0, y: 0 };
        unsafe {
            GetCursorPos(&mut pt);
        }
        if let Some(hwnd) = self.msg_window.handle.hwnd() {
            unsafe {
                // Required so the popup menu dismisses when clicking elsewhere.
                SetForegroundWindow(hwnd);
            }
        }
        self.menu.popup(pt.x, pt.y);
        if let Some(hwnd) = self.msg_window.handle.hwnd() {
            unsafe {
                PostMessageW(hwnd, WM_NULL, 0, 0);
            }
        }
    }

    fn toggle_popup(&self) {
        if self.popup.visible() {
            self.popup.hide();
        } else {
            self.show_popup();
        }
    }

    fn show_popup(&self) {
        if self.state.borrow().client.is_none() {
            self.open_settings();
        }
        self.popup.show();
    }

    fn open_settings(&self) {
        match self.popup.page() {
            Page::Settings => return,
            // The draft was stashed when the editor opened; just drop the edit.
            Page::Edit => {
                let mut st = self.state.borrow_mut();
                st.editing = None;
                st.delete_armed = false;
            }
            Page::Main | Page::History => *self.draft.borrow_mut() = self.popup.edit_text(),
        }
        self.popup.set_page(Page::Settings);
        self.popup.set_edit_text(&config::load().api_token);
        self.popup.focus_edit();
    }

    fn open_history(&self) {
        self.popup.set_page(Page::History);
        self.popup.focus_edit();
    }

    fn close_history(&self) {
        self.popup.set_page(Page::Main);
        self.popup.focus_edit();
    }

    /// Opens the editor for an entry (the running one included) from the main or history page.
    fn open_editor(&self, id: i64) {
        let entry = match self.state.borrow().entry(id) {
            Some(e) => e.clone(),
            None => return,
        };
        let start = parse_rfc3339(&entry.start).unwrap_or_else(now_unix);
        let stop_text = if entry.is_running() {
            String::new()
        } else {
            let stop = entry.stop.as_deref().and_then(parse_rfc3339).unwrap_or(start + entry.duration);
            to_local(stop).hm()
        };
        let from = self.popup.page();
        // The description field is reused for the entry, so keep what the user was typing.
        *self.draft.borrow_mut() = self.popup.edit_text();
        {
            let mut st = self.state.borrow_mut();
            st.edit_project = entry.project_id;
            st.edit_from = if from == Page::History { Page::History } else { Page::Main };
            st.editing = Some(entry.clone());
            st.delete_armed = false;
            st.last_error = None;
        }
        self.popup.set_page(Page::Edit);
        self.popup.set_edit_text(entry.desc());
        self.popup.set_time_texts(&to_local(start).hm(), &stop_text);
        self.popup.focus_edit();
    }

    /// Leaves the editor for the page it was opened from and restores the draft.
    fn close_editor(&self) {
        let from = {
            let mut st = self.state.borrow_mut();
            st.editing = None;
            st.delete_armed = false;
            st.last_error = None;
            st.edit_from
        };
        self.popup.set_page(from);
        self.popup.set_edit_text(&self.draft.borrow());
        self.popup.focus_edit();
    }

    fn save_entry(&self) {
        if self.state.borrow().busy {
            return;
        }
        let entry = match self.state.borrow().editing.clone() {
            Some(e) => e,
            None => return,
        };
        let (start_text, stop_text) = self.popup.time_texts();
        let (start, stop) = match edited_times(&entry, &start_text, &stop_text) {
            Some(t) => t,
            None => {
                self.state.borrow_mut().last_error = Some(t().err_bad_time.into());
                self.popup.invalidate();
                return;
            }
        };
        let description = self.popup.edit_text().trim().to_string();
        let project_id = self.state.borrow().edit_project;
        let (wid, id) = (entry.workspace_id, entry.id);
        self.spawn(move |c| {
            c.update(wid, id, &description, project_id, start, stop)
                .map(Msg::Updated)
        });
    }

    /// First click arms the button (it turns red and asks again); the second deletes.
    fn delete_entry(&self) {
        if self.state.borrow().busy {
            return;
        }
        let entry = match self.state.borrow().editing.clone() {
            Some(e) => e,
            None => return,
        };
        if !self.state.borrow().delete_armed {
            self.state.borrow_mut().delete_armed = true;
            self.popup.invalidate();
            return;
        }
        self.spawn(move |c| {
            c.delete(entry.workspace_id, entry.id)
                .map(|_| Msg::Deleted(entry.id))
        });
    }

    fn close_settings(&self) {
        if self.state.borrow().client.is_none() {
            self.popup.hide();
            return;
        }
        self.popup.set_page(Page::Main);
        self.popup.set_edit_text(&self.draft.borrow());
        self.popup.focus_edit();
    }

    fn save_settings(&self) {
        let token = self.popup.edit_text().trim().to_string();
        if token.is_empty() {
            self.state.borrow_mut().last_error = Some(t().err_token_empty.into());
            self.popup.invalidate();
            return;
        }
        let mut cfg = config::load();
        if cfg.api_token.trim() != token {
            // Another account may have another default workspace.
            cfg.workspace_id = None;
            self.state.borrow_mut().workspace_id = 0;
        }
        cfg.api_token = token.clone();
        if let Err(e) = config::save(&cfg) {
            self.state.borrow_mut().last_error = Some(format!("{}: {e}", t().err_save_failed));
            self.popup.invalidate();
            return;
        }
        self.state.borrow_mut().last_error = None;
        self.set_token(&token);
        self.popup.set_page(Page::Main);
        self.popup.set_edit_text(&self.draft.borrow());
        self.popup.focus_edit();
        self.sync();
    }

    fn set_token(&self, token: &str) {
        match Client::new(token) {
            Ok(c) => self.state.borrow_mut().client = Some(c),
            Err(e) => self.notify(&format!("{}: {e}", t().err_init_failed), true),
        }
    }

    fn notify(&self, text: &str, error: bool) {
        let flags = if error {
            nwg::TrayNotificationFlags::ERROR_ICON
        } else {
            nwg::TrayNotificationFlags::INFO_ICON
        };
        self.tray.show(text, Some("Togglite"), Some(flags), None);
    }

    /// Project chip on the main page (next entry) or in the editor (entry being edited).
    fn pick_project(&self) {
        let editing = self.popup.page() == Page::Edit;
        let (names, current) = {
            let st = self.state.borrow();
            let names: Vec<String> = st.projects.iter().map(|p| p.name.clone()).collect();
            let selected = if editing { st.edit_project } else { st.selected_project };
            let current = selected.and_then(|id| st.projects.iter().position(|p| p.id == id));
            (names, current)
        };
        if let Some(choice) = self.popup.pick_project(&names, current) {
            let mut st = self.state.borrow_mut();
            let id = choice.and_then(|i| st.projects.get(i).map(|p| p.id));
            if editing {
                st.edit_project = id;
            } else {
                st.selected_project = id;
            }
        }
        self.popup.invalidate();
        self.popup.focus_edit();
    }

    fn pick_language(&self) {
        use i18n::Lang;
        let names: Vec<String> = Lang::ALL.iter().map(|l| l.native_name().to_string()).collect();
        let current = i18n::preference().and_then(|p| Lang::ALL.iter().position(|l| *l == p));
        if let Some(choice) = self.popup.pick_language(&names, current) {
            let pref = choice.and_then(|i| Lang::ALL.get(i).copied());
            if pref != i18n::preference() {
                let mut cfg = config::load();
                cfg.language = pref.map(|l| l.tag().to_string());
                let _ = config::save(&cfg);
                i18n::set_preference(pref);
                self.apply_language();
            }
        }
        self.popup.invalidate();
        self.popup.focus_edit();
    }

    /// Re-renders everything that was built with the previous language.
    fn apply_language(&self) {
        set_menu_item_text(&self.mi_open, &format!("{}\tCtrl+Alt+T", t().menu_open));
        set_menu_item_text(&self.mi_sync, t().menu_reload);
        set_menu_item_text(&self.mi_settings, t().menu_settings);
        set_menu_item_text(&self.mi_quit, t().menu_quit);
        // A message from the old language would look out of place.
        self.state.borrow_mut().last_error = None;
        self.popup.refresh_text();
        self.refresh_ui();
    }

    fn toggle_timer(&self) {
        let running = self.state.borrow().current.as_ref().map(|c| c.id);
        match running {
            Some(id) => self.stop_entry(id),
            None => self.start_from_form(),
        }
    }

    fn start_from_form(&self) {
        if self.state.borrow().current.is_some() {
            return;
        }
        let description = self.popup.edit_text().trim().to_string();
        let project_id = self.state.borrow().selected_project;
        self.start_entry(description, project_id);
    }

    fn start_recent(&self, idx: usize) {
        let item = match self.state.borrow().recent.get(idx) {
            Some(r) => r.clone(),
            None => return,
        };
        self.start_entry(item.description, item.project_id);
    }

    fn start_entry(&self, description: String, project_id: Option<i64>) {
        if self.state.borrow().busy {
            return;
        }
        let wid = self.state.borrow().workspace_id;
        if wid == 0 {
            self.state.borrow_mut().last_error = Some(t().err_not_synced.into());
            self.sync();
            return;
        }
        self.spawn(move |c| c.start(wid, &description, project_id).map(Msg::Started));
    }

    fn stop_entry(&self, id: i64) {
        if self.state.borrow().busy {
            return;
        }
        let wid = {
            let st = self.state.borrow();
            st.current
                .as_ref()
                .map(|c| c.workspace_id)
                .filter(|w| *w > 0)
                .unwrap_or(st.workspace_id)
        };
        self.spawn(move |c| c.stop(wid, id).map(Msg::Stopped));
    }

    /// Two requests (three when the default workspace is not cached yet): the plan quota is
    /// small (30/h on the free plan), so `/me` and `/me/time_entries/current` are avoided.
    fn sync(&self) {
        if self.state.borrow().busy {
            return;
        }
        let known_wid = self.state.borrow().workspace_id;
        self.spawn(move |c| {
            let workspace_id = if known_wid > 0 { None } else { Some(c.me()?.default_workspace_id) };
            let entries = c.recent()?;
            let projects = c.projects()?;
            Ok(Msg::Synced {
                workspace_id,
                entries,
                projects,
            })
        });
    }

    fn spawn<F>(&self, f: F)
    where
        F: FnOnce(&Client) -> Result<Msg, String> + Send + 'static,
    {
        let client = match self.state.borrow().client.clone() {
            Some(c) => c,
            None => {
                self.open_settings();
                self.popup.show();
                return;
            }
        };
        self.state.borrow_mut().busy = true;
        self.refresh_ui();
        let tx = self.tx.clone();
        let sender = self.notice.sender();
        thread::spawn(move || {
            let msg = f(&client).unwrap_or_else(Msg::Error);
            let _ = tx.send(msg);
            sender.notice();
        });
    }

    fn drain(&self) {
        while let Ok(msg) = self.rx.try_recv() {
            let mut st = self.state.borrow_mut();
            st.busy = false;
            match msg {
                Msg::Synced {
                    workspace_id,
                    entries,
                    projects,
                } => {
                    if let Some(wid) = workspace_id {
                        st.workspace_id = wid;
                        let mut cfg = config::load();
                        cfg.workspace_id = Some(wid);
                        let _ = config::save(&cfg);
                    }
                    st.projects = projects
                        .into_iter()
                        .filter(|p| p.active.unwrap_or(true))
                        .collect();
                    st.entries = entries;
                    st.entries_changed();
                    // The running entry, if any, is the newest one still open.
                    st.current = st.entries.iter().find(|e| e.is_running()).cloned();
                    st.last_error = None;
                }
                Msg::Started(entry) => {
                    st.current = Some(entry.clone());
                    st.upsert(entry);
                    st.selected_project = None;
                    st.last_error = None;
                    drop(st);
                    self.popup.set_edit_text("");
                }
                Msg::Stopped(entry) => {
                    st.current = None;
                    st.upsert(entry);
                    st.last_error = None;
                }
                Msg::Updated(entry) => {
                    if st.current.as_ref().map(|c| c.id) == Some(entry.id) {
                        st.current = Some(entry.clone()).filter(|e| e.is_running());
                    }
                    let done = st.editing.as_ref().map(|e| e.id) == Some(entry.id);
                    st.upsert(entry);
                    st.last_error = None;
                    drop(st);
                    if done && self.popup.page() == Page::Edit {
                        self.close_editor();
                    }
                }
                Msg::Deleted(id) => {
                    if st.current.as_ref().map(|c| c.id) == Some(id) {
                        st.current = None;
                    }
                    let done = st.editing.as_ref().map(|e| e.id) == Some(id);
                    st.entries.retain(|e| e.id != id);
                    st.entries_changed();
                    st.last_error = None;
                    drop(st);
                    if done && self.popup.page() == Page::Edit {
                        self.close_editor();
                    }
                }
                Msg::Error(e) => {
                    st.last_error = Some(e.clone());
                    drop(st);
                    if !self.popup.visible() {
                        self.notify(&e, true);
                    }
                }
            }
        }
        self.refresh_ui();
    }

    fn refresh_ui(&self) {
        let st = self.state.borrow();
        let now = now_unix();
        let busy = st.busy;

        let (tip, menu_text) = match &st.current {
            Some(c) => {
                let elapsed = fmt_hms(c.elapsed(now));
                let d = if c.desc().is_empty() { t().no_description } else { c.desc() };
                let proj = st
                    .project(c.project_id)
                    .map(|p| format!("  ·  {}", p.name))
                    .unwrap_or_default();
                (
                    format!("Togglite ▶ {} ({elapsed}){proj}", truncate(d, 60)),
                    format!("{}: {} ({elapsed})", t().menu_stop, truncate(d, 40)),
                )
            }
            None => (t().tip_stopped.to_string(), t().menu_start.to_string()),
        };
        let running = st.current.is_some();
        drop(st);

        self.tray.set_icon(if running { &self.icon_run } else { &self.icon_idle });
        self.tray.set_tip(&truncate(&tip, 120));
        set_menu_item_text(&self.mi_toggle, &menu_text);
        self.mi_toggle.set_enabled(!busy);
        self.mi_sync.set_enabled(!busy);
        self.popup.invalidate();
    }

    /// `TOGGLITE_DEMO=idle|running` seeds sample data so the UI can be reviewed without an account.
    fn seed_demo(&self, mode: &str) {
        let mut st = self.state.borrow_mut();
        st.client = Client::new("demo").ok();
        st.workspace_id = 1;
        // Sample text in the UI language so screenshots read naturally.
        let (projects, recent): ([&str; 4], [&str; 8]) = match i18n::current() {
            i18n::Lang::Ja => (
                ["Togglite", "社内ツール改善", "読書/学習", "Side project"],
                [
                    "トレイ UI の再設計",
                    "週次ミーティング",
                    "API クライアントのテスト",
                    "CI パイプラインの調査",
                    "Rust の本を読む",
                    "ブログ下書き",
                    "メール返信",
                    "デザインレビュー",
                ],
            ),
            i18n::Lang::En => (
                ["Togglite", "Internal tools", "Reading / learning", "Side project"],
                [
                    "Redesign the tray UI",
                    "Weekly meeting",
                    "API client tests",
                    "Investigate CI pipeline",
                    "Read the Rust book",
                    "Blog post draft",
                    "Reply to emails",
                    "Design review",
                ],
            ),
        };
        let colors = ["#e55ca8", "#3d8bff", "#f5a623", "#2ec4b6"];
        st.projects = projects
            .iter()
            .zip(colors)
            .enumerate()
            .map(|(i, (name, color))| Project {
                id: i as i64 + 1,
                name: name.to_string(),
                color: Some(color.to_string()),
                active: Some(true),
            })
            .collect();
        // (description, project, hours ago, minutes long): three days, a couple of repeats.
        let sample: [(usize, Option<i64>, i64, i64); 10] = [
            (0, Some(1), 2, 45),
            (1, None, 5, 30),
            (2, Some(1), 7, 80),
            (0, Some(1), 9, 50),
            (3, Some(2), 26, 40),
            (4, Some(3), 29, 65),
            (5, Some(4), 31, 25),
            (6, None, 50, 15),
            (7, Some(2), 53, 90),
            (1, None, 55, 20),
        ];
        let now = now_unix();
        st.entries = sample
            .iter()
            .enumerate()
            .map(|(i, &(d, p, ago, mins))| {
                let start = now - ago * 3600;
                TimeEntry {
                    id: i as i64 + 1,
                    workspace_id: 1,
                    description: Some(recent[d].into()),
                    project_id: p,
                    start: util::rfc3339_utc(start),
                    stop: Some(util::rfc3339_utc(start + mins * 60)),
                    duration: mins * 60,
                    server_deleted_at: None,
                }
            })
            .collect();
        if mode == "running" {
            let current = TimeEntry {
                id: 100,
                workspace_id: 1,
                description: Some(recent[0].into()),
                project_id: Some(1),
                start: util::rfc3339_utc(now - 754),
                stop: None,
                duration: -1,
                server_deleted_at: None,
            };
            st.entries.insert(0, current.clone());
            st.current = Some(current);
        }
        st.entries_changed();
    }

    fn tick(&self) {
        let running = self.state.borrow().current.is_some();
        if running {
            self.refresh_ui();
        }
    }
}

impl Host for App {
    fn view(&self) -> View {
        let st = self.state.borrow();
        let now = now_unix();
        View {
            busy: st.busy,
            error: st.last_error.clone(),
            has_token: st.client.is_some(),
            running: st.current.as_ref().map(|c| RunningView {
                id: c.id,
                desc: c.desc().to_string(),
                project: st.project_view(c.project_id),
                elapsed: fmt_hms(c.elapsed(now)),
            }),
            project: st.project_view(st.selected_project),
            recent: st
                .recent
                .iter()
                .map(|r| RecentView {
                    desc: r.description.clone(),
                    project: st.project_view(r.project_id),
                })
                .collect(),
            language: match i18n::preference() {
                Some(l) => l.native_name().to_string(),
                None => format!("{} ({})", t().language_auto, i18n::current().native_name()),
            },
        }
    }

    fn history(&self) -> HistoryView {
        let st = self.state.borrow();
        let now = now_unix();
        let today = to_local(now).days();
        let mut rows: Vec<HistoryRow> = Vec::new();
        let mut day: Option<i64> = None;
        let mut header = 0usize;
        let mut total = 0i64;
        let close_day = |rows: &mut Vec<HistoryRow>, header: usize, total: i64| {
            if let Some(HistoryRow::Day { total: t, .. }) = rows.get_mut(header) {
                *t = fmt_hms(total);
            }
        };
        for e in &st.entries {
            let start = parse_rfc3339(&e.start).unwrap_or(0);
            let lt = to_local(start);
            if day != Some(lt.days()) {
                close_day(&mut rows, header, total);
                let label = match today - lt.days() {
                    0 => t().today.to_string(),
                    1 => t().yesterday.to_string(),
                    _ => i18n::date_label(lt.month, lt.day, lt.weekday),
                };
                header = rows.len();
                rows.push(HistoryRow::Day { label, total: String::new() });
                day = Some(lt.days());
                total = 0;
            }
            let elapsed = e.elapsed(now);
            total += elapsed;
            let range = if e.is_running() {
                format!("{} –", lt.hm())
            } else {
                let stop = e.stop.as_deref().and_then(parse_rfc3339).unwrap_or(start + e.duration);
                format!("{} – {}", lt.hm(), to_local(stop).hm())
            };
            rows.push(HistoryRow::Entry {
                id: e.id,
                desc: e.desc().to_string(),
                project: st.project_view(e.project_id),
                range,
                duration: fmt_hms(elapsed),
                running: e.is_running(),
            });
        }
        close_day(&mut rows, header, total);
        HistoryView {
            busy: st.busy,
            error: st.last_error.clone(),
            has_token: st.client.is_some(),
            rows,
        }
    }

    fn editor(&self) -> EditorView {
        let st = self.state.borrow();
        let now = now_unix();
        let (start_text, stop_text) = self.popup.time_texts();
        let (running, date, duration) = match &st.editing {
            Some(e) => {
                let lt = to_local(parse_rfc3339(&e.start).unwrap_or(now));
                let duration = edited_times(e, &start_text, &stop_text)
                    .map(|(start, stop)| fmt_hms(stop.unwrap_or(now) - start));
                (e.is_running(), i18n::date_label(lt.month, lt.day, lt.weekday), duration)
            }
            None => (false, String::new(), None),
        };
        EditorView {
            busy: st.busy,
            error: st.last_error.clone(),
            project: st.project_view(st.edit_project),
            running,
            date,
            duration,
            delete_armed: st.delete_armed,
        }
    }

    fn perform(&self, action: Action) {
        // Anything but the confirming click disarms the delete button.
        if action != Action::DeleteEntry && self.state.borrow().delete_armed {
            self.state.borrow_mut().delete_armed = false;
            self.popup.invalidate();
        }
        match action {
            Action::Start => self.start_from_form(),
            Action::Stop => {
                let id = self.state.borrow().current.as_ref().map(|c| c.id);
                if let Some(id) = id {
                    self.stop_entry(id);
                }
            }
            Action::Refresh => self.sync(),
            Action::OpenSettings => self.open_settings(),
            Action::CloseSettings => self.close_settings(),
            Action::OpenHistory => self.open_history(),
            Action::CloseHistory => self.close_history(),
            Action::EditEntry(id) => self.open_editor(id),
            Action::CloseEditor => self.close_editor(),
            Action::SaveEntry => self.save_entry(),
            Action::DeleteEntry => self.delete_entry(),
            Action::PickProject => self.pick_project(),
            Action::PickLanguage => self.pick_language(),
            Action::Recent(i) => self.start_recent(i),
            Action::Save => self.save_settings(),
            Action::OpenTokenPage => ui::open_url("https://track.toggl.com/profile"),
            Action::Activate => self.show_popup(),
            Action::Quit => nwg::stop_thread_dispatch(),
        }
    }

    fn moved(&self, x: i32, y: i32) {
        let mut cfg = config::load();
        cfg.popup_x = Some(x);
        cfg.popup_y = Some(y);
        let _ = config::save(&cfg);
    }
}

/// Start and stop implied by the editor's time fields for `entry`, or None if they don't parse.
/// A field left as displayed keeps its original timestamp (seconds included); a changed one is
/// read as HH:MM on the entry's local date, the stop rolling over to the next day when it falls
/// before the start. A running entry has no stop.
fn edited_times(entry: &TimeEntry, start_text: &str, stop_text: &str) -> Option<(i64, Option<i64>)> {
    let orig_start = parse_rfc3339(&entry.start)?;
    let start = resolve_time(start_text, orig_start)?;
    if entry.is_running() {
        return Some((start, None));
    }
    let orig_stop = entry
        .stop
        .as_deref()
        .and_then(parse_rfc3339)
        .unwrap_or(orig_start + entry.duration);
    if stop_text.trim() == to_local(orig_stop).hm() && orig_stop >= start {
        return Some((start, Some(orig_stop)));
    }
    let (h, m) = parse_hm(stop_text)?;
    let d = to_local(start);
    let mut stop = from_local(d.year, d.month, d.day, h, m, 0);
    if stop < start {
        stop += 86400;
    }
    Some((start, Some(stop)))
}

fn resolve_time(text: &str, orig: i64) -> Option<i64> {
    let lt = to_local(orig);
    if text.trim() == lt.hm() {
        return Some(orig);
    }
    let (h, m) = parse_hm(text)?;
    Some(from_local(lt.year, lt.month, lt.day, h, m, 0))
}

/// `entries` must be newest first.
fn dedupe_recent(entries: &[TimeEntry]) -> Vec<RecentItem> {
    let mut out: Vec<RecentItem> = Vec::new();
    for e in entries {
        let item = RecentItem {
            description: e.desc().trim().to_string(),
            project_id: e.project_id,
        };
        if item.description.is_empty() && item.project_id.is_none() {
            continue;
        }
        if !out.contains(&item) {
            out.push(item);
        }
        if out.len() >= 30 {
            break;
        }
    }
    out
}

fn set_menu_item_text(item: &nwg::MenuItem, text: &str) {
    if let nwg::ControlHandle::MenuItem(hmenu, id) = item.handle {
        let wide = ui::wide(text);
        unsafe {
            let mut info: MENUITEMINFOW = std::mem::zeroed();
            info.cbSize = std::mem::size_of::<MENUITEMINFOW>() as u32;
            info.fMask = MIIM_STRING;
            info.dwTypeData = wide.as_ptr() as *mut u16;
            SetMenuItemInfoW(hmenu, id, 0, &info);
        }
    }
}

/// Returns false if Togglite is already running; that instance is asked to show its popup instead.
/// The mutex handle is intentionally leaked so it lives as long as the process.
fn claim_single_instance() -> bool {
    use winapi::shared::winerror::ERROR_ALREADY_EXISTS;
    use winapi::um::errhandlingapi::GetLastError;
    use winapi::um::synchapi::CreateMutexW;
    unsafe {
        let mutex = CreateMutexW(std::ptr::null_mut(), 0, ui::wide("Local\\Togglite.SingleInstance").as_ptr());
        if !mutex.is_null() && GetLastError() == ERROR_ALREADY_EXISTS {
            let other = FindWindowW(ui::wide(ui::POPUP_CLASS).as_ptr(), std::ptr::null());
            if !other.is_null() {
                PostMessageW(other, ui::WM_TOGGLITE_SHOW, 0, 0);
            }
            return false;
        }
    }
    true
}

fn main() {
    if !claim_single_instance() {
        return;
    }
    nwg::init().expect("Failed to init Native Windows GUI");
    ui::init_gdiplus();
    ui::enable_dark_menus();
    // Menu labels are fixed at build time, so the language must be chosen first.
    i18n::init(config::load().language.as_deref());

    let app = App::build().expect("Failed to build UI");
    let handler = App::bind(&app);
    let hotkey_handler = App::bind_hotkey(&app);

    let binding = Box::new(PopupBinding {
        host: Rc::as_ptr(&app) as *const c_void,
        dispatch: App::dispatch,
    });
    app.popup.bind(&*binding);

    let hwnd = app.msg_window.handle.hwnd().expect("message window hwnd");
    unsafe {
        RegisterHotKey(
            hwnd,
            HOTKEY_ID,
            (MOD_CONTROL | MOD_ALT | MOD_NOREPEAT) as u32,
            b'T' as u32,
        );
    }

    if let Ok(mode) = std::env::var("TOGGLITE_DEMO") {
        app.seed_demo(&mode);
        app.refresh_ui();
        app.show_popup();
    } else {
        main_startup(&app);
    }

    nwg::dispatch_thread_events();
    shutdown(&app, hwnd, hotkey_handler, handler, binding);
}

fn main_startup(app: &Rc<App>) {
    let cfg = config::load();
    if cfg.legacy_plaintext {
        // Re-save once so the plaintext token becomes DPAPI-encrypted.
        let _ = config::save(&cfg);
    }
    if let (Some(x), Some(y)) = (cfg.popup_x, cfg.popup_y) {
        app.popup.set_pinned(Some((x, y)));
    }
    if cfg.api_token.trim().is_empty() {
        app.refresh_ui();
        app.show_popup();
    } else {
        app.state.borrow_mut().workspace_id = cfg.workspace_id.unwrap_or(0);
        app.set_token(&cfg.api_token);
        app.refresh_ui();
        app.sync();
    }
}

fn shutdown(
    app: &Rc<App>,
    hwnd: HWND,
    hotkey_handler: nwg::RawEventHandler,
    handler: nwg::EventHandler,
    binding: Box<PopupBinding>,
) {
    unsafe {
        UnregisterHotKey(hwnd, HOTKEY_ID);
    }
    app.popup.bind(std::ptr::null());
    let _ = nwg::unbind_raw_event_handler(&hotkey_handler);
    nwg::unbind_event_handler(&handler);
    drop(binding);
}

#[cfg(test)]
mod tests {
    use super::*;
    use util::rfc3339_utc;

    fn entry(start: i64, duration: i64) -> TimeEntry {
        TimeEntry {
            id: 1,
            workspace_id: 1,
            description: None,
            project_id: None,
            start: rfc3339_utc(start),
            stop: (duration >= 0).then(|| rfc3339_utc(start + duration)),
            duration,
            server_deleted_at: None,
        }
    }

    /// Unix time of `hh:mm:ss` on the same local day as `base`.
    fn at(base: i64, h: u32, m: u32, s: u32) -> i64 {
        let d = to_local(base);
        from_local(d.year, d.month, d.day, h, m, s)
    }

    #[test]
    fn unchanged_fields_keep_exact_timestamps() {
        // 2025-09-16 12:00 UTC: a plain weekday, no DST transition anywhere.
        let noon = 1758024000;
        let start = at(noon, 9, 30, 17);
        let e = entry(start, 2 * 3600 + 30);
        let (s, stop) = edited_times(&e, "09:30", "11:30").unwrap();
        assert_eq!(s, start);
        assert_eq!(stop, Some(start + 2 * 3600 + 30));
        // Surrounding whitespace doesn't count as a change.
        assert_eq!(edited_times(&e, " 09:30 ", "11:30 ").unwrap(), (s, stop));
    }

    #[test]
    fn changed_fields_are_read_on_the_entry_day() {
        let noon = 1758024000;
        let start = at(noon, 9, 30, 17);
        let e = entry(start, 2 * 3600);
        let (s, stop) = edited_times(&e, "8:45", "1215").unwrap();
        assert_eq!(s, at(noon, 8, 45, 0));
        assert_eq!(stop, Some(at(noon, 12, 15, 0)));
        // Only the start moved: the stop keeps its seconds.
        let (s, stop) = edited_times(&e, "09:00", "11:30").unwrap();
        assert_eq!(s, at(noon, 9, 0, 0));
        assert_eq!(stop, Some(start + 2 * 3600));
    }

    #[test]
    fn stop_before_start_rolls_to_next_day() {
        let noon = 1758024000;
        let start = at(noon, 23, 0, 0);
        let e = entry(start, 3600);
        let (_, stop) = edited_times(&e, "23:00", "00:30").unwrap();
        assert_eq!(stop, Some(at(noon, 0, 30, 0) + 86400));
        // Moving the start past the (unchanged) stop re-reads the stop on the new start's day.
        let e = entry(at(noon, 9, 0, 0), 3600);
        let (s, stop) = edited_times(&e, "11:00", "10:00").unwrap();
        assert_eq!(stop, Some(s + 23 * 3600));
    }

    #[test]
    fn running_entries_have_no_stop_and_bad_input_is_rejected() {
        let noon = 1758024000;
        let e = entry(at(noon, 9, 30, 0), -1);
        assert_eq!(edited_times(&e, "09:00", "").unwrap(), (at(noon, 9, 0, 0), None));
        assert_eq!(edited_times(&e, "9:", ""), None);
        let e = entry(at(noon, 9, 30, 0), 600);
        assert_eq!(edited_times(&e, "09:30", "x"), None);
        assert_eq!(edited_times(&e, "25:00", "09:40"), None);
    }

    #[test]
    fn recent_list_dedupes_newest_first() {
        let mut e1 = entry(1000, 60);
        e1.description = Some("a".into());
        let mut e2 = entry(2000, 60);
        e2.description = Some("b".into());
        let mut e3 = entry(3000, 60);
        e3.description = Some("a".into());
        let blank = entry(4000, 60);
        let mut st = State { entries: vec![e1, blank, e3, e2], ..State::default() };
        st.entries_changed();
        let descs: Vec<&str> = st.recent.iter().map(|r| r.description.as_str()).collect();
        assert_eq!(descs, ["a", "b"]);
        assert_eq!(st.entries[0].start, rfc3339_utc(4000));
    }
}
