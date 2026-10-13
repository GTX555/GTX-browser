//! Application state: window chrome, tabs, navigation, painting.
//!
//! Threading model: every callback (Win32 window procs and wry handlers) runs
//! on the main thread. wry handlers never touch `App` directly — they push
//! [`Ev`] events into a thread-local queue that the message loop drains, which
//! keeps re-entrancy out of the picture.

use std::cell::RefCell;
use std::ffi::OsString;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::time::Instant;

use wry::dpi::{PhysicalPosition, PhysicalSize};
use wry::{
    NewWindowResponse, Rect, Theme, WebContext, WebView, WebViewBuilder,
    WebViewBuilderExtWindows, WebViewExtWindows,
};
use windows::core::{w, HSTRING, PCWSTR, PWSTR};
use windows::Win32::Foundation::{COLORREF, HANDLE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, BitBlt, ClientToScreen, CreateCompatibleBitmap, CreateCompatibleDC, CreatePen,
    CreateSolidBrush, DeleteDC, DeleteObject, DrawTextW, EndPaint, FillRect, GetDC,
    GetMonitorInfoW, InvalidateRect, LineTo, MonitorFromRect, MonitorFromWindow,
    MoveToEx, ReleaseDC, RoundRect, ScreenToClient, SelectObject,
    SetBkMode, SetTextColor, Ellipse, DRAW_TEXT_FORMAT, DT_CENTER, DT_END_ELLIPSIS, DT_NOPREFIX,
    DT_SINGLELINE, DT_VCENTER, DT_WORDBREAK, HBITMAP, HBRUSH, HDC, HFONT, HGDIOBJ, PAINTSTRUCT,
    PS_SOLID,
    SRCCOPY, TRANSPARENT, MONITORINFO, MONITOR_DEFAULTTONULL, MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::UI::Controls::Dialogs::{
    GetSaveFileNameW, OPENFILENAMEW, OFN_EXPLORER, OFN_NOCHANGEDIR, OFN_OVERWRITEPROMPT,
    OFN_PATHMUSTEXIST,
};
use windows::Win32::UI::Controls::{EM_GETSEL, EM_SETMARGINS, EM_SETSEL};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    ReleaseCapture, SetCapture, SetFocus, VK_DOWN, VK_ESCAPE, VK_F11, VK_F12, VK_F5, VK_HOME,
    VK_LEFT, VK_NEXT, VK_OEM_COMMA, VK_OEM_MINUS, VK_OEM_PLUS, VK_PRIOR, VK_RETURN, VK_RIGHT,
    VK_TAB, VK_UP,
};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CallWindowProcW, CreateAcceleratorTableW, CreatePopupMenu,
    CreateWindowExW, DefWindowProcW, DestroyAcceleratorTable, DestroyMenu, DestroyWindow,
    EN_CHANGE, EN_KILLFOCUS, EN_SETFOCUS, GetWindowLongPtrW, GetWindowLongW, GetWindowPlacement,
    GetWindowRect, GetWindowTextLengthW, GWL_STYLE, HTCAPTION, IsIconic, IsZoomed, IDYES,
    MB_ICONWARNING, MB_YESNO, MF_SEPARATOR, MF_STRING, MessageBoxW, MoveWindow,
    KillTimer, SendMessageW, SetTimer, SetWindowLongPtrW, SetWindowPos, ShowWindow, TrackPopupMenu,
    ACCEL,
    ACCEL_VIRT_FLAGS, FALT, FCONTROL, FSHIFT, FVIRTKEY, GWLP_WNDPROC, HACCEL, HMENU,
    MESSAGEBOX_RESULT, SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOZORDER, SW_MAXIMIZE, SW_MINIMIZE,
    SW_RESTORE, TPM_RETURNCMD, TPM_RIGHTBUTTON, WINDOWPLACEMENT, WM_GETFONT, WM_KEYDOWN,
    WM_LBUTTONUP, WM_NCLBUTTONDOWN, WM_PAINT, WM_SETFONT, WINDOW_STYLE, WNDPROC, WS_CHILD,
    WS_OVERLAPPEDWINDOW, WS_POPUP, WS_TABSTOP, WS_VISIBLE,
};

use crate::{
    chrome, config, default_browser, downloads, favicon, guard, history, icon_fetch, internal, log,
    nav, popup, session, settings, suggest, toast, toolbar_icons,
};
use crate::w32::{self, AppPtr, R, WinHandle};

// ---------------------------------------------------------------------------
// Design constants (96-dpi units; scaled at runtime)
// ---------------------------------------------------------------------------
pub const PAD: i32 = 12;
const BTN_GAP: i32 = 4;

const TAB_MAX_W: i32 = 180;
const TAB_MIN_W: i32 = 44;
const CLOSE_W: i32 = 20;
/// Corner radius of the omnibox pill and the tab pills.
const RADIUS: i32 = 7;

// Control ids (child windows).
pub const IDC_ADDRESS: u16 = 1004;

// Command ids (paints + accelerator table).
pub const IDM_BACK: u16 = 2001;
pub const IDM_FORWARD: u16 = 2002;
pub const IDM_RELOAD: u16 = 2003;
pub const IDM_NEW_TAB: u16 = 2004;
pub const IDM_CLOSE_TAB: u16 = 2005;
pub const IDM_FOCUS_ADDRESS: u16 = 2006;
pub const IDM_NEXT_TAB: u16 = 2007;
pub const IDM_PREV_TAB: u16 = 2008;
pub const IDM_DEVTOOLS: u16 = 2009;
pub const IDM_ZOOM_IN: u16 = 2010;
pub const IDM_ZOOM_OUT: u16 = 2011;
pub const IDM_ZOOM_RESET: u16 = 2012;
pub const IDM_HOME: u16 = 2013;
pub const IDM_OPEN_SETTINGS: u16 = 2014;
pub const IDM_FULLSCREEN: u16 = 2015;
pub const IDM_REOPEN_TAB: u16 = 2016;
/// Ctrl+1…9 map to `IDM_TAB1 + (n - 1)`.
pub const IDM_TAB1: u16 = 2020;
// Kebab-menu commands (kept clear of IDM_TAB1..=IDM_TAB1+8).
pub const IDM_HISTORY: u16 = 2041;
pub const IDM_OPEN_DOWNLOADS: u16 = 2042;
pub const IDM_ABOUT: u16 = 2043;
pub const IDM_EXIT: u16 = 2044;
pub const IDM_RETRY_TAB: u16 = 2045;

pub const IDT_TICK: usize = 1;
pub const IDT_SMOKE: usize = 2;
/// Drives the omnibox loading bar: progress while a page loads, then the
/// hold-and-fade once it is done.
pub const IDT_LOAD: usize = 3;

// Loading-bar timings (seconds unless the name says otherwise): how fast the
// bar eases towards its ceiling, how long a finished page keeps the full bar,
// how long the fade takes, when a page that never reports back gives up, and
// how often the bar repaints.
const LOAD_TAU: f64 = 0.7;
const LOAD_HOLD_MS: u32 = 200;
const LOAD_FADE_MS: u32 = 300;
const LOAD_ABANDON_SECS: u64 = 60;
const LOAD_TICK_MS: u32 = 50;

/// Default WebView2 browser args, plus GPU disabling when asked. Applied when
/// the *first* web view is built — WebView2 bakes these into the environment,
/// so changing them needs a restart (the settings page says so).
const BROWSER_ARGS_NO_GPU: &str = "--disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection \
    --autoplay-policy=no-user-gesture-required --disable-gpu";

/// Clamp a f64 to a COLORREF component.
const fn rgb(r: u8, g: u8, b: u8) -> COLORREF {
    COLORREF(r as u32 | (g as u32) << 8 | (b as u32) << 16)
}

/// Everything the chrome paints, resolved from `theme` + `accent`.
#[derive(Clone, Copy)]
pub struct Palette {
    pub dark: bool,
    /// Toolbar row background.
    pub bar: COLORREF,
    /// Frame band around the page (the class background brush).
    pub frame: COLORREF,
    /// Omnibox pill background / outline.
    pub pill: COLORREF,
    pub pill_line: COLORREF,
    /// Hover / pressed backgrounds for icon buttons.
    pub hover: COLORREF,
    pub text: COLORREF,
    pub muted: COLORREF,
    /// Tab pills.
    pub tab: COLORREF,
    pub tab_active: COLORREF,
    pub tab_line: COLORREF,
    pub tab_text: COLORREF,
    pub tab_text_active: COLORREF,
    /// Accent as RGB, for the active tab's indicator bar.
    pub accent: (u8, u8, u8),
}

impl Palette {
    const fn dark() -> Self {
        Palette {
            dark: true,
            bar: rgb(0x2B, 0x2B, 0x2B),
            frame: rgb(0x1E, 0x1E, 0x1E),
            pill: rgb(0x3A, 0x3A, 0x3A),
            pill_line: rgb(0x4A, 0x4A, 0x4A),
            hover: rgb(0x3A, 0x3A, 0x3A),
            text: rgb(0xE8, 0xEA, 0xED),
            muted: rgb(0x9A, 0xA0, 0xA6),
            tab: rgb(0x33, 0x33, 0x33),
            tab_active: rgb(0x45, 0x45, 0x45),
            tab_line: rgb(0x4E, 0x4E, 0x4E),
            tab_text: rgb(0xB9, 0xBD, 0xC2),
            tab_text_active: rgb(0xFF, 0xFF, 0xFF),
            accent: (0x00, 0xE6, 0x76),
        }
    }

    const fn light() -> Self {
        Palette {
            dark: false,
            bar: rgb(0xF1, 0xF3, 0xF4),
            frame: rgb(0xDA, 0xDC, 0xE0),
            pill: rgb(0xFF, 0xFF, 0xFF),
            pill_line: rgb(0xD6, 0xD9, 0xDD),
            hover: rgb(0xE3, 0xE5, 0xE8),
            text: rgb(0x20, 0x21, 0x24),
            muted: rgb(0x5F, 0x63, 0x68),
            tab: rgb(0xE7, 0xE9, 0xEC),
            tab_active: rgb(0xFF, 0xFF, 0xFF),
            tab_line: rgb(0xD6, 0xD9, 0xDD),
            tab_text: rgb(0x5F, 0x63, 0x68),
            tab_text_active: rgb(0x20, 0x21, 0x24),
            accent: (0x00, 0x9E, 0x60),
        }
    }
}

// ---------------------------------------------------------------------------
// Events queued by wry callbacks, drained by the message loop
// ---------------------------------------------------------------------------

pub(crate) enum Ev {
    NavStarted { tab: u64, url: String },
    PageLoad { tab: u64, url: String },
    Title { tab: u64, title: String },
    OpenTab { url: String },
    Ipc { tab: u64, msg: String },
    DownloadDone {
        ok: bool,
        name: String,
        url: String,
        /// The engine may omit the path when the download failed early.
        path: Option<PathBuf>,
    },
}

thread_local! {
    static PENDING: RefCell<Vec<Ev>> = const { RefCell::new(Vec::new()) };
}

/// Called from wry handlers and the window proc - always the main thread,
/// which is what keeps the queue lock-free.
pub(crate) fn push_ev(ev: Ev) {
    PENDING.with(|p| p.borrow_mut().push(ev));
}

/// One line on stdout, never panicking.
///
/// The browser is a `windows_subsystem = "windows"` process in every build —
/// including debug — so there may be no console handle at all. `println!`
/// panics when the write fails, which would turn `--smoke` into exit code 101.
fn out(line: &str) {
    use std::io::Write;
    let _ = writeln!(std::io::stdout(), "{line}");
}

/// The acknowledgement for a reset — deliberately **not** `__gtxAck`.
///
/// The old flow reused the save acknowledgement, so the settings footer said
/// "Saved ✓" the moment you pressed "Reset all settings", which reads as
/// "your settings were kept".
fn reset_ack_js(cfg: &config::Config) -> String {
    format!("window.__gtxReset && window.__gtxReset({});", cfg.to_json())
}

/// Put `text` on the system clipboard as Unicode text.
///
/// The settings page's "Copy log path" uses this instead of
/// `navigator.clipboard`, which is not dependable from a `data:` document.
fn copy_to_clipboard(owner: HWND, text: &str) -> bool {
    use windows::Win32::System::DataExchange::{
        CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData,
    };
    use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
    use windows::Win32::System::Ole::CF_UNICODETEXT;
    unsafe {
        if OpenClipboard(Some(owner)).is_err() {
            return false;
        }
        let mut ok = false;
        if EmptyClipboard().is_ok() {
            let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
            let bytes = wide.len() * 2;
            if let Ok(mem) = GlobalAlloc(GMEM_MOVEABLE, bytes) {
                let dst = GlobalLock(mem) as *mut u16;
                if !dst.is_null() {
                    for (i, w) in wide.iter().enumerate() {
                        *dst.add(i) = *w;
                    }
                    let _ = GlobalUnlock(mem);
                    ok = SetClipboardData(CF_UNICODETEXT.0.into(), Some(HANDLE(mem.0))).is_err() == false;
                }
                if !ok {
                    let _ = windows::Win32::Foundation::GlobalFree(Some(mem));
                }
            }
        }
        let _ = CloseClipboard();
        ok
    }
}

/// What [`App::initialize`] builds, decided without touching the window.
///
/// The point of the exercise: the link that brought the process up must
/// never be swallowed by a restored session - that is exactly what made
/// "click a link, GTX opens without it" reproducible on every start.
#[derive(Debug, PartialEq, Eq)]
enum Startup {
    /// Nothing to restore: one tab, holding the link (or the start page).
    Fresh(String),
    /// The saved tabs come back first; `link` opens on top as the active tab.
    Restored { link: Option<String> },
}

fn plan_startup(
    session_tabs: usize,
    restore_session: bool,
    link: Option<&str>,
    new_tab_url: &str,
) -> Startup {
    let link = link.map(str::to_string);
    if restore_session && session_tabs > 0 {
        Startup::Restored { link }
    } else {
        Startup::Fresh(link.unwrap_or_else(|| new_tab_url.to_string()))
    }
}

/// Native confirmation before the config is overwritten on disk.
///
/// Owned by the browser window (so the dialog sits on top of it) and modal
/// for the thread: nothing else can click while the question is open.
fn confirm_reset(owner: HWND) -> bool {
    let text = format!(
        "Reset every setting to its GTX default?\n\n\
         Your browsing history, downloads and profile data are kept.\n\n\
         The values in\n  {}\nwill be replaced.",
        config::config_path().display()
    );
    let answer: MESSAGEBOX_RESULT = unsafe {
        MessageBoxW(
            Some(owner),
            &HSTRING::from(text),
            &HSTRING::from("Reset all settings"),
            MB_YESNO | MB_ICONWARNING,
        )
    };
    answer == IDYES
}

// The address-bar subclass keeps its original proc here so the trampoline
// never has to touch `App` while it is mutably borrowed.
/// Drop-down row height and inner padding (96-dpi units, scaled on use).
const POPUP_ROW: i32 = 44;
const POPUP_PAD: i32 = 4;

/// Which row of the drop-down a client `y` falls in — shared by the painter
/// and the click handler so the two can never disagree.
fn row_at(y: i32, rows: usize, row_h: i32, pad: i32) -> Option<usize> {
    if row_h <= 0 || y < pad {
        return None;
    }
    let i = (y - pad) / row_h;
    (i >= 0 && (i as usize) < rows).then_some(i as usize)
}

/// What the little glyph on a drop-down row stands for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RowIcon {
    /// A magnifier: this row runs a search.
    Search,
    /// A globe: this row is the address you are about to visit.
    Address,
    /// A clock: a page from history.
    History,
    /// A document: a downloaded file.
    File,
    /// A folder: “open the downloads folder”.
    Folder,
}

/// One two-line row of the drop-down panel, with its icon in the gutter.
unsafe fn draw_popup_row(
    hdc: HDC,
    r: R,
    label: &str,
    sub: &str,
    selected: bool,
    icon: Option<RowIcon>,
    favicon: Option<HBITMAP>,
    p: &Palette,
    inset: i32,
    font: HFONT,
) {
    if r.w <= 0 || r.h <= 0 {
        return;
    }
    // The panel behind a row is `p.pill`, and in the dark theme that is the
    // very colour chrome uses for hover — rows need their own, lighter tone
    // or a hovered row would not light up at all.
    let hl = if p.dark { rgb(0x4A, 0x4A, 0x4A) } else { p.hover };
    if selected {
        unsafe {
            round_box(hdc, r, inset.max(4), hl, hl);
        }
    }
    let mut x = r.x + inset;
    let mut w = r.w - 2 * inset;
    if let Some(ic) = icon {
        let g = (r.h / 2 - inset).clamp(10, 22);
        let box_ = R::new(x, r.y + (r.h - g) / 2, g, g);
        unsafe {
            match favicon {
                // The site's own icon where we have one; the vector glyph is
                // only the fallback (a file, a clock, a magnifier).
                Some(hbm) => favicon::draw(hdc, hbm, box_.x + 1, box_.y + 1, g - 2, g - 2),
                None => draw_row_icon(hdc, box_, ic, p.muted, if selected { hl } else { p.pill }),
            }
        }
        x += g + inset / 2;
        w = r.right() - inset - x;
    }
    let fmt = DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS | DT_NOPREFIX;
    if sub.is_empty() {
        draw_text(hdc, label, x, r.y, w, r.h, p.text, font, fmt);
    } else {
        let half = r.h / 2;
        draw_text(hdc, label, x, r.y, w, half + 1, p.text, font, fmt);
        draw_text(hdc, sub, x, r.y + half, w, r.h - half, p.muted, font, fmt);
    }
}

/// The row glyphs, drawn as vectors so they stay crisp at any DPI and never
/// depend on an icon font being installed. `bg` is the colour behind the row
/// (the panel's pill, or the hover colour when it is selected) and hollows
/// out the round shapes.
unsafe fn draw_row_icon(hdc: HDC, r: R, icon: RowIcon, color: COLORREF, bg: COLORREF) {
    if r.w <= 0 || r.h <= 0 {
        return;
    }
    let cx = r.x + r.w / 2;
    let cy = r.y + r.h / 2;
    let rad = (r.w.min(r.h) / 2 - 2).max(3);
    let width = 1.max(r.w / 14);
    unsafe {
        match icon {
            RowIcon::Search => draw_magnifier(hdc, cx, cy, rad - 1, color, bg, width),
            RowIcon::Address => {
                // Globe: outline, equator, meridian.
                circle(hdc, cx, cy, rad, color, bg, width);
                stroke(hdc, cx - rad, cy, cx + rad, cy, color, width);
                stroke(hdc, cx, cy - rad, cx, cy + rad, color, width);
            }
            RowIcon::History => {
                // Clock: outline, then the hands.
                circle(hdc, cx, cy, rad, color, bg, width);
                stroke(hdc, cx, cy, cx, cy - rad * 2 / 3, color, width);
                stroke(hdc, cx, cy, cx + rad / 2, cy, color, width);
            }
            RowIcon::File => {
                // A page with two lines of text on it.
                let q = R::new(r.x + 2, r.y + 1, r.w - 4, r.h - 2);
                stroke_rect(hdc, q, color, width);
                for k in 1..=2 {
                    let y = q.y + (q.h * k) / 3;
                    stroke(hdc, q.x + 3, y, q.right() - 3, y, color, width);
                }
            }
            RowIcon::Folder => {
                // Tab over a body.
                stroke(hdc, r.x + 1, r.y + r.h * 2 / 5, r.x + 1, r.y + r.h / 5, color, width);
                stroke(hdc, r.x + 1, r.y + r.h / 5, r.x + r.w / 2, r.y + r.h / 5, color, width);
                stroke(
                    hdc,
                    r.x + r.w / 2,
                    r.y + r.h / 5,
                    r.x + r.w / 2,
                    r.y + r.h * 2 / 5,
                    color,
                    width,
                );
                stroke_rect(
                    hdc,
                    R::new(r.x + 1, r.y + r.h * 2 / 5, r.w - 2, r.h * 3 / 5 - 1),
                    color,
                    width,
                );
            }
        }
    }
}

