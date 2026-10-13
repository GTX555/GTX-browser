//! Win32 plumbing shared across the app: raw-window-handle bridge, the
//! copyable pointer to app state used by window procs, and small GDI/DPI/theme
//! helpers.

use core::num::NonZeroIsize;

use windows::core::{PCWSTR, w};
use windows::Win32::Foundation::{COLORREF, ERROR_SUCCESS, HWND, RECT};
use windows::Win32::Graphics::Dwm::{
    DwmSetWindowAttribute, DWMWA_USE_IMMERSIVE_DARK_MODE, DWMWA_WINDOW_CORNER_PREFERENCE,
    DWMWCP_ROUND,
};
use windows::Win32::Graphics::Gdi::{
    CreateFontIndirectW, CreateSolidBrush, DeleteObject, GetDC, GetTextMetricsW, ReleaseDC,
    SelectObject, TEXTMETRICW, FONT_CHARSET, FONT_CLIP_PRECISION, FONT_OUTPUT_PRECISION,
    FONT_QUALITY, HFONT, HGDIOBJ, LOGFONTW,
};
use windows::Win32::System::Registry::{
    RegCloseKey, RegOpenKeyW, RegQueryValueExW, HKEY_CURRENT_USER,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetClientRect, GetSystemMetrics, GetWindowLongPtrW, SetClassLongPtrW, SetWindowLongPtrW,
    GCLP_HBRBACKGROUND, GWLP_USERDATA, SM_CXDRAG, SM_CXSIZEFRAME, SM_CXPADDEDBORDER, SM_CYDRAG,
};

use crate::app::App;

/// Copyable pointer to the app state.
///
/// Every window proc and wry callback runs on the main thread, so a raw
/// pointer is the standard way to reach the app from Win32 callbacks.
#[derive(Clone, Copy)]
pub struct AppPtr(pub *mut App);

impl AppPtr {
    pub const fn null() -> Self {
        AppPtr(core::ptr::null_mut())
    }

    pub fn is_null(self) -> bool {
        self.0.is_null()
    }

    /// # Safety
    /// `self` must point at a live `App`, and the caller must be on the main
    /// thread (which every window proc is).
    pub unsafe fn get(self) -> &'static mut App {
        unsafe { &mut *self.0 }
    }
}

/// Read the `AppPtr` stashed in a window's `GWLP_USERDATA`.
///
/// # Safety
/// `hwnd` must be one of GTX's windows (or have no user data yet).
pub unsafe fn user_data(hwnd: HWND) -> AppPtr {
    let raw = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) };
    AppPtr(raw as *mut App)
}

/// Stash an `AppPtr` in a window's `GWLP_USERDATA`.
///
/// # Safety
/// `hwnd` must be one of GTX's windows; `ptr` must stay valid for their life.
pub unsafe fn set_user_data(hwnd: HWND, ptr: AppPtr) {
    unsafe {
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, ptr.0 as isize);
    }
}

/// Minimal `HasWindowHandle` wrapper around an `HWND` for wry.
#[derive(Clone, Copy)]
pub struct WinHandle(pub HWND);

impl wry::raw_window_handle::HasWindowHandle for WinHandle {
    fn window_handle(
        &self,
    ) -> Result<wry::raw_window_handle::WindowHandle<'_>, wry::raw_window_handle::HandleError> {
        let hwnd = NonZeroIsize::new(self.0.0 as isize)
            .ok_or(wry::raw_window_handle::HandleError::Unavailable)?;
        let handle = wry::raw_window_handle::Win32WindowHandle::new(hwnd);
        Ok(unsafe {
            wry::raw_window_handle::WindowHandle::borrow_raw(
                wry::raw_window_handle::RawWindowHandle::Win32(handle),
            )
        })
    }
}

/// A rectangle in client coordinates (`x`, `y`, `w`, `h`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct R {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl R {
    pub const fn new(x: i32, y: i32, w: i32, h: i32) -> Self {
        R { x, y, w, h }
    }

    pub fn contains(self, x: i32, y: i32) -> bool {
        x >= self.x && x < self.x + self.w && y >= self.y && y < self.y + self.h
    }

    pub fn right(self) -> i32 {
        self.x + self.w
    }

    pub fn bottom(self) -> i32 {
        self.y + self.h
    }

}

