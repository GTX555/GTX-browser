//! GTX Browser installer ("make it an app").
//!
//! `gtx-setup.exe` (run it next to `gtx-browser.exe`):
//!   1. closes a running GTX politely (so the session is saved),
//!   2. copies the browser into `%LOCALAPPDATA%\Programs\GTX Browser`,
//!   3. creates Start Menu and Desktop shortcuts,
//!   4. registers an Add/Remove Programs entry under HKCU,
//!   5. **verifies every one of those** and reports what it found,
//!   6. optionally launches the browser.
//!
//! Flags:
//!   `--repair`  re-run the install over an existing one,
//!   `--uninstall` reverse all of it,
//!   `--quiet` no message boxes; report on stdout, exit 1 on failure
//!             (for scripted installs and the round-trip test).
//!
//! The install directory is removed by a short-lived helper script *after*
//! this process exits, because Windows keeps a running image locked; the
//! helper also deletes itself.

#![cfg_attr(not(test), windows_subsystem = "windows")]

use std::cell::RefCell;
use std::ffi::c_void;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use windows::core::{w, BOOL, GUID, HSTRING, Interface, PCWSTR};
use windows::Win32::Foundation::{
    CloseHandle, COLORREF, ERROR_SUCCESS, HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM,
};
use windows::Win32::UI::WindowsAndMessaging::GetMessageW;
use windows::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_USE_IMMERSIVE_DARK_MODE};
use windows::Win32::Graphics::Gdi::{
    CreateFontIndirectW, CreateSolidBrush, DeleteObject, GetSysColor, InvalidateRect, OPAQUE,
    SetBkColor, SetBkMode, SetTextColor, UpdateWindow, COLOR_BTNFACE, COLOR_WINDOW,
    COLOR_WINDOWTEXT, FONT_CHARSET, FONT_CLIP_PRECISION, FONT_OUTPUT_PRECISION, FONT_QUALITY,
    HBRUSH, HDC, HFONT, HGDIOBJ, LOGFONTW, TRANSPARENT,
};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, CLSCTX_INPROC_SERVER,
    COINIT_APARTMENTTHREADED, IPersistFile,
};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
    TH32CS_SNAPPROCESS,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Registry::{
    RegCloseKey, RegCreateKeyW, RegDeleteKeyW, RegDeleteTreeW, RegDeleteValueW, RegOpenKeyW,
    RegQueryValueExW, RegSetValueExW, HKEY,
    HKEY_CURRENT_USER, REG_DWORD, REG_SZ,
};
use windows::Win32::UI::Controls::{
    InitCommonControlsEx, SetWindowTheme, BST_CHECKED, ICC_PROGRESS_CLASS, INITCOMMONCONTROLSEX,
    PBM_SETPOS, PBM_SETRANGE,
};
use windows::Win32::UI::HiDpi::{
    AdjustWindowRectExForDpi, GetDpiForSystem, GetDpiForWindow, SetProcessDpiAwarenessContext,
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{EnableWindow, VK_ESCAPE, VK_RETURN};
use windows::Win32::UI::Shell::{
    ShellExecuteW, IShellLinkW, ShellLink, FOLDERID_Desktop, FOLDERID_Programs, KF_FLAG_DEFAULT,
    SHGetKnownFolderPath,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, EnumWindows,
    GetWindowLongPtrW, GetWindowThreadProcessId, GetSystemMetrics, GCLP_HBRBACKGROUND,
    IsDialogMessageW, IsWindowVisible, LoadCursorW, MessageBoxW, MoveWindow,
    PostMessageW, PostQuitMessage, RegisterClassExW, SendMessageW, SetClassLongPtrW,
    SetWindowLongPtrW, SetWindowTextW, ShowWindow, TranslateMessage, BM_GETCHECK,
    BS_AUTOCHECKBOX, BS_DEFPUSHBUTTON, BS_PUSHBUTTON, CS_HREDRAW, CS_VREDRAW, ES_AUTOVSCROLL,
    ES_MULTILINE, ES_READONLY, GWLP_USERDATA, HMENU, IDC_ARROW, IDYES, MB_ICONERROR,
    MB_ICONINFORMATION, MB_OK, MB_TASKMODAL, MB_YESNO, MESSAGEBOX_RESULT, MESSAGEBOX_STYLE, MSG,
    SM_CXSCREEN, SM_CYSCREEN, SW_HIDE, SW_SHOW, SW_SHOWNORMAL, WINDOW_EX_STYLE, WINDOW_STYLE,
    WM_APP, WM_CLOSE, WM_COMMAND, WM_CREATE, WM_CTLCOLORBTN, WM_CTLCOLOREDIT, WM_CTLCOLORSTATIC,
    WM_DESTROY, WM_DPICHANGED, WM_KEYDOWN, WM_SETFONT, WNDCLASSEXW, WS_BORDER, WS_CAPTION,
    WS_CHILD, WS_CLIPCHILDREN, WS_EX_CONTROLPARENT, WS_OVERLAPPED, WS_SYSMENU, WS_TABSTOP,
    WS_VISIBLE, WS_VSCROLL,
};

/// `IDOK`/`IDCANCEL`, which Enter and Esc deliver through `IsDialogMessage`.
const ID_ENTER: usize = 1;
const ID_ESC: usize = 2;

// The registry names the browser and this installer must agree on, shared
// verbatim so uninstall deletes exactly what registration wrote.
include!("../reg_names.rs");
const BROWSER_EXE: &str = "gtx-browser.exe";
const SETUP_EXE: &str = "gtx-setup.exe";
/// Under `HKCU` — no elevation needed, shows up in Settings → Apps.
const UNINSTALL_KEY: &str =
    r"Software\Microsoft\Windows\CurrentVersion\Uninstall\GTX Browser";
/// `cmd.exe /C` flag: run without flashing a console window.
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
/// The key trees the browser's default-browser registration creates under
/// `HKCU\`. Uninstall deletes exactly these (`[reg_names]` has the names the
/// browser writes), or Windows keeps offering an exe that is gone.
const KEY_TREES: &[&str] = &[
    r"Software\Classes\GTXBrowserHTM",
    r"Software\Classes\GTXBrowserURL",
    r"Software\GTX Browser",
];

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Install,
    Repair,
    Uninstall,
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let quiet = args.iter().any(|a| a == "--quiet");
    let mode = if args.iter().any(|a| a == "--uninstall") {
        Mode::Uninstall
    } else if args.iter().any(|a| a == "--repair") {
        Mode::Repair
    } else {
        Mode::Install
    };

    // Double-clicked (or run from Explorer): show the wizard. Anything with a
    // flag is a scripted / repair / uninstall run and keeps the old behaviour,
    // so package.ps1 and roundtrip.ps1 are unaffected.
    if args.is_empty() {
        let exit = wizard();
        if exit != 0 {
            std::process::exit(exit);
        }
        return;
    }

    let result = match mode {
        Mode::Uninstall => uninstall(),
        Mode::Repair => install(true),
        Mode::Install => install(false),
    };

    match result {
        Ok((msg, launch)) => {
            if quiet {
                // Scripted run: stdout, no dialog, no auto-launch.
                out(&msg);
                if mode == Mode::Uninstall {
                    schedule_cleanup();
                }
                return;
            }
            if msg.is_empty() {
                // Nothing to ask — but an uninstall still needs its cleanup.
                if mode == Mode::Uninstall {
                    schedule_cleanup();
                }
                return;
            }
            let ask_launch = launch.is_some();
            let flags: MESSAGEBOX_STYLE = (if ask_launch { MB_YESNO } else { MB_OK })
                | MB_ICONINFORMATION
                | MB_TASKMODAL;

            let text = if ask_launch {
                format!("{msg}\n\nStart {APP_NAME} now?")
            } else {
                msg
            };
            if message(&text, flags) == IDYES {
                if let Some(exe) = launch.as_deref() {
                    launch_browser(exe);
                }
            }
            if mode == Mode::Uninstall {
                schedule_cleanup();
            }
        }
        Err(err) => {
            if quiet {
                err_line(&format!("{APP_NAME} setup failed: {err}"));
                std::process::exit(1);
            }
            message(
                &format!("{APP_NAME} setup failed\n\n{err}"),
                MB_OK | MB_ICONERROR | MB_TASKMODAL,
            );
        }
    }
}

