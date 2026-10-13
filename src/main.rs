//! GTX Browser entry point: argument parsing, frameless window class, message
//! loop.
//!
//! The loop is plain Win32 (`GetMessage` + `TranslateAccelerator`), which is
//! what WebView2 expects; web-view callbacks queue events that we drain at the
//! top of every iteration.
//!
//! The window is frameless: `WM_NCCALCSIZE` gives the client the whole window
//! rectangle (so no title bar is drawn) while `WS_OVERLAPPEDWINDOW` is kept for
//! resizing, snapping and taskbar behaviour. `WM_NCHITTEST` then supplies the
//! invisible resize band around the edges — and the chrome row supplies it
//! along the top, where it covers the window.

#![cfg_attr(not(test), windows_subsystem = "windows")]

mod app;
mod chrome;
mod config;
mod default_browser;
mod downloads;
mod favicon;
mod guard;
mod history;
mod icon_fetch;
mod internal;
mod log;
mod nav;
mod popup;
mod reg_names;
mod session;
mod settings;
mod suggest;
mod toast;
mod toolbar_icons;
mod w32;

use std::time::Instant;

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{
    ERROR_ALREADY_EXISTS, GetLastError, HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM,
};
use windows::Win32::Graphics::Gdi::{
    COLOR_BTNFACE, GetMonitorInfoW, MonitorFromWindow, HBRUSH, MONITORINFO,
    MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::System::DataExchange::COPYDATASTRUCT;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::HiDpi::{
    SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AllowSetForegroundWindow, CreateWindowExW, DefWindowProcW, DispatchMessageW, FindWindowW,
    GetMessageW, GetSystemMetrics, GetWindowRect, GetWindowThreadProcessId, IsIconic, IsZoomed,
    LoadCursorW, LoadIconW, MSG, NCCALCSIZE_PARAMS, PostQuitMessage, RegisterClassExW,
    SendMessageTimeoutW, SetForegroundWindow, SetWindowPos, ShowWindow, TranslateAcceleratorW,
    TranslateMessage, CS_HREDRAW, CS_VREDRAW, IDC_ARROW, IDI_APPLICATION, SM_CXSCREEN, SM_CYSCREEN,
    SMTO_ABORTIFHUNG, SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER,
    SW_RESTORE, WNDCLASSEXW, WM_CLOSE, WM_COMMAND, WM_COPYDATA, WM_DPICHANGED, WM_DESTROY,
    WM_NCCALCSIZE, WM_NCHITTEST, WM_PAINT, WM_SETFOCUS, WM_SIZE, WM_ACTIVATE, WM_TIMER,
    WS_CLIPCHILDREN, WS_OVERLAPPEDWINDOW, WS_VISIBLE,
};

use crate::app::App;

// ---------------------------------------------------------------------------
// Arguments
// ---------------------------------------------------------------------------

struct Args {
    /// Positional argument: URL to open in the first tab.
    url: Option<String>,
    /// `--smoke [ms]`: start, report, exit (used by automated checks).
    smoke_ms: Option<u32>,
    /// `--no-restore`: ignore the saved session.
    no_restore: bool,
}

fn parse_args() -> Args {
    let mut args = Args {
        url: None,
        smoke_ms: None,
        no_restore: false,
    };
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--smoke" => {
                args.smoke_ms = Some(it.next().and_then(|v| v.parse().ok()).unwrap_or(3000));
            }
            "--no-restore" => args.no_restore = true,
            other if !other.starts_with("--") => args.url = Some(other.to_string()),
            _ => {}
        }
    }
    args
}

// ---------------------------------------------------------------------------
// Window class + creation
// ---------------------------------------------------------------------------

unsafe fn register_class() {
    unsafe {
        let module = GetModuleHandleW(None).expect("module handle");
        let cursor = LoadCursorW(None, IDC_ARROW).expect("cursor");
        // Our own icon from resource.rc when the build had an SDK to embed it.
        let icon = LoadIconW(
            Some(HINSTANCE(module.0)),
            PCWSTR(1usize as *const u16), // MAKEINTRESOURCE(1)
        )
        .unwrap_or_else(|_| LoadIconW(None, IDI_APPLICATION).expect("icon"));
        let class = WNDCLASSEXW {
            cbSize: core::mem::size_of::<WNDCLASSEXW>() as u32,
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(wnd_proc),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: HINSTANCE(module.0),
            hIcon: icon,
            hCursor: cursor,
            // Replaced at startup by the theme's frame colour; this is only
            // the placeholder visible before `App::initialize` runs.
            hbrBackground: HBRUSH((COLOR_BTNFACE.0 + 1) as isize as *mut core::ffi::c_void),
            lpszMenuName: PCWSTR::null(),
            lpszClassName: w32::main_class(),
            hIconSm: icon,
        };
        RegisterClassExW(&class);
    }
}

