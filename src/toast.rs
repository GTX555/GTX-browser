//! A small transient notification pill ("toast") that floats over the bottom
//! left of the page: blocked navigations, finished downloads, save failures
//! and tab errors all report through it instead of vanishing silently.
//!
//! Like [`crate::popup`], it is an *owned top-level* window rather than a
//! child, because WebView2's DirectComposition content composites above any
//! sibling GDI child window regardless of z-order — a child toast would be
//! hidden behind the page it reports on.
//!
//! One window per app, painted by [`crate::app::App::paint_toast`] and
//! shown/hidden by these helpers.

use windows::core::{w, HSTRING, PCWSTR};
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, POINT, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    ClientToScreen, DeleteDC, DrawTextW, CreateCompatibleDC, DT_CALCRECT, DT_NOPREFIX,
    DT_SINGLELINE,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, GetParent, KillTimer, LoadCursorW, RegisterClassExW, SetTimer,
    SetWindowPos, SetWindowTextW, ShowWindow, IDC_ARROW, SW_HIDE, SW_SHOWNA, SWP_NOACTIVATE,
    SWP_NOSIZE, SWP_NOZORDER, HWND_TOP, WM_ERASEBKGND, WM_PAINT, WM_TIMER, WNDCLASSEXW,
    WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_POPUP, WS_VISIBLE,
};

use crate::w32::{self, AppPtr};

/// Timer id that hides an expired toast.
pub const IDT_TOAST: usize = 10;
/// How long a toast stays on screen, in milliseconds.
const TTL_MS: u32 = 4_500;

pub fn class_name() -> PCWSTR {
    w!("GtxToast")
}

pub fn register() {
    unsafe {
        let module = GetModuleHandleW(None).expect("module handle");
        let cursor = LoadCursorW(None, IDC_ARROW).expect("cursor");
        let class = WNDCLASSEXW {
            cbSize: core::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(wnd_proc),
            hInstance: HINSTANCE(module.0),
            hCursor: cursor,
            lpszClassName: class_name(),
            ..Default::default()
        };
        RegisterClassExW(&class);
    }
}

unsafe extern "system" fn wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let app: AppPtr = unsafe { w32::user_data(hwnd) };
    match msg {
        WM_PAINT => {
            if !app.is_null() {
                unsafe { app.get() }.paint_toast(hwnd);
            } else {
                let _ = unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
            }
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        WM_TIMER => {
            if wparam.0 == IDT_TOAST {
                unsafe {
                    let _ = KillTimer(Some(hwnd), IDT_TOAST);
                    let _ = ShowWindow(hwnd, SW_HIDE);
                }
            }
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

/// Create the toast as an initially hidden window *owned by* the main
/// window (`WS_POPUP` makes the parent slot an owner).
///
/// # Safety
/// `parent` must be a live window; the stashed `app` must outlive it.
pub unsafe fn create(parent: HWND, app: AppPtr) -> HWND {
    unsafe {
        let module = GetModuleHandleW(None).expect("module handle");
        let hwnd = CreateWindowExW(
            WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            class_name(),
            w!(""),
            WS_POPUP | WS_VISIBLE,
            0,
            0,
            0,
            0,
            Some(parent),
            None,
            Some(HINSTANCE(module.0)),
            None,
        )
        .expect("toast window");
        // User data first so early messages find a valid pointer.
        w32::set_user_data(hwnd, app);
        let _ = ShowWindow(hwnd, SW_HIDE);
        hwnd
    }
}

fn dpi_of(hwnd: HWND) -> u32 {
    unsafe {
        let d = GetDpiForWindow(hwnd);
        if d == 0 {
            96
        } else {
            d
        }
    }
}

/// Width/height the pill needs for `text` (never smaller than 160×44).
fn extent(text: &str) -> (i32, i32) {
    unsafe {
        let hdc = CreateCompatibleDC(None);
        let mut r = RECT {
            left: 0,
            top: 0,
            right: 400,
            bottom: 0,
        };
        let mut buf = w32::wide_c(text);
        DrawTextW(hdc, &mut buf, &mut r, DT_CALCRECT | DT_SINGLELINE | DT_NOPREFIX);
        let _ = DeleteDC(hdc);
        (r.right.max(132) + 30, r.bottom.max(24) + 18)
    }
}

/// Show `text` for a few seconds at the bottom-left of the content area.
pub fn show(hwnd: HWND, text: &str) {
    if hwnd.0.is_null() {
        return;
    }
    unsafe {
        let _ = SetWindowTextW(hwnd, &HSTRING::from(text));
        let (w, h) = extent(text);
        let dpi = dpi_of(hwnd);
        let parent = GetParent(hwnd).unwrap_or(hwnd);
        let (_, ph) = w32::client_size(parent);
        let x = w32::scale(16, dpi);
        let y = (ph - h - w32::scale(18, dpi)).max(w32::scale(8, dpi));
        // Parent client coordinates for a top-level window become screen
        // coordinates (a child window would take them as-is).
        let mut pt = POINT { x, y };
        let _ = ClientToScreen(parent, &mut pt);
        let _ = SetWindowPos(
            hwnd,
            Some(HWND_TOP),
            pt.x,
            pt.y,
            w,
            h,
            SWP_NOZORDER | SWP_NOACTIVATE | SWP_NOSIZE,
        );
        let _ = ShowWindow(hwnd, SW_SHOWNA);
        let _ = SetTimer(Some(hwnd), IDT_TOAST, TTL_MS, None);
        let _ = windows::Win32::Graphics::Gdi::InvalidateRect(Some(hwnd), None, true);
    }
}

/// Hide the toast immediately.
pub fn hide(hwnd: HWND) {
    if hwnd.0.is_null() {
        return;
    }
    unsafe {
        let _ = KillTimer(Some(hwnd), IDT_TOAST);
        let _ = ShowWindow(hwnd, SW_HIDE);
    }
}