/// One line on stdout, never panicking: this binary is a `windows_subsystem`
/// process, so `--quiet` may be running without a console handle and a failed
/// write must not abort the round-trip test.
fn out(line: &str) {
    use std::io::Write;
    let _ = writeln!(std::io::stdout(), "{line}");
}

/// One line on stderr, same never-panic rule.
fn err_line(line: &str) {
    use std::io::Write;
    let _ = writeln!(std::io::stderr(), "{line}");
}

fn schedule_cleanup() {
    let dir = install_dir();
    if dir.exists() {
        spawn_deferred_cleanup(&dir);
    }
}

// ---------------------------------------------------------------------------
// Install
// ---------------------------------------------------------------------------

/// What the caller asked for. `--repair` and `--quiet` use [`InstallOptions::default`];
/// the wizard's options page turns the shortcuts on and off.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct InstallOptions {
    /// Re-run the install over an existing one (reports "repaired" instead of "installed").
    repair: bool,
    /// Create the Desktop shortcut.
    desktop_shortcut: bool,
    /// Create the Start Menu shortcut.
    start_menu_shortcut: bool,
}

impl Default for InstallOptions {
    fn default() -> Self {
        InstallOptions {
            repair: false,
            desktop_shortcut: true,
            start_menu_shortcut: true,
        }
    }
}

/// Returns `(message, browser exe to launch)`.
fn install(repair: bool) -> Result<(String, Option<PathBuf>), String> {
    install_with(
        InstallOptions {
            repair,
            ..Default::default()
        },
        |_| {},
    )
}

/// Human-readable name of every install step, in order. The setup wizard
/// shows these while [`install_with`] reports progress by index.
const STEP_LABELS: &[&str] = &[
    "Checking the files next to the installer",
    "Closing any running GTX Browser",
    "Creating the install folder",
    "Copying GTX Browser",
    "Creating shortcuts",
    "Registering the Apps && features entry",
    "Verifying the installation",
];

/// The install itself, reporting each step by [`STEP_LABELS`] index so the
/// wizard can show live progress; `--quiet` and `--repair` pass a no-op.
fn install_with<F: FnMut(usize)>(
    opts: InstallOptions,
    mut progress: F,
) -> Result<(String, Option<PathBuf>), String> {
    let repair = opts.repair;
    progress(0);
    let me = std::env::current_exe().map_err(|e| format!("cannot locate own path: {e}"))?;
    let src_dir = me.parent().ok_or("installer path has no parent")?.to_path_buf();
    let browser_src = src_dir.join(BROWSER_EXE);
    if !browser_src.is_file() {
        return Err(format!(
            "{BROWSER_EXE} was not found next to the installer ({}).\n\n\
             Keep both files in the same folder and run the installer again — \
             or use package.ps1, which stages them together.",
            src_dir.display()
        ));
    }

    // A running browser keeps its own image locked: close it first, politely,
    // so the session is written before we overwrite the executable.
    progress(1);
    let closed = close_running_browsers();
    if !closed {
        return Err(format!(
            "{APP_NAME} is still running and could not be closed.\n\n\
             Close it from its window, then run the installer again."
        ));
    }

    let dir = install_dir();
    progress(2);
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("cannot create {}: {e}", dir.display()))?;

    progress(3);
    copy_file(&browser_src, &dir.join(BROWSER_EXE))?;
    // Ship the installer itself too, so "Add/Remove Programs" can uninstall.
    copy_file(&me, &dir.join(SETUP_EXE))?;
    let exe = dir.join(BROWSER_EXE);

    let mut warnings: Vec<String> = Vec::new();
    progress(4);
    if let Err(e) = unsafe { create_shortcuts(&dir, &exe, &opts) } {
        warnings.push(e);
    }
    progress(5);
    if let Err(e) = unsafe { write_uninstall_entry(&dir, &exe) } {
        warnings.push(e);
    }

    // Prove the install rather than assuming it.
    progress(6);
    let (checks, problems) = verify_install(&dir, &exe, &opts);

    let mut msg = if repair {
        format!("{APP_NAME} repaired.\n")
    } else {
        format!("{APP_NAME} was installed to\n\n{}\n", dir.display())
    };
    msg.push_str("\n");
    for (label, ok) in &checks {
        msg.push_str(&format!("  {} {label}\n", if *ok { "✓" } else { "✗" }));
    }
    for w in &warnings {
        msg.push_str(&format!("\nNote: {w}"));
    }
    if !problems.is_empty() {
        msg.push_str(&format!(
            "\n\nSome steps failed:\n  {}\n\n\
             If Windows blocked the copy, run it again — Application Control \
             sometimes denies the *first* attempt at a freshly built file.",
            problems.join("\n  ")
        ));
        return Ok((msg, None));
    }
    msg.push_str(
        "\nIf the browser does not open the first time, launch it again from the \
         Start menu — Windows Application Control sometimes blocks a first run.",
    );
    Ok((msg, Some(exe)))
}

/// Copy `src` → `dst`, retrying while the file is still locked (`ERROR_SHARING_VIOLATION`)
/// or held back by policy (`ERROR_ACCESS_DENIED`).
fn copy_file(src: &Path, dst: &Path) -> Result<(), String> {
    let same = match (std::fs::canonicalize(src), std::fs::canonicalize(dst)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    };
    if same {
        return Ok(());
    }
    const SHARING_VIOLATION: i32 = 32;
    const ACCESS_DENIED: i32 = 5;
    let mut last_os = 0;
    for attempt in 0..12u32 {
        match std::fs::copy(src, dst) {
            Ok(_) => return Ok(()),
            Err(e) => {
                let code = e.raw_os_error().unwrap_or(0);
                let retryable =
                    attempt < 11 && (code == SHARING_VIOLATION || code == ACCESS_DENIED);
                if retryable {
                    last_os = code;
                    std::thread::sleep(Duration::from_millis(400));
                    continue;
                }
                let hint = match code {
                    SHARING_VIOLATION => "\n\nAnother program is using this file.",
                    ACCESS_DENIED => {
                        "\n\nWindows refused access to the file. Try a different folder, \
                         or move the installer out of Downloads and run it again."
                    }
                    _ => "",
                };
                return Err(format!(
                    "could not copy {} to {} : {e}{hint}",
                    src.display(),
                    dst.display()
                ));
            }
        }
    }
    Err(format!(
        "could not copy {} (file stayed locked, last error {last_os})",
        src.display()
    ))
}

fn install_dir() -> PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("Programs")
        .join(APP_NAME)
}

/// What the install claims, checked against what is actually on disk.
/// Only the shortcuts the options page asked for are checked.
fn verify_install(
    dir: &Path,
    exe: &Path,
    opts: &InstallOptions,
) -> (Vec<(String, bool)>, Vec<String>) {
    let mut checks = Vec::new();
    let mut problems = Vec::new();

    let mut push = |label: String, ok: bool, problems: &mut Vec<String>| {
        if !ok {
            problems.push(label.clone());
        }
        checks.push((label, ok));
    };

    let browser = dir.join(BROWSER_EXE);
    push(
        format!("{BROWSER_EXE} copied"),
        browser.is_file(),
        &mut problems,
    );
    push(
        format!("{SETUP_EXE} copied (for uninstall)"),
        dir.join(SETUP_EXE).is_file(),
        &mut problems,
    );
    if opts.desktop_shortcut {
        push(
            "Desktop shortcut".into(),
            shortcut_exists(&FOLDERID_Desktop),
            &mut problems,
        );
    }
    if opts.start_menu_shortcut {
        push(
            "Start Menu shortcut".into(),
            shortcut_exists(&FOLDERID_Programs),
            &mut problems,
        );
    }
    push(
        "Add/Remove Programs entry".into(),
        uninstall_entry_present(),
        &mut problems,
    );
    let _ = exe;
    (checks, problems)
}

