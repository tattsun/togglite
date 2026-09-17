//! Custom-drawn popup window (GDI+ shapes, GDI text). No common controls except a
//! borderless EDIT for text input so IME keeps working.

use crate::i18n::t;
use std::cell::{Cell, RefCell};
use winapi::ctypes::c_void;
use std::ptr::{null, null_mut};
use winapi::shared::minwindef::*;
use winapi::shared::windef::*;
use winapi::um::dwmapi::DwmSetWindowAttribute;
use winapi::um::libloaderapi::{GetModuleHandleW, GetProcAddress, LoadLibraryW};
use winapi::um::shellapi::ShellExecuteW;
use winapi::um::wingdi::*;
use winapi::um::winreg::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_DWORD};
use winapi::um::winuser::*;

// ---------------------------------------------------------------- colours

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Rgb(pub u8, pub u8, pub u8);

impl Rgb {
    fn cr(self) -> COLORREF {
        RGB(self.0, self.1, self.2)
    }
    fn argb(self) -> u32 {
        0xFF00_0000 | (self.0 as u32) << 16 | (self.1 as u32) << 8 | self.2 as u32
    }
    pub fn from_hex(s: &str) -> Option<Rgb> {
        let s = s.trim().trim_start_matches('#');
        if s.len() != 6 {
            return None;
        }
        let v = u32::from_str_radix(s, 16).ok()?;
        Some(Rgb((v >> 16) as u8, (v >> 8) as u8, v as u8))
    }
}

#[derive(Clone, Copy)]
pub struct Palette {
    bg: Rgb,
    surface: Rgb,
    surface2: Rgb,
    border: Rgb,
    text: Rgb,
    muted: Rgb,
    accent: Rgb,
    accent_hover: Rgb,
    on_accent: Rgb,
    success: Rgb,
    danger: Rgb,
}

fn palette(dark: bool) -> Palette {
    if dark {
        Palette {
            bg: Rgb(0x1C, 0x1C, 0x21),
            surface: Rgb(0x26, 0x26, 0x2D),
            surface2: Rgb(0x30, 0x30, 0x39),
            border: Rgb(0x3B, 0x3B, 0x46),
            text: Rgb(0xF2, 0xF2, 0xF5),
            muted: Rgb(0x9A, 0x9A, 0xA8),
            accent: Rgb(0xE5, 0x5C, 0xA8),
            accent_hover: Rgb(0xF0, 0x74, 0xB8),
            on_accent: Rgb(0xFF, 0xFF, 0xFF),
            success: Rgb(0x3D, 0xD6, 0x8C),
            danger: Rgb(0xF2, 0x6B, 0x66),
        }
    } else {
        Palette {
            bg: Rgb(0xFF, 0xFF, 0xFF),
            surface: Rgb(0xF4, 0xF4, 0xF7),
            surface2: Rgb(0xEC, 0xEC, 0xF1),
            border: Rgb(0xE1, 0xE1, 0xE8),
            text: Rgb(0x1B, 0x1B, 0x1F),
            muted: Rgb(0x6E, 0x6E, 0x7C),
            accent: Rgb(0xDF, 0x3E, 0x98),
            accent_hover: Rgb(0xE9, 0x56, 0xA8),
            on_accent: Rgb(0xFF, 0xFF, 0xFF),
            success: Rgb(0x1D, 0xB3, 0x6B),
            danger: Rgb(0xDE, 0x45, 0x40),
        }
    }
}

/// True when Windows "apps" theme is dark (HKCU ... Personalize\AppsUseLightTheme == 0).
pub fn system_dark() -> bool {
    match std::env::var("TOGGLITE_THEME").as_deref() {
        Ok("dark") => return true,
        Ok("light") => return false,
        _ => {}
    }
    let key = wide(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize");
    let name = wide("AppsUseLightTheme");
    let mut val: u32 = 1;
    let mut size: u32 = 4;
    let rc = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            key.as_ptr(),
            name.as_ptr(),
            RRF_RT_REG_DWORD,
            null_mut(),
            &mut val as *mut u32 as *mut c_void,
            &mut size,
        )
    };
    rc == 0 && val == 0
}

/// Lets Win32 popup menus follow the dark theme (undocumented uxtheme ordinal 135,
/// used by Explorer itself). Silently does nothing on older builds.
pub fn enable_dark_menus() {
    unsafe {
        let lib = LoadLibraryW(wide("uxtheme.dll").as_ptr());
        if lib.is_null() {
            return;
        }
        let set = GetProcAddress(lib, 135 as *const i8);
        if !set.is_null() {
            let set: extern "system" fn(i32) -> i32 = std::mem::transmute(set);
            set(1); // AllowDark: follow the system setting
        }
        let flush = GetProcAddress(lib, 136 as *const i8);
        if !flush.is_null() {
            let flush: extern "system" fn() = std::mem::transmute(flush);
            flush();
        }
    }
}

pub use crate::util::wide;

// ---------------------------------------------------------------- GDI+ (flat API)

#[repr(C)]
struct GdiplusStartupInput {
    version: u32,
    callback: *mut c_void,
    suppress_background_thread: BOOL,
    suppress_external_codecs: BOOL,
}

#[link(name = "gdiplus")]
extern "system" {
    fn GdiplusStartup(token: *mut usize, input: *const GdiplusStartupInput, output: *mut c_void) -> i32;
    fn GdipCreateFromHDC(hdc: HDC, graphics: *mut *mut c_void) -> i32;
    fn GdipDeleteGraphics(g: *mut c_void) -> i32;
    fn GdipSetSmoothingMode(g: *mut c_void, mode: i32) -> i32;
    fn GdipFlush(g: *mut c_void, intention: i32) -> i32;
    fn GdipCreatePath(mode: i32, path: *mut *mut c_void) -> i32;
    fn GdipDeletePath(path: *mut c_void) -> i32;
    fn GdipAddPathArc(path: *mut c_void, x: f32, y: f32, w: f32, h: f32, start: f32, sweep: f32) -> i32;
    fn GdipClosePathFigure(path: *mut c_void) -> i32;
    fn GdipFillPath(g: *mut c_void, brush: *mut c_void, path: *mut c_void) -> i32;
    fn GdipDrawPath(g: *mut c_void, pen: *mut c_void, path: *mut c_void) -> i32;
    fn GdipCreateSolidFill(color: u32, brush: *mut *mut c_void) -> i32;
    fn GdipDeleteBrush(brush: *mut c_void) -> i32;
    fn GdipCreatePen1(color: u32, width: f32, unit: i32, pen: *mut *mut c_void) -> i32;
    fn GdipDeletePen(pen: *mut c_void) -> i32;
    fn GdipFillEllipse(g: *mut c_void, brush: *mut c_void, x: f32, y: f32, w: f32, h: f32) -> i32;
    fn GdipDrawEllipse(g: *mut c_void, pen: *mut c_void, x: f32, y: f32, w: f32, h: f32) -> i32;
    fn GdipDrawLine(g: *mut c_void, pen: *mut c_void, x1: f32, y1: f32, x2: f32, y2: f32) -> i32;
}