/// Default window rect: mostly-fullscreen but never edge-to-edge.
fn initial_rect() -> (i32, i32, i32, i32) {
    unsafe {
        let sw = GetSystemMetrics(SM_CXSCREEN);
        let sh = GetSystemMetrics(SM_CYSCREEN);
        let w = (sw * 9 / 10).clamp(640, 1366);
        let h = (sh * 8 / 10).clamp(480, 860);
        ((sw - w) / 2, (sh - h) / 3, w, h)
    }
}

// ---------------------------------------------------------------------------
// Main window procedure
// ---------------------------------------------------------------------------

unsafe extern "system" fn wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let app = unsafe { w32::user_data(hwnd) };

    match msg {
        // Frameless: the client is the whole window (no title bar). When
        // maximised we take the monitor's work area instead so the page never
        // hides behind the taskbar.
        WM_NCCALCSIZE => {
            if wparam.0 == 0 {
                return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
            }
            let params = lparam.0 as *mut NCCALCSIZE_PARAMS;
            if params.is_null() {
                return LRESULT(0);
            }
            if unsafe { IsZoomed(hwnd) }.as_bool() {
                let mon = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) };
                let mut mi = MONITORINFO {
                    cbSize: core::mem::size_of::<MONITORINFO>() as u32,
                    ..Default::default()
                };
                if unsafe { GetMonitorInfoW(mon, &mut mi) }.as_bool() {
                    unsafe { (*params).rgrc[0] = mi.rcWork };
                }
            }
            LRESULT(0)
        }
        // The invisible resize band around the eight edges. The chrome window
        // supplies the same thing for the top edge, which it covers.
        WM_NCHITTEST => {
            let pt = (
                (lparam.0 & 0xFFFF) as u16 as i16 as i32,
                ((lparam.0 >> 16) & 0xFFFF) as u16 as i16 as i32,
            );
            let mut wr = RECT::default();
            if unsafe { GetWindowRect(hwnd, &mut wr) }.is_err() {
                return LRESULT(w32::HTCLIENT as isize);
            }
            let code = w32::edge_hit_test(wr, pt, w32::frame_band());
            LRESULT(code as isize)
        }
        WM_PAINT => {
            // When the active tab has no web view (creation failed) the content
            // area is bare window — paint the reason for it there.
            if !app.is_null() && unsafe { app.get() }.needs_error_paint() {
                unsafe { app.get() }.paint_error_page();
                return LRESULT(0);
            }
            unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
        }
        WM_SIZE => {
            if !app.is_null() {
                unsafe { app.get() }.layout();
            }
            LRESULT(0)
        }
        WM_SETFOCUS => {
            // Clicking the frame hands the keyboard back to the page.
            if !app.is_null() {
                unsafe { app.get() }.focus_page();
            }
            LRESULT(0)
        }
        WM_ACTIVATE => {
            // The drop-down and the toast float above this window, so when
            // another application takes over the foreground the panel must
            // go with it (low word 0 = WA_INACTIVE).
            if !app.is_null() && w32::lo_word(wparam.0) == 0 {
                unsafe { app.get() }.hide_popup();
            }
            LRESULT(0)
        }
        WM_COMMAND => {
            if !app.is_null() {
                let id = w32::lo_word(wparam.0);
                let notify = w32::hi_word(wparam.0);
                unsafe { app.get() }.on_command(id, notify);
            }
            LRESULT(0)
        }
        WM_TIMER => {
            if !app.is_null() {
                unsafe { app.get() }.on_timer(wparam.0);
            }
            LRESULT(0)
        }
        WM_DPICHANGED => {
            if !app.is_null() {
                let dpi = w32::lo_word(wparam.0) as u32;
                let suggested = unsafe { *(lparam.0 as *const RECT) };
                unsafe { app.get() }.on_dpi_changed(dpi, suggested);
            }
            LRESULT(0)
        }
        WM_CLOSE => {
            log::write("WM_CLOSE");
            if !app.is_null() {
                // Saves the session (and the config), then destroys the window.
                unsafe { app.get() }.on_close();
                LRESULT(0)
            } else {
                unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
            }
        }
        // A second instance hands us the link it was started with, then
        // exits: one browser, and the click that opened it is not lost.
        WM_COPYDATA => {
            let cds = unsafe { *(lparam.0 as *const COPYDATASTRUCT) };
            if cds.dwData != COPYDATA_URL || cds.lpData.is_null() || cds.cbData < 2 {
                return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
            }
            let units = cds.cbData as usize / 2;
            let slice = unsafe { std::slice::from_raw_parts(cds.lpData as *const u16, units) };
            let raw = String::from_utf16_lossy(slice);
            let url = nav::resolve_launch_arg(
                raw.trim_end_matches('\0').trim(),
                &config::load().search_template,
            );
            if !url.is_empty() {
                // The queue is thread-local and this is the main thread, so
                // the event drains whether or not `App` is bound yet.
                app::push_ev(app::Ev::OpenTab { url });
            }
            // The link is why the user is looking at the screen.
            unsafe {
                if IsIconic(hwnd).as_bool() {
                    let _ = ShowWindow(hwnd, SW_RESTORE);
                }
                let _ = SetForegroundWindow(hwnd);
            }
            LRESULT(1)
        }
        WM_DESTROY => {
            log::write("WM_DESTROY");
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

// ---------------------------------------------------------------------------
// Message loop
// ---------------------------------------------------------------------------

unsafe fn run_loop(app: &mut App, hwnd: HWND) {
    let mut msg = MSG::default();
    loop {
        // Apply anything the web views queued during the last dispatch.
        app.drain_events();

        let got = unsafe { GetMessageW(&mut msg, None, 0, 0) };
        if got.0 <= 0 {
            break;
        }
        let accel = app.accel;
        if !accel.0.is_null() && unsafe { TranslateAcceleratorW(hwnd, accel, &msg) } != 0 {
            continue;
        }
        unsafe {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

// ---------------------------------------------------------------------------
// main
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Single instance: a link must land in the window that is already open
// ---------------------------------------------------------------------------

/// Marker in `COPYDATASTRUCT::dwData`: this payload is a URL to open.
const COPYDATA_URL: usize = 0x4754_5831; // "GTX1"

/// If another GTX already owns the profile, hand it our URL and report true.
///
/// Windows starts a *second* process when a link is clicked; without this it
/// would open a duplicate window and lose the link. The mutex decides who
/// runs, `FindWindow` delivers - the poll covers the moment the winner has
/// claimed the mutex but not yet created its window. `--smoke` opts out:
/// automated checks must be able to start a standalone instance.
fn forward_to_running_instance(url: Option<&str>) -> bool {
    unsafe {
        let Ok(_mutex) = CreateMutexW(None, false, w!(r"Local\GTXBrowser")) else {
            return false; // could not even claim the name: start normally
        };
        // Dropping the wrapper does not close the OS handle: it stays open
        // for the life of this process, which *is* the single-instance claim.
        let already = GetLastError() == ERROR_ALREADY_EXISTS;
        if !already {
            return false;
        }
        let mut target = HWND(core::ptr::null_mut());
        for _ in 0..60 {
            if let Ok(h) = FindWindowW(w32::main_class(), PCWSTR::null()) {
                target = h;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        if target.0.is_null() {
            return false; // the owner died in between: start normally
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(target, Some(&mut pid));
        let _ = AllowSetForegroundWindow(pid);
        let wide: Vec<u16> = url
            .unwrap_or("")
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        let cds = COPYDATASTRUCT {
            dwData: COPYDATA_URL,
            cbData: (wide.len() * 2) as u32,
            lpData: wide.as_ptr() as *mut _,
        };
        let mut done = 0usize;
        let _ = SendMessageTimeoutW(
            target,
            WM_COPYDATA,
            WPARAM(0),
            LPARAM(&cds as *const COPYDATASTRUCT as isize),
            SMTO_ABORTIFHUNG,
            3000,
            Some(&mut done),
        );
        true
    }
}

fn main() {
    // Release builds are `windows_subsystem = "windows"`, so a panic would
    // vanish without a trace — record it before anything else can fail.
    let previous_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        log::write(&format!("PANIC: {info}"));
        previous_hook(info);
    }));

    let mut args = parse_args();
    let started = Instant::now();

    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }

    let cfg = config::load();
    // Whatever Windows handed us - URL, .htm path, bare host - becomes a
    // navigable address before anybody looks at it.
    if let Some(u) = args.url.take() {
        args.url = Some(nav::resolve_launch_arg(&u, &cfg.search_template));
    }
    // A link from another app lands in the window that already exists
    // instead of starting a second browser beside it.
    if args.smoke_ms.is_none() && forward_to_running_instance(args.url.as_deref()) {
        return;
    }

    let session = if args.smoke_ms.is_some() || args.no_restore {
        None
    } else {
        session::load()
    };

    unsafe { register_class() };
    chrome::register();
    toast::register();
    popup::register();

    let (x, y, w, h) = initial_rect();
    let module = unsafe { GetModuleHandleW(None) }.expect("module handle");
    let hwnd = unsafe {
        CreateWindowExW(
            Default::default(),
            w32::main_class(),
            w!("GTX Browser"),
            WS_OVERLAPPEDWINDOW | WS_VISIBLE | WS_CLIPCHILDREN,
            x,
            y,
            w,
            h,
            None,
            None,
            Some(HINSTANCE(module.0)),
            None,
        )
        .expect("main window")
    };

    // Force a WM_NCCALCSIZE pass so the frameless client rect is applied
    // before anything is positioned inside it.
    unsafe {
        let _ = SetWindowPos(
            hwnd,
            None,
            0,
            0,
            0,
            0,
            SWP_FRAMECHANGED | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
        );
    }

    // Publish the ProgIds/capabilities Windows needs to offer GTX in
    // *Default apps*. Cheap, idempotent and never fatal: a portable exe has
    // no installer to do it once.
    default_browser::register();

    let mut app = Box::new(App::new(hwnd, cfg, started, args.url, args.smoke_ms));
    app.bind();
    app.initialize(session);

    unsafe { run_loop(&mut app, hwnd) };
    log::write("message loop ended; exiting");
}
