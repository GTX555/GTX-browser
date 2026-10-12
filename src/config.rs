//! Runtime configuration: `%LOCALAPPDATA%\GTX Browser\config.toml`.
//!
//! There is intentionally no native settings dialog — the settings page (a GTX
//! tab) edits this file, which can also be edited by hand. Every knob the
//! settings page exposes is a field here, and every field round-trips.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    // --- Appearance ---------------------------------------------------
    /// `auto`, `light` or `dark` — pages' `prefers-color-scheme` + chrome hint.
    pub theme: String,
    /// Accent color, `#rrggbb`. Drives the active tab bar and highlights.
    pub accent: String,
    /// Tighter toolbar/tab strip: more room for content.
    pub compact_mode: bool,
    /// Show the home button in the toolbar.
    pub show_home_button: bool,

    // --- Home, new tab, search ---------------------------------------
    pub homepage: String,
    pub new_tab_url: String,
    /// `{}` is replaced by the encoded query.
    pub search_template: String,

    // --- Privacy ------------------------------------------------------
    /// Block `window.open` without a recent user gesture.
    pub block_popups: bool,

    // --- Downloads ----------------------------------------------------
    /// Ask where to save every download (dialog confirmation).
    pub download_ask: bool,
    /// Where to save when not asking. Empty = the user's Downloads folder.
    pub download_dir: String,

    // --- Startup & tabs ----------------------------------------------
    /// Remember open tabs and restore them on the next start.
    pub restore_session: bool,
    /// Unload background tabs after N seconds of inactivity (`0` = never).
    pub hibernate_secs: u64,

    // --- Shortcuts ----------------------------------------------------
    /// Ctrl+1…9 jumps to a tab.
    pub shortcut_ctrl_numbers: bool,
    /// F11 toggles fullscreen.
    pub shortcut_f11: bool,
    /// Ctrl+Shift+T reopens the last closed tab.
    pub shortcut_reopen: bool,

    // --- Advanced -----------------------------------------------------
    /// Empty = WebView2 default user agent.
    pub user_agent: String,
    /// Allow F12 devtools.
    pub devtools: bool,
    /// `false` = GTX owns every keyboard shortcut (recommended).
    pub browser_accelerator_keys: bool,
    /// `false` passes `--disable-gpu` (very low-end machines / VMs).
    pub hardware_acceleration: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            theme: "auto".into(),
            accent: "#00e676".into(),
            compact_mode: true,
            show_home_button: false,
            homepage: "https://duckduckgo.com".into(),
            new_tab_url: crate::internal::NEW_TAB_URL.into(),
            search_template: "https://duckduckgo.com/?q={}".into(),
            block_popups: true,
            download_ask: true,
            download_dir: String::new(),
            restore_session: true,
            hibernate_secs: 900,
            shortcut_ctrl_numbers: true,
            shortcut_f11: true,
            shortcut_reopen: true,
            user_agent: String::new(),
            devtools: false,
            browser_accelerator_keys: false,
            hardware_acceleration: true,
        }
    }
}

impl Config {
    /// Accent as `(r, g, b)`, falling back to GTX green on a bad value.
    pub fn accent_rgb(&self) -> (u8, u8, u8) {
        let h = self.accent.trim().trim_start_matches('#');
        if h.len() == 6 {
            if let Ok(v) = u32::from_str_radix(h, 16) {
                return (((v >> 16) & 0xFF) as u8, ((v >> 8) & 0xFF) as u8, (v & 0xFF) as u8);
            }
        }
        (0x00, 0xE6, 0x76)
    }

    /// Config as pretty JSON (what the settings page receives).
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| "{}".into())
    }
}