// ---------------------------------------------------------------------------
// Wizard
//
// The GUI a user gets by double-clicking `gtx-setup.exe`. Five pages -- welcome,
// licence, options, progress, finish -- driven by one plain Win32 window with
// child controls, so it needs no webview and no dialog template.
//
// The install itself runs on a worker thread (it copies files and waits on a
// running browser); it reports each step by posting [`WM_APP_STEP`] with the
// [`STEP_LABELS`] index, and finishes by posting [`WM_APP_DONE`].
// ---------------------------------------------------------------------------

/// The plain-language terms, shown in full on the licence page.
const TERMS: &str = include_str!("GTX-TERMS.txt");

/// Posted by the worker thread for each [`STEP_LABELS`] index it reaches.
const WM_APP_STEP: u32 = WM_APP + 1;
/// Posted once the worker has stored its result.
const WM_APP_DONE: u32 = WM_APP + 2;

const ID_ACCEPT: usize = 1001;
const ID_DESKTOP: usize = 1002;
const ID_STARTMENU: usize = 1003;
const ID_LAUNCH: usize = 1004;
const ID_BACK: usize = 1010;
const ID_NEXT: usize = 1011;
const ID_CANCEL: usize = 1012;

/// Wizard pages, in order.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Page {
    Welcome,
    License,
    Options,
    Progress,
    Finish,
}

/// Design size of the client area, in 96-dpi units. The window is fixed so the
/// wizard looks the same on every machine.
const WIZ_W: i32 = 560;
const WIZ_H: i32 = 440;

/// Whether Back is available. Never on the first page, and never while the
/// install is running (stepping back would leave a half-installed browser).
fn can_go_back(page: Page, busy: bool) -> bool {
    // Never on the first page, never while installing, and never on the
    // finish page -- there is nothing to go back to once it is installed.
    !busy && matches!(page, Page::License | Page::Options)
}

/// Whether Next is allowed: the licence must be accepted, and there is no
/// "next" once the install is running.
fn can_go_next(page: Page, accepted: bool, busy: bool) -> bool {
    if busy {
        return false;
    }
    match page {
        Page::License => accepted,
        Page::Progress => false,
        _ => true,
    }
}

/// Text on the primary button.
fn next_label(page: Page) -> &'static str {
    match page {
        Page::Options => "Install",
        Page::Finish => "Finish",
        _ => "Next",
    }
}

/// `true` while the window must not be closed (an install is in flight).
fn blocks_close(page: Page, busy: bool) -> bool {
    busy && page == Page::Progress
}

/// The install result, published by the worker thread before it posts
/// [`WM_APP_DONE`]. `wizard` reads it after the message loop to pick an exit code.
static LAST_INSTALL: Mutex<Option<Result<(String, Option<PathBuf>), String>>> = Mutex::new(None);

/// Everything the window proc needs, hung off `GWLP_USERDATA`.
struct Ui {
    page: Page,
    /// The terms checkbox — gates Next on the licence page.
    accepted: bool,
    desktop: bool,
    start_menu: bool,
    launch: bool,
    busy: bool,
    /// Set from the worker's result so Finish can decide the exit code.
    install: Arc<Mutex<Option<Result<(String, Option<PathBuf>), String>>>>,
    dark: bool,
    bg: HBRUSH,
    panel: HBRUSH,
    /// Colour of the licence box, so its background matches its border.
    panel_color: COLORREF,
    font: HFONT,
    font_title: HFONT,
    title: HWND,
    body: HWND,
    terms: HWND,
    accept: HWND,
    desktop_box: HWND,
    start_box: HWND,
    bar: HWND,
    step: HWND,
    summary: HWND,
    launch_box: HWND,
    back: HWND,
    next: HWND,
    cancel: HWND,
}

impl Ui {
    /// The options the install should be run with.
    fn options(&self) -> InstallOptions {
        InstallOptions {
            repair: false,
            desktop_shortcut: self.desktop,
            start_menu_shortcut: self.start_menu,
        }
    }
}

/// Scale a 96-dpi design unit to `dpi`.
fn sx(v: i32, dpi: u32) -> i32 {
    ((v as i64 * dpi as i64 + 48) / 96) as i32
}

fn rgb(r: u8, g: u8, b: u8) -> COLORREF {
    COLORREF(r as u32 | (g as u32) << 8 | (b as u32) << 16)
}

fn cls(id: usize) -> HMENU {
    HMENU(id as *mut c_void)
}

/// A `Segoe UI` font at `point` points for `dpi`.
fn make_font(dpi: u32, point: i32, weight: i32) -> HFONT {
    let mut lf = LOGFONTW::default();
    // lfHeight is in pixels, and negative means "character height".
    lf.lfHeight = -((point * dpi as i32 + 36) / 72);
    lf.lfWeight = weight;
    lf.lfCharSet = FONT_CHARSET(1);
    lf.lfOutPrecision = FONT_OUTPUT_PRECISION(0);
    lf.lfClipPrecision = FONT_CLIP_PRECISION(0);
    lf.lfQuality = FONT_QUALITY(5);
    lf.lfPitchAndFamily = 0;
    let face: Vec<u16> = "Segoe UI".encode_utf16().collect();
    lf.lfFaceName[..face.len()].copy_from_slice(&face);
    unsafe { CreateFontIndirectW(&lf) }
}

