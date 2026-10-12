//! Browsing history: an in-memory list capped to the last [`MAX`] visits,
//! persisted to `%LOCALAPPDATA%\GTX Browser\history.toml` so `gtx://history`
//! survives a restart.
//!
//! Recording happens on the main thread from the app's event drain, which
//! keeps this module free of locks.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::{config, favicon, nav};

/// Newest-first cap. Small on purpose: this is a convenience list, not a
/// database.
const MAX: usize = 500;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Entry {
    pub title: String,
    pub url: String,
    /// Seconds since the Unix epoch.
    pub ts: u64,
}

#[derive(Default, Serialize, Deserialize)]
struct File {
    #[serde(default)]
    entries: Vec<Entry>,
}

thread_local! {
    static ENTRIES: std::cell::RefCell<Vec<Entry>> =
        const { std::cell::RefCell::new(Vec::new()) };
    static LOADED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

fn path() -> PathBuf {
    config::data_dir().join("history.toml")
}

/// Where history is persisted (shown on the settings page's About section).
pub fn location() -> PathBuf {
    path()
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn ensure_loaded() {
    LOADED.with(|l| {
        if l.get() {
            return;
        }
        l.set(true);
        if let Ok(text) = std::fs::read_to_string(path()) {
            if let Ok(f) = toml::from_str::<File>(&text) {
                ENTRIES.with(|e| *e.borrow_mut() = f.entries);
            }
        }
    });
}

fn persist() {
    let entries = ENTRIES.with(|e| e.borrow().clone());
    if let Some(parent) = path().parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(text) = toml::to_string_pretty(&File { entries }) {
        let _ = std::fs::write(path(), text);
    }
}

/// Record a visit. Consecutive repeats of the same URL collapse into one entry.
pub fn record(url: &str, title: &str) {
    ensure_loaded();
    ENTRIES.with(|e| {
        let mut list = e.borrow_mut();
        if let Some(first) = list.first_mut() {
            if first.url == url {
                if !title.is_empty() {
                    first.title = title.to_string();
                }
                first.ts = now();
                return;
            }
        }
        list.insert(
            0,
            Entry {
                title: title.to_string(),
                url: url.to_string(),
                ts: now(),
            },
        );
        if list.len() > MAX {
            list.truncate(MAX);
        }
    });
    persist();
}

/// All visits, newest first.
pub fn entries() -> Vec<Entry> {
    ensure_loaded();
    ENTRIES.with(|e| e.borrow().clone())
}


/// Escape text for inclusion in HTML.
fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// Human readable timestamp (`YYYY-MM-DD HH:MM`) — shared with the
/// downloads panel so both lists read the same way.
pub(crate) fn stamp(ts: u64) -> String {
    // Civil-from-days conversion — no chrono, no dependency.
    let days = (ts / 86_400) as i64;
    let secs = (ts % 86_400) as i32;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02}",
        secs / 3600,
        (secs % 3600) / 60
    )
}

/// The site's favicon as an `<img>` for one row, or nothing at all when the
/// URL has no host (`gtx://`, `file:`) - such a row keeps its letter tile.
/// Only the site's own origin is ever asked: the history page must not tell
/// anybody outside the visited site what you have been reading.
fn favicon_img(url: &str) -> String {
    let Some(host) = nav::web_host(url) else {
        return String::new();
    };
    let letter = favicon::letter(&host).map(|c| c.to_ascii_uppercase()).unwrap_or('?');
    let h = esc(&host);
    let first = favicon::ICON_PATHS[0];
    let second = favicon::ICON_PATHS[1];
    format!("<img class=\"fav\" loading=\"lazy\" data-t=\"0\" data-l=\"{letter}\" data-f=\"https://{h}{second}\" src=\"https://{h}{first}\" alt=\"\">")
}

/// Render the `gtx://history` page. Light/dark via `prefers-color-scheme` so
/// switching the theme never requires a reload.
pub fn render() -> String {
    render_entries(&entries())
}