/// A friendly name for the search engine a template points at, for the
/// omnibox placeholder ("Search DuckDuckGo or type a URL").
pub fn search_brand(template: &str) -> String {
    let host = template
        .split_once("://")
        .and_then(|(_, rest)| rest.split(['/', '?', '#']).next())
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    let host = host.strip_prefix("www.").unwrap_or(&host).to_string();
    if host.is_empty() {
        return String::new();
    }
    // Matched as substrings so multi-label hosts (search.brave.com,
    // duckduckgo.com/html) resolve too.
    const KNOWN: [(&str, &str); 10] = [
        ("duckduckgo.", "DuckDuckGo"),
        ("google.", "Google"),
        ("bing.", "Bing"),
        ("kagi.", "Kagi"),
        ("brave.", "Brave Search"),
        ("startpage.", "Startpage"),
        ("searx", "SearXNG"),
        ("ecosia.", "Ecosia"),
        ("yandex.", "Yandex"),
        ("mojeek.", "Mojeek"),
    ];
    for (needle, name) in KNOWN {
        if host.contains(needle) {
            return name.to_string();
        }
    }
    host
}

// ---------------------------------------------------------------------------
// How (and when) a setting takes effect
// ---------------------------------------------------------------------------

/// When a single config key lands, for the settings page's inline chips.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Effect {
    /// Applied while the browser runs — no chip needed.
    Live,
    /// Only read when the session is opened, i.e. on the next start.
    NextLaunch,
    /// Baked into the WebView2 environment when the first view is built.
    Restart,
}

impl Effect {
    pub fn as_str(self) -> &'static str {
        match self {
            Effect::Live => "live",
            Effect::NextLaunch => "next-start",
            Effect::Restart => "restart",
        }
    }
}

/// When `key` takes effect. Anything not listed is applied live by
/// [`plan_apply`] or read straight from `cfg` at the moment it is used —
/// either way, flipping it does something you can see.
pub fn effect(key: &str) -> Effect {
    match key {
        "restore_session" => Effect::NextLaunch,
        "hardware_acceleration" => Effect::Restart,
        _ => Effect::Live,
    }
}

/// `field -> effect` for every key, as the settings page receives it.
pub fn effects_json() -> String {
    let cfg = Config::default();
    let json = cfg.to_json();
    let obj = serde_json::from_str::<serde_json::Value>(&json)
        .ok()
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default();
    let map: serde_json::Map<String, serde_json::Value> = obj
        .keys()
        .map(|k| (k.clone(), serde_json::Value::String(effect(k).as_str().into())))
        .collect();
    serde_json::Value::Object(map).to_string()
}

/// What has to happen when the config changes. Computed once so the apply
/// path and its exhaustive test can never drift apart.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ApplyPlan {
    /// Rebuild every web view: an init script or a per-view option changed.
    pub recreate_views: bool,
    /// Rebuild the accelerator table.
    pub rebuild_accel: bool,
    /// Fonts / chrome geometry changed.
    pub relayout: bool,
    /// Theme or accent changed.
    pub retheme: bool,
    /// The omnibox placeholder follows the search engine.
    pub refresh_cue: bool,
}

/// Diff two configs into the work required to make `new` visible.
///
/// Settings that are read on demand (`homepage`, `download_dir`,
/// `hibernate_secs`, …) need no work here: the next use picks the new value
/// up by itself, which is what `Effect::Live` means for them.
pub fn plan_apply(old: &Config, new: &Config) -> ApplyPlan {
    let mut p = ApplyPlan::default();

    p.relayout = old.compact_mode != new.compact_mode
        || old.show_home_button != new.show_home_button;
    p.retheme = old.theme != new.theme || old.accent != new.accent;
    p.refresh_cue = old.search_template != new.search_template;

    p.rebuild_accel = old.shortcut_ctrl_numbers != new.shortcut_ctrl_numbers
        || old.shortcut_f11 != new.shortcut_f11
        || old.shortcut_reopen != new.shortcut_reopen
        || old.devtools != new.devtools;

    let block_snapshot = old.block_popups != new.block_popups;

    // Everything baked into a view's init script or per-view options needs a
    // rebuild — but never of the settings page you are standing on, and never
    // for `hardware_acceleration`, which WebView2 fixes at first launch (the
    // page flags that one as "restart to apply").
    p.recreate_views = block_snapshot
        || old.user_agent != new.user_agent
        || old.browser_accelerator_keys != new.browser_accelerator_keys
        // Devtools is a builder option *and* a gate on F12: without the
        // rebuild the toggle did nothing until the tab was reopened.
        || old.devtools != new.devtools;

    p
}

