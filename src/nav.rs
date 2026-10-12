//! Address bar input resolution: decide whether the user typed a URL or a
//! search query, and turn it into a navigable address.

/// Schemes we pass through untouched.
const KNOWN_SCHEMES: &[&str] = &[
    "http://",
    "https://",
    "file://",
    "ftp://",
    "ws://",
    "wss://",
    "about:",
    "data:",
    "view-source:",
    "chrome://",
    "edge://",
    "javascript:",
    "mailto:",
];

/// Resolve address-bar input into a URL.
///
/// * empty input -> `homepage`
/// * known scheme -> passed through
/// * anything with whitespace -> search
/// * host-looking input (`example.com`, `localhost:8080`, `192.168.0.1/x`) -> `https://…`
/// * everything else -> search
pub fn resolve(input: &str, search_template: &str, homepage: &str) -> String {
    let t = input.trim();
    if t.is_empty() {
        return homepage.to_string();
    }
    if KNOWN_SCHEMES.iter().any(|s| {
        t.len() >= s.len() && t[..s.len()].eq_ignore_ascii_case(s)
    }) {
        return t.to_string();
    }
    if t.contains(char::is_whitespace) {
        return search(t, search_template);
    }
    if looks_like_host(t) {
        return format!("https://{t}");
    }
    search(t, search_template)
}

/// Build a search URL from the template's `{}` placeholder.
pub fn search(query: &str, template: &str) -> String {
    let encoded = encode_query(query);
    if template.contains("{}") {
        template.replacen("{}", &encoded, 1)
    } else {
        format!("{template}{encoded}")
    }
}

/// True when the input looks like a scheme-less host (`host`, `host:port`,
/// `host/path`, IPv4) rather than a search phrase. Public because the
/// autocomplete wants the same verdict as [`resolve`].
pub fn looks_like_host(t: &str) -> bool {
    // Split off path/query/fragment to inspect just the authority part.
    let authority = t.split(['/', '?', '#']).next().unwrap_or("");
    if authority.is_empty() {
        return false;
    }
    // host:port — port must be all digits, otherwise treat as a search
    // (e.g. "rust:book") .
    let host = match authority.rsplit_once(':') {
        Some((h, p)) if !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()) => h,
        _ => authority,
    };
    if host.is_empty() {
        return false;
    }
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    if host.parse::<std::net::Ipv4Addr>().is_ok() {
        return true;
    }
    // A label with a dot in it: `example.com`, `example.com:8080`, `a.b/c`.
    let b = host.as_bytes();
    host.contains('.')
        && !host.starts_with('.')
        && !host.ends_with('.')
        && b.iter().any(|c| c.is_ascii_alphanumeric())
}

/// Resolve what Windows hands us when another app opens a link.
///
/// `http(s)` associations pass a full URL, the `.htm` file association a bare
/// Windows path - both must become navigable addresses, and the result must
/// be stable: normalising twice changes nothing, so it can run at launch and
/// again when a link arrives over `WM_COPYDATA`.
pub fn resolve_launch_arg(input: &str, search_template: &str) -> String {
    let t = input.trim().trim_matches('"');
    if t.is_empty() {
        return String::new();
    }
    if KNOWN_SCHEMES
        .iter()
        .any(|s| t.len() >= s.len() && t[..s.len()].eq_ignore_ascii_case(s))
    {
        return t.to_string();
    }
    if looks_like_windows_path(t) {
        return file_url(t);
    }
    resolve(t, search_template, "")
}

/// `\...`, `\/...` or a UNC share - what a file association passes.
fn looks_like_windows_path(t: &str) -> bool {
    let b = t.as_bytes();
    (b.len() >= 3
        && b[0].is_ascii_alphabetic()
        && b[1] == b':'
        && (b[2] == b'\\' || b[2] == b'/'))
        || t.starts_with("\\\\")
}

/// `\a b.htm` -> `file:///C:/a%20b.htm`; a UNC share keeps its server as
/// the host (`file://nas/share/f.htm`). Everything outside the URL's safe
/// set is percent-encoded, so `#`, `?` and spaces in a filename stay literal.
fn file_url(path: &str) -> String {
    let p = path.replace('\\', "/");
    let raw = if let Some(rest) = p.strip_prefix("//") {
        format!("file://{rest}")
    } else {
        format!("file:///{p}")
    };
    let mut out = String::with_capacity(raw.len());
    for by in raw.bytes() {
        match by {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' | b':' => {
                out.push(by as char)
            }
            _ => out.push_str(&format!("%{by:02X}")),
        }
    }
    out
}

/// Lowercased host of a URL. `None` for non-navigable schemes
/// (`about:`, `data:`, `blob:`, GTX's own pages, …).
pub fn host_of(url: &str) -> Option<String> {
    let (scheme, rest) = url.split_once("://")?;
    if scheme.is_empty() || rest.is_empty() {
        return None;
    }
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host = match authority.rsplit_once('@') {
        Some((_, h)) => h, // strip userinfo
        None => authority,
    };
    let host = match host.rsplit_once(':') {
        Some((h, p)) if p.bytes().all(|b| b.is_ascii_digit()) => h,
        _ => host,
    };
    let host = host.trim().to_ascii_lowercase();
    (!host.is_empty()).then_some(host)
}