/// A stroked circle hollowed out with `bg`, so it never paints over what is
/// behind it.
unsafe fn circle(hdc: HDC, cx: i32, cy: i32, rad: i32, color: COLORREF, bg: COLORREF, width: i32) {
    unsafe {
        let pen = CreatePen(PS_SOLID, width.max(1), color);
        let brush = CreateSolidBrush(bg);
        let op = SelectObject(hdc, HGDIOBJ(pen.0));
        let ob = SelectObject(hdc, HGDIOBJ(brush.0));
        let _ = Ellipse(hdc, cx - rad, cy - rad, cx + rad, cy + rad);
        SelectObject(hdc, op);
        SelectObject(hdc, ob);
        let _ = DeleteObject(HGDIOBJ(pen.0));
        let _ = DeleteObject(HGDIOBJ(brush.0));
    }
}

thread_local! {
    static ADDRESS_ORIG: core::cell::Cell<usize> = const { core::cell::Cell::new(0) };
}

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

#[derive(Default)]
pub struct Controls {
    /// The single painted row at the top of the window.
    pub chrome: HWND,
    /// The real `EDIT` living inside the chrome's omnibox pill.
    pub address: HWND,
    /// Floating notification pill over the page.
    pub toast: HWND,
    /// Drop-down panel: address-bar suggestions and the downloads list.
    pub popup: HWND,
}

pub struct Tab {
    pub id: u64,
    pub url: String,
    pub title: String,
    /// `None` = not instantiated (fresh/background/hibernated tab).
    pub view: Option<WebView>,
    pub zoom: f64,
    pub last_active: Instant,
    pub kind: TabKind,
}

/// Which drop-down (if any) is open under the chrome.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PopupKind {
    None,
    /// Address-bar suggestions for what is being typed.
    Suggest,
    /// The downloads list, under the toolbar's download button.
    Downloads,
}