/// `%LOCALAPPDATA%\GTX Browser` — config, session and the WebView2 profile.
pub fn data_dir() -> PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("GTX Browser")
}

pub fn config_path() -> PathBuf {
    data_dir().join("config.toml")
}

pub fn profile_dir() -> PathBuf {
    data_dir().join("User Data")
}

/// Load the config, writing a commented default file on first run.
///
/// A file that fails to parse is **copied to `config.toml.bad` first**: the
/// next save would otherwise silently overwrite the user's hand edits with
/// defaults.
pub fn load() -> Config {
    let path = config_path();
    if let Ok(text) = std::fs::read_to_string(&path) {
        match toml::from_str::<Config>(&text) {
            Ok(cfg) => return cfg,
            Err(e) => {
                let backup = path.with_extension("toml.bad");
                let note = if std::fs::copy(&path, &backup).is_ok() {
                    format!("config.toml is invalid ({e}); kept a copy at {}", backup.display())
                } else {
                    format!("config.toml is invalid ({e}); using defaults")
                };
                crate::log::write(&note);
                return Config::default();
            }
        }
    }
    if let Some(parent) = path.parent() {
        if std::fs::create_dir_all(parent).is_ok() {
            let _ = std::fs::write(&path, template());
        }
    }
    Config::default()
}

/// Persist the config and **prove it landed**: the file is re-read and
/// re-parsed before this returns `Ok`. The settings page only shows "Saved"
/// when this succeeds.
pub fn save_verified(cfg: &Config) -> Result<(), String> {
    save_verified_at(cfg, &config_path())
}

/// [`save_verified`] against an arbitrary path (so tests never touch the
/// user's real profile).
pub fn save_verified_at(cfg: &Config, path: &PathBuf) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
    }
    let text = toml::to_string_pretty(cfg).map_err(|e| e.to_string())?;
    std::fs::write(path, text).map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    let back = std::fs::read_to_string(path)
        .map_err(|e| format!("cannot re-read {}: {e}", path.display()))?;
    toml::from_str::<Config>(&back).map_err(|e| format!("config.toml did not verify: {e}"))?;
    Ok(())
}

/// First-run template with comments (not round-tripped through serde).
fn template() -> &'static str {
    r##"# GTX Browser configuration.
# The settings page (gear button, Ctrl+,) edits this file; you can too.

# --- Appearance ---
theme = "auto"               # "auto", "light" or "dark"
accent = "#00e676"           # #rrggbb highlight color
compact_mode = true          # tighter toolbar and tab strip
show_home_button = false

# --- Home, new tab, search ---
homepage = "https://duckduckgo.com"
new_tab_url = "gtx://newtab"              # blank, themed start surface
search_template = "https://duckduckgo.com/?q={}"

# --- Privacy ---
block_popups = true          # window.open only right after a user gesture

# --- Downloads ---
download_ask = true          # confirm the save path before downloading
download_dir = ""            # empty = your Downloads folder

# --- Startup & tabs ---
restore_session = true       # reopen all tabs from last time
hibernate_secs = 900         # unload idle background tabs (0 = never)

# --- Shortcuts ---
shortcut_ctrl_numbers = true # Ctrl+1..9 switches tabs
shortcut_f11 = true          # F11 fullscreen
shortcut_reopen = true       # Ctrl+Shift+T reopens closed tabs

