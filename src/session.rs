//! Session persistence: `%LOCALAPPDATA%\GTX Browser\session.toml`.

use serde::{Deserialize, Serialize};

use crate::config;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Session {
    /// Index of the active tab.
    pub active: usize,
    pub tabs: Vec<TabEntry>,
    pub window: Option<WindowEntry>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TabEntry {
    pub url: String,
    #[serde(default)]
    pub title: String,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct WindowEntry {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
    #[serde(default)]
    pub maximized: bool,
}

fn path() -> std::path::PathBuf {
    config::data_dir().join("session.toml")
}

/// Returns `None` when there is nothing to restore (or the file is corrupt).
pub fn load() -> Option<Session> {
    let text = std::fs::read_to_string(path()).ok()?;
    let session: Session = toml::from_str(&text).ok()?;
    if session.tabs.is_empty() {
        None
    } else {
        Some(session)
    }
}

pub fn save(session: &Session) {
    let path = path();
    if let Some(parent) = path.parent() {
        if std::fs::create_dir_all(parent).is_err() {
            return;
        }
    }
    if let Ok(text) = toml::to_string_pretty(session) {
        let _ = std::fs::write(&path, text);
    }
}

pub fn clear() {
    let _ = std::fs::remove_file(path());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let s = Session {
            active: 1,
            tabs: vec![
                TabEntry { url: "https://a.example".into(), title: "A".into() },
                TabEntry { url: "https://b.example".into(), title: "B".into() },
            ],
            window: Some(WindowEntry { x: 1, y: 2, w: 800, h: 600, maximized: false }),
        };
        let text = toml::to_string_pretty(&s).unwrap();
        let back: Session = toml::from_str(&text).unwrap();
        assert_eq!(back.active, 1);
        assert_eq!(back.tabs.len(), 2);
        assert_eq!(back.tabs[1].url, "https://b.example");
        assert_eq!(back.window.unwrap().w, 800);
    }
}