/// Scale a 96-dpi design unit to the given DPI.
pub fn scale(v: i32, dpi: u32) -> i32 {
    ((v as i64 * dpi as i64 + 48) / 96) as i32
}

/// `MulDiv`: round(n * d / m).
pub fn mul_div(n: i32, d: i32, m: i32) -> i32 {
    ((n as i64 * d as i64 + m as i64 / 2) / m as i64) as i32
}

/// Client size in physical pixels.
pub fn client_size(hwnd: HWND) -> (i32, i32) {
    let mut r = RECT::default();
    unsafe {
        if GetClientRect(hwnd, &mut r).is_err() {
            return (0, 0);
        }
    }
    (r.right - r.left, r.bottom - r.top)
}

/// Build a `LOGFONTW`-backed UI font at the given DPI.
pub fn create_font(dpi: u32, face: &str, point_size: i32, weight: i32) -> HFONT {
    let mut lf = LOGFONTW::default();
    lf.lfHeight = -mul_div(point_size, dpi as i32, 72);
    lf.lfWeight = weight;
    lf.lfCharSet = FONT_CHARSET(1); // DEFAULT_CHARSET
    lf.lfOutPrecision = FONT_OUTPUT_PRECISION(0);
    lf.lfClipPrecision = FONT_CLIP_PRECISION(0);
    lf.lfQuality = FONT_QUALITY(5); // CLEARTYPE_QUALITY
    lf.lfPitchAndFamily = 0;
    let mut wide: Vec<u16> = face.encode_utf16().collect();
    wide.truncate(lf.lfFaceName.len() - 1);
    let n = wide.len();
    lf.lfFaceName[..n].copy_from_slice(&wide);
    lf.lfFaceName[n] = 0;
    unsafe { CreateFontIndirectW(&lf) }
}

pub fn delete_font(font: HFONT) {
    if !font.0.is_null() {
        unsafe {
            let _ = DeleteObject(HGDIOBJ(font.0));
        }
    }
}

/// `tmHeight` of a font (its full line box) on a screen DC, or 0.
///
/// A single-line `EDIT` draws its text flush to the top of its client area —
/// knowing the line height is what lets the omnibox centre that line inside
/// the pill instead of letting it hug the top edge.
pub fn font_line_height(font: HFONT) -> i32 {
    if font.0.is_null() {
        return 0;
    }
    unsafe {
        let hdc = GetDC(None);
        if hdc.is_invalid() {
            return 0;
        }
        let prev = SelectObject(hdc, HGDIOBJ(font.0));
        let mut tm = TEXTMETRICW::default();
        let h = if GetTextMetricsW(hdc, &mut tm).as_bool() {
            tm.tmHeight
        } else {
            0
        };
        SelectObject(hdc, prev);
        let _ = ReleaseDC(None, hdc);
        h
    }
}

/// Convenience: UTF-16 buffer for `GetWindowTextW`.
pub fn window_text(hwnd: HWND) -> String {
    let mut buf = vec![0u16; 2048];
    let n = unsafe { windows::Win32::UI::WindowsAndMessaging::GetWindowTextW(hwnd, &mut buf) };
    String::from_utf16_lossy(&buf[..n.max(0) as usize])
}

/// Low/high word of a `WPARAM` (control id + notification code).
pub fn lo_word(v: usize) -> u16 {
    (v & 0xFFFF) as u16
}

pub fn hi_word(v: usize) -> u16 {
    ((v >> 16) & 0xFFFF) as u16
}

/// Sign-extend the low 16 bits of an `LPARAM` (mouse coordinates).
pub fn signed_lo16(v: isize) -> i32 {
    (v & 0xFFFF) as u16 as i16 as i32
}

