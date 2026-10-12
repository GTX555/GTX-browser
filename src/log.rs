//! Best-effort file logging.
//!
//! Release builds are `windows_subsystem = "windows"`, so `eprintln!` reaches
//! nobody — and "the browser stopped loading things" is exactly the kind of
//! failure you need to be able to read about afterwards. Everything that used
//! to vanish into a void now lands in `%LOCALAPPDATA%\GTX Browser\gtx.log`.

use std::io::Write;
use std::path::PathBuf;

/// Rotate at half a megabyte so the file can never grow without bound.
const MAX_BYTES: u64 = 512 * 1024;

fn path() -> PathBuf {
    crate::config::data_dir().join("gtx.log")
}

/// `HH:MM:SS` from a Unix timestamp (UTC) — readable without a dependency.
fn clock(secs: u64) -> String {
    format!(
        "{:02}:{:02}:{:02}",
        (secs % 86_400) / 3600,
        (secs % 3600) / 60,
        secs % 60
    )
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Append one line. Never panics and never fails loudly: logging must not be
/// able to take the browser down.
pub fn write(msg: &str) {
    let p = path();
    if let Some(parent) = p.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(md) = std::fs::metadata(&p) {
        if md.len() > MAX_BYTES {
            let _ = std::fs::rename(&p, p.with_extension("log.old"));
        }
    }
    let line = format!("[{}] {msg}\n", clock(now()));
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&p)
    {
        let _ = f.write_all(line.as_bytes());
    }
    #[cfg(debug_assertions)]
    eprint!("[gtx] {msg}");
}

/// Where the log lives (shown on the settings page's About section).
pub fn location() -> PathBuf {
    path()
}
