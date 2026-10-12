//! Making GTX the default browser.
//!
//! Windows keeps the *authoritative* answer in
//! `HKCU\Software\Microsoft\Windows\Shell\Associations\UrlAssociations\<scheme>\UserChoice`,
//! and that key is hash-protected: an application may not write it. What an
//! application is allowed to do — and what every browser does — is:
//!
//! 1. **Register** ([`register`]): publish its `ProgId`s, its capabilities and
//!    a `RegisteredApplications` entry under `HKCU`. That entry is what puts
//!    GTX into *Settings → Apps → Default apps*. It runs at every start
//!    because a portable exe has no installer to do it once.
//! 2. **Ask** ([`set_default`]): call `IApplicationAssociationRegistration`,
//!    the API Microsoft documents for changing the association. Windows 8 and
//!    later decline it (the choice belongs to the user), so on failure we open
//!    the Default Apps page *directly at GTX* instead — the
//!    `registeredAppUser` deep-link parameter works on Windows 11 21H2+ with
//!    the 2023 cumulative update, and degrades to the plain page before that.
//!
//! [`is_default`] reads `UserChoice` for both `http` and `https`; nothing else
//! is trusted as an answer.

use windows::core::{w, HSTRING};
use windows::Win32::Foundation::{ERROR_SUCCESS, RPC_E_CHANGED_MODE, S_FALSE, S_OK};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_INPROC_SERVER,
    COINIT_APARTMENTTHREADED,
};
use windows::Win32::System::Registry::{
    RegCloseKey, RegCreateKeyW, RegOpenKeyW, RegQueryValueExW, RegSetValueExW, HKEY,
    HKEY_CURRENT_USER, REG_SZ,
};
use windows::Win32::UI::Shell::{
    ApplicationAssociationRegistration, IApplicationAssociationRegistration, ShellExecuteW,
    AT_URLPROTOCOL,
};
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

// The registry names live next door so the installer can share them when it
// uninstalls — see `crate::reg_names`.
pub use crate::reg_names::{APP_NAME, CAPS_KEY, PROGID_FILE, PROGID_URL, REGISTERED_KEY};

/// The command line that opens a link (or an `.htm` file) in GTX.
fn open_command(exe: &str) -> String {
    format!("\"{exe}\" \"%1\"")
}

/// Every `(key, value, data)` the registration writes — pure, so the tests can
/// pin the exact shape without touching the registry. An empty value name is
/// the key's default value. Keys are relative to `HKCU\`.
fn registrations(exe: &str) -> Vec<(String, &'static str, String)> {
    let cmd = open_command(exe);
    let icon = format!("{exe},0");
    vec![
        // The file ProgId.
        (
            format!("Software\\Classes\\{PROGID_FILE}"),
            "",
            "GTX Browser HTML Document".into(),
        ),
        (
            format!("Software\\Classes\\{PROGID_FILE}\\DefaultIcon"),
            "",
            icon.clone(),
        ),
        (
            format!("Software\\Classes\\{PROGID_FILE}\\shell\\open\\command"),
            "",
            cmd.clone(),
        ),
        // The protocol ProgId (`URL Protocol` marks it as one).
        (
            format!("Software\\Classes\\{PROGID_URL}"),
            "",
            "GTX Browser URL".into(),
        ),
        (
            format!("Software\\Classes\\{PROGID_URL}"),
            "URL Protocol",
            String::new(),
        ),
        (
            format!("Software\\Classes\\{PROGID_URL}\\DefaultIcon"),
            "",
            icon.clone(),
        ),
        (
            format!("Software\\Classes\\{PROGID_URL}\\shell\\open\\command"),
            "",
            cmd,
        ),
        // What Windows lists under *Default apps*.
        (
            CAPS_KEY.into(),
            "ApplicationDescription",
            "GTX Browser \u{2014} an extremely lightweight, no-bloat web browser".into(),
        ),
        (CAPS_KEY.into(), "ApplicationIcon", icon),
        (
            format!("{CAPS_KEY}\\FileAssociations"),
            ".htm",
            PROGID_FILE.into(),
        ),
        (
            format!("{CAPS_KEY}\\FileAssociations"),
            ".html",
            PROGID_FILE.into(),
        ),
        (
            format!("{CAPS_KEY}\\URLAssociations"),
            "http",
            PROGID_URL.into(),
        ),
        (
            format!("{CAPS_KEY}\\URLAssociations"),
            "https",
            PROGID_URL.into(),
        ),
        // The advertisement itself: name → capabilities key, as Windows
        // documents it (relative to the hive the entry lives in).
        (REGISTERED_KEY.into(), APP_NAME, CAPS_KEY.into()),
    ]
}

fn exe_path() -> Option<String> {
    std::env::current_exe()
        .ok()
        .map(|p| p.to_string_lossy().into_owned())
}