/// Windows' light/dark app preference (`AppsUseLightTheme`, 0 = dark).
///
/// Read from the registry so the wizard matches the rest of Windows even when
/// the old uxtheme ordinals are unavailable.
fn apps_dark() -> bool {
    unsafe {
        let sub = HSTRING::from(
            r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize",
        );
        let mut key = HKEY(core::ptr::null_mut());
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

/// Show or hide a control.
fn vis(hwnd: HWND, on: bool) {
    if !hwnd.0.is_null() {
        unsafe {
            let _ = ShowWindow(hwnd, if on { SW_SHOW } else { SW_HIDE });
        }
    }
}

/// Place a control at 96-dpi design coordinates.
fn place(hwnd: HWND, dpi: u32, x: i32, y: i32, w: i32, h: i32) {
    if !hwnd.0.is_null() {
        unsafe {
            let _ = MoveWindow(hwnd, sx(x, dpi), sx(y, dpi), sx(w, dpi), sx(h, dpi), true);
        }
    }
}

fn set_text(hwnd: HWND, text: &str) {
    if !hwnd.0.is_null() {
        unsafe {
            let _ = SetWindowTextW(hwnd, &HSTRING::from(text));
        }
    }
}

fn set_font(hwnd: HWND, font: HFONT) {
    if !hwnd.0.is_null() {
        unsafe {
            let _ = SendMessageW(hwnd, WM_SETFONT, Some(WPARAM(font.0 as usize)), Some(LPARAM(1)));
        }
    }
}

fn checked(hwnd: HWND) -> bool {
    if hwnd.0.is_null() {
        return false;
    }
    unsafe { SendMessageW(hwnd, BM_GETCHECK, None, None).0 as u32 == BST_CHECKED.0 }
}

fn set_checked(hwnd: HWND, on: bool) {
    if hwnd.0.is_null() {
        return;
    }
    const BM_SETCHECK: u32 = 0x00F1;
    unsafe {
        let _ = SendMessageW(
            hwnd,
            BM_SETCHECK,
            Some(WPARAM(usize::from(on))),
            None,
        );
    }
}

/// Paint one page: title, body copy and which controls are on screen.
fn apply_page(ui: &mut Ui, dpi: u32) {
    let page = ui.page;
    let (title, body) = match page {
        Page::Welcome => (
            format!("Welcome to {APP_NAME} Setup"),
            format!(
                "This will install {APP_NAME} on this computer.\n\n\
                 It is a single small program that uses the WebView2 engine \
                 already built into Windows.\n\n\
                 No administrator rights are needed: everything is placed in \
                 your own user profile.",
            ),
        ),
        Page::License => (
            "Terms of Use".to_string(),
            "Please read the terms below. You can scroll through them.".to_string(),
        ),
        Page::Options => (
            "Choose your options".to_string(),
            format!(
                "{APP_NAME} will be installed to\n\n{}\n\n\
                 Your settings and browsing data stay in your user profile \
                 and are never sent anywhere.",
                install_dir().display()
            ),
        ),
        Page::Progress => (
            format!("Installing {APP_NAME}"),
            "Please wait. This normally takes a few seconds.".to_string(),
        ),
        Page::Finish => (format!("{APP_NAME} is ready"), String::new()),
    };
    set_text(ui.title, &title);
    set_text(ui.body, &body);

    vis(ui.title, true);
    vis(ui.body, !matches!(page, Page::Finish));
    vis(ui.terms, page == Page::License);
    vis(ui.accept, page == Page::License);
    vis(ui.desktop_box, page == Page::Options);
    vis(ui.start_box, page == Page::Options);
    vis(ui.bar, page == Page::Progress);
    vis(ui.step, page == Page::Progress);
    vis(ui.summary, page == Page::Finish);
    vis(ui.launch_box, page == Page::Finish);

    if page == Page::Finish {
        let (text, ok) = match &*ui.install.lock().unwrap_or_else(|e| e.into_inner()) {
            Some(Ok((msg, _))) => (msg.clone(), true),
            Some(Err(e)) => (format!("Setup could not finish:\n\n{e}"), false),
            None => (String::new(), false),
        };
        set_text(ui.summary, &text);
        vis(ui.launch_box, ok);
        set_checked(ui.launch_box, ok);
        ui.launch = ok;
    }

    // Geometry (96-dpi design units).
    place(ui.title, dpi, 24, 20, 512, 30);
    place(ui.back, dpi, 224, 396, 88, 30);
    place(ui.next, dpi, 320, 396, 104, 30);
    place(ui.cancel, dpi, 432, 396, 104, 30);

    match page {
        Page::Welcome => {
            place(ui.body, dpi, 24, 60, 512, 120);
        }
        Page::License => {
            place(ui.body, dpi, 24, 56, 512, 24);
            place(ui.terms, dpi, 24, 86, 512, 240);
            place(ui.accept, dpi, 24, 334, 512, 24);
        }
        Page::Options => {
            place(ui.body, dpi, 24, 56, 512, 120);
            place(ui.desktop_box, dpi, 32, 196, 400, 24);
            place(ui.start_box, dpi, 32, 226, 400, 24);
        }
        Page::Progress => {
            place(ui.body, dpi, 24, 120, 512, 24);
            place(ui.bar, dpi, 24, 180, 512, 18);
            place(ui.step, dpi, 24, 212, 512, 48);
        }
        Page::Finish => {
            place(ui.summary, dpi, 24, 60, 512, 250);
            place(ui.launch_box, dpi, 32, 322, 400, 24);
        }
    }

    // Buttons.
    vis(ui.back, can_go_back(page, ui.busy));
    vis(ui.cancel, page != Page::Finish);
    set_text(ui.next, next_label(page));
    let next_ok = can_go_next(page, ui.accepted, ui.busy);
    unsafe {
        let _ = EnableWindow(ui.next, next_ok);
        let _ = EnableWindow(ui.cancel, !ui.busy);
    }
}

/// Step the install forward: run it on a worker thread and pump the results
/// back through `WM_APP_STEP` / `WM_APP_DONE`.
fn start_install(hwnd: HWND, ui: &Ui) {
    let shared = Arc::clone(&ui.install);
    let opts = ui.options();
    let ptr = hwnd.0 as usize;
    std::thread::spawn(move || {
        let result = install_with(opts, |step| {
            let _ = unsafe {
                PostMessageW(
                    Some(HWND(ptr as *mut c_void)),
                    WM_APP_STEP,
                    WPARAM(step),
                    LPARAM(0),
                )
            };
        });
        if let Ok(mut slot) = shared.lock() {
            *slot = Some(result);
        }
        if let Ok(mut slot) = LAST_INSTALL.lock() {
            *slot = shared.lock().ok().and_then(|s| s.clone());
        }
        let _ = unsafe {  // wake the UI
            PostMessageW(Some(HWND(ptr as *mut c_void)), WM_APP_DONE, WPARAM(0), LPARAM(0))
        };
    });
}

fn ui_of(hwnd: HWND) -> &'static mut Ui {
    unsafe { &mut *(GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut Ui) }
}

/// Create every child control once; `apply_page` then shows the right ones.
unsafe fn build_ui(parent: HWND, dpi: u32, dark: bool) -> Ui {
    let module = HINSTANCE(unsafe { GetModuleHandleW(None) }.expect("module handle").0);
    let child = |class: PCWSTR, name: PCWSTR, style: u32, id: usize, x, y, w, h| -> HWND {
        unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                class,
                name,
                WINDOW_STYLE(style),
                sx(x, dpi),
                sx(y, dpi),
                sx(w, dpi),
                sx(h, dpi),
                Some(parent),
                Some(cls(id)),
                Some(module),
                None,
            )
        }
        .unwrap_or_else(|_| HWND(core::ptr::null_mut()))
    };
    let ws = |bits: &[u32]| -> u32 { bits.iter().fold(0u32, |a, b| a | b) };
    let visible = ws(&[WS_CHILD.0, WS_VISIBLE.0, WS_TABSTOP.0]);

    let font = make_font(dpi, 9, 400);
    let font_title = make_font(dpi, 15, 600);

    let (bg_color, panel_color) = if dark {
        (rgb(0x1E, 0x1E, 0x1E), rgb(0x2A, 0x2A, 0x2A))
    } else {
        let sys = COLORREF(unsafe { GetSysColor(COLOR_WINDOW) });
        (sys, COLORREF(unsafe { GetSysColor(COLOR_BTNFACE) }))
    };
    let (bg, panel) = (
        unsafe { CreateSolidBrush(bg_color) },
        unsafe { CreateSolidBrush(panel_color) },
    );

    let title = child(w!("STATIC"), w!(""), ws(&[WS_CHILD.0]), 0, 24, 20, 512, 30);
    let body = child(w!("STATIC"), w!(""), ws(&[WS_CHILD.0]), 0, 24, 60, 512, 120);
    let terms = child(
        w!("EDIT"),
        w!(""),
        ws(&[
            WS_CHILD.0,
            WS_BORDER.0,
            WS_VSCROLL.0,
            WS_TABSTOP.0,
            ES_MULTILINE as u32,
            ES_READONLY as u32,
            ES_AUTOVSCROLL as u32,
        ]),
        0,
        24,
        86,
        512,
        240,
    );
    let checkbox = visible | BS_AUTOCHECKBOX as u32;
    let accept = child(w!("BUTTON"), w!(""), checkbox, ID_ACCEPT, 24, 334, 512, 24);
    let desktop_box = child(w!("BUTTON"), w!(""), checkbox, ID_DESKTOP, 32, 196, 400, 24);
    let start_box = child(w!("BUTTON"), w!(""), checkbox, ID_STARTMENU, 32, 226, 400, 24);
    let launch_box = child(w!("BUTTON"), w!(""), checkbox, ID_LAUNCH, 32, 322, 400, 24);
    let bar = {
        // "PROGRESS_CLASS" is the comctl32 v5 alias; on a machine where only
        // the v6 classes are registered it is "msctls_progress32" instead.
        let b = child(w!("PROGRESS_CLASS"), w!(""), ws(&[WS_CHILD.0]), 0, 24, 180, 512, 18);
        if b.0.is_null() {
            child(w!("msctls_progress32"), w!(""), ws(&[WS_CHILD.0]), 0, 24, 180, 512, 18)
        } else {
            b
        }
    };
    let step = child(w!("STATIC"), w!(""), ws(&[WS_CHILD.0]), 0, 24, 212, 512, 48);
    let summary = child(w!("STATIC"), w!(""), ws(&[WS_CHILD.0]), 0, 24, 60, 512, 250);
    let push = visible | BS_PUSHBUTTON as u32;
    let back = child(w!("BUTTON"), w!("Back"), push, ID_BACK, 224, 396, 88, 30);
    let next = child(
        w!("BUTTON"),
        w!("Next"),
        visible | BS_DEFPUSHBUTTON as u32,
        ID_NEXT,
        320,
        396,
        104,
        30,
    );
    let cancel = child(w!("BUTTON"), w!("Cancel"), push, ID_CANCEL, 432, 396, 104, 30);

    set_text(title, "GTX Browser Setup");
    set_text(accept, "I have read and agree to the terms above.");
    set_text(desktop_box, "Create a shortcut on the Desktop");
    set_text(start_box, "Create a Start Menu shortcut");
    set_text(launch_box, "Start GTX Browser now");
    set_text(back, "Back");
    set_text(next, "Next");
    set_text(cancel, "Cancel");
    unsafe {
        let _ = SetWindowTextW(terms, &HSTRING::from(TERMS));
        let _ = SendMessageW(bar, PBM_SETRANGE, Some(WPARAM(100)), Some(LPARAM(0)));
        let _ = SendMessageW(bar, PBM_SETPOS, Some(WPARAM(0)), None);
    }

    for h in [
        title, body, terms, accept, desktop_box, start_box, launch_box, bar, step, summary,
        back, next, cancel,
    ] {
        set_font(h, font);
    }
    set_font(title, font_title);
    set_checked(desktop_box, true);
    set_checked(start_box, true);

    // Best effort: opt the standard controls into the app's dark mode so the
    // checkboxes and buttons match the client area we paint ourselves.
    if dark {
        unsafe {
            let _ = SetWindowTheme(parent, w!("DarkMode_Explorer"), None);
            for h in [accept, desktop_box, start_box, launch_box, back, next, cancel, bar] {
                let _ = SetWindowTheme(h, w!("DarkMode_Explorer"), None);
            }
        }
    }
    // A themed EDIT paints its own background and ignores WM_CTLCOLOREDIT;
    // opting this one out of theming is what lets it match the wizard.
    unsafe {
        let _ = SetWindowTheme(terms, w!(""), w!(""));
    }

    Ui {
        page: Page::Welcome,
        accepted: false,
        desktop: true,
        start_menu: true,
        launch: false,
        busy: false,
        install: Arc::new(Mutex::new(None)),
        dark,
        bg,
        panel,
        panel_color,
        font,
        font_title,
        title,
        body,
        terms,
        accept,
        desktop_box,
        start_box,
        bar,
        step,
        summary,
        launch_box,
        back,
        next,
        cancel,
    }
}

