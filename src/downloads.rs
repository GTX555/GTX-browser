//! Recent downloads — what the toolbar's download button lists.
//!
//! Mirrors [`crate::history`]: an in-memory list capped to the last [`MAX`]
//! entries, persisted to `%LOCALAPPDATA%\GTX Browser\downloads.toml` so the
//! panel survives a restart. A row is written when the engine accepts the
//! download (a cancelled save dialog never shows up) and updated when it
//! finishes, which is also where the byte count comes from.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::{config, history};

/// Newest-first cap. A convenience list, not an archive.
const MAX: usize = 50;
/// Rows the toolbar panel lists (plus the "open folder" footer row).
pub const PANEL_MAX: usize = 6;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Download {
    /// File name only — the panel's main line.
    pub name: String,
    pub url: String,
    /// Full destination path, as a plain string so toml round-trips it.
    pub path: String,
    /// Seconds since the Unix epoch (start, refreshed on completion).
    pub ts: u64,
    /// `false` = failed or cancelled.
    pub ok: bool,
    /// Bytes on disk once finished; `0` while it is still running.
    pub size: u64,
}

#[derive(Default, Serialize, Deserialize)]
struct File {
    #[serde(default)]
    entries: Vec<Download>,
}

thread_local! {
    static ENTRIES: std::cell::RefCell<Vec<Download>> =
        const { std::cell::RefCell::new(Vec::new()) };
    static LOADED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

fn path_file() -> PathBuf {
    config::data_dir().join("downloads.toml")
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn file_name(p: &Path) -> String {
    p.file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "download".into())
}

fn size_of(path: &str) -> u64 {
    if path.is_empty() {
        return 0;
    }
    std::fs::metadata(path).map(|m| m.len()).unwrap_or(0)
}

// ------------------------------------------------------------------
// Pure list operations (exercised directly by the tests)
// ------------------------------------------------------------------

/// Newest row first, replacing any earlier row for the same destination and
/// enforcing the cap.
fn insert(list: &mut Vec<Download>, d: Download) {
    if !d.path.is_empty() {
        list.retain(|e| e.path != d.path);
    }
    list.insert(0, d);
    if list.len() > MAX {
        list.truncate(MAX);
    }
}

/// Mark a download finished: the started row is updated in place, so the
/// panel never shows the same file twice.
fn complete(list: &mut Vec<Download>, url: &str, path: &str, ok: bool) {
    let size = if ok { size_of(path) } else { 0 };
    if let Some(e) = list
        .iter_mut()
        .find(|e| e.url == url && (path.is_empty() || e.path == path))
    {
        e.ok = ok;
        e.size = size;
        e.ts = now();
        return;
    }
    let name = if path.is_empty() {
        url.rsplit('/').next().unwrap_or("download").to_string()
    } else {
        file_name(Path::new(path))
    };
    insert(
        list,
        Download {
            name,
            url: url.to_string(),
            path: path.to_string(),
            ts: now(),
            ok,
            size,
        },
    );
}

/// After loading from disk: a row that was still running when the browser
/// died never produced a file, so it was never finished — say Failed rather
/// than showing "Downloading…" forever. Existing files get their real size.
fn reconcile(list: &mut Vec<Download>) {
    for d in list.iter_mut().filter(|d| d.ok && d.size == 0) {
        if d.path.is_empty() {
            continue;
        }
        match std::fs::metadata(&d.path).map(|m| m.len()) {
            Ok(n) => d.size = n,
            Err(_) => d.ok = false,
        }
    }
}

// ------------------------------------------------------------------
// Persistence
// ------------------------------------------------------------------

fn ensure_loaded() {
    LOADED.with(|l| {
        if l.get() {
            return;
        }
        l.set(true);
        let Ok(text) = std::fs::read_to_string(path_file()) else {
            return; // first run: nothing recorded yet
        };
        match toml::from_str::<File>(&text) {
            Ok(f) => ENTRIES.with(|e| {
                let mut list = f.entries;
                reconcile(&mut list);
                *e.borrow_mut() = list;
            }),
            Err(e) => crate::log::write(&format!(
                "{} did not parse ({e}); starting an empty list",
                path_file().display()
            )),
        }
    });
}

fn persist() {
    let entries = ENTRIES.with(|e| e.borrow().clone());
    if let Some(parent) = path_file().parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(text) = toml::to_string_pretty(&File { entries }) {
        let _ = std::fs::write(path_file(), text);
    }
}

// ------------------------------------------------------------------
// Public API (main thread only — called from the event drain)
// ------------------------------------------------------------------

/// The engine accepted a download: the destination is final here.
pub fn started(url: &str, dest: &Path) {
    ensure_loaded();
    let dest_str = dest.to_string_lossy().into_owned();
    ENTRIES.with(|e| {
        insert(
            &mut e.borrow_mut(),
            Download {
                name: file_name(dest),
                url: url.to_string(),
                path: dest_str,
                ts: now(),
                ok: true,
                size: 0,
            },
        );
    });
    persist();
}

/// The engine reported a result; `path` may be absent when it failed early.
pub fn finished(url: &str, dest: Option<&Path>, ok: bool) {
    ensure_loaded();
    let dest_str = dest
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    ENTRIES.with(|e| complete(&mut e.borrow_mut(), url, &dest_str, ok));
    persist();
}

/// All downloads, newest first.
pub fn entries() -> Vec<Download> {
    ensure_loaded();
    ENTRIES.with(|e| e.borrow().clone())
}

/// Where downloads are recorded (shown in the About box style listings).
pub fn location() -> PathBuf {
    path_file()
}

// ------------------------------------------------------------------
// Presentation
// ------------------------------------------------------------------

/// `12.3 MB`, `48 KB`, `612 B` — one place so the panel never disagrees
/// with itself.
pub fn human_size(n: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    const GB: f64 = MB * 1024.0;
    if (n as f64) < KB {
        format!("{n} B")
    } else if (n as f64) < MB {
        format!("{:.1} KB", n as f64 / KB)
    } else if (n as f64) < GB {
        format!("{:.1} MB", n as f64 / MB)
    } else {
        format!("{:.2} GB", n as f64 / GB)
    }
}

/// The panel's second line: `12.3 MB · 2026-10-03 14:02`, `Failed · …` or
/// `Downloading… · …`. A size of 0 is checked against disk, so a real
/// zero-byte file reads as `0 B` while a row still in flight reads as
/// running.
pub fn meta(d: &Download) -> String {
    let when = history::stamp(d.ts);
    if !d.ok {
        return format!("Failed · {when}");
    }
    let size = if d.size > 0 {
        Some(d.size)
    } else {
        std::fs::metadata(&d.path).ok().map(|m| m.len())
    };
    match size {
        Some(n) => format!("{} · {when}", human_size(n)),
        None => format!("Downloading… · {when}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(path: &str) -> Download {
        Download {
            name: file_name(Path::new(path)),
            url: "https://example.com/f".into(),
            path: path.into(),
            ts: 1,
            ok: true,
            size: 0,
        }
    }

    #[test]
    fn newest_first_and_capped() {
        let mut list = Vec::new();
        for i in 0..(MAX + 20) {
            insert(&mut list, row(&format!("C:/dl/file-{i}.zip")));
        }
        assert_eq!(list.len(), MAX);
        assert_eq!(list[0].path, format!("C:/dl/file-{}.zip", MAX + 19));
    }

    #[test]
    fn the_same_destination_never_stacks_up() {
        let mut list = Vec::new();
        insert(&mut list, row("C:/dl/a.zip"));
        insert(&mut list, row("C:/dl/b.zip"));
        insert(&mut list, row("C:/dl/a.zip"));
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].path, "C:/dl/a.zip");
        assert_eq!(list[1].path, "C:/dl/b.zip");
    }

    #[test]
    fn finishing_updates_the_started_row() {
        let mut list = Vec::new();
        insert(&mut list, row("C:/dl/a.zip"));
        complete(&mut list, "https://example.com/f", "C:/dl/a.zip", false);
        assert_eq!(list.len(), 1, "no duplicate row");
        assert!(!list[0].ok);
        assert_eq!(list[0].size, 0);

        // A completion with no matching start still produces a row.
        complete(&mut list, "https://other.test/x", "", true);
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].url, "https://other.test/x");
        assert!(list[0].name.contains("x"));
    }

    #[test]
    fn an_unfinished_download_is_not_pending_forever() {
        let mut list = Vec::new();
        insert(&mut list, row("C:/definitely/not/here.bin"));
        reconcile(&mut list);
        assert!(!list[0].ok, "no file on disk means it never finished");
    }

    #[test]
    fn sizes_and_status_read_naturally() {
        assert_eq!(human_size(0), "0 B");
        assert_eq!(human_size(612), "612 B");
        assert_eq!(human_size(48 * 1024), "48.0 KB");
        assert_eq!(human_size(12_582_912), "12.0 MB");
        assert_eq!(human_size(3_221_225_472), "3.00 GB");

        let mut failed = row("C:/dl/a.zip");
        failed.ok = false;
        assert!(meta(&failed).starts_with("Failed · "));
        assert!(meta(&row("C:/definitely/not/here.bin")).starts_with("Downloading… · "));
    }

    /// The profile's own record file must keep parsing — a format change
    /// would silently empty the panel on the user's machine.
    #[test]
    fn the_profile_file_parses_when_present() {
        let path = path_file();
        if !path.exists() {
            return;
        }
        let text = std::fs::read_to_string(&path).expect("readable");
        let f: File = toml::from_str(&text).expect("downloads.toml must parse");
        assert!(f.entries.len() <= MAX);
        eprintln!("profile downloads: {} rows", f.entries.len());
    }

    #[test]
    fn file_names_come_from_the_path() {
        assert_eq!(file_name(Path::new("C:/dl/report.pdf")), "report.pdf");
        assert_eq!(file_name(Path::new("")), "download", "no name to work with");
    }
}