/// Write one `REG_SZ` value, creating its key along the way.
fn set_sz(key_path: &str, name: &str, data: &str) -> Result<(), String> {
    let mut key: HKEY = unsafe { core::mem::zeroed() };
    let err = unsafe { RegCreateKeyW(HKEY_CURRENT_USER, &HSTRING::from(key_path), &mut key) };
    if err != ERROR_SUCCESS {
        return Err(format!("cannot create {key_path} ({})", err.0));
    }
    let mut wide: Vec<u16> = data.encode_utf16().collect();
    wide.push(0);
    let bytes = unsafe {
        std::slice::from_raw_parts(
            wide.as_ptr() as *const u8,
            wide.len() * core::mem::size_of::<u16>(),
        )
    };
    let err = unsafe { RegSetValueExW(key, &HSTRING::from(name), None, REG_SZ, Some(bytes)) };
    let _ = unsafe { RegCloseKey(key) };
    if err != ERROR_SUCCESS {
        return Err(format!("cannot write {key_path}\\{name} ({})", err.0));
    }
    Ok(())
}

/// Publish GTX under `HKCU` so Windows can offer it as a default.
///
/// Best effort: a failure (a locked-down profile) is logged, never fatal —
/// the browser works exactly the same without it.
pub fn register() {
    let Some(exe) = exe_path() else {
        crate::log::write("default browser: could not resolve the exe path");
        return;
    };
    for (key, name, data) in registrations(&exe) {
        if let Err(e) = set_sz(&key, name, &data) {
            crate::log::write(&format!("default browser registration: {e}"));
            return;
        }
    }
}

/// Read a string value from under `HKCU`.
fn get_sz(key_path: &str, name: &str) -> Option<String> {
    let mut key: HKEY = unsafe { core::mem::zeroed() };
    if unsafe { RegOpenKeyW(HKEY_CURRENT_USER, &HSTRING::from(key_path), &mut key) }
        != ERROR_SUCCESS
    {
        return None;
    }
    let mut len = 0u32;
    let err = unsafe { RegQueryValueExW(key, &HSTRING::from(name), None, None, None, Some(&mut len)) };
    if err != ERROR_SUCCESS || len < 2 {
        let _ = unsafe { RegCloseKey(key) };
        return None;
    }
    let mut buf = vec![0u8; len as usize];
    let err = unsafe {
        RegQueryValueExW(
            key,
            &HSTRING::from(name),
            None,
            None,
            Some(buf.as_mut_ptr()),
            Some(&mut len),
        )
    };
    let _ = unsafe { RegCloseKey(key) };
    if err != ERROR_SUCCESS {
        return None;
    }
    buf.truncate(len as usize);
    let wide: Vec<u16> = buf
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect();
    let trimmed: Vec<u16> = wide.into_iter().take_while(|&c| c != 0).collect();
    String::from_utf16(&trimmed).ok()
}

/// The `ProgId` Windows currently uses for a URL scheme, as the user chose it.
fn user_choice(scheme: &str) -> Option<String> {
    get_sz(
        &format!(
            "Software\\Microsoft\\Windows\\Shell\\Associations\\UrlAssociations\\{scheme}\\UserChoice"
        ),
        "ProgId",
    )
}

/// Is GTX already the default? Both protocols have to point at it.
pub fn is_default() -> bool {
    is_default_from(
        user_choice("http").as_deref(),
        user_choice("https").as_deref(),
    )
}

/// The answer, from two `UserChoice` values — pure so the rule is testable.
fn is_default_from(http: Option<&str>, https: Option<&str>) -> bool {
    http == Some(PROGID_URL) && https == Some(PROGID_URL)
}

/// What [`set_default`] achieved, in words the settings page can show.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Windows accepted the change.
    Set,
    /// It already was the default.
    Already,
    /// Windows declined (it always does, since 8.0): the Default Apps page is
    /// open at GTX for the user to confirm.
    SettingsOpened,
    /// Not even the settings page could be opened.
    Failed(String),
}

impl Outcome {
    /// The sentence shown next to the button.
    pub fn message(&self) -> String {
        match self {
            Outcome::Set => "GTX Browser is now your default browser".into(),
            Outcome::Already => "GTX Browser is already your default browser".into(),
            Outcome::SettingsOpened => {
                "Windows keeps this choice for the user \u{2014} the Default \
                 apps page is open; pick GTX Browser there"
                    .into()
            }
            Outcome::Failed(e) => format!("Could not hand this to Windows: {e}"),
        }
    }
}

/// Percent-encode everything outside the URI "unreserved" set, so a name with
/// spaces or `&` in it can never break the deep link.
fn uri_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// The deep link straight to GTX's own page inside *Default apps*.
fn settings_uri() -> String {
    format!(
        "ms-settings:defaultapps?registeredAppUser={}",
        uri_escape(APP_NAME)
    )
}