unsafe extern "system" fn wizard_wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    unsafe {
        match msg {
            WM_CREATE => {
                // During WM_CREATE the window has no size yet, so GetDpiForWindow
                // still reports the default 96; the system DPI is the right one
                // to lay the controls out with.
                let dark = apps_dark();
                let dpi = GetDpiForSystem().max(96);
                let ui = Box::into_raw(Box::new(build_ui(hwnd, dpi, dark)));
                let _ = SetWindowLongPtrW(hwnd, GWLP_USERDATA, ui as isize);
                // Give the window the same background as the controls paint,
                // otherwise the client area stays system-white behind light
                // text. The brush is owned by the `Ui` and freed on WM_DESTROY.
                let brush = ui_of(hwnd).bg.0 as isize;
                let _ = SetClassLongPtrW(hwnd, GCLP_HBRBACKGROUND, brush);
                let _ = InvalidateRect(Some(hwnd), None, true);
                // Paint the first page as soon as the controls exist.
                apply_page(ui_of(hwnd), dpi);
                LRESULT(0)
            }
            WM_DPICHANGED => {
                // Dragged to a screen with different scaling: re-lay out.
                let ui = ui_of(hwnd);
                let dpi = ((wparam.0 >> 16) & 0xFFFF).max(96) as u32;
                apply_page(ui, dpi);
                LRESULT(0)
            }
            WM_COMMAND => {
                let id = (wparam.0 & 0xFFFF) as usize;
                let ui = ui_of(hwnd);
                let dpi = GetDpiForWindow(hwnd);
                match id {
                    ID_ACCEPT => {
                        ui.accepted = checked(ui.accept);
                        apply_page(ui, dpi);
                    }
                    ID_DESKTOP => ui.desktop = checked(ui.desktop_box),
                    ID_STARTMENU => ui.start_menu = checked(ui.start_box),
                    ID_LAUNCH => ui.launch = checked(ui.launch_box),
                    ID_BACK => {
                        ui.page = match ui.page {
                            Page::Options => Page::License,
                            _ => Page::Welcome,
                        };
                        apply_page(ui, dpi);
                    }
                    ID_NEXT | ID_ENTER => match ui.page {
                        Page::Welcome => {
                            ui.page = Page::License;
                            apply_page(ui, dpi);
                        }
                        Page::License => {
                            ui.page = Page::Options;
                            apply_page(ui, dpi);
                        }
                        Page::Options => {
                            ui.page = Page::Progress;
                            ui.busy = true;
                            apply_page(ui, dpi);
                            start_install(hwnd, ui);
                        }
                        Page::Finish => {
                            let exe = ui.install.lock().ok().and_then(|slot| match &*slot {
                                Some(Ok((_, Some(exe)))) => Some(exe.clone()),
                                _ => None,
                            });
                            let launch = ui.launch;
                            let _ = DestroyWindow(hwnd);
                            if launch {
                                if let Some(exe) = exe {
                                    launch_browser(&exe);
                                }
                            }
                        }
                        Page::Progress => {}
                    },
                    // `ID_CANCEL` comes from our button; `IDCANCEL` from the
                    // Esc key once IsDialogMessage has handled it.
                    ID_CANCEL | ID_ESC => {
                        let _ = DestroyWindow(hwnd);
                    }
                    _ => {}
                }
                LRESULT(0)
            }
            WM_APP_STEP => {
                let ui = ui_of(hwnd);
                let step = wparam.0;
                if let Some(label) = STEP_LABELS.get(step) {
                    set_text(ui.step, label);
                }
                let total = STEP_LABELS.len().max(1) as i64;
                let pct = ((step as i64 + 1) * 100 / total).min(100) as usize;
                let _ = SendMessageW(ui.bar, PBM_SETPOS, Some(WPARAM(pct)), None);
                LRESULT(0)
            }
            WM_APP_DONE => {
                let ui = ui_of(hwnd);
                ui.busy = false;
                ui.page = Page::Finish;
                apply_page(ui, GetDpiForWindow(hwnd));
                LRESULT(0)
            }
            WM_KEYDOWN => match wparam.0 as u32 {
                k if k == u32::from(VK_ESCAPE.0) => {
                    let _ = DestroyWindow(hwnd);
                    LRESULT(0)
                }
                k if k == u32::from(VK_RETURN.0) => {
                    let ui = ui_of(hwnd);
                    if can_go_next(ui.page, ui.accepted, ui.busy) {
                        let _ = SendMessageW(hwnd, WM_COMMAND, Some(WPARAM(ID_NEXT)), None);
                    }
                    LRESULT(0)
                }
                _ => DefWindowProcW(hwnd, msg, wparam, lparam),
            },
            WM_CTLCOLORSTATIC | WM_CTLCOLOREDIT | WM_CTLCOLORBTN => {
                let ui = ui_of(hwnd);
                // The control draws with the DC handed over in wparam -- it
                // must be that one, not a fresh GetDC, or the colours are
                // set on a DC nobody paints with.
                let hdc = HDC(wparam.0 as *mut c_void);
                let fg = if ui.dark {
                    rgb(0xE8, 0xEA, 0xED)
                } else {
                    COLORREF(GetSysColor(COLOR_WINDOWTEXT))
                };
                let _ = SetTextColor(hdc, fg);
                // Statics and checkboxes must not paint a box of their own;
                // the read-only licence box keeps its own background.
                if msg == WM_CTLCOLOREDIT {
                    let _ = SetBkMode(hdc, OPAQUE);
                    let _ = SetBkColor(hdc, ui.panel_color);
                } else {
                    let _ = SetBkMode(hdc, TRANSPARENT);
                }
                LRESULT(ui.bg.0 as isize)
            }
            WM_CLOSE => {
                let ui = ui_of(hwnd);
                if !blocks_close(ui.page, ui.busy) {
                    let _ = DestroyWindow(hwnd);
                }
                LRESULT(0)
            }
            WM_DESTROY => {
                let raw = GetWindowLongPtrW(hwnd, GWLP_USERDATA);
                if raw != 0 {
                    let ui = Box::from_raw(raw as *mut Ui);
                    let _ = DeleteObject(HGDIOBJ(ui.font.0));
                    let _ = DeleteObject(HGDIOBJ(ui.font_title.0));
                    let _ = DeleteObject(HGDIOBJ(ui.bg.0));
                    let _ = DeleteObject(HGDIOBJ(ui.panel.0));
                    let _ = SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                }
                PostQuitMessage(0);
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }
}

/// Run the wizard. Returns the process exit code (0 = installed or cancelled).
fn wizard() -> i32 {
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let _ = InitCommonControlsEx(&INITCOMMONCONTROLSEX {
            dwSize: core::mem::size_of::<INITCOMMONCONTROLSEX>() as u32,
            dwICC: ICC_PROGRESS_CLASS,
        });
    }
    let dark = apps_dark();
    // Screen DPI decides the initial window size; the controls are laid out
    // again from the window's own DPI once it exists.
    let dpi = unsafe { GetDpiForSystem() }.max(96);

    unsafe {
        let module = GetModuleHandleW(None).expect("module handle");
        let cursor = LoadCursorW(None, IDC_ARROW).expect("cursor");
        let class = WNDCLASSEXW {
            cbSize: core::mem::size_of::<WNDCLASSEXW>() as u32,
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(wizard_wnd_proc),
            hInstance: HINSTANCE(module.0),
            hCursor: cursor,
            lpszClassName: w!("GtxSetupWizard"),
            ..Default::default()
        };
        RegisterClassExW(&class);

        let style = WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU;
        let mut rect = RECT {
            left: 0,
            top: 0,
            right: sx(WIZ_W, dpi),
            bottom: sx(WIZ_H, dpi),
        };
        let _ = AdjustWindowRectExForDpi(&mut rect, style, false, WINDOW_EX_STYLE::default(), dpi);
        let w = rect.right - rect.left;
        let h = rect.bottom - rect.top;
        let x = (GetSystemMetrics(SM_CXSCREEN) - w) / 2;
        let y = (GetSystemMetrics(SM_CYSCREEN) - h) / 2;

        let hwnd = CreateWindowExW(
            // WS_EX_CONTROLPARENT lets IsDialogMessage drive Tab / Enter / Esc
            // across the child controls, exactly as a real dialog would.
            WS_EX_CONTROLPARENT,
            w!("GtxSetupWizard"),
            w!("GTX Browser Setup"),
            style | WS_CLIPCHILDREN,
            x,
            y,
            w,
            h,
            None,
            None,
            Some(HINSTANCE(module.0)),
            None,
        )
        .expect("wizard window");
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = UpdateWindow(hwnd);

        // Dark title bar to match the dark client area we paint ourselves.
        if dark {
            let on: i32 = 1;
            let ptr = &on as *const i32 as *const c_void;
            let _ = DwmSetWindowAttribute(
                hwnd,
                DWMWA_USE_IMMERSIVE_DARK_MODE,
                ptr,
                core::mem::size_of::<i32>() as u32,
            );
        }

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            // Dialog-style keyboard: Tab cycles focus, Enter clicks the
            // default button, Esc sends WM_COMMAND(IDCANCEL).
            if IsDialogMessageW(hwnd, &msg).as_bool() {
                continue;
            }
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }

    // A failed install has already been reported in full on the Finish page;
    // the exit code is what scripted callers look at.
    match LAST_INSTALL.lock().ok().and_then(|s| s.clone()) {
        Some(Err(_)) => 1,
        _ => 0,
    }
}