/// Mouse position from an `LPARAM`, in screen pixels.
pub fn screen_point(lparam: isize) -> (i32, i32) {
    (
        (lparam & 0xFFFF) as u16 as i16 as i32,
        ((lparam >> 16) & 0xFFFF) as u16 as i16 as i32,
    )
}

/// Encode a string as a null-terminated wide string for `DrawTextW`.
pub fn wide_c(s: &str) -> Vec<u16> {
    let mut v: Vec<u16> = s.encode_utf16().collect();
    v.push(0);
    v
}

/// Main window class name.
pub fn main_class() -> PCWSTR {
    w!("GtxBrowserWindow")
}

// ---------------------------------------------------------------------------
// Frameless window support
// ---------------------------------------------------------------------------

/// Thickness (in screen pixels) of the invisible resize band drawn around a
/// borderless window. Matches the metrics Windows uses for a resizable frame.
pub fn frame_band() -> i32 {
    let (w, h) = unsafe {
        (
            GetSystemMetrics(SM_CXSIZEFRAME),
            GetSystemMetrics(SM_CXPADDEDBORDER),
        )
    };
    (w + h).max(4)
}

/// Drag threshold in pixels.
pub fn drag_threshold() -> i32 {
    unsafe { GetSystemMetrics(SM_CXDRAG).max(GetSystemMetrics(SM_CYDRAG)) }.max(4)
}

/// `HTCLIENT` — the ordinary "this is my content area" hit code.
pub const HTCLIENT: u32 = 1;

/// Hit-test a screen point against `rect` (screen coordinates).
///
/// Returns a `HT*` code: one of the eight edge codes when `pt` is inside the
/// `band`-wide border, otherwise `HTCLIENT`.
pub fn edge_hit_test(rect: RECT, pt: (i32, i32), band: i32) -> u32 {
    const HTLEFT: u32 = 10;
    const HTRIGHT: u32 = 11;
    const HTTOP: u32 = 12;
    const HTTOPLEFT: u32 = 13;
    const HTTOPRIGHT: u32 = 14;
    const HTBOTTOM: u32 = 15;
    const HTBOTTOMLEFT: u32 = 16;
    const HTBOTTOMRIGHT: u32 = 17;

    let (x, y) = pt;
    if x < rect.left || x > rect.right || y < rect.top || y > rect.bottom {
        return HTCLIENT;
    }
    let left = x - rect.left <= band;
    let right = rect.right - x <= band;
    let top = y - rect.top <= band;
    let bottom = rect.bottom - y <= band;
    match (top, bottom, left, right) {
        (true, _, true, _) => HTTOPLEFT,
        (true, _, _, true) => HTTOPRIGHT,
        (true, _, _, _) => HTTOP,
        (_, true, true, _) => HTBOTTOMLEFT,
        (_, true, _, true) => HTBOTTOMRIGHT,
        (_, true, _, _) => HTBOTTOM,
        (_, _, true, _) => HTLEFT,
        (_, _, _, true) => HTRIGHT,
        _ => HTCLIENT,
    }
}

/// Rounded corners + immersive dark title/frame colour. Best effort: the
/// attributes simply do not exist before Windows 11 / 20H1.
pub fn apply_window_effects(hwnd: HWND, dark: bool) {
    unsafe {
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_WINDOW_CORNER_PREFERENCE,
            &DWMWCP_ROUND as *const _ as *const core::ffi::c_void,
            core::mem::size_of::<i32>() as u32,
        );
        let dark_val: i32 = dark.into();
        let ptr = &dark_val as *const i32 as *const core::ffi::c_void;
        if DwmSetWindowAttribute(
            hwnd,
            DWMWA_USE_IMMERSIVE_DARK_MODE,
            ptr,
            core::mem::size_of::<i32>() as u32,
        )
        .is_err()
        {
            // Pre-20H1 builds used attribute 19.
            let _ = DwmSetWindowAttribute(
                hwnd,
                windows::Win32::Graphics::Dwm::DWMWINDOWATTRIBUTE(19),
                ptr,
                core::mem::size_of::<i32>() as u32,
            );
        }
    }
}