/// The sanctioned API. Modern Windows refuses it, older Windows may not — we
/// try it and let the caller re-check [`is_default`] rather than trusting a
/// return code.
fn try_association_api() -> bool {
    unsafe {
        let hr = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        // Only balance the call when *we* were the one that initialized COM.
        let ours = hr == S_OK;
        if hr != S_OK && hr != S_FALSE && hr != RPC_E_CHANGED_MODE {
            return false;
        }
        let mut attempted = false;
        if let Ok(reg) = CoCreateInstance::<_, IApplicationAssociationRegistration>(
            &ApplicationAssociationRegistration,
            None,
            CLSCTX_INPROC_SERVER,
        ) {
            for scheme in ["http", "https"] {
                // Err on Windows 8+ is the expected answer; the caller checks
                // the registry instead of believing us.
                let _ = reg.SetAppAsDefault(
                    &HSTRING::from(APP_NAME),
                    &HSTRING::from(scheme),
                    AT_URLPROTOCOL,
                );
            }
            attempted = true;
        }
        if ours {
            CoUninitialize();
        }
        attempted
    }
}

/// Open *Default apps* at GTX's own page.
fn open_settings_page() -> Result<(), String> {
    let uri = settings_uri();
    let ret = unsafe {
        ShellExecuteW(
            None,
            w!("open"),
            &HSTRING::from(uri),
            &HSTRING::new(),
            &HSTRING::new(),
            SW_SHOWNORMAL,
        )
    };
    // ShellExecute's contract: a value above 32 is success.
    if ret.0 as isize > 32 {
        Ok(())
    } else {
        Err(format!("ms-settings could not be launched ({})", ret.0 as isize))
    }
}

/// Make GTX the default: register, ask Windows, and if Windows insists the
/// user decides, take them to the page that can.
pub fn set_default() -> Outcome {
    register();
    if is_default() {
        return Outcome::Already;
    }
    try_association_api();
    if is_default() {
        return Outcome::Set;
    }
    match open_settings_page() {
        Ok(()) => Outcome::SettingsOpened,
        Err(e) => Outcome::Failed(e),
    }
}

/// Everything the settings page needs after a click: the new status, whether
/// it is an error, and the sentence to show with them.
pub struct Report {
    pub is_default: bool,
    pub err: bool,
    pub msg: String,
}