pub fn init_gdiplus() {
    let input = GdiplusStartupInput {
        version: 1,
        callback: null_mut(),
        suppress_background_thread: 0,
        suppress_external_codecs: 0,
    };
    let mut token = 0usize;
    unsafe {
        GdiplusStartup(&mut token, &input, null_mut());
    }
}

/// One paint's worth of drawing state: a GDI+ Graphics over a GDI memory DC.
struct Canvas {
    hdc: HDC,
    g: *mut c_void,
}

impl Canvas {
    fn new(hdc: HDC) -> Canvas {
        let mut g = null_mut();
        unsafe {
            GdipCreateFromHDC(hdc, &mut g);
            GdipSetSmoothingMode(g, 4); // AntiAlias
            SetBkMode(hdc, TRANSPARENT as i32);
        }
        Canvas { hdc, g }
    }

    fn round_path(r: &RECT, radius: f32) -> *mut c_void {
        let (x, y) = (r.left as f32, r.top as f32);
        let (w, h) = ((r.right - r.left) as f32, (r.bottom - r.top) as f32);
        let d = (radius * 2.0).min(w).min(h);
        let mut path = null_mut();
        unsafe {
            GdipCreatePath(0, &mut path);
            GdipAddPathArc(path, x, y, d, d, 180.0, 90.0);
            GdipAddPathArc(path, x + w - d, y, d, d, 270.0, 90.0);
            GdipAddPathArc(path, x + w - d, y + h - d, d, d, 0.0, 90.0);
            GdipAddPathArc(path, x, y + h - d, d, d, 90.0, 90.0);
            GdipClosePathFigure(path);
        }
        path
    }

    fn fill_round(&self, r: &RECT, radius: f32, color: Rgb) {
        unsafe {
            let path = Self::round_path(r, radius);
            let mut brush = null_mut();
            GdipCreateSolidFill(color.argb(), &mut brush);
            GdipFillPath(self.g, brush, path);
            GdipDeleteBrush(brush);
            GdipDeletePath(path);
        }
    }

    fn stroke_round(&self, r: &RECT, radius: f32, color: Rgb, width: f32) {
        unsafe {
            let mut rr = *r;
            rr.left += 1;
            rr.top += 1;
            rr.right -= 1;
            rr.bottom -= 1;
            let path = Self::round_path(&rr, radius);
            let mut pen = null_mut();
            GdipCreatePen1(color.argb(), width, 2, &mut pen);
            GdipDrawPath(self.g, pen, path);
            GdipDeletePen(pen);
            GdipDeletePath(path);
        }
    }

    fn dot(&self, cx: i32, cy: i32, d: i32, color: Rgb) {
        unsafe {
            let mut brush = null_mut();
            GdipCreateSolidFill(color.argb(), &mut brush);
            GdipFillEllipse(self.g, brush, (cx - d / 2) as f32, (cy - d / 2) as f32, d as f32, d as f32);
            GdipDeleteBrush(brush);
        }
    }

    fn ring(&self, cx: i32, cy: i32, d: i32, color: Rgb, width: f32) {
        unsafe {
            let mut pen = null_mut();
            GdipCreatePen1(color.argb(), width, 2, &mut pen);
            let o = width / 2.0;
            GdipDrawEllipse(
                self.g,
                pen,
                (cx - d / 2) as f32 + o,
                (cy - d / 2) as f32 + o,
                d as f32 - width,
                d as f32 - width,
            );
            GdipDeletePen(pen);
        }
    }

    fn hline(&self, x1: i32, x2: i32, y: i32, color: Rgb) {
        unsafe {
            let mut pen = null_mut();
            GdipCreatePen1(color.argb(), 1.0, 2, &mut pen);
            GdipDrawLine(self.g, pen, x1 as f32, y as f32 + 0.5, x2 as f32, y as f32 + 0.5);
            GdipDeletePen(pen);
        }
    }

    fn text(&self, font: HFONT, color: Rgb, r: &RECT, s: &str, flags: UINT) {
        let w = wide(s);
        let mut rc = *r;
        unsafe {
            GdipFlush(self.g, 1);
            SelectObject(self.hdc, font as HGDIOBJ);
            SetTextColor(self.hdc, color.cr());
            DrawTextW(self.hdc, w.as_ptr(), -1, &mut rc, flags | DT_NOPREFIX);
        }
    }
}

impl Drop for Canvas {
    fn drop(&mut self) {
        unsafe {
            GdipDeleteGraphics(self.g);
        }
    }
}

// ---------------------------------------------------------------- fonts

struct Fonts {
    small: HFONT,
    caption: HFONT,
    body: HFONT,
    body_semi: HFONT,
    timer: HFONT,
    icon: HFONT,
}

fn make_font(dpi: i32, pt: f32, weight: i32, face: &str) -> HFONT {
    let height = -((pt * dpi as f32 / 72.0).round() as i32);
    let face = wide(face);
    unsafe {
        CreateFontW(
            height,
            0,
            0,
            0,
            weight,
            0,
            0,
            0,
            DEFAULT_CHARSET,
            OUT_DEFAULT_PRECIS,
            CLIP_DEFAULT_PRECIS,
            CLEARTYPE_QUALITY,
            DEFAULT_PITCH | FF_DONTCARE,
            face.as_ptr(),
        )
    }
}

impl Fonts {
    fn new(dpi: i32) -> Fonts {
        Fonts {
            small: make_font(dpi, 9.0, FW_NORMAL, "Segoe UI"),
            caption: make_font(dpi, 10.0, FW_NORMAL, "Segoe UI"),
            body: make_font(dpi, 11.0, FW_NORMAL, "Segoe UI"),
            body_semi: make_font(dpi, 11.0, FW_SEMIBOLD, "Segoe UI"),
            timer: make_font(dpi, 26.0, FW_SEMIBOLD, "Segoe UI"),
            icon: make_font(dpi, 11.5, FW_NORMAL, "Segoe MDL2 Assets"),
        }
    }
}

impl Drop for Fonts {
    fn drop(&mut self) {
        unsafe {
            for f in [self.small, self.caption, self.body, self.body_semi, self.timer, self.icon] {
                DeleteObject(f as HGDIOBJ);
            }
        }
    }
}