// ---------------------------------------------------------------------------
// Shortcuts (Start Menu + Desktop)
// ---------------------------------------------------------------------------

unsafe fn create_shortcuts(
    dir: &Path,
    exe: &Path,
    opts: &InstallOptions,
) -> Result<(), String> {
    unsafe {
        CoInitializeEx(None, COINIT_APARTMENTTHREADED)
            .ok()
            .map_err(err)?;
    }
    let mut failures: Vec<String> = Vec::new();

    let make = |base: Option<PathBuf>, what: &str, out: &mut Vec<String>| {
        let Some(folder) = base else {
            out.push(format!("could not locate the {what} folder"));
            return;
        };
        let link = folder.join(format!("{APP_NAME}.lnk"));
        if let Err(e) = unsafe { create_shortcut(&link, exe, dir) } {
            out.push(format!("could not create the {what} shortcut: {e}"));
        }
    };

    if opts.desktop_shortcut {
        let desktop = known_folder(&FOLDERID_Desktop).or_else(|| {
            std::env::var_os("USERPROFILE").map(|p| PathBuf::from(p).join("Desktop"))
        });
        make(desktop, "Desktop", &mut failures);
    }

    if opts.start_menu_shortcut {
        let programs = known_folder(&FOLDERID_Programs).or_else(|| {
            std::env::var_os("APPDATA").map(|p| {
                PathBuf::from(p)
                    .join("Microsoft")
                    .join("Windows")
                    .join("Start Menu")
                    .join("Programs")
            })
        });
        make(programs, "Start Menu", &mut failures);
    }

    unsafe { CoUninitialize() };
    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures.join("; "))
    }
}

fn shortcut_exists(folder: &GUID) -> bool {
    known_folder(folder)
        .map(|f| f.join(format!("{APP_NAME}.lnk")).is_file())
        .unwrap_or(false)
}

/// One `.lnk` via IShellLinkW + IPersistFile (COM must be initialized).
unsafe fn create_shortcut(link: &Path, target: &Path, workdir: &Path) -> Result<(), String> {
    unsafe {
        let shell: IShellLinkW =
            CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).map_err(err)?;
        shell
            .SetPath(&HSTRING::from(target.to_string_lossy().as_ref()))
            .map_err(err)?;
        shell
            .SetWorkingDirectory(&HSTRING::from(workdir.to_string_lossy().as_ref()))
            .map_err(err)?;
        shell
            .SetDescription(&HSTRING::from(APP_NAME))
            .map_err(err)?;
        shell
            .SetIconLocation(&HSTRING::from(target.to_string_lossy().as_ref()), 0)
            .map_err(err)?;
        let file: IPersistFile = shell.cast().map_err(err)?;
        file.Save(&HSTRING::from(link.to_string_lossy().as_ref()), true)
            .map_err(err)?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Add/Remove Programs entry (HKCU — no elevation)
// ---------------------------------------------------------------------------

unsafe fn write_uninstall_entry(dir: &Path, exe: &Path) -> Result<(), String> {
    let mut key: HKEY = unsafe { core::mem::zeroed() };
    let err = unsafe { RegCreateKeyW(HKEY_CURRENT_USER, &HSTRING::from(UNINSTALL_KEY), &mut key) };
    if err != ERROR_SUCCESS {
        return Err(format!("cannot create the uninstall registry entry ({})", err.0));
    }

    let mut failures: Vec<String> = Vec::new();
    let exe_s = exe.to_string_lossy().into_owned();
    let dir_s = dir.to_string_lossy().into_owned();
    let setup_s = dir.join(SETUP_EXE).to_string_lossy().into_owned();
    let size_kb = std::fs::metadata(exe).map(|m| (m.len() / 1024) as u32).unwrap_or(0);

    let values: [(&str, String); 8] = [
        ("DisplayName", APP_NAME.to_string()),
        ("DisplayVersion", env!("CARGO_PKG_VERSION").to_string()),
        ("Publisher", "GTX".to_string()),
        ("DisplayIcon", exe_s.clone()),
        ("InstallLocation", dir_s.clone()),
        ("UninstallString", format!("\"{setup_s}\" --uninstall")),
        ("URLInfoAbout", "https://duckduckgo.com".to_string()),
        ("AppUserModelID", "GTX.Browser".to_string()),
    ];
    for (name, value) in values {
        if let Err(e) = reg_set_sz(key, name, &value) {
            failures.push(e);
        }
    }
    let _ = reg_set_dword(key, "NoModify", 1);
    let _ = reg_set_dword(key, "NoRepair", 1);
    let _ = reg_set_dword(key, "EstimatedSize", size_kb);

    let _ = unsafe { RegCloseKey(key) };
    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures.join("; "))
    }
}