pub fn status_after_attempt() -> Report {
    let outcome = set_default();
    Report {
        is_default: is_default(),
        err: matches!(outcome, Outcome::Failed(_)),
        msg: outcome.message(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The launch command must quote both halves: a path with spaces (this is
    /// a *portable* exe, so `C:\Users\...\My Tools\gtx-browser.exe` is the
    /// normal case) and Windows' `%1` placeholder for the URL.
    #[test]
    fn the_open_command_quotes_the_exe_and_the_url() {
        assert_eq!(
            open_command(r"C:\Users\luis-\Downloads\GTX-browser\gtx-browser.exe"),
            r#""C:\Users\luis-\Downloads\GTX-browser\gtx-browser.exe" "%1""#
        );
        assert!(open_command(r"C:\a b\g.exe").starts_with("\"C:\\a b\\g.exe\""));
    }

    /// Everything Windows needs to offer GTX in *Default apps*, spelled out:
    /// both ProgIds with a command, both file extensions, both protocols, and
    /// the `RegisteredApplications` entry pointing at the capabilities key.
    #[test]
    fn the_registration_declares_two_progids_and_both_protocols() {
        let rows = registrations(r"C:\gtx\gtx-browser.exe");

        // Paths are HKCU-relative and never wander outside Software.
        for (key, _, _) in &rows {
            assert!(
                key.starts_with("Software\\"),
                "{key} is not relative to HKCU\\Software"
            );
        }

        let get = |key: &str, name: &str| -> Option<&str> {
            rows.iter()
                .find(|(k, n, _)| k == key && *n == name)
                .map(|(_, _, v)| v.as_str())
        };

        // Capabilities: what the app is, and what it can open.
        assert!(get(CAPS_KEY, "ApplicationDescription").is_some());
        assert_eq!(
            get(&format!("{CAPS_KEY}\\URLAssociations"), "http"),
            Some(PROGID_URL)
        );
        assert_eq!(
            get(&format!("{CAPS_KEY}\\URLAssociations"), "https"),
            Some(PROGID_URL)
        );
        assert_eq!(
            get(&format!("{CAPS_KEY}\\FileAssociations"), ".htm"),
            Some(PROGID_FILE)
        );
        assert_eq!(
            get(&format!("{CAPS_KEY}\\FileAssociations"), ".html"),
            Some(PROGID_FILE)
        );

        // The advertisement Windows reads first.
        assert_eq!(get(REGISTERED_KEY, APP_NAME), Some(CAPS_KEY));

        // Both ProgIds open things in GTX.
        for progid in [PROGID_FILE, PROGID_URL] {
            let cmd = get(&format!("Software\\Classes\\{progid}\\shell\\open\\command"), "")
                .expect("open command");
            assert!(cmd.contains("\"%1\""), "the URL placeholder is lost: {cmd}");
            assert!(cmd.starts_with("\""), "the exe path must be quoted: {cmd}");
            assert!(cmd.contains("gtx-browser.exe"), "{cmd}");
        }
        assert_eq!(
            get(&format!("Software\\Classes\\{PROGID_URL}"), "URL Protocol"),
            Some(""),
        );
    }

    /// The protected key stays untouched: `UserChoice` is Windows' to write.
    #[test]
    fn registration_never_writes_the_user_choice_key() {
        for (key, _, _) in registrations("C:\\gtx\\gtx-browser.exe") {
            assert!(
                !key.contains("UserChoice"),
                "registration must not write {key}"
            );
            assert!(
                !key.starts_with("Software\\Microsoft\\Windows\\Shell\\Associations"),
                "registration must not write {key}"
            );
        }
    }

    /// Default-ness is exactly "both protocols chose us", decided from what
    /// Windows itself reports — no guessing from our own registration.
    #[test]
    fn default_is_decided_by_both_user_choices() {
        assert!(is_default_from(Some(PROGID_URL), Some(PROGID_URL)));
        assert!(!is_default_from(Some(PROGID_URL), Some("MSEdgeHTM")));
        assert!(!is_default_from(Some("MSEdgeHTM"), Some(PROGID_URL)));
        assert!(!is_default_from(None, Some(PROGID_URL)));
        assert!(!is_default_from(Some(PROGID_URL), None));
        assert!(!is_default_from(None, None));
    }

    /// The deep link escapes the registered name and lands on GTX's page.
    #[test]
    fn the_settings_deep_link_points_at_gtx() {
        assert_eq!(uri_escape("GTX Browser"), "GTX%20Browser");
        assert_eq!(uri_escape("a&b=c"), "a%26b%3Dc");
        assert_eq!(uri_escape("Safe-1.0_~"), "Safe-1.0_~");
        assert_eq!(
            settings_uri(),
            "ms-settings:defaultapps?registeredAppUser=GTX%20Browser"
        );
    }

    /// Every outcome must be expressible to the user, never blank.
    #[test]
    fn every_outcome_has_a_sentence() {
        for outcome in [
            Outcome::Set,
            Outcome::Already,
            Outcome::SettingsOpened,
            Outcome::Failed("no".into()),
        ] {
            assert!(!outcome.message().trim().is_empty());
        }
        assert!(Outcome::Set.message().contains("GTX Browser"));
        assert!(Outcome::SettingsOpened.message().contains("Default apps"));
    }

    /// Live check of the registry plumbing: a value written under GTX's own
    /// scratch key comes back verbatim. Ignored by default because it writes
    /// to `HKCU` — run explicitly with `cargo test -- --ignored`.
    ///
    /// It deliberately does *not* call [`register`]: `current_exe()` here is
    /// the test binary, and the test must never advertise itself as a
    /// browser. The real registration is exercised by starting the app.
    #[test]
    #[ignore = "touches HKCU; run with --ignored"]
    fn registry_values_round_trip() {
        use windows::Win32::System::Registry::RegDeleteKeyW;

        let scratch = "Software\\GTX Browser\\Tests";
        set_sz(scratch, "RoundTrip", "a value with spaces").expect("write");
        assert_eq!(
            get_sz(scratch, "RoundTrip").as_deref(),
            Some("a value with spaces")
        );
        assert_eq!(get_sz(scratch, "NoSuchValue"), None);
        assert_eq!(get_sz("Software\\GTX Browser\\NoSuchKey", "x"), None);
        let _ = unsafe { RegDeleteKeyW(HKEY_CURRENT_USER, &HSTRING::from(scratch)) };
    }

    /// Live check of the whole flow: registration, the sanctioned API (which
    /// modern Windows refuses) and the fallback that opens *Default apps* at
    /// GTX. Ignored by default because it brings Windows Settings to the
    /// front — run explicitly with `cargo test -- --ignored`.
    #[test]
    #[ignore = "opens Windows Settings; run with --ignored"]
    fn the_set_default_flow_never_fails_silently() {
        let outcome = set_default();
        // Whatever Windows answered, the sentence shown to the user is real,
        // and "nothing worked" is never an acceptable quiet outcome.
        assert!(!outcome.message().trim().is_empty());
        println!("set_default -> {} (is_default={})", outcome.message(), is_default());
        if let Outcome::Failed(e) = outcome {
            panic!("set_default could not hand over to Windows: {e}");
        }
    }
}
