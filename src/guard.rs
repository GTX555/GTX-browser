//! Page-side guards driven by [`Config`].
//!
//! The only guard left is the popup blocker: `window.open` is allowed only
//! shortly after a real user gesture, exactly like a browser's own popup
//! settings. The former ad/tracker blocker was removed — it broke video
//! playback on sites like YouTube — so no request is ever cancelled or
//! rewritten because of a list.

use crate::config::Config;

/// The complete initialization script for a new web view, honouring the
/// current config. Empty when the popup blocker is off.
pub fn init_script(cfg: &Config) -> String {
    if cfg.block_popups {
        POPUP_GUARD.to_string()
    } else {
        String::new()
    }
}

/// `window.open` only shortly after a real user gesture; otherwise `null`,
/// exactly like a browser popup blocker.
const POPUP_GUARD: &str = r#"
(() => {
  let last = 0;
  const mark = () => { last = Date.now(); };
  for (const t of ["pointerdown", "mousedown", "keydown", "touchstart"]) {
    window.addEventListener(t, mark, true);
  }
  const orig = window.open;
  window.open = function (...args) {
    if (Date.now() - last <= 3000) return orig.apply(this, args);
    return null;
  };
})();
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_popup_guard_is_the_only_page_side_guard() {
        let js = init_script(&Config::default());
        assert!(js.contains("window.open"), "the popup guard must stay");
        assert!(js.contains("Date.now() - last"), "gated on a user gesture");
        assert!(
            !js.contains("__gtxBlock"),
            "no request blocklist: ad blocking was removed"
        );
    }

    #[test]
    fn turning_the_blocker_off_yields_no_script_at_all() {
        let mut cfg = Config::default();
        cfg.block_popups = false;
        assert!(init_script(&cfg).is_empty());
    }
}
