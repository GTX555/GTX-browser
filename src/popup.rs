//! One drop-down panel under the chrome, shared by the address-bar
//! suggestions and the downloads list.
//!
//! It is an *owned top-level* window (menu style: `WS_POPUP` +
//! `WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE`) — not a child window. WebView2
//! renders its DirectComposition content above every sibling GDI child
//! regardless of z-order, so a child panel would be composited underneath
//! the page it is supposed to cover; an owned window always stacks above
//! its owner (the main window) and therefore above the web views too.
//!
//! It paints itself from the app's state and never takes keyboard focus:
//! the address `EDIT` keeps it while the user types, so ↓/↑/Enter keep
//! working and clicking a row does not interrupt them. Showing it re-asserts
//! its place just above the owner without activating anything
//! (`SWP_NOACTIVATE` + `WM_MOUSEACTIVATE`).

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, POINT, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::Graphics::Gdi::{ClientToScreen, InvalidateRect, UpdateWindow};
use windows::Win32::UI::Controls::WM_MOUSELEAVE;
use windows::Win32::UI::Input::KeyboardAndMouse::{TrackMouseEvent, TRACKMOUSEEVENT, TME_LEAVE};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, GetAncestor, IsWindowVisible, LoadCursorW, RegisterClassExW,
    SetWindowPos, ShowWindow, GA_ROOTOWNER, IDC_ARROW, MA_NOACTIVATE, SW_HIDE,
    SWP_NOACTIVATE, SWP_SHOWWINDOW, SW_SHOWNOACTIVATE, HWND_TOP,
    WM_ERASEBKGND, WM_LBUTTONDOWN, WM_MOUSEACTIVATE, WM_MOUSEMOVE, WM_PAINT, WNDCLASSEXW,
    WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_POPUP,
};

use crate::w32::{self, AppPtr};

pub fn class_name() -> PCWSTR {
    w!("GtxPopup")
}

/// Register the popup class. Called once before any window is created.
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
    // Mouse coordinates arrive sign-extended in the low words.
    let local = || {
        (
            w32::signed_lo16(lparam.0),
            (lparam.0 >> 16) as u16 as i16 as i32,
        )
    };
    match msg {
        WM_PAINT => {
            if !app.is_null() {
                unsafe { app.get() }.paint_popup(hwnd);
            } else {
                let _ = unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
            }
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        // Clicking a row must not move focus out of the address bar.
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        WM_MOUSEMOVE => {
            if !app.is_null() {
                unsafe {
                    let _ = TrackMouseEvent(&mut TRACKMOUSEEVENT {
                        cbSize: core::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                        dwFlags: TME_LEAVE,
                        hwndTrack: hwnd,
                        dwHoverTime: 0,
                    });
                }
                let (x, y) = local();
                unsafe { app.get() }.popup_hover(x, y);
            }
            LRESULT(0)
        }
        WM_MOUSELEAVE => {
            if !app.is_null() {
                unsafe { app.get() }.popup_leave();
            }
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            let (x, y) = local();
            if !app.is_null() {
                unsafe { app.get() }.popup_click(x, y);
            }
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

/// Create the popup as an initially hidden window *owned by* the main
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
            WS_POPUP,
            0,
            0,
            0,
            0,
            Some(parent),
            None,
            Some(HINSTANCE(module.0)),
            None,
        )
        .expect("popup window");
        w32::set_user_data(hwnd, app);
        hwnd
    }
}

/// Show the panel anchored at `x, y` in the main window's client area,
/// sized `w × h`, stacked directly above its owner (so above the web views)
/// but without taking focus.
pub fn show(hwnd: HWND, x: i32, y: i32, w: i32, h: i32) {
    if hwnd.0.is_null() {
        return;
    }
    unsafe {
        // The caller works in the main window's client coordinates; a
        // top-level window is positioned in screen coordinates.
        let owner = GetAncestor(hwnd, GA_ROOTOWNER);
        let mut pt = POINT { x, y };
        if !owner.0.is_null() {
            let _ = ClientToScreen(owner, &mut pt);
        }
        let _ = SetWindowPos(
            hwnd,
            // `Some(owner)` would place the panel *below* its owner (the
            // owner precedes it in z); the panel must be on top while open.
            // The caller hides it whenever the app is deactivated, so it
            // never floats above other applications.
            Some(HWND_TOP),
            pt.x,
            pt.y,
            w,
            h,
            SWP_NOACTIVATE | SWP_SHOWWINDOW,
        );
        // Both ways of showing, then a synchronous repaint: a window that has
        // never been shown through ShowWindow can end up unpresented.
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        let _ = InvalidateRect(Some(hwnd), None, true);
        let _ = UpdateWindow(hwnd);
    }
}

/// Hide the panel (focus is wherever it already was).
pub fn hide(hwnd: HWND) {
    if hwnd.0.is_null() {
        return;
    }
    unsafe {
        let _ = ShowWindow(hwnd, SW_HIDE);
    }
}

pub fn is_open(hwnd: HWND) -> bool {
    !hwnd.0.is_null() && unsafe { IsWindowVisible(hwnd) }.as_bool()
}