/// The page for a specific list - split out so the markup stays testable
/// without touching the profile's real history.
pub fn render_entries(rows: &[Entry]) -> String {
    let body = if rows.is_empty() {
        "<p class=\"empty\">No history yet — pages you visit will show up here.</p>"
            .to_string()
    } else {
        rows.iter()
            .map(|e| {
                let title = if e.title.is_empty() {
                    esc(&e.url)
                } else {
                    esc(&e.title)
                };
                format!(
                    "<li>{icon}<a href=\"{url}\">{title}</a><span class=\"m\">{when}</span></li>",
                    icon = favicon_img(&e.url),
                    url = esc(&e.url),
                    when = stamp(e.ts),
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    };

    format!(
        r##"<!doctype html>
<html lang="en"><head><meta charset="utf-8"><title>History — GTX Browser</title>
<style>
  :root {{ color-scheme: light dark; }}
  html, body {{ margin: 0; height: 100%; }}
  body {{
    background: Canvas; color: CanvasText;
    font: 13px/1.5 "Segoe UI", system-ui, sans-serif;
    padding: 24px 28px 40px;
  }}
  h1 {{ font-size: 18px; margin: 0 0 2px; }}
  .sub {{ opacity: .6; margin: 0 0 18px; }}
  ul {{ list-style: none; margin: 0; padding: 0; max-width: 900px; }}
  li {{
    display: flex; justify-content: space-between; gap: 18px;
    padding: 9px 12px; border-radius: 8px; border-bottom: 1px solid color-mix(in srgb, CanvasText 12%, transparent);
  }}
  li:hover {{ background: color-mix(in srgb, CanvasText 7%, transparent); }}
  .fav {{ width: 16px; height: 16px; border-radius: 4px; flex: none;
          align-self: center; background: color-mix(in srgb, CanvasText 10%, transparent); }}
  .fav.letter {{ display: flex; align-items: center; justify-content: center;
                 font-size: 9px; font-weight: 700; }}
  a {{ color: Highlight; text-decoration: none; overflow: hidden;
       text-overflow: ellipsis; white-space: nowrap; flex: 1 1 auto; min-width: 0; }}
  a:hover {{ text-decoration: underline; }}
  .m {{ opacity: .55; white-space: nowrap; font-size: 12px; }}
  .empty {{ opacity: .6; }}
</style></head>
<body>
  <h1>History</h1>
  <p class="sub">Last {n} visited pages, newest first.</p>
  <ul>{rows}</ul>
<script>
// Real favicons, straight from the sites that own them: favicon.ico, then
// the touch icon, then a letter tile. The listener sits on the list in the
// capture phase because `error` events do not bubble.
document.querySelector("ul").addEventListener("error", e => {{
  const img = e.target;
  if (!img || !img.classList || !img.classList.contains("fav")) return;
  if (img.dataset.t === "0") {{
    img.dataset.t = "1";
    img.src = img.dataset.f;
    return;
  }}
  const tile = document.createElement("span");
  tile.className = "fav letter";
  tile.textContent = img.dataset.l || "?";
  img.replaceWith(tile);
}}, true);
</script>
</body></html>"##,
        n = MAX,
        rows = body,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escaping() {
        assert_eq!(esc("a<b>&\"c\""), "a&lt;b&gt;&amp;&quot;c&quot;");
    }

    #[test]
    fn timestamp_format() {
        assert_eq!(stamp(0), "1970-01-01 00:00");
        assert_eq!(stamp(1_700_000_000), "2023-11-14 22:13");
    }

    #[test]
    fn renderer_lists_entries() {
        let html = render();
        assert!(html.contains("History"));
        assert!(html.contains("<!doctype html>"));
    }

    fn row(title: &str, url: &str) -> Entry {
        Entry { title: title.into(), url: url.into(), ts: 1 }
    }

    /// Every http(s) row asks the site it points at - favicon.ico first,
    /// apple-touch-icon second, and carries the pre-computed letter that
    /// replaces the image if both fail.
    #[test]
    fn rows_carry_the_sites_own_icon() {
        let html = render_entries(&[row("Example", "https://www.Example.com/page?a=1&b=2")]);
        assert!(html.contains("https://www.example.com/favicon.ico"));
        assert!(html.contains("data-f=\"https://www.example.com/apple-touch-icon.png\""));
        assert!(html.contains("loading=\"lazy\""));
        assert!(html.contains("data-l=\"E\""), "letter tile comes from the host");
        assert!(html.contains("href=\"https://www.Example.com/page?a=1&amp;b=2\""));
        let tag = html.split("<img").nth(1).expect("an img tag").split('>').next().unwrap();
        assert!(!tag.contains("page?a=1"), "the icon URL is host-only: no query");
    }

    /// GTX's own pages and local files have no host to ask.
    #[test]
    fn rows_without_a_host_make_no_requests() {
        let html = render_entries(&[row("Blank", "gtx://newtab"), row("Local", "file:///C:/a.htm")]);
        assert!(!html.contains("<img"), "no icon slot means no network traffic");
        assert!(html.contains("<li><a href=\"gtx://newtab\">"), "rows still list");
    }

    /// The privacy promise is testable: only the visited site's own origin
    /// is ever fetched - no icon proxy learns what you read.
    #[test]
    fn the_icon_pipeline_never_leaves_the_sites_own_origin() {
        let html = render();
        assert!(!html.contains("duckduckgo.com/ip3"), "no third-party icon service");
        assert!(!html.contains("google.com/s2"), "no third-party icon service");
        assert!(html.contains("img.dataset.f"), "the fallback follows the row's own host");
        assert!(html.contains("addEventListener(\"error\""), "broken icons degrade to a tile");
    }
}
