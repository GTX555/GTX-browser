//! Address-bar autocomplete: turning what is typed plus the visited history
//! into the rows drawn under the omnibox by [`crate::popup`].
//!
//! Row 0 is always the decision the user is *about* to make: the search when
//! the input is a phrase, the address when it looks like one. History rows
//! follow, ranked from the most literal match down to a substring, and the
//! search row is appended last for host-like input so Enter still lands on
//! the site the user typed rather than a search for its name.

use crate::{config, history, nav};

/// What pressing Enter on the row does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// Run the input through the configured search engine.
    Search,
    /// Navigate to `target`.
    Visit,
}

/// One row of the address-bar dropdown.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Suggestion {
    /// The final URL to load — already resolved, never re-interpreted.
    pub target: String,
    /// Main line.
    pub label: String,
    /// Second line; empty rows draw centred instead.
    pub sub: String,
    pub kind: Kind,
}

/// Rows under the omnibox. Small on purpose: the list is a nudge, not a
/// search engine.
const MAX_ROWS: usize = 8;

/// History match scores, best first. Each rung is strict enough that a
/// literal URL match always beats a lucky substring in a title.
const SCORE_URL_EXACT: u32 = 100;
const SCORE_URL_TAIL: u32 = 95;
const SCORE_URL_PREFIX: u32 = 90;
const SCORE_HOST_PREFIX: u32 = 80;
const SCORE_HOST_CONTAINS: u32 = 70;
const SCORE_TITLE_PREFIX: u32 = 60;
const SCORE_TITLE_CONTAINS: u32 = 50;
const SCORE_URL_CONTAINS: u32 = 40;

/// Build the dropdown for `input`.
///
/// `history` is newest-first (as [`history::entries`] returns it); `template`
/// is the search URL template used for the search row.
pub fn suggest(input: &str, entries: &[history::Entry], template: &str) -> Vec<Suggestion> {
    let q = input.trim();
    if q.is_empty() {
        // Focused but empty: recent pages, no search row.
        return entries.iter().take(MAX_ROWS).map(history_row).collect();
    }

    let mut rows = Vec::new();
    let resolved = nav::resolve(q, template, "");
    let searches = resolved == nav::search(q, template);
    if searches {
        rows.push(search_row(q, template));
    } else {
        rows.push(Suggestion {
            target: resolved,
            label: q.to_string(),
            sub: String::new(),
            kind: Kind::Visit,
        });
    }
    for e in ranked(q, entries) {
        rows.push(history_row(e));
    }
    if !searches && rows.len() < MAX_ROWS {
        // Host-like input put the address on row 0; keep the search one
        // keypress away at the bottom. (For a phrase the search row already
        // *is* row 0.)
        rows.push(search_row(q, template));
    }
    rows.truncate(MAX_ROWS);
    rows
}

/// The search row as the engine would run it.
fn search_row(q: &str, template: &str) -> Suggestion {
    let brand = config::search_brand(template);
    Suggestion {
        target: nav::search(q, template),
        label: if brand.is_empty() {
            format!("Search for \u{201c}{q}\u{201d}")
        } else {
            format!("Search {brand} for \u{201c}{q}\u{201d}")
        },
        sub: String::new(),
        kind: Kind::Search,
    }
}

/// A history entry as a row: what it is called, then where it lives.
fn history_row(e: &history::Entry) -> Suggestion {
    let (label, sub) = if e.title.trim().is_empty() {
        (e.url.clone(), String::new())
    } else {
        (e.title.clone(), e.url.clone())
    };
    Suggestion {
        target: e.url.clone(),
        label,
        sub,
        kind: Kind::Visit,
    }
}

/// History entries matching `q`, best match first, one row per URL, recency
/// breaking ties within a score.
fn ranked<'a>(q: &str, entries: &'a [history::Entry]) -> Vec<&'a history::Entry> {
    let ql = q.to_ascii_lowercase();
    let mut scored: Vec<(u32, usize, &history::Entry)> = entries
        .iter()
        .enumerate()
        .filter_map(|(i, e)| score(&ql, e).map(|s| (s, i, e)))
        .collect();
    // Stable sort: equal scores keep their (newest-first) order.
    scored.sort_by(|a, b| b.0.cmp(&a.0));

    let mut seen: Vec<&str> = Vec::new();
    scored
        .into_iter()
        .filter(|(_, _, e)| {
            if seen.iter().any(|u| *u == e.url) {
                return false;
            }
            seen.push(&e.url);
            true
        })
        .map(|(_, _, e)| e)
        .collect()
}