/// Replace the window class background brush so the exposed frame band around
/// the page matches the chrome colour.
///
/// `SetClassLongPtrW` needs a live window of the class — the main window.
pub fn set_class_background_for(hwnd: HWND, color: COLORREF) {
    unsafe {
        let brush = CreateSolidBrush(color);
        if brush.is_invalid() {
            return;
        }
        let old = SetClassLongPtrW(hwnd, GCLP_HBRBACKGROUND, brush.0 as isize);
        if old != 0 {
            let _ = DeleteObject(HGDIOBJ(old as *mut core::ffi::c_void));
        }
    }
}

// ---------------------------------------------------------------------------
// Theme detection
// ---------------------------------------------------------------------------

/// Windows' app light/dark preference (`AppsUseLightTheme`, 0 = dark).
///
/// Read from the registry instead of the undocumented `uxtheme` ordinals so it
/// keeps working across Windows builds.
pub fn apps_dark() -> bool {
    use windows::core::HSTRING;
    unsafe {
        let sub = HSTRING::from(
            r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize",
        );
        let mut key = windows::Win32::System::Registry::HKEY(core::ptr::null_mut());
        if RegOpenKeyW(HKEY_CURRENT_USER, &sub, &mut key) != ERROR_SUCCESS {
            return false;
        }
        let name = HSTRING::from("AppsUseLightTheme");
        let mut ty = Default::default();
        let mut data = 0u32;
        let mut len = core::mem::size_of::<u32>() as u32;
        let err = RegQueryValueExW(
            key,
            &name,
            None,
            Some(&mut ty),
            Some(&mut data as *mut u32 as *mut u8),
            Some(&mut len),
        );
        let _ = RegCloseKey(key);
        if err != ERROR_SUCCESS {
            return false;
        }
        data == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const fn r(left: i32, top: i32, right: i32, bottom: i32) -> RECT {
        RECT { left, top, right, bottom }
    }

    #[test]
    fn edge_hit_test_finds_the_resize_band() {
        let win = r(100, 100, 900, 700);
        // Deep inside → client.
        assert_eq!(edge_hit_test(win, (500, 400), 8), HTCLIENT);
        // Top edge and its two corners.
        assert_eq!(edge_hit_test(win, (500, 104), 8), 12); // HTTOP
        assert_eq!(edge_hit_test(win, (104, 104), 8), 13); // HTTOPLEFT
        assert_eq!(edge_hit_test(win, (896, 104), 8), 14); // HTTOPRIGHT
        // Sides and bottom.
        assert_eq!(edge_hit_test(win, (102, 400), 8), 10); // HTLEFT
        assert_eq!(edge_hit_test(win, (898, 400), 8), 11); // HTRIGHT
        assert_eq!(edge_hit_test(win, (500, 697), 8), 15); // HTBOTTOM
        assert_eq!(edge_hit_test(win, (104, 697), 8), 16); // HTBOTTOMLEFT
        assert_eq!(edge_hit_test(win, (896, 697), 8), 17); // HTBOTTOMRIGHT
    }

    #[test]
    fn edge_hit_test_ignores_points_outside() {
        assert_eq!(edge_hit_test(r(0, 0, 100, 100), (-5, 50), 8), HTCLIENT);
        assert_eq!(edge_hit_test(r(0, 0, 100, 100), (50, 101), 8), HTCLIENT);
    }

    #[test]
    fn scaling_is_stable_at_common_dpis() {
        assert_eq!(scale(44, 96), 44);
        assert_eq!(scale(44, 144), 66);
        assert_eq!(scale(44, 192), 88);
        assert_eq!(mul_div(9, 192, 72), 24);
    }

    #[test]
    fn rect_helpers() {
        let q = R::new(10, 20, 30, 40);
        assert!(q.contains(10, 20));
        assert!(q.contains(39, 59));
        assert!(!q.contains(40, 20));
        assert!(!q.contains(10, 60));
        assert_eq!(q.right(), 40);
        assert_eq!(q.bottom(), 60);
    }
}