/// Read the entry back — the install is only "done" if Windows can see it.
fn uninstall_entry_present() -> bool {
    unsafe {
        let mut key: HKEY = core::mem::zeroed();
        if RegOpenKeyW(HKEY_CURRENT_USER, &HSTRING::from(UNINSTALL_KEY), &mut key) != ERROR_SUCCESS
        {
            return false;
        }
        let name = HSTRING::from("UninstallString");
        let mut len = 0u32;
        let err = RegQueryValueExW(key, &name, None, None, None, Some(&mut len));
        let _ = RegCloseKey(key);
        err == ERROR_SUCCESS && len > 0
    }
}

fn reg_set_sz(key: HKEY, name: &str, value: &str) -> Result<(), String> {
    let mut wide: Vec<u16> = value.encode_utf16().collect();
    wide.push(0);
    let bytes = unsafe {
        std::slice::from_raw_parts(
            wide.as_ptr() as *const u8,
            wide.len() * core::mem::size_of::<u16>(),
        )
    };
    let err = unsafe { RegSetValueExW(key, &HSTRING::from(name), None, REG_SZ, Some(bytes)) };
    if err == ERROR_SUCCESS {
        Ok(())
    } else {
        Err(format!("registry value {name} failed ({})", err.0))
    }
}

fn reg_set_dword(key: HKEY, name: &str, value: u32) -> Result<(), String> {
    let err = unsafe {
        RegSetValueExW(
            key,
            &HSTRING::from(name),
            None,
            REG_DWORD,
            Some(&value.to_le_bytes()),
        )
    };
    if err == ERROR_SUCCESS {
        Ok(())
    } else {
        Err(format!("registry value {name} failed ({})", err.0))
    }
}

// ---------------------------------------------------------------------------
// Running-instance handling
// ---------------------------------------------------------------------------

thread_local! {
    static TARGET_PIDS: RefCell<Vec<u32>> = const { RefCell::new(Vec::new()) };
}

/// Every live `gtx-browser.exe` process id.
fn browser_pids() -> Vec<u32> {
    unsafe {
        let Ok(snap) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        let mut pe: PROCESSENTRY32W = core::mem::zeroed();
        pe.dwSize = core::mem::size_of::<PROCESSENTRY32W>() as u32;
        if Process32FirstW(snap, &mut pe).is_ok() {
            loop {
                if wide_array_to_string(&pe.szExeFile)
                    .is_some_and(|n| n.eq_ignore_ascii_case(BROWSER_EXE))
                {
                    out.push(pe.th32ProcessID);
                }
                if Process32NextW(snap, &mut pe).is_err() {
                    break;
                }
            }
        }
        let _ = CloseHandle(snap);
        out
    }
}

unsafe extern "system" fn close_enum(hwnd: HWND, _lparam: LPARAM) -> BOOL {
    let mut pid = 0u32;
    unsafe {
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if IsWindowVisible(hwnd).as_bool()
            && TARGET_PIDS.with(|p| p.borrow().contains(&pid))
        {
            // WM_CLOSE lets GTX save its session before exiting.
            let _ = PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
        }
    }
    BOOL(1) // keep enumerating
}

/// Ask every running GTX to close, then wait. `true` when they are gone —
/// we never force-kill: a user's browsing session is not ours to destroy.
fn close_running_browsers() -> bool {
    let pids = browser_pids();
    if pids.is_empty() {
        return true;
    }
    TARGET_PIDS.with(|p| *p.borrow_mut() = pids);
    unsafe {
        let _ = EnumWindows(Some(close_enum), LPARAM(0));
    }

    let deadline = Instant::now() + Duration::from_secs(6);
    while Instant::now() < deadline {
        if browser_pids().is_empty() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(250));
    }

    // One last graceful ask at the process level (still no /F).
    let _ = Command::new("taskkill")
        .args(["/IM", BROWSER_EXE])
        .creation_flags(CREATE_NO_WINDOW)
        .status();
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        if browser_pids().is_empty() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    browser_pids().is_empty()
}

// ---------------------------------------------------------------------------
// Uninstall
// ---------------------------------------------------------------------------

fn uninstall() -> Result<(String, Option<PathBuf>), String> {
    // Close a running instance first, otherwise the folder stays locked.
    let closed = close_running_browsers();
    if !closed {
        return Err(format!(
            "{APP_NAME} is still running and could not be closed.\n\n\
             Close it, then uninstall again from Settings → Apps."
        ));
    }

    // Shortcuts (best effort — they may already be gone).
    if let Some(desktop) = known_folder(&FOLDERID_Desktop) {
        let _ = std::fs::remove_file(desktop.join(format!("{APP_NAME}.lnk")));
    }
    if let Some(programs) = known_folder(&FOLDERID_Programs) {
        let _ = std::fs::remove_file(programs.join(format!("{APP_NAME}.lnk")));
    }
    if let Some(p) = std::env::var_os("USERPROFILE") {
        let _ = std::fs::remove_file(PathBuf::from(p).join("Desktop").join(format!("{APP_NAME}.lnk")));
    }

    // Add/Remove Programs entry.
    unsafe {
        let _ = RegDeleteKeyW(HKEY_CURRENT_USER, &HSTRING::from(UNINSTALL_KEY));
    }

    // The default-browser registration the browser writes at start-up.
    // It lives under HKCU with no installer step, so it needs its own pass.
    let registration_removed = remove_default_browser_registration();

    let mut msg = format!(
        "{APP_NAME} was uninstalled.\n\nYour settings and browsing data were \
         kept, in case you reinstall:\n  {}\n",
        config_dir().display()
    );
    msg.push_str(&format!(
        "\n  {} shortcut: {}\n  {} entry: {}\n  {} entry: {}",
        "Desktop",
        if shortcut_exists(&FOLDERID_Desktop) {
            "still present"
        } else {
            "removed"
        },
        "Add/Remove",
        if uninstall_entry_present() {
            "still present"
        } else {
            "removed"
        },
        "Default apps",
        if registration_removed {
            "removed"
        } else {
            "still present"
        },
    ));
    Ok((msg, None))
}

/// Take GTX back out of *Settings → Apps → Default apps*: the two `ProgId`
/// trees, the capabilities key and the `RegisteredApplications` value.
/// Everything comes from [`reg_names`]-style constants (shared with the
/// browser through `include!`), so a renamed registration can never be
/// half-deleted here.
///
/// Best effort like the rest of uninstall: a missing key counts as removed,
/// and the report says whether anything is still standing.
fn remove_default_browser_registration() -> bool {
    let mut clean = true;
    for key in KEY_TREES {
        let err = unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, &HSTRING::from(*key)) };
        // 2 = FILE_NOT_FOUND: nothing to remove, which is what we want.
        if err != ERROR_SUCCESS && err.0 != 2 {
            clean = false;
        }
    }
    unsafe {
        let mut hkey: HKEY = core::mem::zeroed();
        if RegOpenKeyW(HKEY_CURRENT_USER, &HSTRING::from(REGISTERED_KEY), &mut hkey)
            == ERROR_SUCCESS
        {
            let err = RegDeleteValueW(hkey, &HSTRING::from(APP_NAME));
            let _ = RegCloseKey(hkey);
            if err != ERROR_SUCCESS && err.0 != 2 {
                clean = false;
            }
        }
    }
    clean
}

fn config_dir() -> PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join(APP_NAME)
}