/// Host of a URL, but only for real web pages. Favicons are fetched from
/// the site's own origin over HTTPS, so `gtx://`, `file://` and friends
/// must never name a host to ask.
pub fn web_host(url: &str) -> Option<String> {
    let (scheme, _) = url.split_once("://")?;
    if scheme.eq_ignore_ascii_case("http") || scheme.eq_ignore_ascii_case("https") {
        host_of(url)
    } else {
        None
    }
}

/// Percent-encode a query for use in a URL query component (`+` for spaces).
fn encode_query(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for byte in s.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const DDG: &str = "https://duckduckgo.com/?q={}";
    const HOME: &str = "https://duckduckgo.com";

    #[test]
    fn empty_goes_home() {
        assert_eq!(resolve("", DDG, HOME), HOME);
        assert_eq!(resolve("   ", DDG, HOME), HOME);
    }

    #[test]
    fn schemes_pass_through() {
        assert_eq!(resolve("https://example.com", DDG, HOME), "https://example.com");
        assert_eq!(resolve("HTTPS://Example.com", DDG, HOME), "HTTPS://Example.com");
        assert_eq!(resolve("about:blank", DDG, HOME), "about:blank");
        assert_eq!(resolve("file:///C:/x.html", DDG, HOME), "file:///C:/x.html");
        assert_eq!(resolve("view-source:https://a.b", DDG, HOME), "view-source:https://a.b");
    }

    #[test]
    fn hosts_get_https() {
        assert_eq!(resolve("example.com", DDG, HOME), "https://example.com");
        assert_eq!(resolve("example.com/path?q=1", DDG, HOME), "https://example.com/path?q=1");
        assert_eq!(resolve("localhost:8080", DDG, HOME), "https://localhost:8080");
        assert_eq!(resolve("localhost", DDG, HOME), "https://localhost");
        assert_eq!(resolve("192.168.0.1/admin", DDG, HOME), "https://192.168.0.1/admin");
        assert_eq!(resolve("docs.rs", DDG, HOME), "https://docs.rs");
    }

    #[test]
    fn phrases_search() {
        assert_eq!(resolve("rust lang", DDG, HOME), "https://duckduckgo.com/?q=rust+lang");
        assert_eq!(resolve("gtx browser", DDG, HOME), "https://duckduckgo.com/?q=gtx+browser");
        // Single word with no dot: search.
        assert_eq!(resolve("weather", DDG, HOME), "https://duckduckgo.com/?q=weather");
        // Scheme-shaped but not a known scheme: search.
        assert_eq!(resolve("rust:book", DDG, HOME), "https://duckduckgo.com/?q=rust%3Abook");
    }

    #[test]
    fn host_extraction() {
        assert_eq!(
            host_of("https://Sub.Example.com:8443/a?b=1").as_deref(),
            Some("sub.example.com")
        );
        assert_eq!(host_of("https://user:pw@example.com/x").as_deref(), Some("example.com"));
        assert_eq!(host_of("about:blank"), None);
        assert_eq!(host_of("data:text/html,<h1>x"), None);
        assert_eq!(host_of("https://"), None);
        assert_eq!(web_host("https://Example.com/x").as_deref(), Some("example.com"));
        assert_eq!(web_host("gtx://newtab"), None, "internal pages name nobody");
        assert_eq!(web_host("file://server/share"), None, "file shares are not sites");
    }

    #[test]
    fn query_encoding() {
        assert_eq!(encode_query("a b&c=d"), "a+b%26c%3Dd");
        assert_eq!(encode_query("héllo"), "h%C3%A9llo");
        assert_eq!(encode_query("safe-._~"), "safe-._~");
    }

    /// What Windows hands GTX when another app opens a link - a URL, a bare
    /// path from the .htm association, or a host - must all become something
    /// WebView2 can navigate to, and do so idempotently: the receiving side
    /// normalises whatever the launching side already normalised.
    #[test]
    fn launch_arguments_become_navigable() {
        assert_eq!(
            resolve_launch_arg("https://example.com/a?b=1", DDG),
            "https://example.com/a?b=1"
        );
        assert_eq!(resolve_launch_arg("HTTPS://example.com/", DDG), "HTTPS://example.com/");
        let win = r"C:\Users\me\My Page.htm";
        assert_eq!(
            resolve_launch_arg(&win, DDG),
            "file:///C:/Users/me/My%20Page.htm"
        );
        let unc = r"\\nas\share\a&b.htm";
        assert_eq!(resolve_launch_arg(&unc, DDG), "file://nas/share/a%26b.htm");
        assert_eq!(resolve_launch_arg("example.com/x", DDG), "https://example.com/x");
        assert_eq!(resolve_launch_arg("just words", DDG), search("just words", DDG));
        assert_eq!(resolve_launch_arg("", DDG), "");
        assert_eq!(resolve_launch_arg("   ", DDG), "");

        for arg in ["https://e.com/", win, "example.com"] {
            let once = resolve_launch_arg(arg, DDG);
            assert_eq!(resolve_launch_arg(&once, DDG), once, "not idempotent for {arg}");
        }
    }
}