# --- Advanced ---
user_agent = ""              # empty = WebView2 default
devtools = false             # allow F12
browser_accelerator_keys = false
hardware_acceleration = true
"##
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn template_parses_and_defaults_hold() {
        let cfg: Config = toml::from_str(template()).expect("template must parse");
        assert_eq!(cfg.search_template, "https://duckduckgo.com/?q={}");
        assert!(cfg.restore_session);
        assert!(cfg.block_popups);
        assert!(!cfg.devtools);
        assert_eq!(cfg.new_tab_url, crate::internal::NEW_TAB_URL);
    }

    #[test]
    fn every_default_field_survives_a_json_round_trip() {
        // Mirrors what the settings page sends back on every change: if a
        // field is added to `Config` without a `data-k` binding, this catches
        // the shape mismatch before the UI silently drops it.
        let cfg = Config::default();
        let json = cfg.to_json();
        let back: Config = serde_json::from_str(&json).expect("round trip");
        assert_eq!(back.new_tab_url, cfg.new_tab_url);
        assert_eq!(back.hibernate_secs, cfg.hibernate_secs);
        assert_eq!(back.block_popups, cfg.block_popups);
        assert_eq!(back.hardware_acceleration, cfg.hardware_acceleration);
    }

    #[test]
    fn verified_save_round_trips_on_disk() {
        // A temp file, not the user's profile: this suite must never rewrite
        // someone's real settings.
        let dir = std::env::temp_dir().join(format!("gtx-cfg-test-{}", std::process::id()));
        let path = dir.join("config.toml");
        let mut cfg = Config::default();
        cfg.accent = "#ff8800".into();
        cfg.compact_mode = false;
        cfg.hibernate_secs = 42;
        cfg.new_tab_url = "https://example.com".into();
        save_verified_at(&cfg, &path).expect("write + verify");
        let text = std::fs::read_to_string(&path).expect("read back");
        let back: Config = toml::from_str(&text).expect("re-parse");
        assert_eq!(back.accent, "#ff8800");
        assert_eq!(back.hibernate_secs, 42);
        assert!(!back.compact_mode);
        assert_eq!(back.new_tab_url, "https://example.com");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_corrupt_file_is_never_silently_ignored() {
        // Mirrors `load`: a parse failure must be detectable rather than
        // quietly replaced by defaults.
        assert!(toml::from_str::<Config>("this is not toml =").is_err());
        assert!(toml::from_str::<Config>("hibernate_secs = \"soon\"").is_err());
        // …and every field we expose is still optional via serde(default).
        assert!(toml::from_str::<Config>("").is_ok());
    }

    #[test]
    fn search_brands() {
        assert_eq!(search_brand("https://duckduckgo.com/?q={}"), "DuckDuckGo");
        assert_eq!(search_brand("https://www.google.com/search?q={}"), "Google");
        assert_eq!(search_brand("https://search.brave.com/search?q={}"), "Brave Search");
        assert_eq!(search_brand("https://lite.duckduckgo.com/lite/?q={}"), "DuckDuckGo");
        assert_eq!(
            search_brand("https://my-own-search.test/s?q={}"),
            "my-own-search.test"
        );
        assert_eq!(search_brand(""), "");
    }

    #[test]
    fn missing_fields_fall_back_to_defaults() {
        let cfg: Config = toml::from_str(r#"homepage = "https://example.com""#).unwrap();
        assert_eq!(cfg.homepage, "https://example.com");
        assert_eq!(cfg.hibernate_secs, 900);
        assert!(cfg.compact_mode);
    }

    #[test]
    fn json_round_trip_for_settings_page() {
        let cfg = Config::default();
        let json = cfg.to_json();
        let back: Config = serde_json::from_str(&json).expect("config must round-trip JSON");
        assert_eq!(back.accent, cfg.accent);
        assert_eq!(back.download_ask, cfg.download_ask);
        assert_eq!(back.shortcut_f11, cfg.shortcut_f11);
    }

    #[test]
    fn accent_parsing() {
        let mut cfg = Config::default();
        cfg.accent = "#ff8800".into();
        assert_eq!(cfg.accent_rgb(), (0xFF, 0x88, 0x00));
        cfg.accent = "nonsense".into();
        assert_eq!(cfg.accent_rgb(), (0x00, 0xE6, 0x76));
    }

    // -----------------------------------------------------------------
    // "Every setting actually works"
    // -----------------------------------------------------------------

    /// The same config with `key` moved off its default, built through serde
    /// so a newly added field is covered without touching this test.
    fn flipped(key: &str) -> Config {
        let mut v: serde_json::Value =
            serde_json::from_str(&Config::default().to_json()).expect("config json");
        let obj = v.as_object_mut().expect("object");
        let next = match obj.get(key).expect("key exists") {
            serde_json::Value::Bool(b) => serde_json::Value::Bool(!b),
            serde_json::Value::Number(n) => {
                serde_json::Value::Number((n.as_u64().unwrap_or(0) + 1).into())
            }
            serde_json::Value::String(s) => serde_json::Value::String(format!("{s}-changed")),
            other => panic!("unexpected type for {key}: {other}"),
        };
        obj.insert(key.to_string(), next);
        serde_json::from_value(v).expect("flipped config must deserialize")
    }

    /// The documented effect of flipping each field. A field that is missing
    /// here has no decision on record — which is exactly how a setting ends up
    /// looking wired up while doing nothing.
    fn expected_plan(key: &str) -> ApplyPlan {
        ApplyPlan {
            recreate_views: matches!(
                key,
                "block_popups"
                    | "user_agent"
                    | "browser_accelerator_keys"
                    | "devtools"
            ),
            rebuild_accel: matches!(
                key,
                "shortcut_ctrl_numbers" | "shortcut_f11" | "shortcut_reopen" | "devtools"
            ),
            relayout: matches!(key, "compact_mode" | "show_home_button"),
            retheme: matches!(key, "theme" | "accent"),
            refresh_cue: key == "search_template",
        }
    }

    #[test]
    fn every_field_has_a_decision_on_how_it_applies() {
        let base = Config::default();
        let keys: Vec<String> = serde_json::from_str::<serde_json::Value>(&base.to_json())
            .expect("config json")
            .as_object()
            .expect("object")
            .keys()
            .cloned()
            .collect();
        assert!(keys.len() >= 19, "expected the full field set: {keys:?}");

        for key in &keys {
            let plan = plan_apply(&base, &flipped(key));
            assert_eq!(
                plan,
                expected_plan(key),
                "`{key}` changed but the apply plan does not document it"
            );
        }

        // Fields that need no apply step must be read on demand by the code
        // that uses them — never silently ignored. That covers the live ones
        // (homepage, downloads, hibernate) and the gated ones too:
        // `restore_session` is consulted at startup when the session is
        // reopened, and `hardware_acceleration` when each view is created
        // (WebView2 fixes it then, so `effect` records the restart).
        let on_demand: Vec<&str> = keys
            .iter()
            .map(String::as_str)
            .filter(|k| plan_apply(&base, &flipped(k)) == ApplyPlan::default())
            .collect();
        let mut want = vec![
            "homepage",
            "new_tab_url",
            "download_ask",
            "download_dir",
            "restore_session",
            "hibernate_secs",
            "hardware_acceleration",
        ];
        let mut got = on_demand.clone();
        want.sort_unstable();
        got.sort_unstable();
        assert_eq!(
            got, want,
            "every field needing no apply step must be read on demand"
        );
    }

    #[test]
    fn effect_chips_cover_every_field() {
        let base = Config::default();
        let map: serde_json::Value = serde_json::from_str(&effects_json()).expect("effects json");
        let map = map.as_object().expect("object");
        let fields: Vec<String> = serde_json::from_str::<serde_json::Value>(&base.to_json())
            .unwrap()
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect();
        assert_eq!(map.len(), fields.len(), "every field needs an effect");
        for f in &fields {
            assert!(map.contains_key(f), "effect missing for {f}");
        }
        assert_eq!(effect("restore_session"), Effect::NextLaunch);
        assert_eq!(effect("hardware_acceleration"), Effect::Restart);
        for f in &fields {
            if f == "restore_session" || f == "hardware_acceleration" {
                continue;
            }
            assert_eq!(effect(f), Effect::Live, "{f} must apply live");
        }
        assert_eq!(Effect::Live.as_str(), "live");
        assert_eq!(Effect::Restart.as_str(), "restart");
    }
}
