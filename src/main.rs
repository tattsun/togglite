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
use ui::{Action, Host, Page, Popup, PopupBinding, ProjectView, RecentView, Rgb, RunningView, View};
use util::{fmt_hms, now_unix, truncate};
use winapi::shared::minwindef::{LPARAM, LRESULT, UINT, WPARAM};
use winapi::shared::windef::HWND;
use winapi::um::winuser::*;

const HOTKEY_ID: i32 = 1;

enum Msg {
    Synced {
        workspace_id: i64,
        current: Option<TimeEntry>,
        recent: Vec<TimeEntry>,
        projects: Vec<Project>,
    },
    Started(TimeEntry),
    Stopped,
    Error(String),
}

#[derive(Clone, PartialEq)]
struct RecentItem {
    description: String,
    project_id: Option<i64>,
}

#[derive(Default)]
struct State {
    client: Option<Client>,
    workspace_id: i64,
    current: Option<TimeEntry>,
    recent: Vec<RecentItem>,
    projects: Vec<Project>,
    selected_project: Option<i64>,
    busy: bool,
    last_error: Option<String>,
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
    /// Description the user was typing before switching to the settings page.
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
        if self.popup.page() == Page::Settings {
            return;
        }
        *self.draft.borrow_mut() = self.popup.edit_text();
        self.popup.set_page(Page::Settings);
        self.popup.set_edit_text(&config::load().api_token);
        self.popup.focus_edit();
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

    fn pick_project(&self) {
        let (names, current) = {
            let st = self.state.borrow();
            let names: Vec<String> = st.projects.iter().map(|p| p.name.clone()).collect();
            let current = st
                .selected_project
                .and_then(|id| st.projects.iter().position(|p| p.id == id));
            (names, current)
        };
        if let Some(choice) = self.popup.pick_project(&names, current) {
            let mut st = self.state.borrow_mut();
            st.selected_project = choice.and_then(|i| st.projects.get(i).map(|p| p.id));
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
        let wid = self.state.borrow().workspace_id;
        self.spawn(move |c| c.stop(wid, id).map(|_| Msg::Stopped));
    }

    fn sync(&self) {
        if self.state.borrow().busy {
            return;
        }
        self.spawn(|c| {
            let me = c.me()?;
            let current = c.current()?;
            let recent = c.recent()?;
            let projects = c.projects()?;
            Ok(Msg::Synced {
                workspace_id: me.default_workspace_id,
                current,
                recent,
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
                    current,
                    recent,
                    projects,
                } => {
                    st.workspace_id = workspace_id;
                    st.current = current.filter(|c| c.is_running());
                    st.projects = projects
                        .into_iter()
                        .filter(|p| p.active.unwrap_or(true))
                        .collect();
                    st.recent = dedupe_recent(recent);
                    st.last_error = None;
                }
                Msg::Started(entry) => {
                    let item = RecentItem {
                        description: entry.desc().to_string(),
                        project_id: entry.project_id,
                    };
                    st.recent.retain(|r| *r != item);
                    st.recent.insert(0, item);
                    st.recent.truncate(30);
                    st.current = Some(entry);
                    st.selected_project = None;
                    st.last_error = None;
                    drop(st);
                    self.popup.set_edit_text("");
                }
                Msg::Stopped => {
                    st.current = None;
                    st.last_error = None;
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
        let project_of = [Some(1), None, Some(1), Some(2), Some(3), Some(4), None, Some(2)];
        st.recent = recent
            .iter()
            .zip(project_of)
            .map(|(d, p)| RecentItem { description: d.to_string(), project_id: p })
            .collect();
        if mode == "running" {
            st.current = Some(TimeEntry {
                id: 1,
                workspace_id: 1,
                description: Some(recent[0].into()),
                project_id: Some(1),
                start: util::rfc3339_utc(now_unix() - 754),
                stop: None,
                duration: -1,
            });
        }
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

    fn perform(&self, action: Action) {
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
            Action::PickProject => self.pick_project(),
            Action::PickLanguage => self.pick_language(),
            Action::Recent(i) => self.start_recent(i),
            Action::Save => self.save_settings(),
            Action::OpenTokenPage => ui::open_url("https://track.toggl.com/profile"),
        }
    }

    fn moved(&self, x: i32, y: i32) {
        let mut cfg = config::load();
        cfg.popup_x = Some(x);
        cfg.popup_y = Some(y);
        let _ = config::save(&cfg);
    }
}

fn dedupe_recent(mut entries: Vec<TimeEntry>) -> Vec<RecentItem> {
    // RFC3339 strings from Toggl share a timezone suffix, so lexical order == chronological.
    entries.sort_by(|a, b| b.start.cmp(&a.start));
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

fn main() {
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