// Segoe MDL2 Assets glyphs
const GLYPH_SETTINGS: &str = "\u{E713}";
const GLYPH_REFRESH: &str = "\u{E72C}";
const GLYPH_PLAY: &str = "\u{E768}";
const GLYPH_STOP: &str = "\u{E71A}";
const GLYPH_CHEVRON: &str = "\u{E70D}";
const GLYPH_CLOSE: &str = "\u{E711}";
const GLYPH_BACK: &str = "\u{E72B}";
const GLYPH_GLOBE: &str = "\u{E774}";

// ---------------------------------------------------------------- view model

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Page {
    Main,
    Settings,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Action {
    Start,
    Stop,
    Refresh,
    OpenSettings,
    CloseSettings,
    PickProject,
    PickLanguage,
    Recent(usize),
    Save,
    OpenTokenPage,
    /// Another instance was launched: bring the popup up.
    Activate,
    /// The installer asked us to exit.
    Quit,
}

#[derive(Clone)]
pub struct ProjectView {
    pub name: String,
    pub color: Option<Rgb>,
}

pub struct RunningView {
    pub desc: String,
    pub project: Option<ProjectView>,
    pub elapsed: String,
}

pub struct RecentView {
    pub desc: String,
    pub project: Option<ProjectView>,
}

pub struct View {
    pub busy: bool,
    pub error: Option<String>,
    pub has_token: bool,
    pub running: Option<RunningView>,
    pub project: Option<ProjectView>,
    pub recent: Vec<RecentView>,
    /// Label of the language chip on the settings page.
    pub language: String,
}

pub trait Host {
    fn view(&self) -> View;
    fn perform(&self, action: Action);
    /// The user dragged the popup to screen position (x, y).
    fn moved(&self, _x: i32, _y: i32) {}
}

// ---------------------------------------------------------------- layout constants (96 dpi)

const WIN_W: i32 = 340;
const WIN_H_MAIN: i32 = 534;
const WIN_H_SETTINGS: i32 = 386;
const PAD: i32 = 16;
const HEADER_Y: i32 = 14;
const ICON_BTN: i32 = 28;
const CONTENT_Y: i32 = 54;
const ROW_H: i32 = 40;
const RECENT_CAPTION_Y: i32 = 210;
const RECENT_ROWS_Y: i32 = 238;

const EM_SETCUEBANNER: u32 = 0x1501;
const DWMWA_WINDOW_ROUNDED_PREFERENCE: u32 = 33;
const DWMWA_BORDER_COLOR: u32 = 34;

/// Window class of the popup; other processes find the running instance by it.
pub const POPUP_CLASS: &str = "TogglitePopup";
/// Posted by a second instance: bring the popup up.
pub const WM_TOGGLITE_SHOW: UINT = WM_APP + 1;
/// Posted by the installer/uninstaller: exit cleanly.
pub const WM_TOGGLITE_QUIT: UINT = WM_APP + 2;

// ---------------------------------------------------------------- popup

pub struct Popup {
    pub hwnd: HWND,
    edit: HWND,
    dpi: i32,
    fonts: Fonts,
    pal: Cell<Palette>,
    edit_brush: Cell<HBRUSH>,
    page: Cell<Page>,
    hover: Cell<Option<usize>>,
    pressed: Cell<Option<usize>>,
    tracking: Cell<bool>,
    regions: RefCell<Vec<(RECT, Action)>>,
    scroll: Cell<usize>,
    /// Position the user dragged the window to; reused on later shows.
    pinned: Cell<Option<POINT>>,
}

unsafe extern "system" fn popup_proc(hwnd: HWND, msg: UINT, w: WPARAM, l: LPARAM) -> LRESULT {
    let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const PopupBinding;
    if ptr.is_null() {
        return DefWindowProcW(hwnd, msg, w, l);
    }
    let binding = &*ptr;
    (binding.dispatch)(binding.host, hwnd, msg, w, l)
}

/// Type-erased link from the HWND back to the application object.
pub struct PopupBinding {
    pub host: *const c_void,
    pub dispatch: fn(*const c_void, HWND, UINT, WPARAM, LPARAM) -> LRESULT,
}

impl Popup {
    pub fn new() -> Popup {
        let class = wide(POPUP_CLASS);
        unsafe {
            let hinst = GetModuleHandleW(null());
            let wc = WNDCLASSEXW {
                cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
                style: CS_HREDRAW | CS_VREDRAW,
                lpfnWndProc: Some(popup_proc),
                cbClsExtra: 0,
                cbWndExtra: 0,
                hInstance: hinst,
                hIcon: null_mut(),
                hCursor: LoadCursorW(null_mut(), IDC_ARROW),
                hbrBackground: null_mut(),
                lpszMenuName: null(),
                lpszClassName: class.as_ptr(),
                hIconSm: null_mut(),
            };
            RegisterClassExW(&wc);

            let hwnd = CreateWindowExW(
                WS_EX_TOOLWINDOW | WS_EX_TOPMOST,
                class.as_ptr(),
                wide("Togglite").as_ptr(),
                WS_POPUP | WS_CLIPCHILDREN,
                0,
                0,
                WIN_W,
                WIN_H_MAIN,
                null_mut(),
                null_mut(),
                hinst,
                null_mut(),
            );

            let dpi = {
                let d = GetDpiForWindow(hwnd) as i32;
                if d == 0 {
                    96
                } else {
                    d
                }
            };
            let fonts = Fonts::new(dpi);
            let pal = palette(system_dark());

            let edit = CreateWindowExW(
                0,
                wide("EDIT").as_ptr(),
                wide("").as_ptr(),
                WS_CHILD | WS_VISIBLE | ES_AUTOHSCROLL | ES_LEFT,
                0,
                0,
                10,
                10,
                hwnd,
                null_mut(),
                hinst,
                null_mut(),
            );
            SendMessageW(edit, WM_SETFONT, fonts.body as WPARAM, 1);

            let popup = Popup {
                hwnd,
                edit,
                dpi,
                fonts,
                pal: Cell::new(pal),
                edit_brush: Cell::new(CreateSolidBrush(pal.surface2.cr())),
                page: Cell::new(Page::Main),
                hover: Cell::new(None),
                pressed: Cell::new(None),
                tracking: Cell::new(false),
                regions: RefCell::new(Vec::new()),
                scroll: Cell::new(0),
                pinned: Cell::new(None),
            };
            popup.apply_dwm();
            popup.apply_page_chrome();
            popup
        }
    }

    pub fn bind(&self, binding: *const PopupBinding) {
        unsafe {
            SetWindowLongPtrW(self.hwnd, GWLP_USERDATA, binding as isize);
        }
    }

    fn s(&self, v: i32) -> i32 {
        (v * self.dpi + 48) / 96
    }

    fn apply_dwm(&self) {
        unsafe {
            let round: u32 = 2; // DWMWCP_ROUND
            DwmSetWindowAttribute(
                self.hwnd,
                DWMWA_WINDOW_ROUNDED_PREFERENCE,
                &round as *const u32 as *const c_void,
                4,
            );
            let border: u32 = self.pal.get().border.cr();
            DwmSetWindowAttribute(
                self.hwnd,
                DWMWA_BORDER_COLOR,
                &border as *const u32 as *const c_void,
                4,
            );
        }
    }

    /// Re-read the system theme (called on WM_SETTINGCHANGE).
    pub fn refresh_theme(&self) {
        let pal = palette(system_dark());
        self.pal.set(pal);
        unsafe {
            DeleteObject(self.edit_brush.get() as HGDIOBJ);
            self.edit_brush.set(CreateSolidBrush(pal.surface2.cr()));
        }
        self.apply_dwm();
        self.invalidate();
    }

    pub fn page(&self) -> Page {
        self.page.get()
    }

    pub fn set_page(&self, page: Page) {
        if self.page.get() == page {
            return;
        }
        self.page.set(page);
        self.hover.set(None);
        self.pressed.set(None);
        self.apply_page_chrome();
        self.invalidate();
    }

    fn apply_page_chrome(&self) {
        let (h, cue, pw) = match self.page.get() {
            Page::Main => (WIN_H_MAIN, t().cue_description, 0usize),
            Page::Settings => (WIN_H_SETTINGS, t().cue_token, '●' as usize),
        };
        let cue = wide(cue);
        unsafe {
            SetWindowPos(
                self.hwnd,
                null_mut(),
                0,
                0,
                self.s(WIN_W),
                self.s(h),
                SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE,
            );
            SendMessageW(self.edit, EM_SETPASSWORDCHAR as u32, pw, 0);
            SendMessageW(self.edit, EM_SETCUEBANNER, 1, cue.as_ptr() as LPARAM);
            InvalidateRect(self.edit, null(), 1);
        }
    }

    pub fn visible(&self) -> bool {
        unsafe { IsWindowVisible(self.hwnd) != 0 }
    }

    pub fn show(&self) {
        unsafe {
            let mut rc: RECT = std::mem::zeroed();
            GetWindowRect(self.hwnd, &mut rc);
            let (w, h) = (rc.right - rc.left, rc.bottom - rc.top);

            let mut work: RECT = std::mem::zeroed();
            SystemParametersInfoW(SPI_GETWORKAREA, 0, &mut work as *mut RECT as *mut c_void, 0);

            let mut pt: POINT = std::mem::zeroed();
            GetCursorPos(&mut pt);

            let (mut x, mut y) = match self.pinned.get() {
                Some(p) => (p.x, p.y),
                None => {
                    let x = pt.x - w / 2;
                    let mut y = pt.y - h - self.s(10);
                    if y < work.top {
                        y = pt.y + self.s(10);
                    }
                    (x, y)
                }
            };
            x = x.clamp(work.left, (work.right - w).max(work.left));
            y = y.clamp(work.top, (work.bottom - h).max(work.top));

            SetWindowPos(self.hwnd, HWND_TOPMOST, x, y, 0, 0, SWP_NOSIZE | SWP_SHOWWINDOW);
            SetForegroundWindow(self.hwnd);
            SetFocus(self.edit);
        }
        self.invalidate();
    }

    pub fn set_pinned(&self, pos: Option<(i32, i32)>) {
        self.pinned.set(pos.map(|(x, y)| POINT { x, y }));
    }

    pub fn hide(&self) {
        unsafe {
            ShowWindow(self.hwnd, SW_HIDE);
        }
        self.hover.set(None);
        self.pressed.set(None);
    }

    pub fn invalidate(&self) {
        unsafe {
            InvalidateRect(self.hwnd, null(), 0);
        }
    }

    pub fn edit_text(&self) -> String {
        unsafe {
            let len = GetWindowTextLengthW(self.edit);
            let mut buf = vec![0u16; len as usize + 1];
            let n = GetWindowTextW(self.edit, buf.as_mut_ptr(), buf.len() as i32);
            String::from_utf16_lossy(&buf[..n as usize])
        }
    }

    pub fn set_edit_text(&self, text: &str) {
        unsafe {
            SetWindowTextW(self.edit, wide(text).as_ptr());
            SendMessageW(self.edit, EM_SETSEL as u32, 0, -1);
        }
    }

    pub fn focus_edit(&self) {
        unsafe {
            SetFocus(self.edit);
        }
    }

    /// Native popup menu listing projects. Returns None if cancelled,
    /// Some(None) for "no project", Some(Some(i)) for `names[i]`.
    pub fn pick_project(&self, names: &[String], current: Option<usize>) -> Option<Option<usize>> {
        let chip = self.layout_main(false).chip;
        self.pick_below(&chip, t().no_project, names, current)
    }

    /// Native popup menu listing languages. Returns None if cancelled,
    /// Some(None) for "automatic", Some(Some(i)) for `names[i]`.
    pub fn pick_language(&self, names: &[String], current: Option<usize>) -> Option<Option<usize>> {
        let chip = self.layout_settings().lang_chip;
        self.pick_below(&chip, t().language_auto, names, current)
    }

    /// Shows a menu anchored under `anchor`: a `none` entry, a separator, then `names`.
    /// `current` is the checked entry (None = the `none` entry).
    fn pick_below(&self, anchor: &RECT, none: &str, names: &[String], current: Option<usize>) -> Option<Option<usize>> {
        unsafe {
            let menu = CreatePopupMenu();
            let check = |i: Option<usize>| if i == current { MF_CHECKED } else { 0 };
            AppendMenuW(menu, MF_STRING | check(None), 1, wide(none).as_ptr());
            if !names.is_empty() {
                AppendMenuW(menu, MF_SEPARATOR, 0, null());
            }
            for (i, n) in names.iter().enumerate() {
                AppendMenuW(menu, MF_STRING | check(Some(i)), 2 + i, wide(n).as_ptr());
            }
            let mut pt = POINT { x: anchor.left, y: anchor.bottom + self.s(4) };
            ClientToScreen(self.hwnd, &mut pt);
            let id = TrackPopupMenu(
                menu,
                TPM_LEFTALIGN | TPM_TOPALIGN | TPM_RETURNCMD | TPM_NONOTIFY,
                pt.x,
                pt.y,
                0,
                self.hwnd,
                null(),
            );
            DestroyMenu(menu);
            match id {
                0 => None,
                1 => Some(None),
                n => Some(Some((n - 2) as usize)),
            }
        }
    }

    /// Re-applies language-dependent window text (the edit's cue banner) and repaints.
    pub fn refresh_text(&self) {
        self.apply_page_chrome();
        self.invalidate();
    }

    // ------------------------------------------------------------ layout

    fn rect(&self, x: i32, y: i32, w: i32, h: i32) -> RECT {
        RECT {
            left: self.s(x),
            top: self.s(y),
            right: self.s(x + w),
            bottom: self.s(y + h),
        }
    }

    fn client_size(&self) -> (i32, i32) {
        unsafe {
            let mut rc: RECT = std::mem::zeroed();
            GetClientRect(self.hwnd, &mut rc);
            (rc.right, rc.bottom)
        }
    }

    fn layout_main(&self, running: bool) -> MainLayout {
        let (_, ch) = self.client_size();
        let inner_w = WIN_W - 2 * PAD;
        let rows_y = self.s(RECENT_ROWS_Y);
        let row_h = self.s(ROW_H);
        let rows = ((ch - rows_y - self.s(PAD)) / row_h).max(0) as usize;
        MainLayout {
            dot: self.rect(PAD, HEADER_Y + 9, 10, 10),
            caption: self.rect(PAD + 18, HEADER_Y, inner_w - 18 - 2 * (ICON_BTN + 6), ICON_BTN),
            icon_a: self.rect(WIN_W - PAD - ICON_BTN, HEADER_Y, ICON_BTN, ICON_BTN),
            icon_b: self.rect(WIN_W - PAD - 2 * ICON_BTN - 6, HEADER_Y, ICON_BTN, ICON_BTN),
            pill: self.rect(PAD, CONTENT_Y, inner_w, 44),
            chip: self.rect(PAD, CONTENT_Y + 52, inner_w, 34),
            card: self.rect(PAD, CONTENT_Y, inner_w, 86),
            button: self.rect(PAD, CONTENT_Y + 98, inner_w, 44),
            recent_caption: self.rect(PAD, RECENT_CAPTION_Y, inner_w, 16),
            divider_y: self.s(RECENT_CAPTION_Y + 22),
            rows_y,
            row_h,
            rows,
            running,
        }
    }

    fn layout_settings(&self) -> SettingsLayout {
        let inner_w = WIN_W - 2 * PAD;
        SettingsLayout {
            caption: self.rect(PAD + 18, HEADER_Y, inner_w - 18 - ICON_BTN - 6, ICON_BTN),
            back: self.rect(PAD - 6, HEADER_Y, ICON_BTN, ICON_BTN),
            label: self.rect(PAD, CONTENT_Y, inner_w, 20),
            pill: self.rect(PAD, CONTENT_Y + 26, inner_w, 44),
            hint: self.rect(PAD, CONTENT_Y + 80, inner_w, 40),
            link: self.rect(PAD, CONTENT_Y + 122, inner_w, 18),
            divider_y: self.s(CONTENT_Y + 156),
            lang_label: self.rect(PAD, CONTENT_Y + 170, inner_w, 20),
            lang_chip: self.rect(PAD, CONTENT_Y + 196, inner_w, 34),
            error: self.rect(PAD, CONTENT_Y + 244, inner_w, 18),
            button: self.rect(PAD, CONTENT_Y + 268, inner_w, 44),
        }
    }

    fn edit_rect_in(&self, pill: &RECT) -> RECT {
        let edit_h = self.s(20);
        let pad = self.s(14);
        let top = pill.top + (pill.bottom - pill.top - edit_h) / 2;
        RECT {
            left: pill.left + pad,
            top,
            right: pill.right - pad,
            bottom: top + edit_h,
        }
    }

    fn place_edit(&self, rect: Option<RECT>) {
        unsafe {
            match rect {
                Some(r) => {
                    SetWindowPos(
                        self.edit,
                        null_mut(),
                        r.left,
                        r.top,
                        r.right - r.left,
                        r.bottom - r.top,
                        SWP_NOZORDER | SWP_SHOWWINDOW,
                    );
                }
                None => {
                    ShowWindow(self.edit, SW_HIDE);
                }
            }
        }
    }

    // ------------------------------------------------------------ painting

    fn paint(&self, host: &dyn Host) {
        let view = host.view();
        let pal = self.pal.get();
        unsafe {
            let mut ps: PAINTSTRUCT = std::mem::zeroed();
            let hdc = BeginPaint(self.hwnd, &mut ps);
            let (cw, ch) = self.client_size();
            let mem = CreateCompatibleDC(hdc);
            let bmp = CreateCompatibleBitmap(hdc, cw, ch);
            let old = SelectObject(mem, bmp as HGDIOBJ);

            let bg = CreateSolidBrush(pal.bg.cr());
            let full = RECT { left: 0, top: 0, right: cw, bottom: ch };
            FillRect(mem, &full, bg);
            DeleteObject(bg as HGDIOBJ);

            let mut regions = Vec::new();
            {
                let canvas = Canvas::new(mem);
                match self.page.get() {
                    Page::Main => self.paint_main(&canvas, &view, &mut regions),
                    Page::Settings => self.paint_settings(&canvas, &view, &mut regions),
                }
            }
            *self.regions.borrow_mut() = regions;

            BitBlt(hdc, 0, 0, cw, ch, mem, 0, 0, SRCCOPY);
            SelectObject(mem, old);
            DeleteObject(bmp as HGDIOBJ);
            DeleteDC(mem);
            EndPaint(self.hwnd, &ps);
        }
    }

    fn hovered(&self, regions: &[(RECT, Action)], action: Action) -> bool {
        match self.hover.get() {
            Some(i) => regions.get(i).map(|r| r.1 == action).unwrap_or(false),
            None => false,
        }
    }

    fn icon_button(&self, c: &Canvas, r: &RECT, glyph: &str, hot: bool) {
        let pal = self.pal.get();
        if hot {
            c.fill_round(r, ((r.right - r.left) / 2) as f32, pal.surface2);
        }
        c.text(
            self.fonts.icon,
            if hot { pal.text } else { pal.muted },
            r,
            glyph,
            DT_CENTER | DT_VCENTER | DT_SINGLELINE,
        );
    }

    fn glyph_and_label(&self, c: &Canvas, r: &RECT, glyph: &str, label: &str, color: Rgb) {
        // Centre "glyph + label" as a unit: glyph cell of s(20), gap s(6).
        let glyph_w = self.s(18);
        let gap = self.s(6);
        let label_w = self.measure(self.fonts.body_semi, label);
        let total = glyph_w + gap + label_w;
        let x = r.left + ((r.right - r.left) - total) / 2;
        let gr = RECT { left: x, top: r.top, right: x + glyph_w, bottom: r.bottom };
        let lr = RECT { left: x + glyph_w + gap, top: r.top, right: r.right, bottom: r.bottom };
        c.text(self.fonts.icon, color, &gr, glyph, DT_CENTER | DT_VCENTER | DT_SINGLELINE);
        c.text(self.fonts.body_semi, color, &lr, label, DT_LEFT | DT_VCENTER | DT_SINGLELINE);
    }

    fn measure(&self, font: HFONT, s: &str) -> i32 {
        unsafe {
            let hdc = GetDC(self.hwnd);
            let old = SelectObject(hdc, font as HGDIOBJ);
            let w = wide(s);
            let mut rc: RECT = std::mem::zeroed();
            DrawTextW(hdc, w.as_ptr(), -1, &mut rc, DT_CALCRECT | DT_SINGLELINE | DT_NOPREFIX);
            SelectObject(hdc, old);
            ReleaseDC(self.hwnd, hdc);
            rc.right - rc.left
        }
    }

    fn paint_main(&self, c: &Canvas, view: &View, regions: &mut Vec<(RECT, Action)>) {
        let pal = self.pal.get();
        let prev = self.regions.borrow();
        let running = view.running.is_some();
        let l = self.layout_main(running);

        // ---- header
        let (dot_color, caption, caption_color) = if view.busy {
            (pal.muted, t().status_syncing.to_string(), pal.muted)
        } else if let Some(e) = &view.error {
            (pal.danger, e.clone(), pal.danger)
        } else if running {
            (pal.success, t().status_tracking.to_string(), pal.muted)
        } else {
            (pal.muted, t().status_stopped.to_string(), pal.muted)
        };
        if running && !view.busy && view.error.is_none() {
            c.dot((l.dot.left + l.dot.right) / 2, (l.dot.top + l.dot.bottom) / 2, self.s(10), dot_color);
        } else {
            c.ring((l.dot.left + l.dot.right) / 2, (l.dot.top + l.dot.bottom) / 2, self.s(10), dot_color, self.s(2) as f32);
        }
        c.text(self.fonts.caption, caption_color, &l.caption, &caption, DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS);

        self.icon_button(c, &l.icon_a, GLYPH_SETTINGS, self.hovered(&prev, Action::OpenSettings));
        regions.push((l.icon_a, Action::OpenSettings));
        self.icon_button(c, &l.icon_b, GLYPH_REFRESH, self.hovered(&prev, Action::Refresh));
        regions.push((l.icon_b, Action::Refresh));

        // ---- entry area
        match &view.running {
            Some(run) => {
                self.place_edit(None);
                c.fill_round(&l.card, self.s(12) as f32, pal.surface);
                let x = l.card.left + self.s(16);
                let w = l.card.right - x - self.s(16);
                let proj_r = RECT { left: x, top: l.card.top + self.s(10), right: x + w, bottom: l.card.top + self.s(26) };
                match &run.project {
                    Some(p) => {
                        let color = p.color.unwrap_or(pal.muted);
                        c.dot(x + self.s(4), (proj_r.top + proj_r.bottom) / 2, self.s(8), color);
                        let tr = RECT { left: x + self.s(14), ..proj_r };
                        c.text(self.fonts.small, pal.muted, &tr, &p.name, DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS);
                    }
                    None => {
                        c.text(self.fonts.small, pal.muted, &proj_r, t().no_project, DT_LEFT | DT_VCENTER | DT_SINGLELINE);
                    }
                }
                let timer_r = RECT { left: x, top: l.card.top + self.s(24), right: x + w, bottom: l.card.top + self.s(62) };
                c.text(self.fonts.timer, pal.text, &timer_r, &run.elapsed, DT_LEFT | DT_VCENTER | DT_SINGLELINE);
                let desc_r = RECT { left: x, top: l.card.top + self.s(60), right: x + w, bottom: l.card.top + self.s(80) };
                let desc = if run.desc.is_empty() { t().no_description } else { run.desc.as_str() };
                c.text(self.fonts.body, if run.desc.is_empty() { pal.muted } else { pal.text }, &desc_r, desc, DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS);

                let hot = self.hovered(&prev, Action::Stop);
                c.fill_round(&l.button, self.s(10) as f32, if hot { pal.surface2 } else { pal.surface });
                c.stroke_round(&l.button, self.s(10) as f32, pal.border, 1.0);
                self.glyph_and_label(c, &l.button, GLYPH_STOP, t().stop, pal.danger);
                regions.push((l.button, Action::Stop));
            }
            None => {
                c.fill_round(&l.pill, self.s(10) as f32, pal.surface2);
                self.place_edit(Some(self.edit_rect_in(&l.pill)));

                let hot = self.hovered(&prev, Action::PickProject);
                c.fill_round(&l.chip, self.s(9) as f32, if hot { pal.surface2 } else { pal.surface });
                let cx = l.chip.left + self.s(16);
                let cy = (l.chip.top + l.chip.bottom) / 2;
                let name_r = RECT { left: l.chip.left + self.s(30), top: l.chip.top, right: l.chip.right - self.s(34), bottom: l.chip.bottom };
                match &view.project {
                    Some(p) => {
                        c.dot(cx, cy, self.s(9), p.color.unwrap_or(pal.muted));
                        c.text(self.fonts.body, pal.text, &name_r, &p.name, DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS);
                    }
                    None => {
                        c.ring(cx, cy, self.s(9), pal.muted, 1.5);
                        c.text(self.fonts.body, pal.muted, &name_r, t().no_project, DT_LEFT | DT_VCENTER | DT_SINGLELINE);
                    }
                }
                let chev = RECT { left: l.chip.right - self.s(32), top: l.chip.top, right: l.chip.right - self.s(8), bottom: l.chip.bottom };
                c.text(self.fonts.icon, pal.muted, &chev, GLYPH_CHEVRON, DT_CENTER | DT_VCENTER | DT_SINGLELINE);
                regions.push((l.chip, Action::PickProject));

                let hot = self.hovered(&prev, Action::Start);
                c.fill_round(&l.button, self.s(10) as f32, if hot { pal.accent_hover } else { pal.accent });
                self.glyph_and_label(c, &l.button, GLYPH_PLAY, t().start, pal.on_accent);
                regions.push((l.button, Action::Start));
            }
        }

        // ---- recent
        c.text(self.fonts.small, pal.muted, &l.recent_caption, t().recent_entries, DT_LEFT | DT_VCENTER | DT_SINGLELINE);
        c.hline(l.recent_caption.left, l.recent_caption.right, l.divider_y, pal.border);

        if view.recent.is_empty() {
            let r = RECT { left: l.recent_caption.left, top: l.rows_y, right: l.recent_caption.right, bottom: l.rows_y + l.row_h };
            let msg = if view.has_token { t().no_entries } else { t().need_token };
            c.text(self.fonts.caption, pal.muted, &r, msg, DT_LEFT | DT_VCENTER | DT_SINGLELINE);
        }

        let max_scroll = view.recent.len().saturating_sub(l.rows);
        let scroll = self.scroll.get().min(max_scroll);
        self.scroll.set(scroll);
        for (row, (idx, item)) in view.recent.iter().enumerate().skip(scroll).take(l.rows).enumerate() {
            let top = l.rows_y + row as i32 * l.row_h;
            let r = RECT { left: self.s(PAD), top, right: self.s(WIN_W - PAD), bottom: top + l.row_h };
            let hot = self.hovered(&prev, Action::Recent(idx));
            if hot {
                c.fill_round(&r, self.s(8) as f32, pal.surface);
            }
            let cy = (r.top + r.bottom) / 2;
            match &item.project {
                Some(p) => c.dot(r.left + self.s(14), cy, self.s(8), p.color.unwrap_or(pal.muted)),
                None => c.ring(r.left + self.s(14), cy, self.s(8), pal.border, 1.5),
            }
            let right_w = ((r.right - r.left) as f32 * 0.38) as i32;
            let play_w = if hot { self.s(22) } else { 0 };
            let name_r = RECT { left: r.right - self.s(12) - right_w - play_w, top: r.top, right: r.right - self.s(12) - play_w, bottom: r.bottom };
            let desc_r = RECT { left: r.left + self.s(28), top: r.top, right: name_r.left - self.s(8), bottom: r.bottom };
            let desc = if item.desc.is_empty() { t().no_description } else { item.desc.as_str() };
            c.text(self.fonts.body, if item.desc.is_empty() { pal.muted } else { pal.text }, &desc_r, desc, DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS);
            if let Some(p) = &item.project {
                c.text(self.fonts.small, pal.muted, &name_r, &p.name, DT_RIGHT | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS);
            }
            if hot {
                let pr = RECT { left: r.right - self.s(12) - play_w, top: r.top, right: r.right - self.s(8), bottom: r.bottom };
                c.text(self.fonts.icon, pal.accent, &pr, GLYPH_PLAY, DT_CENTER | DT_VCENTER | DT_SINGLELINE);
            }
            regions.push((r, Action::Recent(idx)));
        }
    }

    fn paint_settings(&self, c: &Canvas, view: &View, regions: &mut Vec<(RECT, Action)>) {
        let pal = self.pal.get();
        let prev = self.regions.borrow();
        let l = self.layout_settings();

        self.icon_button(c, &l.back, if view.has_token { GLYPH_BACK } else { GLYPH_CLOSE }, self.hovered(&prev, Action::CloseSettings));
        regions.push((l.back, Action::CloseSettings));
        c.text(self.fonts.body_semi, pal.text, &l.caption, t().settings, DT_LEFT | DT_VCENTER | DT_SINGLELINE);

        c.text(self.fonts.caption, pal.muted, &l.label, t().token_label, DT_LEFT | DT_VCENTER | DT_SINGLELINE);
        c.fill_round(&l.pill, self.s(10) as f32, pal.surface2);
        self.place_edit(Some(self.edit_rect_in(&l.pill)));

        c.text(self.fonts.small, pal.muted, &l.hint, t().token_hint, DT_LEFT | DT_WORDBREAK);
        let hot = self.hovered(&prev, Action::OpenTokenPage);
        c.text(self.fonts.small, if hot { pal.accent_hover } else { pal.accent }, &l.link, t().open_profile, DT_LEFT | DT_VCENTER | DT_SINGLELINE);
        regions.push((l.link, Action::OpenTokenPage));

        // ---- language (same chip idiom as the project picker on the main page)
        c.hline(l.lang_label.left, l.lang_label.right, l.divider_y, pal.border);
        c.text(self.fonts.caption, pal.muted, &l.lang_label, t().language, DT_LEFT | DT_VCENTER | DT_SINGLELINE);
        let hot = self.hovered(&prev, Action::PickLanguage);
        c.fill_round(&l.lang_chip, self.s(9) as f32, if hot { pal.surface2 } else { pal.surface });
        let glyph_r = RECT { left: l.lang_chip.left + self.s(8), top: l.lang_chip.top, right: l.lang_chip.left + self.s(24), bottom: l.lang_chip.bottom };
        c.text(self.fonts.icon, pal.muted, &glyph_r, GLYPH_GLOBE, DT_CENTER | DT_VCENTER | DT_SINGLELINE);
        let name_r = RECT { left: l.lang_chip.left + self.s(30), top: l.lang_chip.top, right: l.lang_chip.right - self.s(34), bottom: l.lang_chip.bottom };
        c.text(self.fonts.body, pal.text, &name_r, &view.language, DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS);
        let chev = RECT { left: l.lang_chip.right - self.s(32), top: l.lang_chip.top, right: l.lang_chip.right - self.s(8), bottom: l.lang_chip.bottom };
        c.text(self.fonts.icon, pal.muted, &chev, GLYPH_CHEVRON, DT_CENTER | DT_VCENTER | DT_SINGLELINE);
        regions.push((l.lang_chip, Action::PickLanguage));

        if let Some(e) = &view.error {
            c.text(self.fonts.small, pal.danger, &l.error, e, DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS);
        } else if view.busy {
            c.text(self.fonts.small, pal.muted, &l.error, t().checking, DT_LEFT | DT_VCENTER | DT_SINGLELINE);
        }

        let hot = self.hovered(&prev, Action::Save);
        c.fill_round(&l.button, self.s(10) as f32, if hot { pal.accent_hover } else { pal.accent });
        c.text(self.fonts.body_semi, pal.on_accent, &l.button, t().save, DT_CENTER | DT_VCENTER | DT_SINGLELINE);
        regions.push((l.button, Action::Save));
    }

    // ------------------------------------------------------------ input

    fn hit(&self, x: i32, y: i32) -> Option<usize> {
        self.regions
            .borrow()
            .iter()
            .position(|(r, _)| x >= r.left && x < r.right && y >= r.top && y < r.bottom)
    }

    fn action_at(&self, idx: usize) -> Option<Action> {
        self.regions.borrow().get(idx).map(|r| r.1)
    }

    pub fn handle(&self, host: &dyn Host, hwnd: HWND, msg: UINT, w: WPARAM, l: LPARAM) -> LRESULT {
        unsafe {
            match msg {
                WM_PAINT => {
                    self.paint(host);
                    0
                }
                WM_ERASEBKGND => 1,
                WM_CTLCOLOREDIT => {
                    let pal = self.pal.get();
                    let hdc = w as HDC;
                    SetBkColor(hdc, pal.surface2.cr());
                    SetTextColor(hdc, pal.text.cr());
                    self.edit_brush.get() as LRESULT
                }
                WM_MOUSEMOVE => {
                    let (x, y) = (GET_X_LPARAM(l), GET_Y_LPARAM(l));
                    if !self.tracking.get() {
                        let mut tme = TRACKMOUSEEVENT {
                            cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                            dwFlags: TME_LEAVE,
                            hwndTrack: hwnd,
                            dwHoverTime: 0,
                        };
                        TrackMouseEvent(&mut tme);
                        self.tracking.set(true);
                    }
                    let h = self.hit(x, y);
                    if h != self.hover.get() {
                        self.hover.set(h);
                        self.invalidate();
                    }
                    0
                }
                WM_MOUSELEAVE => {
                    self.tracking.set(false);
                    if self.hover.get().is_some() {
                        self.hover.set(None);
                        self.invalidate();
                    }
                    0
                }
                WM_LBUTTONDOWN => {
                    let (x, y) = (GET_X_LPARAM(l), GET_Y_LPARAM(l));
                    self.pressed.set(self.hit(x, y));
                    0
                }
                WM_LBUTTONUP => {
                    let (x, y) = (GET_X_LPARAM(l), GET_Y_LPARAM(l));
                    let up = self.hit(x, y);
                    let down = self.pressed.take();
                    if up.is_some() && up == down {
                        if let Some(action) = self.action_at(up.unwrap()) {
                            host.perform(action);
                        }
                    }
                    0
                }
                WM_MOUSEWHEEL => {
                    let delta = GET_WHEEL_DELTA_WPARAM(w);
                    let cur = self.scroll.get();
                    let next = if delta > 0 { cur.saturating_sub(1) } else { cur + 1 };
                    if next != cur {
                        self.scroll.set(next);
                        self.hover.set(None);
                        self.invalidate();
                    }
                    0
                }
                WM_SETCURSOR => {
                    if (l & 0xFFFF) as isize == HTCLIENT as isize && self.hover.get().is_some() {
                        SetCursor(LoadCursorW(null_mut(), IDC_HAND));
                        return 1;
                    }
                    DefWindowProcW(hwnd, msg, w, l)
                }
                WM_ACTIVATE => {
                    if (w & 0xFFFF) as u16 == WA_INACTIVE {
                        self.hide();
                    }
                    0
                }
                WM_COMMAND => {
                    match (w & 0xFFFF) as i32 {
                        IDOK => match self.page.get() {
                            Page::Main => host.perform(Action::Start),
                            Page::Settings => host.perform(Action::Save),
                        },
                        IDCANCEL => match self.page.get() {
                            Page::Settings => host.perform(Action::CloseSettings),
                            Page::Main => self.hide(),
                        },
                        _ => {}
                    }
                    0
                }
                WM_NCHITTEST => {
                    // Anything that is not a control acts as a caption so the window can be dragged.
                    let hit = DefWindowProcW(hwnd, msg, w, l);
                    if hit != HTCLIENT {
                        return hit;
                    }
                    let mut pt = POINT { x: GET_X_LPARAM(l), y: GET_Y_LPARAM(l) };
                    let mut er: RECT = std::mem::zeroed();
                    GetWindowRect(self.edit, &mut er);
                    if IsWindowVisible(self.edit) != 0 && PtInRect(&er, pt) != 0 {
                        return HTCLIENT;
                    }
                    ScreenToClient(hwnd, &mut pt);
                    if self.hit(pt.x, pt.y).is_some() {
                        HTCLIENT
                    } else {
                        HTCAPTION
                    }
                }
                WM_EXITSIZEMOVE => {
                    let mut rc: RECT = std::mem::zeroed();
                    GetWindowRect(hwnd, &mut rc);
                    self.pinned.set(Some(POINT { x: rc.left, y: rc.top }));
                    host.moved(rc.left, rc.top);
                    0
                }
                WM_CLOSE => {
                    self.hide();
                    0
                }
                WM_SETTINGCHANGE => {
                    self.refresh_theme();
                    0
                }
                WM_TOGGLITE_SHOW => {
                    host.perform(Action::Activate);
                    0
                }
                WM_TOGGLITE_QUIT => {
                    host.perform(Action::Quit);
                    0
                }
                _ => DefWindowProcW(hwnd, msg, w, l),
            }
        }
    }
}

impl Drop for Popup {
    fn drop(&mut self) {
        unsafe {
            DeleteObject(self.edit_brush.get() as HGDIOBJ);
            DestroyWindow(self.hwnd);
        }
    }
}

pub fn open_url(url: &str) {
    unsafe {
        ShellExecuteW(null_mut(), wide("open").as_ptr(), wide(url).as_ptr(), null(), null(), SW_SHOWNORMAL);
    }
}

struct MainLayout {
    dot: RECT,
    caption: RECT,
    icon_a: RECT,
    icon_b: RECT,
    pill: RECT,
    chip: RECT,
    card: RECT,
    button: RECT,
    recent_caption: RECT,
    divider_y: i32,
    rows_y: i32,
    row_h: i32,
    rows: usize,
    #[allow(dead_code)]
    running: bool,
}

struct SettingsLayout {
    caption: RECT,
    back: RECT,
    label: RECT,
    pill: RECT,
    hint: RECT,
    link: RECT,
    divider_y: i32,
    lang_label: RECT,
    lang_chip: RECT,
    error: RECT,
    button: RECT,
}

#[allow(non_snake_case)]
fn GET_X_LPARAM(l: LPARAM) -> i32 {
    (l & 0xFFFF) as i16 as i32
}
#[allow(non_snake_case)]
fn GET_Y_LPARAM(l: LPARAM) -> i32 {
    ((l >> 16) & 0xFFFF) as i16 as i32
}
#[allow(non_snake_case)]
fn GET_WHEEL_DELTA_WPARAM(w: WPARAM) -> i16 {
    ((w >> 16) & 0xFFFF) as i16
}