/// How well an entry answers `ql` (already lowercased), or `None` for "no
/// relation at all" — those rows never show up.
fn score(ql: &str, e: &history::Entry) -> Option<u32> {
    let url = e.url.to_ascii_lowercase();
    let title = e.title.to_ascii_lowercase();
    let host = nav::host_of(&e.url)
        .unwrap_or_default()
        .to_ascii_lowercase();
    let bare = host.strip_prefix("www.").unwrap_or(&host).to_string();

    if url == ql {
        return Some(SCORE_URL_EXACT);
    }
    // `docs.rs` typed over `https://docs.rs/` — the same place, no scheme.
    let tail = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .unwrap_or(&url)
        .trim_end_matches('/');
    if tail == ql {
        return Some(SCORE_URL_TAIL);
    }
    if url.starts_with(ql) {
        return Some(SCORE_URL_PREFIX);
    }
    if host.starts_with(ql) || bare.starts_with(ql) {
        return Some(SCORE_HOST_PREFIX);
    }
    if host.contains(ql) || bare.contains(ql) {
        return Some(SCORE_HOST_CONTAINS);
    }
    if title.starts_with(ql) {
        return Some(SCORE_TITLE_PREFIX);
    }
    if title.contains(ql) {
        return Some(SCORE_TITLE_CONTAINS);
    }
    if url.contains(ql) {
        return Some(SCORE_URL_CONTAINS);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const DDG: &str = "https://duckduckgo.com/?q={}";

    fn e(url: &str, title: &str) -> history::Entry {
        history::Entry {
            title: title.into(),
            url: url.into(),
            ts: 0,
        }
    }

    #[test]
    fn an_empty_box_lists_recent_pages() {
        let rows = suggest("", &[e("https://a.test/", "Alpha"), e("https://b.test/", "Beta")], DDG);
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(|r| r.kind == Kind::Visit));
        assert_eq!(rows[0].label, "Alpha");
    }

    #[test]
    fn a_phrase_is_a_search_and_history_follows() {
        let rows = suggest("rust", &[e("https://rust-lang.org/", "Rust")], DDG);
        assert_eq!(rows[0].kind, Kind::Search);
        assert!(rows[0].label.starts_with("Search DuckDuckGo for"));
        assert_eq!(rows[0].target, "https://duckduckgo.com/?q=rust");
        assert_eq!(rows[1].label, "Rust", "history still offered underneath");
    }

    #[test]
    fn host_like_input_lands_on_the_site_first() {
        let rows = suggest("example.com", &[], DDG);
        assert_eq!(rows[0].kind, Kind::Visit);
        assert_eq!(rows[0].target, "https://example.com");
        assert_eq!(rows[0].label, "example.com");
        assert!(
            rows.iter().any(|r| r.kind == Kind::Search),
            "the search stays one keypress away"
        );
    }

    #[test]
    fn a_schemed_url_is_never_treated_as_a_query() {
        let rows = suggest("https://example.com/x", &[], DDG);
        assert_eq!(rows[0].kind, Kind::Visit);
        assert_eq!(rows[0].target, "https://example.com/x");
    }

    #[test]
    fn the_best_history_match_wins() {
        let entries = vec![
            e("https://other.test/", "example.com mentioned in a title"),
            e("https://example.com/", "Unrelated page"),
            e("https://example.com/docs", "Docs"),
        ];
        let rows = suggest("example.com", &entries, DDG);
        // Row 0 is the address as typed; the history rows follow it.
        let visits: Vec<&Suggestion> = rows.iter().skip(1).filter(|r| r.kind == Kind::Visit).collect();
        assert_eq!(visits[0].target, "https://example.com/", "exact URL beats a title");
        assert_eq!(visits[1].target, "https://example.com/docs", "same host beats another title");
        assert!(visits.iter().any(|r| r.label.contains("mentioned")), "…which beats nothing");
    }

    #[test]
    fn one_row_per_url_and_a_bounded_list() {
        let mut entries = vec![e("https://dup.test/", "First"), e("https://dup.test/", "Second")];
        entries.extend((0..20).map(|i| e(&format!("https://s{i}.test/"), "Page")));
        let rows = suggest("dup", &entries, DDG);
        assert!(rows.len() <= MAX_ROWS);
        let dups = rows.iter().filter(|r| r.target == "https://dup.test/").count();
        assert_eq!(dups, 1, "the same URL never appears twice");
        assert!(rows.len() < 10, "the list stays a nudge, not a search engine");
    }

    #[test]
    fn ties_keep_the_most_recent_entry() {
        // Both titles start with "zoo", so only recency separates them.
        let entries = vec![
            e("https://alpha.test/", "Zoo news"),
            e("https://beta.test/", "Zoo olds"),
        ];
        let rows = suggest("zoo", &entries, DDG);
        assert_eq!(rows[0].kind, Kind::Search);
        assert_eq!(rows[1].label, "Zoo news", "scored equal, newest first");
        assert_eq!(rows[2].label, "Zoo olds");
    }
}