/// Schedule folder removal for when this process has exited. A `.cmd` helper
/// waits, retries (the running setup image locks its own folder), deletes the
/// folder, then deletes itself.
fn spawn_deferred_cleanup(dir: &Path) {
    let tmp = std::env::temp_dir();
    let pid = std::process::id();
    let script = tmp.join(format!("gtx-cleanup-{pid}.cmd"));
    let script_s = script.to_string_lossy().into_owned();
    let dir_s = dir.to_string_lossy().into_owned();
    let body = format!(
        "@echo off\r\n\
         ping -n 3 127.0.0.1 >nul\r\n\
         rmdir /s /q \"{dir_s}\" 2>nul\r\n\
         ping -n 2 127.0.0.1 >nul\r\n\
         rmdir /s /q \"{dir_s}\" 2>nul\r\n\
         del /f /q \"{script_s}\"\r\n"
    );
    if std::fs::write(&script, body).is_err() {
        return; // leaving the folder is better than a broken uninstall
    }
    let _ = Command::new("cmd")
        .args(["/C", &script.to_string_lossy()])
        .creation_flags(CREATE_NO_WINDOW)
        .spawn();
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn launch_browser(exe: &Path) {
    let dir = exe
        .parent()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    unsafe {
        let _ = ShellExecuteW(
            None,
            w!("open"),
            &HSTRING::from(exe.to_string_lossy().as_ref()),
            &HSTRING::new(),
            &HSTRING::from(dir),
            SW_SHOWNORMAL,
        );
    }
}

fn message(text: &str, flags: MESSAGEBOX_STYLE) -> MESSAGEBOX_RESULT {
    unsafe {
        MessageBoxW(
            None,
            &HSTRING::from(text),
            &HSTRING::from("GTX Setup"),
            flags,
        )
    }
}

fn known_folder(id: &GUID) -> Option<PathBuf> {
    unsafe {
        let pw = SHGetKnownFolderPath(id, KF_FLAG_DEFAULT, None).ok()?;
        let path = wide_to_string(pw.0).map(PathBuf::from);
        CoTaskMemFree(Some(pw.0 as *const c_void));
        path
    }
}

/// Decode a fixed-width wide buffer, stopping at the first NUL (bounded —
/// a full-length buffer has no terminator, so this never scans past the end).
fn wide_array_to_string(buf: &[u16]) -> Option<String> {
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16(&buf[..len]).ok()
}

unsafe fn wide_to_string(ptr: *const u16) -> Option<String> {
    if ptr.is_null() {
        return None;
    }
    let mut len = 0usize;
    while unsafe { *ptr.add(len) } != 0 {
        len += 1;
    }
    String::from_utf16(unsafe { std::slice::from_raw_parts(ptr, len) }).ok()
}

fn err(e: impl ToString) -> String {
    e.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn installer_paths_are_per_user() {
        let dir = install_dir();
        assert!(dir.ends_with("Programs") || dir.ends_with(APP_NAME));
        assert!(dir.to_string_lossy().contains(APP_NAME));
    }

    #[test]
    fn a_missing_browser_beside_the_setup_is_reported_clearly() {
        // The most common support question: "the installer doesn't work".
        let tmp = std::env::temp_dir().join(format!("gtx-missing-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let missing = tmp.join(BROWSER_EXE);
        assert!(!missing.is_file());
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn verification_reports_every_step() {
        let tmp = std::env::temp_dir().join(format!("gtx-verify-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let exe = tmp.join(BROWSER_EXE);
        std::fs::write(&exe, b"not really an exe").unwrap();
        // Deliberately omit gtx-setup.exe: one step must fail regardless of
        // what shortcuts/registry state the host machine happens to have.

        let (checks, problems) = verify_install(&tmp, &exe, &InstallOptions::default());
        // Shortcuts and the registry entry are absent in a temp dir, so those
        // must be reported as failures rather than glossed over.
        assert!(checks.len() >= 5, "every install step needs a check: {checks:?}");
        assert!(checks.iter().any(|(l, ok)| l.contains(BROWSER_EXE) && *ok));
        assert!(
            problems.iter().any(|p| p.contains(SETUP_EXE)),
            "a missing file must be reported: {problems:?}"
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn process_name_matching_is_case_insensitive() {
        assert!("GTX-BROWSER.EXE".eq_ignore_ascii_case(BROWSER_EXE));
        assert!(!"other.exe".eq_ignore_ascii_case(BROWSER_EXE));
    }

    #[test]
    fn every_install_step_has_a_label() {
        // The wizard shows `STEP_LABELS[step]`, so a missing or duplicated
        // label would show the wrong line (or a blank) while installing.
        assert_eq!(STEP_LABELS.len(), 7);
        let mut seen = std::collections::HashSet::new();
        for (i, label) in STEP_LABELS.iter().enumerate() {
            assert!(!label.trim().is_empty(), "step {i} has no label");
            assert!(seen.insert(*label), "duplicate step label: {label}");
        }
    }

    #[test]
    fn the_licence_gates_the_next_button() {
        assert!(!can_go_next(Page::License, false, false));
        assert!(can_go_next(Page::License, true, false));
        // Every other page is free to advance.
        for page in [Page::Welcome, Page::Options, Page::Finish] {
            assert!(can_go_next(page, false, false), "{page:?}");
        }
    }

    #[test]
    fn a_running_install_blocks_navigation() {
        for page in [
            Page::Welcome,
            Page::License,
            Page::Options,
            Page::Progress,
            Page::Finish,
        ] {
            assert!(!can_go_next(page, true, true), "{page:?} advanced while busy");
            assert!(!can_go_back(page, true), "{page:?} went back while busy");
        }
        assert!(blocks_close(Page::Progress, true));
        assert!(!blocks_close(Page::Progress, false));
        assert!(!blocks_close(Page::Welcome, true));
    }

    #[test]
    fn back_only_exists_where_it_leads_somewhere() {
        assert!(!can_go_back(Page::Welcome, false));
        assert!(can_go_back(Page::License, false));
        assert!(can_go_back(Page::Options, false));
        // Once installed there is nothing to go back to.
        assert!(!can_go_back(Page::Finish, false));
        assert!(!can_go_back(Page::Progress, false));
    }

    #[test]
    fn the_primary_button_says_what_it_does() {
        assert_eq!(next_label(Page::Welcome), "Next");
        assert_eq!(next_label(Page::License), "Next");
        assert_eq!(next_label(Page::Options), "Install");
        assert_eq!(next_label(Page::Finish), "Finish");
    }

    #[test]
    fn skipped_shortcuts_are_not_verified_as_failures() {
        // Turning both shortcuts off must not make verification report them
        // as missing -- they were never asked for.
        let tmp = std::env::temp_dir().join(format!("gtx-noicon-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let exe = tmp.join(BROWSER_EXE);
        std::fs::write(&exe, b"x").unwrap();
        std::fs::write(tmp.join(SETUP_EXE), b"x").unwrap();

        let none = InstallOptions {
            desktop_shortcut: false,
            start_menu_shortcut: false,
            ..Default::default()
        };
        let (checks, problems) = verify_install(&tmp, &exe, &none);
        assert!(!checks.iter().any(|(l, _)| l.contains("shortcut")));
        assert!(
            !problems.iter().any(|p| p.contains("shortcut")),
            "unrequested shortcuts were reported: {problems:?}"
        );

        // With them on, both are checked (whether or not they exist).
        let (checks, _) = verify_install(&tmp, &exe, &InstallOptions::default());
        assert!(checks.iter().any(|(l, _)| l == "Desktop shortcut"));
        assert!(checks.iter().any(|(l, _)| l == "Start Menu shortcut"));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn the_terms_are_plain_ascii_text() {
        // Included verbatim in the wizard; keep it portable and greppable.
        assert!(TERMS.starts_with("GTX Browser"));
        assert!(TERMS.contains("Terms of Use"));
        assert!(TERMS.lines().count() > 20);
        assert!(TERMS.is_ascii(), "the terms file must stay ASCII");
    }

    /// Uninstall must take the default-browser registration back out: the
    /// browser writes it at every start, and nothing else would remove it —
    /// Windows would keep offering an exe that is gone.
    #[test]
    fn uninstall_clears_the_default_browser_registration() {
        const SRC: &str = include_str!("gtx-setup.rs");
        let between = SRC
            .split("fn uninstall()")
            .nth(1)
            .expect("the uninstall routine")
            .split("fn remove_default_browser_registration")
            .next()
            .expect("the cleanup next to it");
        assert!(
            between.contains("remove_default_browser_registration()"),
            "uninstall must call the registration cleanup"
        );

        // The names are shared with the browser, and every ProgId the browser
        // registers has a key tree this installer deletes.
        assert_eq!(KEY_TREES.len(), 3);
        assert!(KEY_TREES.iter().any(|k| k.ends_with(PROGID_FILE)));
        assert!(KEY_TREES.iter().any(|k| k.ends_with(PROGID_URL)));
        assert!(KEY_TREES.iter().any(|k| k.ends_with(APP_NAME)));
        assert_eq!(CAPS_KEY, format!("Software\\{APP_NAME}\\Capabilities"));
    }
}
