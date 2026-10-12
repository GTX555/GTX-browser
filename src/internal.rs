//! GTX's own pages: the blank new-tab surface and the URL/title constants
//! shared by the settings and history tabs.
//!
//! Internal pages are rendered with `prefers-color-scheme` + standard system
//! colours (`Canvas` / `CanvasText`), so flipping the theme repaints them
//! through wry's `set_theme` without reloading — which is what keeps the
//! settings tab's scroll position alive.

/// Blank new tab: nothing but the theme's background colour.
pub const NEW_TAB_URL: &str = "gtx://newtab";
pub const NEW_TAB_TITLE: &str = "New Tab";

pub const HISTORY_URL: &str = "gtx://history";
pub const HISTORY_TITLE: &str = "History";

/// True for URLs GTX renders itself rather than fetching.
///
/// `with_html` shows up as a `data:` navigation and WebView2 follows it with
/// `about:blank`, so both must count as ours — otherwise an internal tab gets
/// demoted to a normal one on its very first paint and starts overwriting its
/// own `gtx://…` address.
pub fn is_internal(url: &str) -> bool {
    url.is_empty()
        || url.starts_with("gtx://")
        || url.starts_with("about:")
        || url.starts_with("data:")
        || url.starts_with("blob:")
}

/// The blank new-tab document.
pub fn new_tab_html() -> String {
    r##"<!doctype html>
<html lang="en"><head><meta charset="utf-8"><title>New Tab</title>
<style>
  :root { color-scheme: light dark; }
  html, body { margin: 0; height: 100%; background: Canvas; }
</style></head>
<body></body></html>"##
        .to_string()
}
