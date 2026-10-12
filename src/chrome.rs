//! The single-row browser chrome: navigation buttons, the rounded omnibox,
//! tab pills, the new-tab `+`, the kebab menu and the window caption buttons —
//! all painted in one child window that sits at the very top of a frameless
//! main window.
//!
//! The address bar remains a real Win32 `EDIT` (so IME, selection and the
//! clipboard keep working) but it is parented to this window and drawn inside
//! the painted pill: `WM_CTLCOLOREDIT` gives it the pill's colours, and the
//! pill's rounded corners stay visible around it.
//!
//! Everything geometric lives in [`crate::app`] (which owns the config, the
//! tabs and the DPI); this module only owns the window and its message flow.

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Gdi::{SetBkColor, SetTextColor, HDC};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::WM_MOUSELEAVE;
use windows::Win32::UI::Input::KeyboardAndMouse::{TrackMouseEvent, TRACKMOUSEEVENT, TME_LEAVE};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, LoadCursorW, RegisterClassExW, CS_DBLCLKS, IDC_ARROW,
    MA_NOACTIVATE, WM_COMMAND, WM_CTLCOLOREDIT, WM_ERASEBKGND, WM_LBUTTONDBLCLK, WM_LBUTTONDOWN,
    WM_LBUTTONUP, WM_MBUTTONUP, WM_MOUSEACTIVATE, WM_MOUSEMOVE, WM_NCHITTEST, WM_PAINT,
    WM_SETCURSOR, WNDCLASSEXW, WS_CHILD, WS_CLIPCHILDREN, WS_CLIPSIBLINGS, WS_VISIBLE,
};

use crate::w32::{self, AppPtr};

pub fn class_name() -> PCWSTR {
    w!("GtxChrome")
}

/// Register the chrome window class. Called once before window creation.
pub fn register() {
    unsafe {
        let module = GetModuleHandleW(None).expect("module handle");
        let cursor = LoadCursorW(None, IDC_ARROW).expect("cursor");
        let class = WNDCLASSEXW {
            cbSize: core::mem::size_of::<WNDCLASSEXW>() as u32,
            style: CS_DBLCLKS,
            lpfnWndProc: Some(wnd_proc),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: HINSTANCE(module.0),
            hIcon: Default::default(),
            hCursor: cursor,
            hbrBackground: Default::default(),
            lpszMenuName: PCWSTR::null(),
            lpszClassName: class_name(),
            hIconSm: Default::default(),
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

    // Mouse coordinates arrive sign-extended in the low word of lParam —
    // except for WM_NCHITTEST, where they are screen coordinates.
    let local = || {
        (
            w32::signed_lo16(lparam.0),
            (lparam.0 >> 16) as u16 as i16 as i32,
        )
    };

    match msg {
        WM_PAINT => {
            if !app.is_null() {
                unsafe { app.get() }.paint_chrome(hwnd);
            } else {
                let _ = unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
            }
            LRESULT(0)
        }
        // Everything is painted in one buffered pass — never erase underneath.
        WM_ERASEBKGND => LRESULT(1),
        // The chrome never takes keyboard focus; the page keeps it.
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
                    app.get().chrome_hover(w32::signed_lo16(lparam.0), (lparam.0 >> 16) as u16 as i16 as i32);
                }
            }
            LRESULT(0)
        }
        WM_MOUSELEAVE => {
            if !app.is_null() {
                unsafe { app.get() }.chrome_leave();
            }
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            let (x, y) = local();
            if !app.is_null() {
                unsafe { app.get() }.chrome_down(x, y);
            }
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            let (x, y) = local();
            if !app.is_null() {
                unsafe { app.get() }.chrome_up(x, y);
            }
            LRESULT(0)
        }
        WM_LBUTTONDBLCLK => {
            let (x, y) = local();
            if !app.is_null() {
                unsafe { app.get() }.chrome_dbl(x, y);
            }
            LRESULT(0)
        }
        WM_MBUTTONUP => {
            let (x, y) = local();
            if !app.is_null() {
                unsafe { app.get() }.chrome_middle(x, y);
            }
            LRESULT(0)
        }
        // Resize borders: the chrome spans the full width at the top of a
        // frameless window, so it has to forward the border band itself.
        WM_NCHITTEST => {
            if app.is_null() {
                return LRESULT(w32::HTCLIENT as isize);
            }
            let code = unsafe { app.get() }.chrome_hit_code(hwnd, w32::screen_point(lparam.0));
            LRESULT(code as isize)
        }
        // The address edit asks its parent for colours — give it the pill's.
        // The brush is a real one (not NULL): the edit then paints a rectangle
        // of the pill colour *inside* the rounded pill, whose corners stay
        // visible around it, with no repaint artefacts.
        WM_CTLCOLOREDIT => {
            if app.is_null() {
                return LRESULT(0);
            }
            let hdc = HDC(wparam.0 as *mut core::ffi::c_void);
            let (text, back, brush) = unsafe { app.get() }.address_colors();
            unsafe {
                let _ = SetTextColor(hdc, text);
                let _ = SetBkColor(hdc, back);
            }
            LRESULT(brush.0 as isize)
        }
        // Address bar notifications (EN_SETFOCUS / EN_KILLFOCUS / EN_CHANGE).
        WM_COMMAND => {
            if !app.is_null() {
                let id = w32::lo_word(wparam.0);
                let notify = w32::hi_word(wparam.0);
                unsafe { app.get() }.on_command(id, notify);
            }
            LRESULT(0)
        }
        WM_SETCURSOR => {
            let _ = unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
            LRESULT(1)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

/// Create the chrome as a child of the main window and store the app pointer.
///
/// # Safety
/// `parent` must be a live window; the stashed `app` must outlive it.
pub unsafe fn create(parent: HWND, app: AppPtr, x: i32, y: i32, w: i32, h: i32) -> HWND {
    unsafe {
        let module = GetModuleHandleW(None).expect("module handle");
        let hwnd = CreateWindowExW(
            Default::default(),
            class_name(),
            w!(""),
            WS_CHILD | WS_VISIBLE | WS_CLIPCHILDREN | WS_CLIPSIBLINGS,
            x,
            y,
            w,
            h,
            Some(parent),
            None,
            Some(HINSTANCE(module.0)),
            None,
        )
        .expect("chrome window");
        // User data first so early messages find a valid pointer.
        w32::set_user_data(hwnd, app);
        hwnd
    }
}