/// Where the omnibox loading bar is: hidden, easing towards the ceiling while
/// a page loads, or holding at 100% just before it fades away.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum LoadBar {
    Hidden,
    Loading {
        started: Instant,
    },
    Done {
        at: Instant,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TabKind {
    Normal,
    /// The settings page: IPC commands are only honoured from here.
    Settings,
    /// `gtx://newtab` — the blank themed start surface.
    NewTab,
    /// `gtx://history`.
    History,
}

impl TabKind {
    /// Internal pages are rendered from HTML we generate, never loaded from
    /// the network, and their IPC is not privileged.
    pub fn is_internal(self) -> bool {
        !matches!(self, TabKind::Normal)
    }

    /// The document for an internal tab (regenerated on every load so
    /// history/settings reflect the current state).
    pub fn html(self) -> Option<String> {
        match self {
            TabKind::Normal => None,
            TabKind::Settings => Some(settings::HTML.to_string()),
            TabKind::NewTab => Some(internal::new_tab_html()),
            TabKind::History => Some(history::render()),
        }
    }
}

/// Work that arrived while a web view was being created (WebView2 builds it
/// through a nested message pump, so clicks can re-enter us). Queued here and
/// replayed once the build returns, in order.
enum Defer {
    Activate(usize),
    Close(usize),
    Shutdown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hit {
    None,
    Back,
    Forward,
    Reload,
    Home,
    Omnibox,
    Tab(usize),
    Close(usize),
    Plus,
    /// The downloads button, between `+` and the kebab.
    Downloads,
    Kebab,
    CapMin,
    CapMax,
    CapClose,
}

impl Hit {
    /// Caption buttons must keep returning `HTCLIENT` so Windows never turns
    /// a click on them into a resize.
    pub fn is_caption(self) -> bool {
        matches!(self, Hit::CapMin | Hit::CapMax | Hit::CapClose)
    }
}

/// Geometry of one chrome row, in chrome-window pixels.
#[derive(Clone, Copy, Default)]
pub struct ChromeLayout {
    pub back: R,
    pub forward: R,
    pub reload: R,
    pub home: R,
    /// The rounded pill.
    pub pill: R,
    /// The real `EDIT` inside the pill.
    pub addr: R,
    /// Horizontal band the tab pills may occupy.
    pub tabs_x: i32,
    pub tabs_w: i32,
    pub plus: R,
    pub downloads: R,
    pub kebab: R,
    pub cap_min: R,
    pub cap_max: R,
    pub cap_close: R,
    pub row_h: i32,
}

pub struct App {
    pub ptr: AppPtr,
    pub hwnd: HWND,
    pub started: Instant,
    pub dpi: u32,
    pub cfg: config::Config,
    pub font: HFONT,
    pub font_icon: HFONT,
    pub controls: Controls,
    pub accel: HACCEL,
    pub web_context: WebContext,
    pub tabs: Vec<Tab>,
    pub active: usize,
    pub next_id: u64,
    pub address_focused: bool,
    pub hover: Hit,
    pub no_session: bool,
    pub smoke_ms: Option<u32>,
    pub startup_url: Option<String>,
    /// Recently closed tabs (Ctrl+Shift+T).
    pub closed_tabs: Vec<(String, String)>,
    /// Saved style/rect while fullscreen (F11).
    pub pre_fullscreen: Option<(WINDOW_STYLE, RECT)>,
    /// Last session written to disk, for cheap dirty-checking.
    pub last_saved: Vec<(String, String)>,
    pub last_saved_active: usize,
    /// Windows' app light/dark preference, read once (used when `theme=auto`).
    pub system_dark: bool,
    /// Brush handed to the address edit by `WM_CTLCOLOREDIT`.
    pub edit_brush: HBRUSH,
    pub edit_brush_color: COLORREF,
    /// True while a web view is being built (see [`Defer`]).
    pub busy: bool,
    deferred: Vec<Defer>,
    /// Pending drag origin inside the chrome, in chrome client pixels.
    pub drag: Option<(i32, i32)>,
    /// Message painted across the content area when the active tab has no
    /// web view (creation failed) — the "can't open this tab" page.
    pub last_error: Option<String>,
    /// Loading progress painted along the omnibox's bottom edge.
    pub load: LoadBar,
    /// Focus the address bar once the tab queued by [`Self::new_tab`]
    /// finishes activating (a fresh tab is ready to type into).
    pub focus_addr_pending: bool,
    /// Which drop-down is open, its rows and the highlighted row.
    pub popup: PopupKind,
    pub suggest: Vec<suggest::Suggestion>,
    pub suggest_sel: usize,
    /// Row under the mouse inside the popup.
    pub popup_hover: Option<usize>,
    /// Favicons posted by the pages, one bitmap per host.
    pub icons: favicon::Cache,
    /// Tinted supplied SVG toolbar glyphs, replacing their handles on theme change.
    toolbar_icons: RefCell<toolbar_icons::Set>,
    /// Tab the chrome mouse pressed on: (index, press x, y). Becomes a drag
    /// once the cursor moves past the drag threshold.
    pub tab_press: Option<(usize, i32, i32)>,
    /// Live tab drag: (from, insert-before slot in the current visual order).
    pub tab_drag: Option<(usize, usize)>,
    /// `tmHeight` of `font`: the address EDIT draws its text flush to its
    /// top, so the box is centred on this line height to centre the text.
    pub font_line_h: i32,
}

impl App {
    pub fn new(
        hwnd: HWND,
        cfg: config::Config,
        started: Instant,
        startup_url: Option<String>,
        smoke_ms: Option<u32>,
    ) -> Self {
        let profile = config::profile_dir();
        let _ = std::fs::create_dir_all(&profile);
        let mut dpi = unsafe { GetDpiForWindow(hwnd) };
        if dpi == 0 {
            dpi = 96;
        }
        let system_dark = w32::apps_dark();
        let toolbar_palette = match cfg.theme.as_str() {
            "dark" => Palette::dark(),
            "light" => Palette::light(),
            _ if system_dark => Palette::dark(),
            _ => Palette::light(),
        };
        Self {
            ptr: AppPtr::null(),
            hwnd,
            started,
            dpi,
            cfg,
            font: HFONT(core::ptr::null_mut()),
            font_icon: HFONT(core::ptr::null_mut()),
            controls: Controls::default(),
            accel: HACCEL(core::ptr::null_mut()),
            web_context: WebContext::new(Some(profile)),
            tabs: Vec::new(),
            active: 0,
            next_id: 1,
            address_focused: false,
            hover: Hit::None,
            no_session: smoke_ms.is_some(),
            smoke_ms,
            startup_url,
        closed_tabs: Vec::new(),
        pre_fullscreen: None,
        last_saved: Vec::new(),
            last_saved_active: usize::MAX,
            system_dark,
            edit_brush: HBRUSH(core::ptr::null_mut()),
            edit_brush_color: COLORREF(0xFFFF_FFFF),
            busy: false,
            deferred: Vec::new(),
            drag: None,
            last_error: None,
            load: LoadBar::Hidden,
            focus_addr_pending: false,
            popup: PopupKind::None,
            suggest: Vec::new(),
            suggest_sel: 0,
            popup_hover: None,
            icons: favicon::Cache::load(),
            toolbar_icons: RefCell::new(toolbar_icons::Set::new(
                toolbar_palette.muted,
                toolbar_palette.text,
            )),
            tab_press: None,
            tab_drag: None,
            font_line_h: 0,
        }
    }

    /// Publish `self` so window procs can find it.
    pub fn bind(&mut self) {
        let p = AppPtr(&mut *self as *mut App);
        self.ptr = p;
        unsafe { w32::set_user_data(self.hwnd, p) };
    }

    /// Build chrome, accelerators, timers and the first tab.
    pub fn initialize(&mut self, session: Option<session::Session>) {
        self.recreate_fonts();
        self.create_controls();
        self.apply_frame();
        self.layout();
        self.accel = build_accelerators(&self.cfg);

        unsafe {
            SetTimer(Some(self.hwnd), IDT_TICK, 30_000, None);
            if let Some(ms) = self.smoke_ms {
                SetTimer(Some(self.hwnd), IDT_SMOKE, ms, None);
            }
        }

        let start_page = self.cfg.new_tab_url.clone();
        let saved = session.as_ref().map(|s| s.tabs.len()).unwrap_or(0);
        match plan_startup(
            saved,
            self.cfg.restore_session,
            self.startup_url.as_deref(),
            &start_page,
        ) {
            Startup::Fresh(url) => self.add_tab(&url),
            Startup::Restored { link } => {
                let s = session.expect("a restored plan implies a session");
                for t in &s.tabs {
                    self.tabs.push(Tab {
                        id: self.next_id,
                        url: t.url.clone(),
                        title: t.title.clone(),
                        view: None,
                        zoom: 1.0,
                        last_active: Instant::now(),
                        // Internal pages keep their privileged kind across
                        // restarts so they render from our own HTML.
                        kind: kind_for_url(&t.url),
                    });
                    self.next_id += 1;
                }
                let active = s.active.min(self.tabs.len() - 1);
                if let Some(w) = s.window {
                    self.restore_window(w);
                }
                self.active = usize::MAX; // force the full activation path
                self.set_active(active);
                // The link that launched us opens on top of the session and
                // takes the focus: that is what Windows was asked to do.
                if let Some(u) = link {
                    self.add_tab(&u);
                }
            }
        }

        // Baseline for the dirty-check so the first tick doesn't rewrite an
        // unchanged session.
        self.last_saved = self.session_snapshot();
        self.last_saved_active = if self.tabs.is_empty() {
            0
        } else {
            self.active.min(self.tabs.len() - 1)
        };

        self.drain_events();
    }

    fn restore_window(&self, w: session::WindowEntry) {
        let width = w.w.max(640);
        let height = w.h.max(480);
        // A session can outlive the screen it was saved on: a monitor gets
        // unplugged, or the entry is junk (`GetWindowRect` of a *minimized*
        // window is the -32000 sentinel). Only restore onto a monitor that
        // still exists; otherwise keep the default centred position, which is
        // always visible.
        let rect = RECT {
            left: w.x,
            top: w.y,
            right: w.x + width,
            bottom: w.y + height,
        };
        let on_screen = !unsafe { MonitorFromRect(&rect, MONITOR_DEFAULTTONULL) }.0.is_null();
        unsafe {
            if w.maximized {
                let _ = ShowWindow(self.hwnd, SW_MAXIMIZE);
            } else if on_screen {
                let _ = SetWindowPos(
                    self.hwnd,
                    None,
                    w.x,
                    w.y,
                    width,
                    height,
                    SWP_NOZORDER | SWP_NOACTIVATE,
                );
            }
        }
    }

    // -----------------------------------------------------------------
    // Layout
    // -----------------------------------------------------------------

    pub fn s(&self, v: i32) -> i32 {
        w32::scale(v, self.dpi)
    }

    // Single-row chrome metrics (96-dpi design units, scaled on use).
    /// Total height of the chrome row (which also hosts the top resize band).
    pub fn ui_row_h(&self) -> i32 {
        // Tall enough that a centred control still lands *below* the invisible
        // resize band: row = control + 2 × band (rounded up to an even
        // leftover) means pill, buttons and tabs come out with exactly equal
        // padding above and below — whatever system frame metrics apply, not
        // only the 8px band the 46px design constant was tuned for.
        let design = self.s(if self.cfg.compact_mode { 40 } else { 46 });
        let control = self.ui_addr_h().max(self.ui_btn());
        let mut row = design.max(control + 2 * w32::frame_band());
        if (row - control) % 2 != 0 {
            row += 1;
        }
        row
    }
    pub fn ui_btn(&self) -> i32 {
        self.s(if self.cfg.compact_mode { 24 } else { 28 })
    }
    pub fn ui_addr_h(&self) -> i32 {
        self.s(if self.cfg.compact_mode { 24 } else { 30 })
    }
    pub fn ui_tab_h(&self) -> i32 {
        self.ui_addr_h()
    }
    pub fn ui_capw(&self) -> i32 {
        self.s(if self.cfg.compact_mode { 40 } else { 46 })
    }
    pub fn ui_kebab_w(&self) -> i32 {
        self.s(if self.cfg.compact_mode { 30 } else { 34 })
    }
    pub fn ui_plus_w(&self) -> i32 {
        self.s(if self.cfg.compact_mode { 28 } else { 32 })
    }

    /// Vertical position of an `h`-tall control: centred in the row, but never
    /// inside the invisible resize band along the top edge. With `ui_row_h`
    /// sized as `h + 2 × band` the centred position *is* the band edge, so the
    /// padding comes out equal top and bottom.
    fn row_y(&self, row_h: i32, h: i32) -> i32 {
        ((row_h - h) / 2).max(w32::frame_band())
    }

    /// The page area: inside the frame band, below the chrome row.
    fn content_rect(&self) -> Rect {
        let (w, h) = w32::client_size(self.hwnd);
        let row = self.ui_row_h();
        let band = w32::frame_band();
        Rect {
            position: PhysicalPosition::new(band, row).into(),
            size: PhysicalSize::new((w - 2 * band).max(0), (h - row - band).max(0)).into(),
        }
    }

    /// Position the chrome row, the address edit inside it, and the content
    /// area (every web view).
    pub fn layout(&self) {
        if self.controls.chrome.0.is_null() {
            return;
        }
        let (w, h) = w32::client_size(self.hwnd);
        if w <= 0 || h <= 0 {
            return;
        }
        let row_h = self.ui_row_h();
        unsafe {
            let _ = MoveWindow(self.controls.chrome, 0, 0, w, row_h, true);
        }
        let lay = self.chrome_layout(w, row_h);
        unsafe {
            let _ = MoveWindow(
                self.controls.address,
                lay.addr.x,
                lay.addr.y,
                lay.addr.w,
                lay.addr.h,
                true,
            );
        }

        let content = self.content_rect();
        for t in &self.tabs {
            if let Some(v) = &t.view {
                let _ = v.set_bounds(content);
            }
        }
        // An open drop-down is anchored to chrome geometry that just moved.
        if self.popup != PopupKind::None {
            let (x, y, w, h) = self.popup_geometry();
            popup::show(self.controls.popup, x, y, w, h);
        }
    }

    pub fn on_dpi_changed(&mut self, dpi: u32, suggested: RECT) {
        self.dpi = dpi;
        self.recreate_fonts();
        self.apply_fonts();
        unsafe {
            let _ = SetWindowPos(
                self.hwnd,
                None,
                suggested.left,
                suggested.top,
                suggested.right - suggested.left,
                suggested.bottom - suggested.top,
                SWP_NOZORDER | SWP_NOACTIVATE,
            );
        }
        self.invalidate_chrome();
    }

    // -----------------------------------------------------------------
    // Chrome construction
    // -----------------------------------------------------------------

    fn recreate_fonts(&mut self) {
        w32::delete_font(self.font);
        w32::delete_font(self.font_icon);
        let (ui_pt, icon_pt) = if self.cfg.compact_mode { (8, 9) } else { (9, 10) };
        self.font = w32::create_font(self.dpi, "Segoe UI", ui_pt, 400);
        self.font_icon = w32::create_font(self.dpi, "Segoe UI Symbol", icon_pt, 400);
        self.font_line_h = w32::font_line_height(self.font);
    }

    fn apply_fonts(&self) {
        unsafe {
            SendMessageW(
                self.controls.address,
                WM_SETFONT,
                Some(WPARAM(self.font.0 as usize)),
                Some(LPARAM(1)),
            );
        }
        self.update_cue_banner();
    }

    /// The omnibox placeholder is drawn by [`paint_address_cue`] over the
    /// edit's own paint — `EM_SETCUEBANNER` needs a Comctl32 v6 activation
    /// context this process does not reliably get (even a plain, manifested
    /// control rejects the message). This just asks the edit to repaint so
    /// the cue re-derives from the current font, colours and engine.
    fn update_cue_banner(&self) {
        unsafe {
            let _ = InvalidateRect(Some(self.controls.address), None, false);
        }
    }

    fn create_controls(&mut self) {
        unsafe {
            let parent = self.hwnd;
            self.controls.chrome = chrome::create(parent, self.ptr, 0, 0, 0, 0);
            self.controls.toast = toast::create(parent, self.ptr);
            self.controls.popup = popup::create(parent, self.ptr);

            // A real EDIT, no client edge: the pill is painted behind it and
            // WM_CTLCOLOREDIT gives it the pill's colours.
            let address = CreateWindowExW(
                Default::default(),
                w!("EDIT"),
                w!(""),
                WS_CHILD | WS_VISIBLE | WS_TABSTOP
                    | WINDOW_STYLE(windows::Win32::UI::WindowsAndMessaging::ES_AUTOHSCROLL as u32),
                0,
                0,
                0,
                0,
                Some(self.controls.chrome),
                Some(HMENU(IDC_ADDRESS as usize as *mut core::ffi::c_void)),
                None,
                None,
            )
            .expect("address bar");
            self.controls.address = address;
            SendMessageW(address, WM_SETFONT, Some(WPARAM(self.font.0 as usize)), Some(LPARAM(1)));
            // Typed text and the cue share the same inset.
            SendMessageW(
                address,
                EM_SETMARGINS,
                Some(WPARAM(3)), // EC_LEFTMARGIN | EC_RIGHTMARGIN
                Some(LPARAM(((CUE_X as isize) << 16) | CUE_X as isize)),
            );
            self.subclass_address(address);
            self.update_cue_banner();
        }
    }

    /// Take over Enter/Esc in the address bar. The original proc is kept in a
    /// thread local so the trampoline never has to borrow `App` when forwarding.
    fn subclass_address(&self, address: HWND) {
        unsafe {
            let orig = GetWindowLongPtrW(address, GWLP_WNDPROC);
            ADDRESS_ORIG.with(|c| c.set(orig as usize));
            w32::set_user_data(address, self.ptr);
            let proc: WNDPROC = Some(address_proc);
            SetWindowLongPtrW(address, GWLP_WNDPROC, proc.map(|f| f as usize as isize).unwrap_or(0));
        }
    }

    // -----------------------------------------------------------------
    // Tabs
    // -----------------------------------------------------------------

    pub fn new_tab(&mut self) {
        // A tab the user asked for is handed back ready to type into; the
        // focus lands once it has really become active (see [`Self::set_active`]).
        self.focus_addr_pending = true;
        let url = self.cfg.new_tab_url.clone();
        self.add_tab(&url);
    }

    pub fn add_tab(&mut self, url: &str) {
        let id = self.next_id;
        self.next_id += 1;
        self.tabs.push(Tab {
            id,
            url: url.to_string(),
            title: if url == internal::NEW_TAB_URL {
                internal::NEW_TAB_TITLE.to_string()
            } else {
                String::new()
            },
            view: None,
            zoom: 1.0,
            last_active: Instant::now(),
            kind: kind_for_url(url),
        });
        let idx = self.tabs.len() - 1;
        self.set_active(idx);
    }

    pub fn close_tab(&mut self, idx: usize) {
        if self.busy {
            // A view is mid-build; replay once it settles.
            self.deferred.push(Defer::Close(idx));
            return;
        }
        if idx >= self.tabs.len() {
            return;
        }
        // Remember it for Ctrl+Shift+T (cap the stack).
        if self.tabs[idx].kind == TabKind::Normal {
            self.closed_tabs.push((self.tabs[idx].url.clone(), self.tabs[idx].title.clone()));
            if self.closed_tabs.len() > 25 {
                self.closed_tabs.remove(0);
            }
        }
        let was_active = idx == self.active;
        let new_active = if idx < self.active { self.active - 1 } else { self.active };
        self.tabs.remove(idx); // drops the web view, if any
        if self.tabs.is_empty() {
            self.on_close();
            return;
        }
        if was_active {
            self.active = usize::MAX; // force full activation
            self.set_active(new_active.min(self.tabs.len() - 1));
        } else {
            self.active = new_active;
            self.sync_chrome();
            self.invalidate_chrome();
        }
    }

    pub fn set_active(&mut self, idx: usize) {
        if self.busy {
            self.deferred.push(Defer::Activate(idx));
            return;
        }
        if idx >= self.tabs.len() {
            return;
        }
        let prev = self.active;
        if prev != idx && prev < self.tabs.len() {
            if let Some(v) = &self.tabs[prev].view {
                let _ = v.set_visible(false);
            }
        }
        self.active = idx;
        self.tabs[idx].last_active = Instant::now();
        if self.tabs[idx].view.is_none() {
            self.create_view(idx);
        }
        if let Some(v) = &self.tabs[idx].view {
            let _ = v.set_visible(true);
            let _ = v.focus();
        }
        self.hover = Hit::None;
        toast::hide(self.controls.toast);
        self.hide_popup();
        self.sync_chrome();
        self.invalidate_chrome();
        // A tab queued by `new_tab` is live now: give the user the address
        // bar with its text selected, so they can just start typing.
        self.spend_address_focus();
    }

    /// Spend the request [`Self::new_tab`] made: focus the address bar and
    /// select what is in it. Runs last in an activation, after the view is
    /// built and focused — building a WebView2 steals focus back.
    fn spend_address_focus(&mut self) {
        if core::mem::take(&mut self.focus_addr_pending) {
            self.focus_address();
        }
    }

    fn cycle_tab(&mut self, delta: i32) {
        let n = self.tabs.len();
        if n == 0 {
            return;
        }
        let cur = if self.active >= n { 0 } else { self.active as i32 };
        let next = (cur + delta).rem_euclid(n as i32) as usize;
        self.set_active(next);
    }

    /// Instantiate a tab's web view (lazy: only ever called for the active tab).
    ///
    /// WebView2 builds the view through a nested message pump, so clicks and
    /// timers can re-enter us mid-construction. [`Self::busy`] blocks nested
    /// builds and the re-entrant intent is queued as [`Defer`], replayed by
    /// [`Self::run_deferred`] as soon as this returns.
    fn create_view(&mut self, idx: usize) {
        if self.busy {
            return;
        }
        if idx >= self.tabs.len() {
            return;
        }
        let id = self.tabs[idx].id;
        let url = self.tabs[idx].url.clone();
        let kind = self.tabs[idx].kind;
        let zoom = self.tabs[idx].zoom;
        let cfg = self.cfg.clone();
        let bounds = self.content_rect();
        let parent = WinHandle(self.hwnd);
        let ptr = self.ptr;
        let html = kind.html();
        let dark = self.resolved_dark();

        self.busy = true;
        let built = build_view(
            &cfg,
            &mut self.web_context,
            parent,
            bounds,
            id,
            &url,
            html.as_deref(),
            ptr,
            dark,
        );
        self.busy = false;

        match built {
            Ok(view) => {
                if (zoom - 1.0).abs() > f64::EPSILON {
                    let _ = view.zoom(zoom);
                }
                let theme = theme_from_str(&cfg.theme);
                let _ = view.set_theme(theme);
                let _ = view.set_background_color(page_bg(dark));
                // The tab may have been closed by a click that arrived during
                // the build — only publish the view if it is still ours.
                match self.find_tab(id) {
                    Some(i) => {
                        self.tabs[i].view = Some(view);
                        if i == self.active {
                            self.last_error = None;
                        }
                    }
                    None => drop(view),
                }
                self.invalidate_chrome();
                // The panel is an owned top-level window: it stays stacked
                // above its owner (and therefore above the fresh web view).
            }
            Err(err) => {
                log::write(&format!("web view creation failed for `{url}`: {err}"));
                let msg = format!("GTX could not open this tab\n\n{url}\n\n{err}");
                self.last_error = Some(msg);
                self.show_toast("Tab could not be opened — see the log");
                self.invalidate_content();
            }
        }
        self.run_deferred();
    }

    /// Replay work that was queued while a view was being built.
    fn run_deferred(&mut self) {
        let mut rounds = 0;
        while !self.deferred.is_empty() && rounds < 16 {
            rounds += 1;
            for op in core::mem::take(&mut self.deferred) {
                match op {
                    Defer::Activate(i) => self.set_active(i),
                    Defer::Close(i) => self.close_tab(i),
                    Defer::Shutdown => self.on_close(),
                }
            }
        }
    }

    // -----------------------------------------------------------------
    // Navigation
    // -----------------------------------------------------------------

    pub fn address_submit(&mut self) {
        // A highlighted row is what Enter means while the panel is open.
        if self.popup == PopupKind::Suggest {
            if let Some(s) = self.suggest.get(self.suggest_sel).cloned() {
                self.hide_popup();
                self.load_active(&s.target);
                self.focus_page();
                return;
            }
        }
        let text = w32::window_text(self.controls.address);
        let url = nav::resolve(&text, &self.cfg.search_template, &self.cfg.homepage);
        self.load_active(&url);
        self.focus_page();
    }

    fn load_active(&mut self, url: &str) {
        let i = self.active;
        if i >= self.tabs.len() {
            return;
        }
        self.tabs[i].url = url.to_string();
        if self.tabs[i].view.is_none() {
            self.create_view(i);
        }
        if let Some(v) = &self.tabs[i].view {
            let _ = v.load_url(url);
        }
        self.sync_address();
    }

    pub fn go_back(&mut self) {
        self.with_view(|v| {
            let _ = v.go_back();
        });
    }

    pub fn go_forward(&mut self) {
        self.with_view(|v| {
            let _ = v.go_forward();
        });
    }

    pub fn reload(&mut self) {
        let i = self.active;
        if i >= self.tabs.len() {
            return;
        }
        // Internal pages are generated HTML — regenerate (history/settings
        // changed since the last render) instead of asking the engine.
        if let Some(html) = self.tabs[i].kind.html() {
            if let Some(v) = &self.tabs[i].view {
                let _ = v.load_html(&html);
            }
            return;
        }
        self.with_view(|v| {
            let _ = v.reload();
        });
    }

    /// Open the history page (focus it if it already exists).
    pub fn open_history(&mut self) {
        if let Some(i) = self
            .tabs
            .iter()
            .position(|t| t.kind == TabKind::History)
        {
            self.set_active(i);
            return;
        }
        let id = self.next_id;
        self.next_id += 1;
        self.tabs.push(Tab {
            id,
            url: internal::HISTORY_URL.to_string(),
            title: internal::HISTORY_TITLE.to_string(),
            view: None,
            zoom: 1.0,
            last_active: Instant::now(),
            kind: TabKind::History,
        });
        let idx = self.tabs.len() - 1;
        self.set_active(idx);
    }

    /// Reveal the downloads folder in Explorer (kebab menu, the panel's
    /// footer row and the settings page's "Open this folder now").
    ///
    /// `ShellExecuteW` reports failure only through its return value (a
    /// value ≤ 32), so the code is checked, `explorer.exe` gets a direct
    /// shot when the shell refuses, and the user hears about it either way.
    pub fn open_downloads_folder(&mut self) {
        let dir = if self.cfg.download_dir.is_empty() {
            default_download_dir()
        } else {
            PathBuf::from(&self.cfg.download_dir)
        };
        if let Err(e) = std::fs::create_dir_all(&dir) {
            log::write(&format!("downloads folder {} unusable: {e}", dir.display()));
            self.show_toast(&format!("Downloads folder is unavailable: {}", dir.display()));
            return;
        }
        if let Err(e) = reveal_folder(&dir) {
            log::write(&format!("could not open {}: {e}", dir.display()));
            self.show_toast(&format!("Could not open {}", dir.display()));
        }
    }

    /// "About GTX Browser" from the kebab menu.
    pub fn show_about(&self) {
        let exe_kb = std::env::current_exe()
            .ok()
            .and_then(|p| std::fs::metadata(p).ok())
            .map(|m| m.len() / 1024)
            .unwrap_or(0);
        let text = format!(
            "GTX Browser {}\n\nA tiny, no-bloat browser on the engine Windows already ships (WebView2).\n\n\
             Executable: {exe_kb} KB\nConfig: {}\nLog: {}\nDownloads: {}\n\nCtrl+T new tab · Ctrl+L address · Ctrl+, settings",
            env!("CARGO_PKG_VERSION"),
            config::config_path().display(),
            log::location().display(),
            downloads::location().display(),
        );
        unsafe {
            windows::Win32::UI::WindowsAndMessaging::MessageBoxW(
                None,
                &HSTRING::from(text),
                w!("About GTX Browser"),
                windows::Win32::UI::WindowsAndMessaging::MB_OK
                    | windows::Win32::UI::WindowsAndMessaging::MB_ICONINFORMATION
                    | windows::Win32::UI::WindowsAndMessaging::MB_TASKMODAL,
            );
        }
    }

    /// Open the settings page (focus it if it already exists).
    pub fn open_settings(&mut self) {
        if let Some(i) = self
            .tabs
            .iter()
            .position(|t| t.kind == TabKind::Settings)
        {
            self.set_active(i);
            return;
        }
        let id = self.next_id;
        self.next_id += 1;
        self.tabs.push(Tab {
            id,
            url: settings::TAB_URL.to_string(),
            title: settings::TAB_TITLE.to_string(),
            view: None,
            zoom: 1.0,
            last_active: Instant::now(),
            kind: TabKind::Settings,
        });
        let idx = self.tabs.len() - 1;
        self.set_active(idx);
    }

    /// Reopen the most recently closed tab (Ctrl+Shift+T).
    pub fn reopen_closed_tab(&mut self) {
        if !self.cfg.shortcut_reopen {
            return;
        }
        if let Some((url, _title)) = self.closed_tabs.pop() {
            self.add_tab(&url);
        }
    }

    pub fn go_home(&mut self) {
        let url = self.cfg.homepage.clone();
        self.load_active(&url);
        self.focus_page();
    }

    fn with_view(&mut self, f: impl FnOnce(&WebView)) {
        let i = self.active;
        if i >= self.tabs.len() {
            return;
        }
        if self.tabs[i].view.is_none() {
            self.create_view(i);
        }
        if let Some(v) = &self.tabs[i].view {
            f(v);
        }
    }

    pub fn focus_page(&mut self) {
        let i = self.active;
        if i >= self.tabs.len() {
            return;
        }
        if self.tabs[i].view.is_none() {
            self.create_view(i);
        }
        if let Some(v) = &self.tabs[i].view {
            let _ = v.focus();
        }
    }

    pub fn focus_address(&mut self) {
        let _ = unsafe { SetFocus(Some(self.controls.address)) };
        self.select_all_address();
    }

    fn select_all_address(&self) {
        unsafe {
            SendMessageW(
                self.controls.address,
                EM_SETSEL,
                Some(WPARAM(0)),
                Some(LPARAM(-1)),
            );
        }
    }

    pub fn open_devtools(&mut self) {
        if !self.cfg.devtools {
            return;
        }
        let i = self.active;
        if i < self.tabs.len() {
            if let Some(v) = &self.tabs[i].view {
                v.open_devtools();
            }
        }
    }

    pub fn zoom_step(&mut self, zoom_in: bool) {
        let i = self.active;
        if i >= self.tabs.len() {
            return;
        }
        let cur = self.tabs[i].zoom;
        let z = if zoom_in {
            (cur * 1.1).min(5.0)
        } else {
            (cur / 1.1).max(0.25)
        };
        self.set_zoom(i, z);
    }

    pub fn zoom_reset(&mut self) {
        let i = self.active;
        if i < self.tabs.len() {
            self.set_zoom(i, 1.0);
        }
    }

    fn set_zoom(&mut self, idx: usize, z: f64) {
        self.tabs[idx].zoom = z;
        if let Some(v) = &self.tabs[idx].view {
            let _ = v.zoom(z);
        }
    }

    // -----------------------------------------------------------------
    // Commands (buttons + accelerator table)
    // -----------------------------------------------------------------

    pub fn on_command(&mut self, id: u16, notify: u16) {
        if id == IDC_ADDRESS {
            if notify == EN_SETFOCUS as u16 {
                self.address_focused = true;
                self.select_all_address();
                // Typing will build its own panel; don't leave the old one up.
                if self.popup == PopupKind::Downloads {
                    self.hide_popup();
                }
            } else if notify == EN_KILLFOCUS as u16 {
                self.address_focused = false;
                self.hide_popup();
                self.sync_address();
            } else if notify == EN_CHANGE as u16 {
                self.address_changed();
            }
            return;
        }
        match id {
            IDM_BACK => self.go_back(),
            IDM_FORWARD => self.go_forward(),
            IDM_RELOAD => self.reload(),
            IDM_HOME => self.go_home(),
            IDM_OPEN_SETTINGS => self.open_settings(),
            IDM_NEW_TAB => self.new_tab(),
            IDM_CLOSE_TAB => self.close_tab(self.active),
            IDM_FOCUS_ADDRESS => self.focus_address(),
            IDM_NEXT_TAB => self.cycle_tab(1),
            IDM_PREV_TAB => self.cycle_tab(-1),
            IDM_DEVTOOLS => self.open_devtools(),
            IDM_ZOOM_IN => self.zoom_step(true),
            IDM_ZOOM_OUT => self.zoom_step(false),
            IDM_ZOOM_RESET => self.zoom_reset(),
            IDM_FULLSCREEN => self.toggle_fullscreen(),
            IDM_REOPEN_TAB => self.reopen_closed_tab(),
            IDM_HISTORY => self.open_history(),
            IDM_OPEN_DOWNLOADS => self.open_downloads_folder(),
            IDM_ABOUT => self.show_about(),
            IDM_EXIT => self.on_close(),
            IDM_RETRY_TAB => {
                let i = self.active;
                if i < self.tabs.len() {
                    self.tabs[i].view = None;
                    self.last_error = None;
                    self.create_view(i);
                    self.focus_page();
                }
            }
            _ if (IDM_TAB1..=IDM_TAB1 + 8).contains(&id) => {
                let n = (id - IDM_TAB1 + 1) as usize;
                if !self.tabs.is_empty() {
                    let idx = (n - 1).min(self.tabs.len() - 1);
                    self.set_active(idx);
                }
            }
            _ => {}
        }
    }

    /// F11: borderless fullscreen on the current monitor.
    pub fn toggle_fullscreen(&mut self) {
        if !self.cfg.shortcut_f11 {
            return;
        }
        unsafe {
            if let Some((style, rect)) = self.pre_fullscreen.take() {
                let _ = SetWindowLongPtrW(self.hwnd, GWL_STYLE, style.0 as isize);
                let _ = SetWindowPos(
                    self.hwnd,
                    None,
                    rect.left,
                    rect.top,
                    rect.right - rect.left,
                    rect.bottom - rect.top,
                    SWP_FRAMECHANGED | SWP_NOZORDER | SWP_NOACTIVATE,
                );
            } else {
                let style = WINDOW_STYLE(GetWindowLongW(self.hwnd, GWL_STYLE) as u32);
                let mut rect = RECT::default();
                if GetWindowRect(self.hwnd, &mut rect).is_err() {
                    return;
                }
                let mut mi = MONITORINFO {
                    cbSize: core::mem::size_of::<MONITORINFO>() as u32,
                    ..Default::default()
                };
                let mon = MonitorFromWindow(self.hwnd, MONITOR_DEFAULTTONEAREST);
                let borderless = (style.0 & !WS_OVERLAPPEDWINDOW.0) | WS_POPUP.0;
                let _ = SetWindowLongPtrW(self.hwnd, GWL_STYLE, borderless as isize);
                if GetMonitorInfoW(mon, &mut mi).as_bool() {
                    let m = mi.rcMonitor;
                    let _ = SetWindowPos(
                        self.hwnd,
                        None,
                        m.left,
                        m.top,
                        m.right - m.left,
                        m.bottom - m.top,
                        SWP_FRAMECHANGED | SWP_NOZORDER | SWP_NOACTIVATE,
                    );
                }
                self.pre_fullscreen = Some((style, rect));
            }
        }
    }

    // -----------------------------------------------------------------
    // Chrome state sync
    // -----------------------------------------------------------------

    fn sync_chrome(&mut self) {
        self.sync_address();
        self.update_window_title();
    }

    fn sync_address(&self) {
        if self.address_focused {
            return;
        }
        let (url, blank) = match self.tabs.get(self.active) {
            Some(t) => {
                // The blank new tab shows the placeholder, not `gtx://newtab`.
                (t.url.clone(), t.kind == TabKind::NewTab)
            }
            None => (String::new(), true),
        };
        let text = if blank { String::new() } else { url };
        unsafe {
            let _ = windows::Win32::UI::WindowsAndMessaging::SetWindowTextW(
                self.controls.address,
                &HSTRING::from(text),
            );
        }
    }

    fn update_window_title(&self) {
        let title = self
            .tabs
            .get(self.active)
            .map(|t| t.title.clone())
            .filter(|t| !t.is_empty())
            .unwrap_or_else(|| "GTX Browser".to_string());
        unsafe {
            let _ = windows::Win32::UI::WindowsAndMessaging::SetWindowTextW(
                self.hwnd,
                &HSTRING::from(title),
            );
        }
    }

    // -----------------------------------------------------------------
    // Events from the web views
    // -----------------------------------------------------------------

    fn find_tab(&self, id: u64) -> Option<usize> {
        self.tabs.iter().position(|t| t.id == id)
    }

    // -----------------------------------------------------------------
    // Loading progress (the bar along the omnibox's bottom edge)
    // -----------------------------------------------------------------

    /// A navigation started on the active tab: show the bar, start ticking.
    ///
    /// A second navigation while one is running leaves the bar alone — it
    /// keeps easing forward instead of jumping back to zero.
    pub fn load_begin(&mut self) {
        if matches!(self.load, LoadBar::Loading { .. }) {
            return;
        }
        self.load = LoadBar::Loading {
            started: Instant::now(),
        };
        self.start_load_timer();
        self.invalidate_chrome();
    }

    /// The active tab finished navigating: fill the bar, then let it fade.
    pub fn load_end(&mut self) {
        if matches!(self.load, LoadBar::Done { .. }) {
            return;
        }
        self.load = LoadBar::Done {
            at: Instant::now(),
        };
        self.start_load_timer();
        self.invalidate_chrome();
    }

    /// How full the bar is right now, or `None` once it has faded away.
    ///
    /// While loading it eases towards 95% — the last stretch is what
    /// "finished" gets to fill — and a finished page holds 100% briefly, so
    /// the eye registers that the page actually landed.
    pub fn load_fraction(&self) -> Option<f64> {
        match self.load {
            LoadBar::Hidden => None,
            LoadBar::Loading { started } => {
                let t = started.elapsed().as_secs_f64();
                let frac = 0.95 * (1.0 - (-t / LOAD_TAU).exp());
                Some(frac.clamp(0.0, 0.95))
            }
            LoadBar::Done { at } => {
                let ms = at.elapsed().as_millis() as u32;
                if ms < LOAD_HOLD_MS {
                    Some(1.0)
                } else if ms < LOAD_HOLD_MS + LOAD_FADE_MS {
                    Some((1.0 - (ms - LOAD_HOLD_MS) as f64 / LOAD_FADE_MS as f64).max(0.0))
                } else {
                    None
                }
            }
        }
    }

    /// The repaint timer that animates the bar. `SetTimer` with the same id
    /// simply resets an existing timer, so starting it twice is harmless.
    fn start_load_timer(&self) {
        if self.hwnd.0.is_null() {
            return; // no window to repaint (unit tests)
        }
        unsafe {
            let _ = SetTimer(Some(self.hwnd), IDT_LOAD, LOAD_TICK_MS, None);
        }
    }

    pub fn drain_events(&mut self) {
        let pending = PENDING.with(|p| core::mem::take(&mut *p.borrow_mut()));
        for ev in pending {
            match ev {
                Ev::NavStarted { tab, url } => {
                    let active = self.tabs.get(self.active).map(|t| t.id) == Some(tab);
                    self.on_nav(tab, url, false);
                    if active {
                        self.load_begin();
                    }
                }
                Ev::PageLoad { tab, url } => {
                    let active = self.tabs.get(self.active).map(|t| t.id) == Some(tab);
                    self.on_nav(tab, url, true);
                    if active {
                        self.load_end();
                    }
                }
                Ev::Title { tab, title } => {
                    if let Some(i) = self.find_tab(tab) {
                        self.tabs[i].title = title;
                        if !self.tabs[i].kind.is_internal() {
                            let url = self.tabs[i].url.clone();
                            let title = self.tabs[i].title.clone();
                            history::record(&url, &title);
                        }
                        if i == self.active {
                            self.update_window_title();
                        }
                        self.invalidate_chrome();
                    }
                }
                Ev::OpenTab { url } => self.add_tab(&url),
                Ev::Ipc { tab, msg } => self.handle_ipc(tab, &msg),
                Ev::DownloadDone { ok, name, url, path } => {
                    downloads::finished(&url, path.as_deref(), ok);
                    self.prefetch_icon(&url);
                    if self.popup == PopupKind::Downloads {
                        // The list is open: repaint it with the new row.
                        self.show_popup();
                    }
                    let msg = if ok {
                        format!("Downloaded {name}")
                    } else {
                        format!("Download cancelled: {name}")
                    };
                    self.show_toast(&msg);
                }
            }
        }

        // Favicons fetched in the background: the cache is main-thread state,
        // and any open panel repaints so its rows can show what arrived.
        for (host, rgba) in icon_fetch::drain() {
            if rgba.len() == favicon::RGBA_LEN {
                self.icons.insert(&host, &rgba);
                log::write(&format!("favicon fetched for {host}"));
                self.invalidate_chrome();
                self.invalidate_popup();
            }
        }
    }

    /// A page posted its rasterised favicon.
    ///
    /// The cache key is the *tab's* host, never the message's claim: a page
    /// can only label itself, not mint an icon for somebody else.
    fn on_page_icon(&mut self, i: usize, val: &serde_json::Value) {
        if self.tabs[i].kind != TabKind::Normal {
            return; // GTX's own pages have no icons
        }
        let Some(host) = nav::host_of(&self.tabs[i].url) else {
            return;
        };
        let Some(b64) = val.get("b64").and_then(|b| b.as_str()) else {
            return;
        };
        let Some(rgba) = favicon::decode(b64) else {
            log::write(&format!("favicon for {host} was not decodable"));
            return;
        };
        if rgba.len() != favicon::RGBA_LEN {
            return;
        }
        self.icons.insert(&host, &rgba);
        log::write(&format!("favicon stored for {host}"));
        self.invalidate_chrome();
    }

    /// A navigation was started or finished: keep the tab, the address bar and
    /// the history in sync.
    fn on_nav(&mut self, tab: u64, url: String, loaded: bool) {
        let Some(i) = self.find_tab(tab) else {
            return;
        };
        log::write(&format!(
            "nav tab={tab} i={i} kind={:?} loaded={loaded} url={url}",
            self.tabs[i].kind
        ));
        let internal = internal::is_internal(&url);
        if internal {
            if self.tabs[i].kind.is_internal() {
                return; // keep showing gtx://… / about:blank
            }
        } else {
            if self.tabs[i].kind.is_internal() {
                // The page navigated an internal tab to a real site: from now
                // on it is an ordinary tab.
                self.tabs[i].kind = TabKind::Normal;
            }
            if loaded {
                let title = self.tabs[i].title.clone();
                history::record(&url, &title);
                // Ask for the site's icon in the background: the page-side
                // capture cannot always read it, and the panels need pixels.
                self.prefetch_icon(&url);
            }
        }
        self.tabs[i].url = url;
        if i == self.active {
            self.sync_address();
        }
    }

    /// Show a transient message over the page.
    pub fn show_toast(&mut self, text: &str) {
        toast::show(self.controls.toast, text);
    }

    /// Ask the background fetcher for a site's icon - once, and only while we
    /// have none. The page-side capture stays first choice (it sees the real
    /// `<link rel=icon>`); this is what keeps the panels working when an icon
    /// is not readable cross-origin or the host was never opened in GTX.
    fn prefetch_icon(&self, url: &str) {
        let Some(host) = nav::web_host(url) else {
            return;
        };
        if self.icons.get(&host).is_some() {
            return;
        }
        icon_fetch::request(&host);
    }

    /// The cached favicon for the site a drop-down row points at.
    fn site_icon(&self, url: &str) -> Option<HBITMAP> {
        nav::host_of(url).and_then(|h| self.icons.get(&h))
    }

    /// Settings-page protocol — only honored while the sender is a settings tab.
    fn handle_ipc(&mut self, tab: u64, msg: &str) {
        let Ok(val) = serde_json::from_str::<serde_json::Value>(msg) else {
            return;
        };
        let Some(i) = self.find_tab(tab) else {
            return;
        };
        // Favicons come from any page, so they are routed before the
        // settings-only guard below.
        if val.get("a").and_then(|a| a.as_str()) == Some("gtx-icon") {
            self.on_page_icon(i, &val);
            return;
        }
        if self.tabs[i].kind != TabKind::Settings {
            return;
        }
        match val.get("a").and_then(|a| a.as_str()).unwrap_or("") {
            "get" => {
                let js = format!(
                    "window.__gtxApply && window.__gtxApply({}, {});",
                    self.cfg.to_json(),
                    self.settings_meta()
                );
                self.page_eval(i, &js);
            }
            "save" => {
                let Some(obj) = val.get("cfg") else {
                    return;
                };
                match serde_json::from_value::<config::Config>(obj.clone()) {
                    Ok(new) => {
                        let old = self.cfg.clone();
                        self.cfg = new;
                        // Persist first: the page's "Saved" tick has to mean
                        // the bytes are on disk, not merely applied in memory.
                        match config::save_verified(&self.cfg) {
                            Ok(()) => {
                                // Ack *before* applying, so a rebuild can never
                                // swallow the confirmation.
                                let ack = format!(
                                    "window.__gtxAck && window.__gtxAck({});",
                                    self.cfg.to_json()
                                );
                                self.page_eval(i, &ack);
                                self.apply_settings(&old);
                            }
                            Err(e) => {
                                log::write(&format!("config save failed: {e}"));
                                let payload = serde_json::json!({ "err": e }).to_string();
                                self.page_eval(
                                    i,
                                    &format!("window.__gtxErr && window.__gtxErr({payload});"),
                                );
                                self.show_toast(&format!("Settings not saved: {e}"));
                            }
                        }
                    }
                    Err(e) => {
                        let payload =
                            serde_json::json!({ "err": format!("invalid value: {e}") }).to_string();
                        self.page_eval(
                            i,
                            &format!("window.__gtxErr && window.__gtxErr({payload});"),
                        );
                    }
                }
            }
            "reset" => {
                // A reset overwrites the config on disk, so it has to be
                // unmistakable: native yes/no, and its own acknowledgement
                // (`__gtxReset`, never `__gtxAck`) so the footer can say
                // "Reset to defaults" instead of claiming a save.
                if !confirm_reset(self.hwnd) {
                    return;
                }
                let old = self.cfg.clone();
                self.cfg = config::Config::default();
                match config::save_verified(&self.cfg) {
                    Ok(()) => {
                        let js = format!(
                            "window.__gtxApply && window.__gtxApply({}, {});",
                            self.cfg.to_json(),
                            self.settings_meta()
                        );
                        self.page_eval(i, &js);
                        self.apply_settings(&old);
                        self.page_eval(i, &reset_ack_js(&self.cfg));
                    }
                    Err(e) => {
                        self.cfg = old;
                        let payload = serde_json::json!({ "err": e }).to_string();
                        self.page_eval(
                            i,
                            &format!("window.__gtxErr && window.__gtxErr({payload});"),
                        );
                    }
                }
            }
            "opendir" => self.open_downloads_folder(),
            "copylog" => {
                // The page must not own the clipboard from a data: document;
                // the path is put on it here and the button is told how it went.
                let ok = copy_to_clipboard(self.hwnd, &log::location().display().to_string());
                self.page_eval(i, &format!("window.__gtxCopied && window.__gtxCopied({ok});"));
            },
            "defaultbrowser" => {
                // Windows may only be asked, never told: `set_default`
                // either lands the association or opens the page that can,
                // and the settings page shows whichever happened.
                let report = default_browser::status_after_attempt();
                log::write(&format!("default browser: {}", report.msg));
                let payload = serde_json::json!({
                    "default_browser": report.is_default,
                    "err": report.err,
                    "msg": report.msg,
                })
                .to_string();
                self.page_eval(
                    i,
                    &format!("window.__gtxDefault && window.__gtxDefault({payload});"),
                );
            }
            _ => {}
        }
    }

    fn page_eval(&self, idx: usize, js: &str) {
        if let Some(v) = &self.tabs[idx].view {
            let _ = v.evaluate_script(js);
        }
    }

    /// Paths and version the settings page shows on its About section.
    fn settings_meta(&self) -> String {
        serde_json::json!({
            "version": env!("CARGO_PKG_VERSION"),
            "config": config::config_path().display().to_string(),
            "log": log::location().display().to_string(),
            "history": history::location().display().to_string(),
            "profile": config::profile_dir().display().to_string(),
            "default_browser": default_browser::is_default(),
            "effects": serde_json::from_str::<serde_json::Value>(&config::effects_json())
                .unwrap_or(serde_json::Value::Object(Default::default())),
        })
        .to_string()
    }

    /// Apply the (already saved) config live.
    ///
    /// What has to happen is decided by [`config::plan_apply`], which is
    /// exhaustively tested field by field — this function only carries it out.
    fn apply_settings(&mut self, old: &config::Config) {
        let plan = config::plan_apply(old, &self.cfg);

        if plan.refresh_cue {
            self.update_cue_banner();
        }
        if plan.rebuild_accel {
            self.rebuild_accelerators();
        }
        if plan.retheme {
            self.system_dark = w32::apps_dark();
            let p = self.palette();
            self.toolbar_icons.get_mut().set_colors(p.muted, p.text);
            self.apply_theme();
            self.apply_frame();
        }
        if plan.relayout {
            let p = self.palette();
            self.toolbar_icons.get_mut().set_colors(p.muted, p.text);
            self.recreate_fonts();
            self.apply_fonts();
            self.layout();
        }

        // Everything baked into a view's init script or per-view options needs
        // a rebuild — but never of the settings page you are standing on, and
        // never for `hardware_acceleration`, which WebView2 fixes at first
        // launch (the page flags that one as "restart to apply").
        if plan.recreate_views {
            self.recreate_views();
        }

        self.invalidate_chrome();
    }

    /// Palette for the current config + Windows theme.
    pub fn palette(&self) -> Palette {
        let mut p = match self.cfg.theme.as_str() {
            "light" => Palette::light(),
            "dark" => Palette::dark(),
            _ if self.system_dark => Palette::dark(),
            _ => Palette::light(),
        };
        p.accent = self.cfg.accent_rgb();
        p
    }

    pub fn resolved_dark(&self) -> bool {
        self.palette().dark
    }

    fn apply_frame(&mut self) {
        let p = self.palette();
        w32::apply_window_effects(self.hwnd, p.dark);
        w32::set_class_background_for(self.hwnd, p.frame);
        self.invalidate_chrome();
        self.invalidate_content();
    }

    fn apply_theme(&self) {
        let t = theme_from_str(&self.cfg.theme);
        let dark = self.resolved_dark();
        for tab in &self.tabs {
            if let Some(v) = &tab.view {
                let _ = v.set_theme(t);
                let _ = v.set_background_color(page_bg(dark));
            }
        }
    }

    /// Drop every view so new init scripts (popup guard/UA/keys) take effect.
    ///
    /// Internal pages (settings, new tab, history) are generated by us and
    /// hold no engine options — they are never touched, which is what keeps
    /// your place on the settings page.
    fn recreate_views(&mut self) {
        let active = self.active;
        for i in 0..self.tabs.len() {
            if self.tabs[i].kind.is_internal() || self.tabs[i].view.is_none() {
                continue;
            }
            if let Some(v) = &self.tabs[i].view {
                if let Ok(u) = v.url() {
                    if !u.is_empty() && !internal::is_internal(&u) {
                        self.tabs[i].url = u;
                    }
                }
            }
            self.tabs[i].view = None;
        }
        // Rebuild the tab you are looking at now; the rest come back lazily.
        if active < self.tabs.len() && self.tabs[active].view.is_none() {
            self.create_view(active);
        }
    }

    pub fn rebuild_accelerators(&mut self) {
        let new = build_accelerators(&self.cfg);
        let old = self.accel;
        self.accel = new;
        if !old.0.is_null() {
            unsafe {
                let _ = DestroyAcceleratorTable(old);
            }
        }
    }

    // -----------------------------------------------------------------
    // Timers: background-tab hibernation + smoke test
    // -----------------------------------------------------------------

    pub fn on_timer(&mut self, id: usize) {
        match id {
            IDT_TICK => {
                self.hibernate_idle();
                self.save_session_if_dirty();
            }
            IDT_LOAD => {
                // A page that never reports back must not pin the bar on the
                // address bar for ever.
                if let LoadBar::Loading { started } = self.load {
                    if started.elapsed().as_secs() >= LOAD_ABANDON_SECS {
                        self.load_end();
                    }
                }
                if self.load_fraction().is_some() {
                    self.invalidate_chrome();
                } else {
                    // Fully faded: stop ticking until the next navigation.
                    self.load = LoadBar::Hidden;
                    let _ = unsafe { KillTimer(Some(self.hwnd), IDT_LOAD) };
                    self.invalidate_chrome();
                }
            }
            IDT_SMOKE => {
                self.report_smoke();
                self.on_close();
            }
            _ => {}
        }
    }

    /// Drop web views of background tabs idle longer than `hibernate_secs`.
    /// Their URL/title stay in the tab; the view is recreated on activation.
    fn hibernate_idle(&mut self) {
        let budget = self.cfg.hibernate_secs;
        if budget == 0 {
            return;
        }
        let now = Instant::now();
        for i in 0..self.tabs.len() {
            if i == self.active {
                continue;
            }
            let tab = &mut self.tabs[i];
            let idle = now.duration_since(tab.last_active).as_secs();
            if idle < budget {
                continue;
            }
            if let Some(view) = &tab.view {
                if let Ok(url) = view.url() {
                    if !url.is_empty() {
                        tab.url = url;
                    }
                }
                tab.view = None; // closes the controller, frees the renderer
            }
        }
    }

    fn report_smoke(&self) {
        let ready = self.started.elapsed().as_millis();
        let exe_kb = std::env::current_exe()
            .ok()
            .and_then(|p| std::fs::metadata(p).ok())
            .map(|m| m.len() / 1024)
            .unwrap_or(0);
        let views = self.tabs.iter().filter(|t| t.view.is_some()).count();
        out(&format!(
            "[gtx] ready in {ready} ms | tabs: {} ({} live) | dpi: {} | exe: {exe_kb} KB",
            self.tabs.len(),
            views,
            self.dpi
        ));
        for t in &self.tabs {
            out(&format!("   tab: {} | {:?} | {}", t.url, t.kind, t.title));
        }
    }

    // -----------------------------------------------------------------
    // Downloads
    // -----------------------------------------------------------------

    /// wry's `download_started` hook (main thread). Picks the destination —
    /// asking the user to confirm the path when `download_ask` is set — and
    /// returns `false` to cancel the download outright.
    fn on_download(&mut self, url: &str, path: &mut PathBuf) -> bool {
        // The engine suggests a full path inside the real Downloads folder;
        // reuse its file name (falling back to the URL) as the starting point.
        let name = path
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| file_name_from_url(url));
        let dir = if !self.cfg.download_dir.is_empty() {
            PathBuf::from(&self.cfg.download_dir)
        } else {
            path.parent()
                .filter(|p| !p.as_os_str().is_empty())
                .map(Path::to_path_buf)
                .unwrap_or_else(default_download_dir)
        };

        if !self.cfg.download_ask {
            let dest = dir.join(name);
            if let Some(parent) = dest.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            *path = dest;
            // Recorded the moment the destination is final, so the panel
            // shows an in-flight file rather than nothing at all.
            downloads::started(url, path.as_path());
            return true;
        }

        // Confirm dialog, pre-filled with dir + suggested name.
        const MAX_PATH_BUF: usize = 4096;
        let mut buf: Vec<u16> = dir
            .join(&name)
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        buf.resize(buf.len().max(MAX_PATH_BUF), 0);
        let filter: Vec<u16> = "All files (*.*)\0*.*\0\0".encode_utf16().collect();

        let mut ofn: OPENFILENAMEW = unsafe { core::mem::zeroed() };
        ofn.lStructSize = core::mem::size_of::<OPENFILENAMEW>() as u32;
        ofn.hwndOwner = self.hwnd;
        ofn.lpstrFilter = PCWSTR(filter.as_ptr());
        ofn.nFilterIndex = 1;
        ofn.lpstrFile = PWSTR(buf.as_mut_ptr());
        ofn.nMaxFile = buf.len() as u32;
        ofn.lpstrTitle = w!("Save download as");
        ofn.Flags = OFN_EXPLORER | OFN_OVERWRITEPROMPT | OFN_PATHMUSTEXIST | OFN_NOCHANGEDIR;

        if !unsafe { GetSaveFileNameW(&mut ofn) }.as_bool() {
            return false; // cancelled (or invalid path — dialog stays open until resolved)
        }
        let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        let chosen = PathBuf::from(OsString::from_wide(&buf[..len]));
        if let Some(parent) = chosen.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        *path = chosen;
        downloads::started(url, path.as_path());
        true
    }

    // -----------------------------------------------------------------
    // Lifetime
    // -----------------------------------------------------------------

    pub fn on_close(&mut self) {
        // Never lose an in-memory setting: the page already asked for a save,
        // but a failed write must not stay silent until the next launch either.
        if let Err(e) = config::save_verified(&self.cfg) {
            log::write(&format!("config save on exit failed: {e}"));
        }
        self.save_session();
        unsafe {
            let _ = DestroyWindow(self.hwnd);
        }
    }

    fn save_session(&mut self) {
        if self.no_session {
            return;
        }
        self.last_saved = self.session_snapshot();
        self.last_saved_active = if self.tabs.is_empty() { 0 } else { self.active.min(self.tabs.len() - 1) };
        if self.tabs.is_empty() {
            session::clear();
            return;
        }
        let s = session::Session {
            active: self.last_saved_active,
            tabs: self
                .tabs
                .iter()
                .map(|t| session::TabEntry {
                    url: t.url.clone(),
                    title: t.title.clone(),
                })
                .collect(),
            window: self.window_entry(),
        };
        session::save(&s);
    }

    fn session_snapshot(&self) -> Vec<(String, String)> {
        self.tabs
            .iter()
            .map(|t| (t.url.clone(), t.title.clone()))
            .collect()
    }

    /// Cheap dirty check, run on the periodic tick so a crash can't lose tabs.
    fn save_session_if_dirty(&mut self) {
        if self.no_session {
            return;
        }
        let snap = self.session_snapshot();
        let active = if self.tabs.is_empty() { 0 } else { self.active.min(self.tabs.len() - 1) };
        if snap != self.last_saved || active != self.last_saved_active {
            self.save_session();
        }
    }

    fn window_entry(&self) -> Option<session::WindowEntry> {
        // `GetWindowRect` of a minimized window is the -32000 sentinel, and
        // restoring that puts the window offscreen where nothing can reach
        // it — exactly the "it runs but I see nothing" failure. The
        // placement's *normal* rect is where the window will come back, so
        // that is what a session remembers.
        if unsafe { IsIconic(self.hwnd) }.as_bool() {
            let mut placement = WINDOWPLACEMENT {
                length: core::mem::size_of::<WINDOWPLACEMENT>() as u32,
                ..Default::default()
            };
            if unsafe { GetWindowPlacement(self.hwnd, &mut placement) }.is_ok() {
                let r = placement.rcNormalPosition;
                return Some(session::WindowEntry {
                    x: r.left,
                    y: r.top,
                    w: r.right - r.left,
                    h: r.bottom - r.top,
                    maximized: false,
                });
            }
        }
        let mut r = RECT::default();
        unsafe {
            GetWindowRect(self.hwnd, &mut r).ok()?;
        }
        Some(session::WindowEntry {
            x: r.left,
            y: r.top,
            w: r.right - r.left,
            h: r.bottom - r.top,
            maximized: unsafe { IsZoomed(self.hwnd).as_bool() },
        })
    }

    // -----------------------------------------------------------------
    // Single-row chrome: geometry, hit testing, mouse, painting
    // -----------------------------------------------------------------

    fn invalidate_chrome(&self) {
        if self.controls.chrome.0.is_null() {
            return;
        }
        unsafe {
            let _ = InvalidateRect(Some(self.controls.chrome), None, false);
        }
    }

    /// Repaint the exposed frame band around the page.
    fn invalidate_content(&self) {
        unsafe {
            let _ = InvalidateRect(Some(self.hwnd), None, false);
        }
    }

    fn chrome_size(&self) -> (i32, i32) {
        w32::client_size(self.controls.chrome)
    }

    fn current_layout(&self) -> ChromeLayout {
        let (w, h) = self.chrome_size();
        self.chrome_layout(w, h)
    }

    /// Geometry of the whole row in a `w`×`row_h` chrome window.
    ///
    /// Left → right: nav buttons, rounded omnibox, tab pills, `+`, kebab, then
    /// the three window caption buttons — exactly the reference screenshot.
    pub fn chrome_layout(&self, w: i32, row_h: i32) -> ChromeLayout {
        let pad = self.s(PAD);
        let gap = self.s(BTN_GAP);
        let btn = self.ui_btn();
        let tab_h = self.ui_tab_h();
        let addr_h = self.ui_addr_h();
        let cap_w = self.ui_capw();
        let kebab_w = self.ui_kebab_w();
        let dl_w = self.ui_kebab_w();
        let plus_w = self.ui_plus_w();
        let cy = |h: i32| self.row_y(row_h, h);

        // Right → left: caption buttons, kebab, new tab, then the tab band.
        let mut right = w;
        let cap_close = R::new(right - cap_w, 0, cap_w, row_h);
        right -= cap_w;
        let cap_max = R::new(right - cap_w, 0, cap_w, row_h);
        right -= cap_w;
        let cap_min = R::new(right - cap_w, 0, cap_w, row_h);
        right -= cap_w;
        let kebab = R::new(right - kebab_w, cy(btn), kebab_w, btn);
        right -= kebab_w + gap;
        // Downloads lists recent files; it lives between `+` and the kebab.
        let downloads = R::new(right - dl_w, cy(btn), dl_w, btn);
        right -= dl_w + gap;
        let plus = R::new(right - plus_w, cy(tab_h), plus_w, tab_h);
        right -= plus_w + gap;
        let tabs_end = right.max(0);

        // Left → right: nav buttons, then the omnibox.
        let mut left = pad;
        let back = R::new(left, cy(btn), btn, btn);
        left += btn + gap;
        let forward = R::new(left, cy(btn), btn, btn);
        left += btn + gap;
        let reload = R::new(left, cy(btn), btn, btn);
        left += btn + gap;
        let home = if self.cfg.show_home_button {
            let r = R::new(left, cy(btn), btn, btn);
            left += btn + gap;
            r
        } else {
            R::default()
        };
        left += self.s(4);

        // ~30% of the row, but never at the expense of the tab band.
        let want = (w * 30 / 100).clamp(self.s(180), self.s(520));
        let room = (tabs_end - left - self.s(16)).max(self.s(80));
        let pill_w = want.min(room).max(self.s(80));
        let pill = R::new(left, cy(addr_h), pill_w, addr_h);
        // A single-line EDIT paints its line flush to the top of its client
        // area, so the box is centred on the *line* (`tmHeight`) rather than
        // inset by a fixed 3px: that is what puts the text in the middle of
        // the pill, with equal padding above and below. The box always stays
        // at least 2px inside the pill.
        let line_h = if self.font_line_h > 0 {
            self.font_line_h
        } else {
            self.s(14)
        };
        let addr = R::new(
            pill.x + self.s(30),
            pill.y + (addr_h - line_h) / 2,
            (pill.w - self.s(42)).max(self.s(40)),
            ((addr_h + line_h) / 2 - self.s(2)).max(line_h + self.s(2)),
        );
        let tabs_x = pill.right() + self.s(10);
        let tabs_w = (tabs_end - tabs_x).max(0);

        ChromeLayout {
            back,
            forward,
            reload,
            home,
            pill,
            addr,
            tabs_x,
            tabs_w,
            plus,
            downloads,
            kebab,
            cap_min,
            cap_max,
            cap_close,
            row_h,
        }
    }

    /// One pill per tab; entries beyond the tab band are still produced so the
    /// index always matches `self.tabs` (they are clipped when painted).
    pub fn tab_rects(&self, lay: &ChromeLayout) -> Vec<R> {
        let n = self.tabs.len();
        if n == 0 || lay.tabs_w <= 0 {
            return Vec::new();
        }
        let gap = self.s(4);
        let each = (lay.tabs_w - gap * (n as i32 - 1)) / n as i32;
        if each <= 0 {
            return Vec::new();
        }
        let w_each = each.clamp(self.s(TAB_MIN_W).min(each), self.s(TAB_MAX_W));
        let h = self.ui_tab_h();
        let y = self.row_y(lay.row_h, h);
        let mut x = lay.tabs_x;
        (0..n)
            .map(|_| {
                let r = R::new(x, y, w_each, h);
                x += w_each + gap;
                r
            })
            .collect()
    }

    fn hit_at(&self, lay: &ChromeLayout, x: i32, y: i32) -> Hit {
        if lay.cap_close.contains(x, y) {
            return Hit::CapClose;
        }
        if lay.cap_max.contains(x, y) {
            return Hit::CapMax;
        }
        if lay.cap_min.contains(x, y) {
            return Hit::CapMin;
        }
        if lay.kebab.contains(x, y) {
            return Hit::Kebab;
        }
        if lay.downloads.contains(x, y) {
            return Hit::Downloads;
        }
        if lay.plus.contains(x, y) {
            return Hit::Plus;
        }
        let band_end = lay.tabs_x + lay.tabs_w;
        for (i, r) in self.tab_rects(lay).into_iter().enumerate() {
            if r.x >= band_end || x < r.x || x >= r.right() || y < r.y || y >= r.bottom() {
                continue;
            }
            let active = i == self.active;
            let hovered = matches!(self.hover, Hit::Tab(j) | Hit::Close(j) if j == i);
            if (active || hovered) && x >= r.right() - self.s(CLOSE_W) {
                return Hit::Close(i);
            }
            return Hit::Tab(i);
        }
        if lay.pill.contains(x, y) {
            return Hit::Omnibox;
        }
        if lay.back.contains(x, y) {
            return Hit::Back;
        }
        if lay.forward.contains(x, y) {
            return Hit::Forward;
        }
        if lay.reload.contains(x, y) {
            return Hit::Reload;
        }
        if lay.home.contains(x, y) {
            return Hit::Home;
        }
        Hit::None
    }

    /// The chrome receives mouse input only for its own controls; a press on
    /// empty toolbar space starts a window drag once the cursor moves, and a
    /// press on a tab starts a tab reorder once the cursor moves.
    pub fn chrome_down(&mut self, x: i32, y: i32) {
        let lay = self.current_layout();
        match self.hit_at(&lay, x, y) {
            Hit::Tab(i) => {
                self.tab_press = Some((i, x, y));
                unsafe {
                    SetCapture(self.controls.chrome);
                }
            }
            Hit::None => {
                self.drag = Some((x, y));
                unsafe {
                    SetCapture(self.controls.chrome);
                }
            }
            _ => {}
        }
    }

    pub fn chrome_hover(&mut self, x: i32, y: i32) {
        // A pressed tab turns into a reorder once the cursor moves far
        // enough; from then on the pill previews the slot it would land in.
        if let Some((from, ox, oy)) = self.tab_press {
            if self.tab_drag.is_none()
                && ((x - ox).abs() > w32::drag_threshold()
                    || (y - oy).abs() > w32::drag_threshold())
            {
                self.tab_drag = Some((from, from));
            }
            if let Some((from, _)) = self.tab_drag {
                let lay = self.current_layout();
                let slot = self.tab_drop_slot(&lay, x, from);
                if self.tab_drag != Some((from, slot)) {
                    self.tab_drag = Some((from, slot));
                    self.invalidate_chrome();
                }
                return;
            }
        }
        if let Some((ox, oy)) = self.drag {
            if (x - ox).abs() > w32::drag_threshold() || (y - oy).abs() > w32::drag_threshold()
            {
                self.drag = None;
                unsafe {
                    let _ = ReleaseCapture();
                    SendMessageW(
                        self.hwnd,
                        WM_NCLBUTTONDOWN,
                        Some(WPARAM(HTCAPTION as usize)),
                        Some(LPARAM(0)),
                    );
                }
                return;
            }
        }
        let lay = self.current_layout();
        let hit = self.hit_at(&lay, x, y);
        if hit != self.hover {
            self.hover = hit;
            self.invalidate_chrome();
        }
    }

    pub fn chrome_leave(&mut self) {
        if self.hover != Hit::None {
            self.hover = Hit::None;
            self.invalidate_chrome();
        }
    }

    pub fn chrome_up(&mut self, x: i32, y: i32) {
        // Finish (or cancel) a tab press/drag before anything else.
        if let Some((pressed, _, _)) = self.tab_press.take() {
            unsafe {
                let _ = ReleaseCapture();
            }
            if let Some((from, slot)) = self.tab_drag.take() {
                self.move_tab(from, slot);
                return;
            }
            // Not enough movement to count as a drag: it was a click.
            self.set_active(pressed);
            return;
        }
        if self.drag.take().is_some() {
            unsafe {
                let _ = ReleaseCapture();
            }
            return; // a click on empty toolbar space does nothing
        }
        let lay = self.current_layout();
        let hit = self.hit_at(&lay, x, y);
        // Any other control dismisses an open drop-down first.
        if hit != Hit::Downloads {
            self.hide_popup();
        }
        match hit {
            Hit::Back => self.go_back(),
            Hit::Forward => self.go_forward(),
            Hit::Reload => self.reload(),
            Hit::Home => self.go_home(),
            Hit::Omnibox => self.focus_address(),
            Hit::Tab(i) => self.set_active(i),
            Hit::Close(i) => self.close_tab(i),
            Hit::Plus => self.new_tab(),
            Hit::Downloads => self.toggle_downloads(),
            Hit::Kebab => self.show_kebab(),
            Hit::CapMin => unsafe {
                let _ = ShowWindow(self.hwnd, SW_MINIMIZE);
            },
            Hit::CapMax => self.toggle_maximize(),
            Hit::CapClose => {
                if self.busy {
                    self.deferred.push(Defer::Shutdown);
                } else {
                    self.on_close();
                }
            }
            _ => {}
        }
    }

    pub fn chrome_dbl(&mut self, x: i32, y: i32) {
        self.drag = None;
        self.tab_press = None;
        self.tab_drag = None;
        unsafe {
            let _ = ReleaseCapture();
        }
        let lay = self.current_layout();
        if self.hit_at(&lay, x, y) == Hit::None {
            self.toggle_maximize();
        }
    }

    pub fn chrome_middle(&mut self, x: i32, y: i32) {
        let lay = self.current_layout();
        if let Hit::Tab(i) | Hit::Close(i) = self.hit_at(&lay, x, y) {
            self.close_tab(i);
        }
    }

    /// Post-removal index the dragged tab would land at for cursor `x`: the
    /// number of *other* pills whose centre lies left of the cursor.
    fn tab_drop_slot(&self, lay: &ChromeLayout, x: i32, from: usize) -> usize {
        let mut slot = 0;
        for (i, r) in self.tab_rects(lay).iter().enumerate() {
            if i != from && x > r.x + r.w / 2 {
                slot += 1;
            }
        }
        slot
    }

    /// Tab indices in painted order — during a drag the dragged pill is
    /// shown in the slot it would drop into, which is the whole feedback.
    pub fn tab_display_order(&self) -> Vec<usize> {
        let n = self.tabs.len();
        let mut order: Vec<usize> = (0..n).collect();
        if let Some((from, slot)) = self.tab_drag {
            if from < n {
                order.remove(from);
                order.insert(slot.min(order.len()), from);
            }
        }
        order
    }

    /// Move tab `from` to index `to`, keeping the active tab pointing at the
    /// same tab and its web view; the order is saved with the session.
    pub fn move_tab(&mut self, from: usize, to: usize) {
        if from >= self.tabs.len() || from == to {
            self.invalidate_chrome();
            return;
        }
        let active_id = self.tabs.get(self.active).map(|t| t.id);
        let tab = self.tabs.remove(from);
        let to = to.min(self.tabs.len());
        self.tabs.insert(to, tab);
        if let Some(id) = active_id {
            self.active = self.tabs.iter().position(|t| t.id == id).unwrap_or(0);
        }
        self.hover = Hit::None;
        self.sync_chrome();
        self.invalidate_chrome();
        self.save_session_if_dirty();
    }

    fn toggle_maximize(&self) {
        unsafe {
            let cmd = if IsZoomed(self.hwnd).as_bool() {
                SW_RESTORE
            } else {
                SW_MAXIMIZE
            };
            let _ = ShowWindow(self.hwnd, cmd);
        }
    }

    /// Hit-test used by the chrome's `WM_NCHITTEST`: the invisible resize band
    /// around the top of the window, except over the caption buttons.
    pub fn chrome_hit_code(&self, hwnd: HWND, screen_pt: (i32, i32)) -> u32 {
        let mut wr = RECT::default();
        if unsafe { GetWindowRect(hwnd, &mut wr) }.is_err() {
            return w32::HTCLIENT;
        }
        let code = w32::edge_hit_test(wr, screen_pt, w32::frame_band());
        if code == w32::HTCLIENT {
            return code;
        }
        let mut pt = POINT {
            x: screen_pt.0,
            y: screen_pt.1,
        };
        unsafe {
            let _ = ScreenToClient(hwnd, &mut pt);
        }
        let lay = self.current_layout();
        if self.hit_at(&lay, pt.x, pt.y).is_caption() {
            return w32::HTCLIENT;
        }
        code
    }

    /// The kebab menu, opened under the `⋮` button.
    fn show_kebab(&mut self) {
        let lay = self.current_layout();
        let chrome = self.controls.chrome;
        let mut pt = POINT {
            x: lay.kebab.x,
            y: lay.kebab.bottom(),
        };
        unsafe {
            let _ = ClientToScreen(chrome, &mut pt);
        }
        unsafe {
            let Ok(menu) = CreatePopupMenu() else {
                return;
            };
            let add = |id: u16, text: &str| {
                let _ = AppendMenuW(menu, MF_STRING, id as usize, &HSTRING::from(text));
            };
            let sep = || {
                let _ = AppendMenuW(menu, MF_SEPARATOR, 0, &HSTRING::new());
            };
            add(IDM_NEW_TAB, "New tab\tCtrl+T");
            add(IDM_REOPEN_TAB, "Reopen closed tab\tCtrl+Shift+T");
            add(IDM_FOCUS_ADDRESS, "Address bar\tCtrl+L");
            sep();
            add(IDM_RELOAD, "Reload\tCtrl+R");
            add(IDM_HISTORY, "History\tCtrl+H");
            add(IDM_OPEN_DOWNLOADS, "Downloads folder");
            sep();
            add(IDM_ZOOM_IN, "Zoom in\tCtrl++");
            add(IDM_ZOOM_OUT, "Zoom out\tCtrl+-");
            add(IDM_ZOOM_RESET, "Reset zoom\tCtrl+0");
            sep();
            add(IDM_OPEN_SETTINGS, "Settings\tCtrl+,");
            add(IDM_ABOUT, "About GTX Browser");
            sep();
            add(IDM_EXIT, "Exit");
            let cmd = TrackPopupMenu(
                menu,
                TPM_RETURNCMD | TPM_RIGHTBUTTON,
                pt.x,
                pt.y,
                Some(0),
                self.hwnd,
                None,
            )
            .0;
            let _ = DestroyMenu(menu);
            if cmd != 0 {
                self.on_command(cmd as u16, 0);
            }
        }
    }

    // -----------------------------------------------------------------
    // Drop-down panel: suggestions under the omnibox, downloads under the button
    // -----------------------------------------------------------------

    /// Rows currently in the panel — the count both the painter and the
    /// click handler derive their geometry from.
    fn popup_rows(&self) -> usize {
        match self.popup {
            PopupKind::None => 0,
            PopupKind::Suggest => self.suggest.len(),
            PopupKind::Downloads => {
                let n = downloads::entries().len().min(downloads::PANEL_MAX);
                n.max(1) + 1 // the empty-state line (or the list) + the footer
            }
        }
    }

    fn popup_row_at(&self, y: i32) -> Option<usize> {
        row_at(y, self.popup_rows(), self.s(POPUP_ROW), self.s(POPUP_PAD))
    }

    /// Position and size of the panel in main-window client coordinates.
    fn popup_geometry(&self) -> (i32, i32, i32, i32) {
        let rows = self.popup_rows() as i32;
        let (win_w, win_h) = w32::client_size(self.hwnd);
        let y = self.ui_row_h() + self.s(2);
        let lay = self.current_layout();
        let (x, w) = match self.popup {
            // The downloads list hangs off its button's right edge.
            PopupKind::Downloads => {
                let w = self.s(360).min((win_w - self.s(16)).max(self.s(160)));
                (lay.downloads.right() - w, w)
            }
            _ => (lay.pill.x, lay.pill.w),
        };
        let h = (rows * self.s(POPUP_ROW) + 2 * self.s(POPUP_PAD) + 2)
            .min((win_h - y - self.s(4)).max(self.s(40)));
        let x = x.clamp(0, (win_w - w).max(0));
        (x, y, w, h)
    }

    fn invalidate_popup(&self) {
        if !popup::is_open(self.controls.popup) {
            return;
        }
        unsafe {
            let _ = InvalidateRect(Some(self.controls.popup), None, false);
        }
    }

    /// Show whatever [`Self::popup`] says is open, at its computed spot.
    fn show_popup(&mut self) {
        if self.popup == PopupKind::None || self.popup_rows() == 0 {
            self.hide_popup();
            return;
        }
        let (x, y, w, h) = self.popup_geometry();
        popup::show(self.controls.popup, x, y, w, h);
    }

    pub fn hide_popup(&mut self) {
        if self.popup == PopupKind::None {
            return;
        }
        self.popup = PopupKind::None;
        self.suggest.clear();
        self.suggest_sel = 0;
        self.popup_hover = None;
        popup::hide(self.controls.popup);
    }

    /// The `↓` button: open the downloads list, or close it if it is the
    /// thing already showing.
    pub fn toggle_downloads(&mut self) {
        if self.popup == PopupKind::Downloads {
            self.hide_popup();
            return;
        }
        self.suggest.clear();
        self.suggest_sel = 0;
        self.popup_hover = None;
        self.popup = PopupKind::Downloads;
        // Icons for the listed hosts may never have been captured (fresh
        // profile, unreadable cross-origin): ask in the background now so
        // the rows can swap their glyph for the real favicon while open.
        for d in downloads::entries().iter().take(downloads::PANEL_MAX) {
            self.prefetch_icon(&d.url);
        }
        self.show_popup();
    }

    /// The address bar text changed: rebuild the rows under it.
    ///
    /// Programmatic writes (tab switches restoring the URL) arrive here too,
    /// but only while the bar has focus can they be the user's own typing.
    fn address_changed(&mut self) {
        if !self.address_focused {
            return;
        }
        let text = w32::window_text(self.controls.address);
        self.suggest = suggest::suggest(&text, &history::entries(), &self.cfg.search_template);
        self.suggest_sel = 0;
        self.popup_hover = None;
        if self.suggest.is_empty() {
            self.hide_popup();
            return;
        }
        // Only hosts the user has actually visited: prefetching whatever is
        // being typed would ask DNS about every half-typed address.
        let known: Vec<String> = history::entries()
            .iter()
            .filter_map(|e| nav::web_host(&e.url))
            .collect();
        for sug in &self.suggest {
            if let Some(host) = nav::web_host(&sug.target) {
                if known.iter().any(|k| k == &host) {
                    self.prefetch_icon(&sug.target);
                }
            }
        }
        self.popup = PopupKind::Suggest;
        self.show_popup();
    }

    /// ↓/↑ move the highlight (clamped: the list is a nudge, not a ring).
    pub fn suggest_move(&mut self, delta: i32) {
        if self.popup != PopupKind::Suggest || self.suggest.is_empty() {
            return;
        }
        let n = self.suggest.len() as i32;
        let next = (self.suggest_sel as i32 + delta).clamp(0, n - 1) as usize;
        if next != self.suggest_sel {
            self.suggest_sel = next;
            self.invalidate_popup();
        }
    }

    /// Esc: leave the drop-down if one is open, otherwise leave the bar.
    pub fn address_escape(&mut self) {
        if self.popup != PopupKind::None {
            self.hide_popup();
        } else {
            self.focus_page();
        }
    }

    pub fn popup_hover(&mut self, _x: i32, y: i32) {
        let row = self.popup_row_at(y);
        if row != self.popup_hover {
            self.popup_hover = row;
            self.invalidate_popup();
        }
    }

    pub fn popup_leave(&mut self) {
        if self.popup_hover.take().is_some() {
            self.invalidate_popup();
        }
    }

    /// A row was clicked: run it.
    pub fn popup_click(&mut self, _x: i32, y: i32) {
        let Some(i) = self.popup_row_at(y) else {
            return;
        };
        match self.popup {
            PopupKind::Suggest => {
                let Some(s) = self.suggest.get(i).cloned() else {
                    return;
                };
                self.hide_popup();
                self.load_active(&s.target);
                self.focus_page();
            }
            PopupKind::Downloads => self.download_row_click(i),
            PopupKind::None => {}
        }
    }

    /// Click a download: open it (or its folder footer), explain failures.
    fn download_row_click(&mut self, i: usize) {
        let entries = downloads::entries();
        let n = entries.len().min(downloads::PANEL_MAX);
        if i >= n.max(1) {
            self.hide_popup();
            return; // outside the list
        }
        if i >= n {
            // Footer: “Open downloads folder”.
            self.open_downloads_folder();
            self.hide_popup();
            return;
        }
        let Some(d) = entries.get(i) else {
            return;
        };
        if !d.ok {
            self.show_toast(&format!("{} failed or was cancelled", d.name));
            return;
        }
        if d.path.is_empty() || !Path::new(&d.path).exists() {
            self.show_toast(&format!("{} is no longer at that location", d.name));
            return;
        }
        unsafe {
            let _ = ShellExecuteW(
                None,
                w!("open"),
                &HSTRING::from(&d.path),
                &HSTRING::new(),
                &HSTRING::new(),
                windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL,
            );
        }
        self.hide_popup();
    }

    /// Paint of the drop-down panel, from the same state the click handler
    /// reads (see [`row_at`]).
    pub fn paint_popup(&self, hwnd: HWND) {
        unsafe {
            let mut ps = PAINTSTRUCT::default();
            let hdc = BeginPaint(hwnd, &mut ps);
            let (w, h) = w32::client_size(hwnd);
            if w > 0 && h > 0 {
                let mem = CreateCompatibleDC(Some(hdc));
                let bmp = CreateCompatibleBitmap(hdc, w, h);
                let prev_bmp = SelectObject(mem, HGDIOBJ(bmp.0));
                let _ = SetBkMode(mem, TRANSPARENT);
                let p = self.palette();
                let pad = self.s(POPUP_PAD);
                let rh = self.s(POPUP_ROW);

                fill(mem, 0, 0, w, h, p.pill);
                round_box(mem, R::new(0, 0, w, h), self.s(9), p.pill, p.pill_line);

                let mut footer_at = 0usize;
                match self.popup {
                    PopupKind::Suggest => {
                        for (i, s) in self.suggest.iter().enumerate() {
                            let y = pad + i as i32 * rh;
                            if y + rh > h {
                                break;
                            }
                            draw_popup_row(
                                mem,
                                R::new(pad, y, w - 2 * pad, rh),
                                &s.label,
                                &s.sub,
                                i == self.suggest_sel || self.popup_hover == Some(i),
                                Some(match s.kind {
                                    // Row 0 is the address you are about to
                                    // visit; everything below it is history.
                                    suggest::Kind::Visit if i == 0 => RowIcon::Address,
                                    suggest::Kind::Visit => RowIcon::History,
                                    suggest::Kind::Search => RowIcon::Search,
                                }),
                                self.site_icon(&s.target),
                                &p,
                                self.s(8),
                                self.font,
                            );
                        }
                    }
                    PopupKind::Downloads => {
                        let entries = downloads::entries();
                        let n = entries.len().min(downloads::PANEL_MAX);
                        if n == 0 {
                            draw_popup_row(
                                mem,
                                R::new(pad, pad, w - 2 * pad, rh),
                                "No downloads yet",
                                "Files you download show up here",
                                false,
                                None,
                                None,
                                &p,
                                self.s(8),
                                self.font,
                            );
                        }
                        for (i, d) in entries.iter().take(n).enumerate() {
                            let y = pad + i as i32 * rh;
                            if y + rh > h {
                                break;
                            }
                            draw_popup_row(
                                mem,
                                R::new(pad, y, w - 2 * pad, rh),
                                &d.name,
                                &downloads::meta(d),
                                self.popup_hover == Some(i),
                                Some(RowIcon::File),
                                self.site_icon(&d.url),
                                &p,
                                self.s(8),
                                self.font,
                            );
                        }
                        footer_at = n.max(1);
                    }
                    PopupKind::None => {}
                }

                if self.popup == PopupKind::Downloads {
                    let y = pad + footer_at as i32 * rh;
                    if y + rh <= h {
                        stroke(
                            mem,
                            pad + self.s(4),
                            y,
                            w - pad - self.s(4),
                            y,
                            p.pill_line,
                            1,
                        );
                        draw_popup_row(
                            mem,
                            R::new(pad, y, w - 2 * pad, rh),
                            "Open downloads folder",
                            "",
                            self.popup_hover == Some(footer_at),
                            Some(RowIcon::Folder),
                            None,
                            &p,
                            self.s(8),
                            self.font,
                        );
                    }
                }

                let _ = BitBlt(hdc, 0, 0, w, h, Some(mem), 0, 0, SRCCOPY);
                SelectObject(mem, prev_bmp);
                let _ = DeleteObject(HGDIOBJ(bmp.0));
                let _ = DeleteDC(mem);
            }
            let _ = EndPaint(hwnd, &ps);
        }
    }

    /// Colours + brush for the address edit (`WM_CTLCOLOREDIT`).
    pub fn address_colors(&mut self) -> (COLORREF, COLORREF, HBRUSH) {
        let p = self.palette();
        let (text, back) = (p.text, p.pill);
        if self.edit_brush_color != back || self.edit_brush.0.is_null() {
            if !self.edit_brush.0.is_null() {
                unsafe {
                    let _ = DeleteObject(HGDIOBJ(self.edit_brush.0));
                }
            }
            self.edit_brush = unsafe { CreateSolidBrush(back) };
            self.edit_brush_color = back;
        }
        (text, back, self.edit_brush)
    }

    /// Buffered paint of the whole chrome row.
    pub fn paint_chrome(&self, hwnd: HWND) {
        unsafe {
            let mut ps = PAINTSTRUCT::default();
            let hdc = BeginPaint(hwnd, &mut ps);
            let (w, h) = w32::client_size(hwnd);
            if w > 0 && h > 0 {
                let mem = CreateCompatibleDC(Some(hdc));
                let bmp = CreateCompatibleBitmap(hdc, w, h);
                let prev_bmp = SelectObject(mem, HGDIOBJ(bmp.0));
                let _ = SetBkMode(mem, TRANSPARENT);
                let p = self.palette();
                let lay = self.chrome_layout(w, h);
                let mut toolbar_icons = self.toolbar_icons.borrow_mut();
                toolbar_icons.set_colors(p.muted, p.text);

                fill(mem, 0, 0, w, h, p.bar);

                // Navigation buttons use the supplied Material SVG silhouettes.
                let nav_size = self.s(20);
                for (r, hit, icon) in [
                    (lay.back, Hit::Back, toolbar_icons::Icon::Back),
                    (lay.forward, Hit::Forward, toolbar_icons::Icon::Forward),
                    (lay.reload, Hit::Reload, toolbar_icons::Icon::Reload),
                    (lay.home, Hit::Home, toolbar_icons::Icon::Home),
                ] {
                    if r.w <= 0 {
                        continue;
                    }
                    if self.hover == hit {
                        round_box(mem, r, self.s(RADIUS), p.hover, p.hover);
                    }
                    toolbar_icons.draw(
                        mem,
                        icon,
                        r.x + (r.w - nav_size) / 2,
                        r.y + (r.h - nav_size) / 2,
                        nav_size,
                        self.hover == hit,
                    );
                }

                // Rounded omnibox pill (the EDIT paints on top of it).
                round_box(mem, lay.pill, self.s(RADIUS), p.pill, p.pill_line);
                let search_size = self.s(18);
                toolbar_icons.draw(
                    mem,
                    toolbar_icons::Icon::Search,
                    lay.pill.x + self.s(7),
                    lay.pill.y + (lay.pill.h - search_size) / 2,
                    search_size,
                    false,
                );

                // Loading progress along the pill's bottom edge — the band
                // the address EDIT leaves above the rim. The horizontal
                // inset keeps it inside the rounded corners.
                if let Some(frac) = self.load_fraction() {
                    let inset = self.s(RADIUS);
                    let track = (lay.pill.w - 2 * inset).max(0);
                    let w = ((track as f64 * frac) as i32).clamp(0, track);
                    if w > 0 {
                        let (cr, cg, cb) = p.accent;
                        fill(
                            mem,
                            lay.pill.x + inset,
                            lay.pill.bottom() - self.s(3),
                            w,
                            self.s(2),
                            rgb(cr, cg, cb),
                        );
                    }
                }

                // Tab pills — painted in drag display order, so the dragged
                // pill previews the slot it would drop into.
                let band_end = lay.tabs_x + lay.tabs_w;
                let display = self.tab_display_order();
                for (slot, mut r) in self.tab_rects(&lay).into_iter().enumerate() {
                    if r.x >= band_end {
                        break;
                    }
                    let Some(i) = display.get(slot).copied() else {
                        break;
                    };
                    if r.right() > band_end {
                        r.w = band_end - r.x;
                    }
                    if r.w < self.s(14) {
                        continue;
                    }
                    let active = i == self.active;
                    let hovered = matches!(self.hover, Hit::Tab(j) | Hit::Close(j) if j == i);
                    let bg = if active {
                        p.tab_active
                    } else if hovered {
                        p.hover
                    } else {
                        p.tab
                    };
                    round_box(mem, r, self.s(RADIUS), bg, p.tab_line);

                    let close_visible = active || hovered;
                    let title = self
                        .tabs
                        .get(i)
                        .map(|t| t.title.as_str())
                        .filter(|t| !t.is_empty())
                        .unwrap_or("New tab");
                    // Favicon (or a letter tile) inset on the left; the title
                    // starts after it. Narrow pills get neither.
                    let mut text_x = r.x + self.s(8);
                    if r.w >= self.s(44) {
                        let host = self.tabs.get(i).and_then(|t| nav::host_of(&t.url));
                        let sq = (r.h - self.s(10)).max(self.s(12));
                        let ix = r.x + self.s(6);
                        let iy = r.y + (r.h - sq) / 2;
                        if let Some(hbm) = host.as_deref().and_then(|h| self.icons.get(h)) {
                            favicon::draw(mem, hbm, ix, iy, sq, sq);
                            text_x = ix + sq + self.s(5);
                        } else if let Some(ch) = host.as_deref().and_then(favicon::letter) {
                            let bg = if active { p.tab } else { p.tab_line };
                            let fg = if active { p.tab_text_active } else { p.tab_text };
                            round_box(mem, R::new(ix, iy, sq, sq), self.s(3), bg, bg);
                            draw_text(
                                mem,
                                &ch.to_uppercase().to_string(),
                                ix,
                                iy,
                                sq,
                                sq,
                                fg,
                                self.font,
                                DT_SINGLELINE | DT_VCENTER | DT_CENTER | DT_NOPREFIX,
                            );
                            text_x = ix + sq + self.s(5);
                        }
                    }
                    let text_w = if close_visible {
                        r.right() - self.s(CLOSE_W) - text_x
                    } else {
                        r.right() - self.s(6) - text_x
                    };
                    let color = if active { p.tab_text_active } else { p.tab_text };
                    if text_w > self.s(6) {
                        draw_text(
                            mem,
                            title,
                            text_x,
                            r.y,
                            text_w,
                            r.h,
                            color,
                            self.font,
                            DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS | DT_NOPREFIX,
                        );
                    }
                    if close_visible {
                        draw_text(
                            mem,
                            "\u{D7}",
                            r.right() - self.s(CLOSE_W),
                            r.y,
                            self.s(CLOSE_W),
                            r.h,
                            color,
                            self.font,
                            DT_SINGLELINE | DT_VCENTER | DT_CENTER | DT_NOPREFIX,
                        );
                    }
                    if active {
                        // Accent underline for the active pill.
                        let (ar, ag, ab) = p.accent;
                        fill(
                            mem,
                            r.x + self.s(7),
                            r.bottom() - self.s(5),
                            (r.w - self.s(14)).max(2),
                            self.s(2),
                            rgb(ar, ag, ab),
                        );
                    }
                }

                // New-tab `+`.
                if self.hover == Hit::Plus {
                    round_box(mem, lay.plus, self.s(RADIUS), p.hover, p.hover);
                }
                let plus_size = self.s(18);
                toolbar_icons.draw(
                    mem,
                    toolbar_icons::Icon::Plus,
                    lay.plus.x + (lay.plus.w - plus_size) / 2,
                    lay.plus.y + (lay.plus.h - plus_size) / 2,
                    plus_size,
                    self.hover == Hit::Plus,
                );

                // The downloads button uses the supplied download SVG.
                if self.hover == Hit::Downloads {
                    round_box(mem, lay.downloads, self.s(RADIUS), p.hover, p.hover);
                }
                let download_size = self.s(20);
                toolbar_icons.draw(
                    mem,
                    toolbar_icons::Icon::Download,
                    lay.downloads.x + (lay.downloads.w - download_size) / 2,
                    lay.downloads.y + (lay.downloads.h - download_size) / 2,
                    download_size,
                    self.hover == Hit::Downloads,
                );
                drop(toolbar_icons);

                // Kebab `⋮`.
                if self.hover == Hit::Kebab {
                    round_box(mem, lay.kebab, self.s(RADIUS), p.hover, p.hover);
                }
                draw_dots(
                    mem,
                    lay.kebab,
                    self.s(3),
                    self.s(7),
                    if self.hover == Hit::Kebab { p.text } else { p.muted },
                );

                // Window caption buttons.
                for (r, hit) in [
                    (lay.cap_min, Hit::CapMin),
                    (lay.cap_max, Hit::CapMax),
                    (lay.cap_close, Hit::CapClose),
                ] {
                    let on = self.hover == hit;
                    if on {
                        fill(mem, r.x, r.y, r.w, r.h, if hit == Hit::CapClose { rgb(0xC4, 0x2B, 0x1C) } else { p.hover });
                    }
                    let c = if on && hit == Hit::CapClose {
                        rgb(0xFF, 0xFF, 0xFF)
                    } else if on {
                        p.text
                    } else {
                        p.muted
                    };
                    draw_caption_glyph(mem, r, hit, c, self.s(1), IsZoomed(self.hwnd).as_bool());
                }

                let _ = BitBlt(hdc, 0, 0, w, h, Some(mem), 0, 0, SRCCOPY);
                SelectObject(mem, prev_bmp);
                let _ = DeleteObject(HGDIOBJ(bmp.0));
                let _ = DeleteDC(mem);
            }
            let _ = EndPaint(hwnd, &ps);
        }
    }

    /// Paint of the floating notification pill.
    pub fn paint_toast(&self, hwnd: HWND) {
        unsafe {
            let mut ps = PAINTSTRUCT::default();
            let hdc = BeginPaint(hwnd, &mut ps);
            let (w, h) = w32::client_size(hwnd);
            if w > 0 && h > 0 {
                let mem = CreateCompatibleDC(Some(hdc));
                let bmp = CreateCompatibleBitmap(hdc, w, h);
                let prev_bmp = SelectObject(mem, HGDIOBJ(bmp.0));
                let _ = SetBkMode(mem, TRANSPARENT);
                let p = self.palette();
                round_box(mem, R::new(0, 0, w, h), self.s(9), p.pill, p.pill_line);
                let text = w32::window_text(hwnd);
                draw_text(
                    mem,
                    &text,
                    self.s(14),
                    0,
                    w - self.s(28),
                    h,
                    p.text,
                    self.font,
                    DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS | DT_NOPREFIX,
                );
                let _ = BitBlt(hdc, 0, 0, w, h, Some(mem), 0, 0, SRCCOPY);
                SelectObject(mem, prev_bmp);
                let _ = DeleteObject(HGDIOBJ(bmp.0));
                let _ = DeleteDC(mem);
            }
            let _ = EndPaint(hwnd, &ps);
        }
    }

    /// True when the content area is bare because the active tab failed to
    /// build — in that case we paint the reason ourselves.
    pub fn needs_error_paint(&self) -> bool {
        if self.busy || self.last_error.is_none() {
            return false;
        }
        self.tabs.get(self.active).is_some_and(|t| t.view.is_none())
    }

    /// The "GTX could not open this tab" page, drawn straight onto the main
    /// window (there is no web view to draw it on).
    pub fn paint_error_page(&self) {
        unsafe {
            let mut ps = PAINTSTRUCT::default();
            let hdc = BeginPaint(self.hwnd, &mut ps);
            let (w, h) = w32::client_size(self.hwnd);
            let row = self.ui_row_h();
            let band = w32::frame_band();
            let x = band;
            let y = row;
            let cw = (w - 2 * band).max(0);
            let ch = (h - row - band).max(0);
            if cw > 0 && ch > 0 {
                let p = self.palette();
                fill(
                    hdc,
                    x,
                    y,
                    cw,
                    ch,
                    if p.dark { rgb(0x3A, 0x3A, 0x3A) } else { rgb(0xFF, 0xFF, 0xFF) },
                );
                let msg = self
                    .last_error
                    .clone()
                    .unwrap_or_else(|| "This tab could not be opened.".into());
                let body = format!("{msg}\n\nPress Ctrl+R to try again.");
                draw_text(
                    hdc,
                    &body,
                    x + self.s(48),
                    y + (ch - self.s(160)) / 2,
                    cw - self.s(96),
                    self.s(160),
                    p.text,
                    self.font,
                    DT_CENTER | DT_VCENTER | DT_WORDBREAK | DT_NOPREFIX,
                );
            }
            let _ = EndPaint(self.hwnd, &ps);
        }
    }
}

// ---------------------------------------------------------------------------
// Free helpers
// ---------------------------------------------------------------------------

unsafe fn fill(hdc: HDC, x: i32, y: i32, w: i32, h: i32, color: COLORREF) {
    if w <= 0 || h <= 0 {
        return;
    }
    let r = RECT {
        left: x,
        top: y,
        right: x + w,
        bottom: y + h,
    };
    unsafe {
        let brush = CreateSolidBrush(color);
        let _ = FillRect(hdc, &r, brush);
        let _ = DeleteObject(HGDIOBJ(brush.0));
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_text(
    hdc: HDC,
    s: &str,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    color: COLORREF,
    font: HFONT,
    format: DRAW_TEXT_FORMAT,
) {
    if w <= 0 || h <= 0 {
        return;
    }
    let mut buf = w32::wide_c(s);
    let mut r = RECT {
        left: x,
        top: y,
        right: x + w,
        bottom: y + h,
    };
    unsafe {
        let prev_font = SelectObject(hdc, HGDIOBJ(font.0));
        let _ = SetTextColor(hdc, color);
        DrawTextW(hdc, &mut buf, &mut r, format);
        SelectObject(hdc, prev_font);
    }
}

/// Rounded rectangle with a 1px outline, both in the given colours.
unsafe fn round_box(hdc: HDC, r: R, radius: i32, fill_color: COLORREF, line: COLORREF) {
    if r.w <= 0 || r.h <= 0 {
        return;
    }
    unsafe {
        let brush = CreateSolidBrush(fill_color);
        let pen = CreatePen(PS_SOLID, 1, line);
        let ob = SelectObject(hdc, HGDIOBJ(brush.0));
        let op = SelectObject(hdc, HGDIOBJ(pen.0));
        let _ = RoundRect(hdc, r.x, r.y, r.right(), r.bottom(), radius, radius);
        SelectObject(hdc, ob);
        SelectObject(hdc, op);
        let _ = DeleteObject(HGDIOBJ(brush.0));
        let _ = DeleteObject(HGDIOBJ(pen.0));
    }
}

/// A stroked line segment.
unsafe fn stroke(hdc: HDC, x1: i32, y1: i32, x2: i32, y2: i32, color: COLORREF, width: i32) {
    unsafe {
        let pen = CreatePen(PS_SOLID, width.max(1), color);
        let old = SelectObject(hdc, HGDIOBJ(pen.0));
        let _ = MoveToEx(hdc, x1, y1, None);
        let _ = LineTo(hdc, x2, y2);
        SelectObject(hdc, old);
        let _ = DeleteObject(HGDIOBJ(pen.0));
    }
}

/// Four strokes around `r` — an unfilled rectangle.
unsafe fn stroke_rect(hdc: HDC, r: R, color: COLORREF, width: i32) {
    unsafe {
        stroke(hdc, r.x, r.y, r.right(), r.y, color, width);
        stroke(hdc, r.right(), r.y, r.right(), r.bottom(), color, width);
        stroke(hdc, r.right(), r.bottom(), r.x, r.bottom(), color, width);
        stroke(hdc, r.x, r.bottom(), r.x, r.y, color, width);
    }
}

/// The omnibox's magnifying glass — vector-drawn so it never depends on an
/// icon font being present. `fill` is the pill colour, used to hollow out the
/// circle.
unsafe fn draw_magnifier(
    hdc: HDC,
    cx: i32,
    cy: i32,
    radius: i32,
    color: COLORREF,
    fill: COLORREF,
    width: i32,
) {
    unsafe {
        let pen = CreatePen(PS_SOLID, width.max(1), color);
        let brush = CreateSolidBrush(fill);
        let op = SelectObject(hdc, HGDIOBJ(pen.0));
        let ob = SelectObject(hdc, HGDIOBJ(brush.0));
        let _ = Ellipse(hdc, cx - radius, cy - radius, cx + radius, cy + radius);
        let _ = MoveToEx(hdc, cx + radius * 3 / 4, cy + radius * 3 / 4, None);
        let _ = LineTo(hdc, cx + radius * 2, cy + radius * 2);
        SelectObject(hdc, op);
        SelectObject(hdc, ob);
        let _ = DeleteObject(HGDIOBJ(pen.0));
        let _ = DeleteObject(HGDIOBJ(brush.0));
    }
}

/// Three vertical dots — the kebab `⋮`.
unsafe fn draw_dots(hdc: HDC, r: R, d: i32, gap: i32, color: COLORREF) {
    if r.w <= 0 {
        return;
    }
    let cx = r.x + r.w / 2;
    let cy = r.y + r.h / 2;
    unsafe {
        let pen = CreatePen(PS_SOLID, 1, color);
        let brush = CreateSolidBrush(color);
        let op = SelectObject(hdc, HGDIOBJ(pen.0));
        let ob = SelectObject(hdc, HGDIOBJ(brush.0));
        for k in [-1i32, 0, 1] {
            let top = cy + k * gap - d / 2;
            let _ = Ellipse(hdc, cx - d / 2, top, cx + d / 2 + 1, top + d + 1);
        }
        SelectObject(hdc, op);
        SelectObject(hdc, ob);
        let _ = DeleteObject(HGDIOBJ(pen.0));
        let _ = DeleteObject(HGDIOBJ(brush.0));
    }
}

/// Minimize / maximize / restore / close glyphs, drawn as vectors so they
/// stay crisp at any DPI and never depend on an icon font.
fn draw_caption_glyph(hdc: HDC, r: R, hit: Hit, color: COLORREF, width: i32, maximized: bool) {
    if r.w <= 0 || r.h <= 0 {
        return;
    }
    let cx = r.x + r.w / 2;
    let cy = r.y + r.h / 2;
    let g = (r.h / 6).max(5);
    unsafe {
        match hit {
            Hit::CapMin => stroke(hdc, cx - g, cy, cx + g, cy, color, width),
            Hit::CapMax if maximized => {
                stroke_rect(hdc, R::new(cx - g + 4, cy - g - 2, 2 * g, 2 * g), color, width);
                stroke_rect(hdc, R::new(cx - g, cy - g + 4, 2 * g, 2 * g), color, width);
            }
            Hit::CapMax => stroke_rect(hdc, R::new(cx - g, cy - g, 2 * g, 2 * g), color, width),
            Hit::CapClose => {
                stroke(hdc, cx - g, cy - g, cx + g, cy + g, color, width);
                stroke(hdc, cx + g, cy - g, cx - g, cy + g, color, width);
            }
            _ => {}
        }
    }
}

/// The colour a freshly created web view shows before a page paints: the
/// theme's surface, so a dark browser never flashes white.
fn page_bg(dark: bool) -> (u8, u8, u8, u8) {
    if dark {
        (0x3A, 0x3A, 0x3A, 255)
    } else {
        (255, 255, 255, 255)
    }
}

/// Which internal page (if any) a URL addresses.
fn kind_for_url(url: &str) -> TabKind {
    if url == settings::TAB_URL {
        TabKind::Settings
    } else if url == internal::NEW_TAB_URL {
        TabKind::NewTab
    } else if url == internal::HISTORY_URL {
        TabKind::History
    } else {
        TabKind::Normal
    }
}

fn acc(flags: ACCEL_VIRT_FLAGS, key: u16, cmd: u16) -> ACCEL {
    ACCEL {
        fVirt: flags,
        key,
        cmd,
    }
}

/// The whole keyboard surface of GTX, in one table. Optional shortcuts are
/// gated by config so the settings page can turn them on and off.
pub fn build_accelerators(cfg: &config::Config) -> HACCEL {
    let ctrl = FVIRTKEY | FCONTROL;
    let alt = FVIRTKEY | FALT;
    let mut entries = vec![
        acc(ctrl, 'T' as u16, IDM_NEW_TAB),
        acc(ctrl, 'N' as u16, IDM_NEW_TAB),
        acc(ctrl, 'W' as u16, IDM_CLOSE_TAB),
        acc(ctrl, 'L' as u16, IDM_FOCUS_ADDRESS),
        acc(ctrl, 'R' as u16, IDM_RELOAD),
        acc(FVIRTKEY, VK_F5.0, IDM_RELOAD),
        acc(alt, VK_LEFT.0, IDM_BACK),
        acc(alt, VK_RIGHT.0, IDM_FORWARD),
        acc(ctrl, VK_TAB.0, IDM_NEXT_TAB),
        acc(ctrl | FSHIFT, VK_TAB.0, IDM_PREV_TAB),
        acc(ctrl, VK_NEXT.0, IDM_NEXT_TAB),
        acc(ctrl, VK_PRIOR.0, IDM_PREV_TAB),
        acc(alt, VK_HOME.0, IDM_HOME),
        acc(ctrl, VK_OEM_COMMA.0, IDM_OPEN_SETTINGS),
        acc(ctrl, VK_OEM_PLUS.0, IDM_ZOOM_IN),
        acc(ctrl, VK_OEM_MINUS.0, IDM_ZOOM_OUT),
        acc(ctrl, '0' as u16, IDM_ZOOM_RESET),
        acc(ctrl, 'H' as u16, IDM_HISTORY),
    ];
    if cfg.shortcut_reopen {
        entries.push(acc(ctrl | FSHIFT, 'T' as u16, IDM_REOPEN_TAB));
    }
    if cfg.shortcut_f11 {
        entries.push(acc(FVIRTKEY, VK_F11.0, IDM_FULLSCREEN));
    }
    if cfg.shortcut_ctrl_numbers {
        for k in '1'..='9' {
            entries.push(acc(ctrl, k as u16, IDM_TAB1 + (k as u16 - '1' as u16)));
        }
    }
    if cfg.devtools {
        entries.push(acc(FVIRTKEY, VK_F12.0, IDM_DEVTOOLS));
    }
    unsafe { CreateAcceleratorTableW(&entries).expect("accelerator table") }
}

/// Address-bar subclass: Enter submits, Esc returns focus to the page,
/// and every repaint gets the omnibox cue overlaid while the box is empty.
unsafe extern "system" fn address_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    // The original control paints its background and text first; the cue is
    // ours, drawn on top only while there is nothing typed.
    if msg == WM_PAINT {
        let orig = ADDRESS_ORIG.with(|c| c.get());
        if orig != 0 {
            let proc: WNDPROC = unsafe { core::mem::transmute(orig) };
            let r = unsafe { CallWindowProcW(proc, hwnd, msg, wparam, lparam) };
            unsafe { paint_address_cue(hwnd) };
            return r;
        }
    }
    // Clicking into the bar highlights everything — click, type, Enter is
    // the workflow. Handled on button-*up* so a drag-select or double-click
    // keeps the selection the control itself just made.
    if msg == WM_LBUTTONUP {
        let orig = ADDRESS_ORIG.with(|c| c.get());
        if orig != 0 {
            let proc: WNDPROC = unsafe { core::mem::transmute(orig) };
            let r = unsafe { CallWindowProcW(proc, hwnd, msg, wparam, lparam) };
            let mut start = 0u32;
            let mut end = 0u32;
            unsafe {
                SendMessageW(
                    hwnd,
                    EM_GETSEL,
                    Some(WPARAM(&mut start as *mut u32 as usize)),
                    Some(LPARAM(&mut end as *mut u32 as isize)),
                );
            }
            if start == end {
                unsafe {
                    SendMessageW(hwnd, EM_SETSEL, Some(WPARAM(0)), Some(LPARAM(-1)));
                }
            }
            return r;
        }
    }
    if msg == WM_KEYDOWN {
        let app = unsafe { w32::user_data(hwnd) };
        if !app.is_null() {
            match wparam.0 as u16 {
                v if v == VK_RETURN.0 => {
                    unsafe { app.get() }.address_submit();
                    return LRESULT(0);
                }
                v if v == VK_ESCAPE.0 => {
                    unsafe { app.get() }.address_escape();
                    return LRESULT(0);
                }
                // ↓/↑ walk the suggestion rows instead of the caret.
                v if v == VK_DOWN.0 => {
                    unsafe { app.get() }.suggest_move(1);
                    return LRESULT(0);
                }
                v if v == VK_UP.0 => {
                    unsafe { app.get() }.suggest_move(-1);
                    return LRESULT(0);
                }
                _ => {}
            }
        }
    }
    let orig = ADDRESS_ORIG.with(|c| c.get());
    if orig == 0 {
        return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
    }
    let proc: WNDPROC = unsafe { core::mem::transmute(orig) };
    unsafe { CallWindowProcW(proc, hwnd, msg, wparam, lparam) }
}

/// Horizontal inset shared by the cue and typed text (see `EM_SETMARGINS`).
const CUE_X: i32 = 6;

/// Cue text for the empty omnibox, derived from the configured engine.
pub fn cue_text(cfg: &config::Config) -> String {
    let host = config::search_brand(&cfg.search_template);
    if host.is_empty() {
        "Search or type a URL".to_string()
    } else {
        format!("Search {host} or type a URL")
    }
}

/// Draw the placeholder over the edit's freshly painted background while the
/// omnibox is empty. Drawing it ourselves — instead of `EM_SETCUEBANNER`,
/// whose availability depends on a Comctl32 activation context — keeps the
/// cue in lockstep with every repaint the control makes: caret blink,
/// selection drags, the first typed character clearing it, theme changes.
unsafe fn paint_address_cue(hwnd: HWND) {
    if unsafe { GetWindowTextLengthW(hwnd) } != 0 {
        return;
    }
    let app = unsafe { w32::user_data(hwnd) };
    if app.is_null() {
        return;
    }
    unsafe {
        let hdc = GetDC(Some(hwnd));
        if hdc.0.is_null() {
            return;
        }
        let app = app.get();
        let font_raw = SendMessageW(hwnd, WM_GETFONT, None, None).0;
        let font = if font_raw != 0 {
            HFONT(font_raw as *mut core::ffi::c_void)
        } else {
            app.font
        };
        let (w, h) = w32::client_size(hwnd);
        if w > CUE_X && h > 0 {
            SetBkMode(hdc, TRANSPARENT);
            draw_text(
                hdc,
                &cue_text(&app.cfg),
                CUE_X,
                0,
                w - CUE_X,
                h,
                app.palette().muted,
                font,
                // Top-aligned like the EDIT's own text, so the cue and the
                // first typed character sit on the very same line.
                DT_SINGLELINE | DT_NOPREFIX | DT_END_ELLIPSIS,
            );
        }
        let _ = ReleaseDC(Some(hwnd), hdc);
    }
}

/// Create the tab's web view inside the shared profile.
#[allow(clippy::too_many_arguments)]
fn build_view(
    cfg: &config::Config,
    ctx: &mut WebContext,
    parent: WinHandle,
    bounds: Rect,
    id: u64,
    url: &str,
    html: Option<&str>,
    ptr: AppPtr,
    dark: bool,
) -> wry::Result<WebView> {
    let mut builder = WebViewBuilder::new_with_web_context(ctx)
        .with_bounds(bounds)
        // Match the page background so there is no flash of the wrong colour.
        .with_background_color(page_bg(dark))
        .with_devtools(cfg.devtools)
        .with_theme(theme_from_str(&cfg.theme))
        .with_navigation_handler(move |u| {
            // Every main-frame navigation is announced: it is what drives the
            // address bar's loading bar (and the address itself).
            push_ev(Ev::NavStarted { tab: id, url: u });
            true
        })
        .with_document_title_changed_handler(move |title| {
            push_ev(Ev::Title { tab: id, title });
        })
        .with_on_page_load_handler(move |_event, u| {
            push_ev(Ev::PageLoad { tab: id, url: u });
        })
        .with_new_window_req_handler(move |u, _| {
            // Popups become GTX tabs; deny the engine its own window.
            push_ev(Ev::OpenTab { url: u });
            NewWindowResponse::Deny
        })
        .with_ipc_handler(move |req| {
            push_ev(Ev::Ipc {
                tab: id,
                msg: req.body().clone(),
            });
        })
        .with_download_started_handler(move |dl_url, path| {
            // Runs on the main thread; shows the save dialog when configured.
            let a = ptr;
            !a.is_null() && unsafe { a.get() }.on_download(&dl_url, path)
        })
        .with_download_completed_handler(move |dl_url, path, ok| {
            let name = path
                .as_ref()
                .and_then(|p| p.file_name())
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| file_name_from_url(&dl_url));
            push_ev(Ev::DownloadDone {
                ok,
                name,
                url: dl_url,
                path,
            });
        });

    match html {
        Some(h) => builder = builder.with_html(h),
        None => builder = builder.with_url(url),
    }
    // The popup guard, plus the favicon capture every page runs.
    let init = format!("{}{}", guard::init_script(cfg), favicon::script());
    builder = builder.with_initialization_script(init);
    if !cfg.user_agent.is_empty() {
        builder = builder.with_user_agent(cfg.user_agent.clone());
    }
    if !cfg.browser_accelerator_keys {
        builder = builder.with_browser_accelerator_keys(false);
    }
    if !cfg.hardware_acceleration {
        builder = builder.with_additional_browser_args(BROWSER_ARGS_NO_GPU);
    }
    builder.build_as_child(&parent)
}

fn theme_from_str(s: &str) -> Theme {
    match s.to_ascii_lowercase().as_str() {
        "dark" => Theme::Dark,
        "light" => Theme::Light,
        _ => Theme::Auto,
    }
}

/// `%USERPROFILE%\Downloads` — only used when the engine's suggestion has no
/// parent directory.
fn default_download_dir() -> PathBuf {
    std::env::var_os("USERPROFILE")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("Downloads")
}

/// Hand a folder to Explorer.
///
/// `ShellExecuteW` answers failures with a value ≤ 32 (it never sets
/// `GetLastError`), so that code is the report; when the shell declines the
/// folder, `explorer.exe` is asked directly before we give up.
fn reveal_folder(dir: &Path) -> Result<(), String> {
    let arg = HSTRING::from(dir.to_string_lossy().as_ref());
    let show = windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    let open = unsafe {
        ShellExecuteW(None, w!("open"), &arg, &HSTRING::new(), &HSTRING::new(), show)
    };
    let first = open.0 as isize;
    if first > 32 {
        return Ok(());
    }
    let explorer = unsafe {
        ShellExecuteW(
            None,
            w!("open"),
            &HSTRING::from("explorer.exe"),
            &arg,
            &HSTRING::new(),
            show,
        )
    };
    let second = explorer.0 as isize;
    if second > 32 {
        Ok(())
    } else {
        Err(format!("ShellExecute({first}) then explorer({second}) both failed"))
    }
}

/// Last URL path segment as a download file name (query/fragment stripped,
/// percent-decoded); `"download"` when the URL yields nothing usable.
fn file_name_from_url(url: &str) -> String {
    let path = url.split(['?', '#']).next().unwrap_or("");
    let last = path.rsplit('/').next().unwrap_or("");
    let name = percent_decode(last).trim().to_string();
    if name.is_empty() || name == "." || name == ".." {
        "download".into()
    } else {
        name
    }
}

fn percent_decode(s: &str) -> String {
    fn hex(b: u8) -> Option<u8> {
        match b {
            b'0'..=b'9' => Some(b - b'0'),
            b'a'..=b'f' => Some(b - b'a' + 10),
            b'A'..=b'F' => Some(b - b'A' + 10),
            _ => None,
        }
    }
    let raw = s.as_bytes();
    let mut out = Vec::with_capacity(raw.len());
    let mut i = 0;
    while i < raw.len() {
        if raw[i] == b'%' && i + 3 <= raw.len() {
            if let (Some(hi), Some(lo)) = (hex(raw[i + 1]), hex(raw[i + 2])) {
                out.push((hi << 4) | lo);
                i += 3;
                continue;
            }
        }
        out.push(raw[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cue_text_follows_the_configured_engine() {
        assert_eq!(
            cue_text(&config::Config::default()),
            "Search DuckDuckGo or type a URL"
        );

        let mut google = config::Config::default();
        google.search_template = "https://www.google.com/search?q={}".into();
        assert_eq!(cue_text(&google), "Search Google or type a URL");

        // No engine at all still yields a sensible, non-broken hint.
        let mut none = config::Config::default();
        none.search_template = String::new();
        assert_eq!(cue_text(&none), "Search or type a URL");
    }

    /// The launch contract: Windows hands GTX a link, and no amount of
    /// session restoring may swallow it - that is exactly the bug this pins.
    #[test]
    fn a_link_that_launched_the_browser_always_opens() {
        let link = "https://example.com/clicked";
        assert_eq!(
            plan_startup(3, true, Some(link), "gtx://newtab"),
            Startup::Restored { link: Some(link.into()) },
            "restored tabs come back *and* the clicked link opens on top"
        );
        assert_eq!(
            plan_startup(3, true, None, "gtx://newtab"),
            Startup::Restored { link: None },
            "a plain restart just restores"
        );
        assert_eq!(plan_startup(0, true, Some(link), "gtx://newtab"), Startup::Fresh(link.into()));
        assert_eq!(plan_startup(3, false, Some(link), "gtx://newtab"), Startup::Fresh(link.into()));
        assert_eq!(
            plan_startup(3, false, None, "gtx://newtab"),
            Startup::Fresh("gtx://newtab".into())
        );
        assert_eq!(
            plan_startup(0, true, None, "gtx://newtab"),
            Startup::Fresh("gtx://newtab".into()),
            "an empty session still falls back to the start page"
        );
    }

    #[test]
    fn internal_urls_include_the_html_we_render_ourselves() {
        assert!(internal::is_internal("gtx://settings"));
        assert!(internal::is_internal("gtx://newtab"));
        assert!(internal::is_internal("about:blank"));
        assert!(internal::is_internal("about:srcdoc"));
        // wry's with_html navigates to this, then WebView2 settles on blank.
        assert!(internal::is_internal(
            "data:text/html;charset=utf-8;base64,PCFkb2N0eXBlPgo="
        ));
        assert!(internal::is_internal("blob:https://x/1"));
        assert!(!internal::is_internal("https://example.com"));
        assert!(!internal::is_internal("file:///C:/x.html"));
    }

    /// An internal tab must survive its own first paint: `with_html` reports a
    /// `data:` navigation and a follow-up `about:blank`, and neither may demote
    /// the tab (which would make it overwrite its `gtx://…` address).
    #[test]
    fn internal_tabs_survive_their_own_page_load() {
        let mut a = app();
        a.tabs.push(Tab {
            id: 1,
            url: settings::TAB_URL.into(),
            title: settings::TAB_TITLE.into(),
            view: None,
            zoom: 1.0,
            last_active: Instant::now(),
            kind: kind_for_url(settings::TAB_URL),
        });
        a.active = 0;

        a.on_nav(
            1,
            "data:text/html;charset=utf-8;base64,PCFkb2N0eXBlPgo=".into(),
            false,
        );
        assert_eq!(a.tabs[0].kind, TabKind::Settings);
        assert_eq!(a.tabs[0].url, settings::TAB_URL);

        a.on_nav(1, "about:blank".into(), true);
        assert_eq!(a.tabs[0].kind, TabKind::Settings);
        assert_eq!(a.tabs[0].url, settings::TAB_URL);

        // Leaving for a real site does turn it into an ordinary tab.
        a.on_nav(1, "https://example.com".into(), true);
        assert_eq!(a.tabs[0].kind, TabKind::Normal);
        assert_eq!(a.tabs[0].url, "https://example.com");
    }

    /// An ordinary tab keeps tracking its own address, including about:/data:.
    #[test]
    fn normal_tabs_still_follow_their_url() {
        let mut a = app();
        a.tabs.push(tab(1, "Example"));
        a.active = 0;
        a.on_nav(1, "about:blank".into(), true);
        assert_eq!(a.tabs[0].kind, TabKind::Normal);
        assert_eq!(a.tabs[0].url, "about:blank");
        a.on_nav(1, "https://example.org/next".into(), true);
        assert_eq!(a.tabs[0].url, "https://example.org/next");
    }

    /// An `App` with no window: enough to exercise pure layout maths.
    fn app() -> App {
        App::new(
            HWND(core::ptr::null_mut()),
            config::Config::default(),
            Instant::now(),
            None,
            Some(1),
        )
    }

    fn tab(id: u64, title: &str) -> Tab {
        Tab {
            id,
            url: "https://example.com".into(),
            title: title.into(),
            view: None,
            zoom: 1.0,
            last_active: Instant::now(),
            kind: TabKind::Normal,
        }
    }

    #[test]
    fn chrome_layout_matches_the_reference_row() {
        let a = app();
        let row = a.ui_row_h();
        let lay = a.chrome_layout(1200, row);

        // Left → right: back, forward, reload, omnibox, tabs, +, ⋮, captions.
        assert!(lay.back.x < lay.forward.x);
        assert!(lay.forward.x < lay.reload.x);
        assert!(lay.reload.x + lay.reload.w <= lay.pill.x);
        assert!(lay.pill.right() <= lay.tabs_x);
        assert!(lay.tabs_x + lay.tabs_w <= lay.plus.x);
        assert!(lay.plus.x + lay.plus.w <= lay.kebab.x);
        assert!(lay.kebab.x + lay.kebab.w <= lay.cap_min.x);
        assert_eq!(lay.cap_min.x + a.ui_capw(), lay.cap_max.x);
        assert_eq!(lay.cap_max.x + a.ui_capw(), lay.cap_close.x);
        assert_eq!(lay.cap_close.right(), 1200);

        // Caption buttons span the whole row; controls never enter the
        // invisible resize band along the top.
        assert_eq!(lay.cap_close.h, row);
        let band = w32::frame_band();
        assert!(lay.back.y >= band, "nav buttons overlap the resize band");
        assert!(lay.pill.y >= band);

        // The real EDIT sits inside the rounded pill, leaving room for the
        // magnifier on the left and the pill's corners all round.
        assert!(lay.addr.x > lay.pill.x);
        assert!(lay.addr.right() < lay.pill.right());
        assert!(lay.addr.y > lay.pill.y);
        assert!(lay.addr.bottom() < lay.pill.bottom());
    }

    #[test]
    fn narrow_windows_shrink_the_omnibox_before_the_tabs() {
        let a = app();
        let row = a.ui_row_h();
        let wide = a.chrome_layout(1400, row);
        let narrow = a.chrome_layout(700, row);
        assert!(narrow.pill.w < wide.pill.w);
        // The caption/kebab/+ column keeps its size; only the middle flexes.
        assert_eq!(narrow.cap_close.w, wide.cap_close.w);
        assert_eq!(narrow.kebab.w, wide.kebab.w);
        assert!(narrow.tabs_w >= 0);
    }

    #[test]
    fn tab_pills_fill_the_band_in_order() {
        let mut a = app();
        a.tabs = (0..4).map(|i| tab(i + 1, "Title")).collect();
        let row = a.ui_row_h();
        let lay = a.chrome_layout(1200, row);
        let rects = a.tab_rects(&lay);
        assert_eq!(rects.len(), 4);
        for w in rects.windows(2) {
            assert!(w[0].right() + a.s(4) <= w[1].x, "pills must not overlap");
            assert!(w[1].x < lay.tabs_x + lay.tabs_w, "pills clipped to the band");
        }
        assert!(rects[0].x >= lay.tabs_x);
        assert!(rects.iter().all(|r| r.h == a.ui_tab_h()));
    }

    #[test]
    fn the_row_gives_every_control_equal_padding() {
        // The complaint that started this: text and icons visibly sat lower
        // than they should. Lock the invariant for both toolbar densities.
        let mut a = app();
        for compact in [false, true] {
            a.cfg.compact_mode = compact;
            let row = a.ui_row_h();
            let band = w32::frame_band();
            let lay = a.chrome_layout(1200, row);
            for (name, r) in [
                ("pill", lay.pill),
                ("back", lay.back),
                ("forward", lay.forward),
                ("reload", lay.reload),
                ("kebab", lay.kebab),
                ("plus", lay.plus),
            ] {
                assert_eq!(r.y, row - r.bottom(), "{name} compact={compact}");
                assert!(r.y >= band, "{name} must clear the resize band");
            }
            // Tabs ride the pill's line exactly.
            a.tabs = (0..3).map(|i| tab(i + 1, "Title")).collect();
            let lay = a.chrome_layout(1200, row);
            for r in a.tab_rects(&lay) {
                assert_eq!((r.y, r.h), (lay.pill.y, lay.pill.h), "compact={compact}");
            }
        }
    }

    #[test]
    fn the_drop_slot_follows_the_cursor_between_pill_centres() {
        let mut a = app();
        a.tabs = (1..=3).map(|i| tab(i, "Title")).collect();
        let row = a.ui_row_h();
        let lay = a.chrome_layout(1200, row);
        let r = a.tab_rects(&lay);

        // Left of everything lands before the first pill, right of everything
        // lands last — whichever tab is being dragged.
        assert_eq!(a.tab_drop_slot(&lay, r[0].x, 0), 0);
        assert_eq!(a.tab_drop_slot(&lay, r[2].right(), 0), 2);
        assert_eq!(a.tab_drop_slot(&lay, r[0].x, 2), 0);
        assert_eq!(a.tab_drop_slot(&lay, r[2].right(), 2), 2);
        // The slot flips exactly when the cursor passes a pill's centre.
        assert_eq!(a.tab_drop_slot(&lay, r[1].x + r[1].w / 2, 0), 0);
        assert_eq!(a.tab_drop_slot(&lay, r[1].x + r[1].w / 2 + 1, 0), 1);
    }

    #[test]
    fn the_drag_preview_is_exactly_where_the_tab_lands() {
        let mut a = app();
        a.tabs = (1..=4).map(|i| tab(i, "Title")).collect();
        a.no_session = true; // never write the real session from a test
        a.active = 2;
        let active_id = a.tabs[a.active].id;
        let row = a.ui_row_h();
        let _lay = a.chrome_layout(1200, row);

        for from in 0..4usize {
            for to in 0..4usize {
                let before: Vec<u64> = a.tabs.iter().map(|t| t.id).collect();
                a.tab_drag = Some((from, to));
                let preview = a.tab_display_order();
                a.tab_drag = None;
                a.move_tab(from, to);

                // Every slot must show exactly the tab the drag preview drew
                // there, and the active tab must have travelled with itself.
                for (slot, &shown) in preview.iter().enumerate() {
                    assert_eq!(
                        a.tabs[slot].id, before[shown],
                        "from={from} to={to}: slot {slot} of {preview:?}"
                    );
                }
                assert_eq!(
                    a.tabs[a.active].id, active_id,
                    "from={from} to={to}: active tab must not change identity"
                );
            }
        }
    }

    #[test]
    fn hit_testing_walks_the_row_left_to_right() {
        let mut a = app();
        a.tabs = (0..2).map(|i| tab(i + 1, "Title")).collect();
        let row = a.ui_row_h();
        let lay = a.chrome_layout(1200, row);

        let mid = |r: R| ((r.x + r.right()) / 2, (r.y + r.bottom()) / 2);
        assert_eq!(a.hit_at(&lay, mid(lay.back).0, mid(lay.back).1), Hit::Back);
        assert_eq!(a.hit_at(&lay, mid(lay.reload).0, mid(lay.reload).1), Hit::Reload);
        assert_eq!(a.hit_at(&lay, mid(lay.pill).0, mid(lay.pill).1), Hit::Omnibox);
        assert_eq!(a.hit_at(&lay, mid(lay.plus).0, mid(lay.plus).1), Hit::Plus);
        assert_eq!(a.hit_at(&lay, mid(lay.kebab).0, mid(lay.kebab).1), Hit::Kebab);
        assert_eq!(a.hit_at(&lay, mid(lay.cap_min).0, mid(lay.cap_min).1), Hit::CapMin);
        assert_eq!(a.hit_at(&lay, mid(lay.cap_close).0, mid(lay.cap_close).1), Hit::CapClose);

        // Tabs: body selects, the right-hand strip on the active one closes.
        let rects = a.tab_rects(&lay);
        a.active = 0;
        let t0 = rects[0];
        assert_eq!(a.hit_at(&lay, t0.x + 4, t0.y + 4), Hit::Tab(0));
        assert_eq!(
            a.hit_at(&lay, t0.right() - 4, t0.y + 4),
            Hit::Close(0),
            "the active tab always exposes its close button"
        );
        // The gap between two pills is empty toolbar, i.e. a drag area.
        let gap_x = rects[0].right() + 1;
        assert_eq!(a.hit_at(&lay, gap_x, rects[0].y + 2), Hit::None);
    }

    #[test]
    fn downloads_button_sits_between_plus_and_kebab() {
        let a = app();
        let row = a.ui_row_h();
        let lay = a.chrome_layout(1200, row);

        assert!(lay.plus.right() <= lay.downloads.x, "after the new-tab +");
        assert!(lay.downloads.right() <= lay.kebab.x, "before the kebab");
        assert_eq!(lay.downloads.w, a.ui_kebab_w());
        assert_eq!(lay.downloads.h, a.ui_btn());
        assert_eq!(
            a.hit_at(
                &lay,
                (lay.downloads.x + lay.downloads.right()) / 2,
                (lay.downloads.y + lay.downloads.bottom()) / 2,
            ),
            Hit::Downloads,
            "the button is where it is painted"
        );
    }

    /// The drop-down's painter and click handler share this geometry.
    #[test]
    fn popup_rows_line_up_with_their_clicks() {
        assert_eq!(row_at(0, 3, 44, 4), None, "the top padding is not a row");
        assert_eq!(row_at(4, 3, 44, 4), Some(0));
        assert_eq!(row_at(47, 3, 44, 4), Some(0));
        assert_eq!(row_at(48, 3, 44, 4), Some(1));
        assert_eq!(row_at(135, 3, 44, 4), Some(2));
        assert_eq!(row_at(136, 3, 44, 4), None, "the bottom padding is not a row");
        assert_eq!(row_at(100, 0, 44, 4), None, "no rows means no hit");
        assert_eq!(row_at(10, 3, 0, 4), None, "degenerate geometry never hits");
    }

    #[test]
    fn closing_the_panel_forgets_its_rows() {
        let mut a = app();
        a.suggest = suggest::suggest("example", &[], &a.cfg.search_template);
        a.popup = PopupKind::Suggest;
        a.popup_hover = Some(0);
        assert!(!a.suggest.is_empty());
        a.hide_popup();
        assert_eq!(a.popup, PopupKind::None);
        assert!(a.suggest.is_empty());
        assert_eq!(a.popup_hover, None);
        a.hide_popup(); // idempotent: nothing to close
    }

    #[test]
    fn internal_urls_map_to_their_own_tab_kind() {
        assert_eq!(kind_for_url(settings::TAB_URL), TabKind::Settings);
        assert_eq!(kind_for_url(internal::NEW_TAB_URL), TabKind::NewTab);
        assert_eq!(kind_for_url(internal::HISTORY_URL), TabKind::History);
        assert_eq!(kind_for_url("https://example.com"), TabKind::Normal);
        assert!(TabKind::Settings.is_internal());
        assert!(!TabKind::Normal.is_internal());
        assert!(TabKind::Settings.html().is_some());
        assert!(TabKind::Normal.html().is_none());
    }

    #[test]
    fn palette_follows_theme_and_accent() {
        let mut a = app();
        a.cfg.theme = "dark".into();
        assert!(a.resolved_dark());
        a.cfg.theme = "light".into();
        assert!(!a.resolved_dark());
        a.cfg.accent = "#ff8800".into();
        assert_eq!(a.palette().accent, (0xFF, 0x88, 0x00));
        a.cfg.theme = "auto".into();
        a.system_dark = true;
        assert!(a.resolved_dark());
        a.system_dark = false;
        assert!(!a.resolved_dark());
    }

    /// The loading bar's maths: ease towards 95% while a page loads, snap to
    /// 100% when it is done, then disappear.
    #[test]
    fn the_loading_bar_advances_and_then_disappears() {
        let mut a = app();
        assert_eq!(a.load_fraction(), None, "nothing is loading at first");

        a.load_begin();
        let early = a.load_fraction().expect("a running load shows a bar");
        assert!(early >= 0.0 && early < 0.95, "starts low: {early}");
        // Never a full bar while the page is still in flight — the last 5%
        // is what "finished" gets to fill.
        assert!(a.load_fraction().unwrap() <= 0.95);

        a.load_end();
        let done = a.load_fraction().expect("finishing still shows the bar");
        assert!(done >= 0.95, "the last stretch is at least the 95% mark");

        // A second navigation while one is running keeps the bar going.
        a.load_begin();
        assert!(a.load_fraction().is_some());
    }

    /// A tab the user asked for is handed back ready to type into: the
    /// request survives a deferred activation (which is exactly what happens
    /// while a view is being built) and is spent by the activation that
    /// finally runs.
    #[test]
    fn a_new_tab_is_ready_to_type_into() {
        let mut a = app();

        // Activation is deferred while a view builds elsewhere: the request
        // must not be lost with it.
        a.busy = true;
        a.new_tab();
        assert_eq!(a.tabs.len(), 1);
        assert!(a.focus_addr_pending, "the fresh tab must ask for the bar");
        assert!(
            a.deferred.iter().any(|d| matches!(d, Defer::Activate(_))),
            "its activation is queued behind the build"
        );

        // The activation tail is the only place that spends the request,
        // and it spends it exactly once.
        a.focus_addr_pending = true;
        a.spend_address_focus();
        assert!(!a.focus_addr_pending, "the request must be spent");
        a.spend_address_focus();
        assert!(!a.focus_addr_pending, "and never fire twice");
    }
}
