//! The settings page: a GTX tab (no native dialog) that round-trips the whole
//! [`Config`] over wry's IPC.
//!
//! Flow: page sends `{"a":"get"}` on load → Rust replies with
//! `window.__gtxApply(<config json>)`. Changes are auto-saved: the control
//! collects the full config and sends `{"a":"save","cfg":{…}}` → Rust
//! validates it with serde, persists it, applies it live, and acks with
//! `window.__gtxAck()`. The footer's `Save settings` button flushes a pending
//! edit (and retries a failed one) without waiting for the debounce.
//! `{"a":"reset"}` asks Rust for a confirmation, restores defaults and acks
//! with `window.__gtxReset()` — never `__gtxAck`, so the footer can say
//! "Reset to defaults" instead of claiming the old settings were saved.
//!
//! Layout is a flat list: system colours (`Canvas` / `CanvasText`) so the page
//! tracks the browser's theme, hairline dividers instead of cards, and a
//! horizontal chip bar instead of a sidebar.

/// URL shown in the address bar for the settings tab.
pub const TAB_URL: &str = "gtx://settings";
pub const TAB_TITLE: &str = "Settings";

pub const HTML: &str = r##"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<title>Settings — GTX Browser</title>
<style>
  :root {
    --accent: #00e676;
    --line: color-mix(in srgb, CanvasText 13%, transparent);
    --hover: color-mix(in srgb, CanvasText 6%, transparent);
    --muted: color-mix(in srgb, CanvasText 62%, transparent);
    --field: color-mix(in srgb, CanvasText 7%, transparent);
    color-scheme: light dark;
  }
  * { box-sizing: border-box; }
  html, body { height: 100%; margin: 0; }
  body {
    display: flex; flex-direction: column;
    background: Canvas; color: CanvasText;
    font: 14px/1.5 "Segoe UI", system-ui, -apple-system, sans-serif;
    -webkit-font-smoothing: antialiased;
  }

  /* ---- chip bar (replaces the sidebar) ---- */
  header {
    flex: none; display: flex; align-items: center; gap: 18px;
    padding: 9px 20px; background: Canvas; border-bottom: 1px solid var(--line);
  }
  .brand { font-size: 15px; font-weight: 700; letter-spacing: .2px; white-space: nowrap; }
  .brand b { color: var(--accent); }
  .brand span { color: var(--muted); font-weight: 400; margin-left: 5px; font-size: 13px; }
  nav { display: flex; gap: 4px; overflow-x: auto; scrollbar-width: none; }
  nav::-webkit-scrollbar { display: none; }
  nav button {
    all: unset; cursor: pointer; white-space: nowrap;
    font-size: 13px; padding: 5px 12px; border-radius: 999px; color: var(--muted);
  }
  nav button:hover { background: var(--hover); color: CanvasText; }
  nav button.on {
    background: color-mix(in srgb, var(--accent) 22%, transparent);
    color: CanvasText; font-weight: 600;
    box-shadow: inset 0 0 0 1px color-mix(in srgb, var(--accent) 60%, transparent);
  }

  /* ---- content ---- */
  main { flex: 1; overflow-y: auto; }
  .wrap { max-width: 720px; margin: 0 auto; padding: 26px 24px 48px; }
  section { display: none; }
  section.on { display: block; }
  h1 { font-size: 21px; font-weight: 600; margin: 0 0 2px; letter-spacing: -.2px; }
  .sub { color: var(--muted); font-size: 13px; margin: 0 0 6px; }
  h2 {
    font-size: 11.5px; font-weight: 600; text-transform: uppercase; letter-spacing: .7px;
    color: var(--muted); margin: 28px 0 0; padding-bottom: 6px;
  }

  .row {
    display: flex; align-items: center; justify-content: space-between; gap: 24px;
    padding: 13px 8px; margin: 0 -8px; border-bottom: 1px solid var(--line);
  }
  .row:hover { background: var(--hover); }
  .row:last-child { border-bottom: none; }
  .row.col { display: block; }
  .lbl { font-size: 14px; }
  .hint { color: var(--muted); font-size: 12.5px; margin-top: 2px; max-width: 46ch; }

  input[type=text], select, textarea {
    background: var(--field); border: 1px solid var(--line); color: CanvasText;
    border-radius: 6px; padding: 6px 9px; font: inherit; min-width: 230px;
  }
  textarea { min-width: 100%; min-height: 78px; resize: vertical; font-family: Consolas, monospace; font-size: 13px; }
  input:focus, select:focus, textarea:focus { outline: none; border-color: var(--accent); }
  input[type=color] {
    width: 42px; height: 30px; padding: 3px; background: var(--field);
    border: 1px solid var(--line); border-radius: 6px; cursor: pointer;
  }
  select { cursor: pointer; }
  option { background: Canvas; color: CanvasText; }

  /* switch */
  .sw { position: relative; display: inline-block; width: 36px; height: 20px; flex: none; }
  .sw input { opacity: 0; width: 0; height: 0; }
  .sw i {
    position: absolute; inset: 0; background: var(--field);
    border: 1px solid var(--line); border-radius: 20px; transition: .15s;
  }
  .sw i::before {
    content: ""; position: absolute; width: 13px; height: 13px; left: 3px; top: 2.5px;
    background: var(--muted); border-radius: 50%; transition: .15s;
  }
  .sw input:checked + i { background: color-mix(in srgb, var(--accent) 45%, transparent); border-color: var(--accent); }
  .sw input:checked + i::before { transform: translateX(16px); background: var(--accent); }
  .sw input:disabled + i { opacity: .4; }

  .presets { display: inline-flex; gap: 6px; margin-left: 10px; vertical-align: middle; }
  .presets button {
    all: unset; width: 18px; height: 18px; border-radius: 50%; cursor: pointer;
    border: 2px solid transparent; box-shadow: 0 0 0 1px var(--line);
  }
  .presets button:hover { border-color: CanvasText; }

  /* a setting that only lands on the next launch / restart */
  .chip {
    display: none; align-items: center; gap: 5px; font-size: 11px; font-weight: 600;
    padding: 2px 9px; border-radius: 999px; white-space: nowrap;
    background: color-mix(in srgb, #ffb020 22%, transparent);
    border: 1px solid color-mix(in srgb, #ffb020 65%, transparent);
    color: CanvasText;
  }
  .chip.on { display: inline-flex; }

  table { border-collapse: collapse; width: 100%; margin-top: 4px; }
  td, th { text-align: left; padding: 8px 4px; border-bottom: 1px solid var(--line); }
  th { color: var(--muted); font-size: 11px; text-transform: uppercase; letter-spacing: .6px; }
  kbd {
    background: var(--field); border: 1px solid var(--line); border-bottom-width: 2px;
    border-radius: 4px; padding: 1px 6px; font: 12px Consolas, monospace; white-space: nowrap;
  }
  code { background: var(--field); border: 1px solid var(--line); border-radius: 4px; padding: 1px 5px; font: 12px Consolas, monospace; }
  .warn { color: #d08700; }
  @media (prefers-color-scheme: dark) { .warn { color: #ffca5e; } }

  .btn {
    all: unset; cursor: pointer; font-size: 13px; font-weight: 600;
    padding: 7px 15px; border-radius: 6px; text-align: center;
  }
  .btn.primary { background: var(--accent); color: #06240f; }
  .btn.ghost { border: 1px solid var(--line); color: CanvasText; font-weight: 400; }
  .btn.ghost:hover { background: var(--hover); }
  .btn:hover { filter: brightness(1.06); }
  .btn:disabled { opacity: .45; cursor: default; filter: none; }
  .actions { display: flex; gap: 8px; flex-wrap: wrap; margin-top: 10px; }

  /* ---- footer ---- */
  footer {
    flex: none; display: flex; align-items: center; justify-content: space-between; gap: 16px;
    padding: 10px 20px; background: Canvas; border-top: 1px solid var(--line);
  }
  .right { display: flex; align-items: center; gap: 14px; }
  .status { font-size: 12.5px; color: var(--muted); }
  .status.ok { color: color-mix(in srgb, var(--accent) 70%, CanvasText); font-weight: 600; }
  .status.bad { color: #e5484d; font-weight: 600; }
</style>
</head>
<body>
<header>
  <div class="brand"><b>GTX</b><span>Settings</span></div>
  <nav>
    <button data-s="general" class="on">General</button>
    <button data-s="appearance">Appearance</button>
    <button data-s="privacy">Privacy</button>
    <button data-s="downloads">Downloads</button>
    <button data-s="tabs">Tabs &amp; Startup</button>
    <button data-s="search">Search</button>
    <button data-s="shortcuts">Shortcuts</button>
    <button data-s="advanced">Advanced</button>
  </nav>
</header>

<main><div class="wrap">

  <section id="s-general" class="on">
    <h1>General</h1>
    <p class="sub">What GTX does when Windows asks which program should open a link.</p>

    <h2>Default browser</h2>
    <div class="row"><div><div class="lbl">GTX Browser</div><div class="hint">GTX opens web links (http, https) and .htm pages from other apps</div></div>
      <div class="right"><span class="status" id="default-status">-</span><button class="btn primary" id="set-default">Set as default</button></div></div>
  </section>

  <section id="s-appearance">
    <h1>Appearance</h1>
    <p class="sub">How GTX looks on your desktop and how pages are told to look.</p>

    <h2>Theme</h2>
    <div class="row"><div><div class="lbl">Page color scheme</div><div class="hint">Sent to websites as prefers-color-scheme, and applied to GTX itself</div></div>
      <select data-k="theme"><option value="auto">Follow Windows</option><option value="dark">Dark</option><option value="light">Light</option></select></div>
    <div class="row"><div><div class="lbl">Accent color</div><div class="hint">Active tab bar, highlights and this page's controls</div></div>
      <div><input type="color" data-k="accent"><span class="presets">
        <button data-accent="#00e676" style="background:#00e676" title="GTX green"></button>
        <button data-accent="#4dabf7" style="background:#4dabf7" title="Blue"></button>
        <button data-accent="#ff6b6b" style="background:#ff6b6b" title="Red"></button>
        <button data-accent="#ffd43b" style="background:#ffd43b" title="Yellow"></button>
        <button data-accent="#c084fc" style="background:#c084fc" title="Violet"></button>
        <button data-accent="#f8f9fa" style="background:#f8f9fa" title="White"></button>
      </span></div></div>

    <h2>Toolbar</h2>
    <div class="row"><div><div class="lbl">Compact mode</div><div class="hint">Smaller tabs and toolbar - more room for pages</div></div><label class="sw"><input type="checkbox" data-k="compact_mode"><i></i></label></div>
    <div class="row"><div><div class="lbl">Show home button</div><div class="hint">Adds a home button next to reload</div></div><label class="sw"><input type="checkbox" data-k="show_home_button"><i></i></label></div>
  </section>

  <section id="s-privacy">
    <h1>Privacy</h1>
    <p class="sub">Blocking happens locally, inside GTX. Nothing is sent anywhere.</p>

    <h2>Popups</h2>
    <div class="row"><div><div class="lbl">Block pop-ups</div><div class="hint">window.open only works right after a click or key press</div></div><label class="sw"><input type="checkbox" data-k="block_popups"><i></i></label></div>
  </section>

  <section id="s-downloads">
    <h1>Downloads</h1>
    <p class="sub">Recent downloads are also listed under the downloads button in the toolbar.</p>

    <h2>Behaviour</h2>
    <div class="row"><div><div class="lbl">Ask where to save each file</div><div class="hint">Confirm the path in a dialog before anything downloads</div></div><label class="sw"><input type="checkbox" data-k="download_ask"><i></i></label></div>
    <div class="row col"><div class="lbl">Download folder</div><div class="hint" style="margin-bottom:8px">Used when "ask" is off. Empty = your Downloads folder.</div>
      <input type="text" data-k="download_dir" spellcheck="false" placeholder="C:\Users\you\Downloads" style="width:100%">
      <div class="actions">
        <button class="btn ghost" id="use-downloads">Use my Downloads folder</button>
        <button class="btn ghost" id="open-downloads">Open this folder now</button>
      </div>
    </div>
  </section>

  <section id="s-tabs">
    <h1>Tabs &amp; Startup</h1>
    <p class="sub">What GTX opens with, and what it does with idle tabs.</p>

    <h2>Startup</h2>
    <div class="row"><div><div class="lbl">Remember my tabs <span class="chip" data-chip="restore_session">next start</span></div><div class="hint">Reopen every tab from last time when GTX starts</div></div><label class="sw"><input type="checkbox" data-k="restore_session"><i></i></label></div>
    <div class="row"><div><div class="lbl">Hibernate background tabs</div><div class="hint">Release a tab's memory after it stays in the background</div></div>
      <select data-k="hibernate_secs" data-num><option value="0">Never</option><option value="300">After 5 minutes</option><option value="900">After 15 minutes</option><option value="1800">After 30 minutes</option><option value="7200">After 2 hours</option></select></div>

    <h2>Pages</h2>
    <div class="row col"><div class="lbl">Home page</div><div class="hint">Opened by Alt+Home and the home button</div>
      <input type="text" data-k="homepage" spellcheck="false" style="width:100%;margin-top:8px"></div>
    <div class="row col"><div class="lbl">New tab page</div><div class="hint">gtx://newtab for the blank start page, or any address</div>
      <input type="text" data-k="new_tab_url" spellcheck="false" style="width:100%;margin-top:8px"></div>
  </section>

  <section id="s-search">
    <h1>Search</h1>
    <p class="sub">Used when you type words instead of an address. Your typing also autocompletes from history.</p>

    <h2>Address bar</h2>
    <div class="row"><div><div class="lbl">Search engine</div><div class="hint">The address bar searches with this engine</div></div>
      <select id="engine"><option value="duckduckgo">DuckDuckGo</option><option value="google">Google</option><option value="brave">Brave Search</option><option value="kagi">Kagi</option><option value="custom">Custom...</option></select></div>
    <div class="row col"><div class="lbl">Search URL template</div><div class="hint" style="margin-bottom:8px"><code>&#123;&#125;</code> is replaced with your query</div>
      <input type="text" data-k="search_template" spellcheck="false" style="width:100%"></div>
  </section>

  <section id="s-shortcuts">
    <h1>Shortcuts</h1>
    <p class="sub">Turn individual GTX shortcuts off without giving up the rest.</p>

    <h2>Switches</h2>
    <div class="row"><div class="lbl">Ctrl + 1...9 jumps to tab</div><label class="sw"><input type="checkbox" data-k="shortcut_ctrl_numbers"><i></i></label></div>
    <div class="row"><div class="lbl">F11 fullscreen</div><label class="sw"><input type="checkbox" data-k="shortcut_f11"><i></i></label></div>
    <div class="row"><div class="lbl">Ctrl + Shift + T reopens closed tabs</div><label class="sw"><input type="checkbox" data-k="shortcut_reopen"><i></i></label></div>

    <h2>All shortcuts</h2>
    <table>
      <tr><th>Keys</th><th>Action</th></tr>
      <tr><td><kbd>Ctrl</kbd>+<kbd>T</kbd> / <kbd>Ctrl</kbd>+<kbd>N</kbd></td><td>New tab</td></tr>
      <tr><td><kbd>Ctrl</kbd>+<kbd>W</kbd></td><td>Close tab</td></tr>
      <tr><td><kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>T</kbd></td><td>Reopen closed tab</td></tr>
      <tr><td><kbd>Ctrl</kbd>+<kbd>Tab</kbd> / <kbd>Ctrl</kbd>+<kbd>PgDn</kbd></td><td>Next tab</td></tr>
      <tr><td><kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>Tab</kbd> / <kbd>Ctrl</kbd>+<kbd>PgUp</kbd></td><td>Previous tab</td></tr>
      <tr><td><kbd>Ctrl</kbd>+<kbd>1</kbd>...<kbd>9</kbd></td><td>Jump to tab</td></tr>
      <tr><td><kbd>Ctrl</kbd>+<kbd>L</kbd></td><td>Focus address bar</td></tr>
      <tr><td><kbd>Ctrl</kbd>+<kbd>R</kbd> / <kbd>F5</kbd></td><td>Reload</td></tr>
      <tr><td><kbd>Alt</kbd>+<kbd>&larr;</kbd> / <kbd>Alt</kbd>+<kbd>&rarr;</kbd></td><td>Back / forward</td></tr>
      <tr><td><kbd>Alt</kbd>+<kbd>Home</kbd></td><td>Home page</td></tr>
      <tr><td><kbd>Ctrl</kbd>+<kbd>,</kbd></td><td>Settings</td></tr>
      <tr><td><kbd>F11</kbd></td><td>Fullscreen</td></tr>
      <tr><td><kbd>Ctrl</kbd>+<kbd>+</kbd> / <kbd>-</kbd> / <kbd>0</kbd></td><td>Zoom in / out / reset</td></tr>
      <tr><td><kbd>F12</kbd></td><td>Devtools <span class="hint">(if enabled)</span></td></tr>
    </table>
  </section>

  <section id="s-advanced">
    <h1>Advanced</h1>
    <p class="sub">Engine switches and where GTX keeps its files.</p>

    <h2>Engine</h2>
    <div class="row"><div><div class="lbl">Hardware acceleration <span class="chip" id="restart">restart to apply</span></div><div class="hint">Turn off on very low-end machines or VMs</div></div><label class="sw"><input type="checkbox" data-k="hardware_acceleration"><i></i></label></div>
    <div class="row"><div><div class="lbl">Developer tools</div><div class="hint">Allow F12 - applies to open tabs immediately</div></div><label class="sw"><input type="checkbox" data-k="devtools"><i></i></label></div>
    <div class="row"><div><div class="lbl">WebView2 browser shortcut keys</div><div class="hint warn">Off = GTX owns every shortcut (recommended)</div></div><label class="sw"><input type="checkbox" data-k="browser_accelerator_keys"><i></i></label></div>
    <div class="row col"><div class="lbl">Custom user agent</div><div class="hint" style="margin-bottom:8px">Empty = WebView2 default. Applies to open and new tabs.</div>
      <input type="text" data-k="user_agent" spellcheck="false" style="width:100%"></div>

    <h2>Files</h2>
    <div class="row"><div><div class="lbl">Config</div><div class="hint">Hand-edit it if you prefer - GTX re-reads it on the next start</div></div><code id="path-config">-</code></div>
    <div class="row"><div><div class="lbl">History</div><div class="hint">Also the source of address bar autocomplete</div></div><code id="path-history">-</code></div>
    <div class="row"><div><div class="lbl">Profile</div><div class="hint">Cookies and site data, kept by WebView2</div></div><code id="path-profile">-</code></div>
    <div class="row"><div><div class="lbl">Log</div><div class="hint" id="path-log">-</div></div><button class="btn ghost" id="copy-log">Copy log path</button></div>

    <h2>About</h2>
    <div class="row"><div><div class="lbl">GTX Browser</div><div class="hint" id="about-ver">version -</div></div></div>
  </section>

</div></main>

<footer>
  <button class="btn ghost" id="reset">Reset all settings</button>
  <div class="right">
    <span class="status" id="saved">Loading...</span>
    <button class="btn primary" id="save">Save settings</button>
  </div>
</footer>

<script>
  let cfg = null;      // config as last edited here
  let applied = null;  // config Rust last confirmed is on disk
  let dirty = false;   // an edit is waiting for its save to land
  const post = m => window.ipc.postMessage(JSON.stringify(m));
  const $ = s => document.querySelector(s);

  const ENGINES = {
    duckduckgo: "https://duckduckgo.com/?q={}",
    google: "https://www.google.com/search?q={}",
    brave: "https://search.brave.com/search?q={}",
    kagi: "https://kagi.com/search?q={}",
  };

  // Baked into the WebView2 environment when the browser starts.
  // Effects (live / next-start / restart) come from Rust so the chips can
  // never disagree with how a setting is actually applied.

  function status(msg, cls) {
    const el = $("#saved");
    el.textContent = msg;
    el.className = "status" + (cls ? " " + cls : "");
  }

  // A chip shows while a value differs from what Rust confirmed is on disk,
  // i.e. while there is a change that still has to land.
  function syncChips() {
    if (!applied || !cfg || !document._effects) return;
    for (const [k, v] of Object.entries(document._effects)) {
      if (v === "live") continue;
      const el = document.querySelector(`[data-chip="${k}"]`);
      if (el) el.classList.toggle("on", String(applied[k]) !== String(cfg[k]));
    }
  }

  function syncEngine() {
    const tpl = document.querySelector('[data-k="search_template"]').value;
    const match = Object.entries(ENGINES).find(([, v]) => v === tpl);
    $("#engine").value = match ? match[0] : "custom";
  }

  // Rust hands these callbacks *objects* (their JSON is parsed by the
  // evaluator, not by us). `j` also accepts a string, so a shape mismatch can
  // never kill the page again: with cfg left null, every control silently
  // did nothing and no setting ever saved.
  const j = v => (typeof v === "string" ? JSON.parse(v) : v);

  window.__gtxApply = (json, meta) => {
    cfg = j(json);
    applied = j(json);
    document.documentElement.style.setProperty("--accent", cfg.accent);
    for (const el of document.querySelectorAll("[data-k]")) {
      const k = el.dataset.k;
      if (!(k in cfg)) continue;
      if (el.type === "checkbox") el.checked = !!cfg[k];
      else if (el.type === "color") el.value = /^#[0-9a-fA-F]{6}$/.test(cfg[k]) ? cfg[k] : "#00e676";
      else el.value = cfg[k] == null ? "" : String(cfg[k]);
    }
    for (const el of document.querySelectorAll("[data-chip]")) {
      // text is filled in below from the Rust-provided effect map
      const k = el.dataset.chip;
      const v = (meta && meta.effects && meta.effects[k]) || "live";
      el.textContent = v === "restart" ? "restart to apply" : "next start";
    }
    if (meta) {
      document._effects = meta.effects || null;
      $("#about-ver").textContent = "version " + meta.version + " - WebView2 engine";
      $("#path-config").textContent = meta.config;
      $("#path-log").textContent = meta.log;
      $("#path-history").textContent = meta.history;
      $("#path-profile").textContent = meta.profile;
      document._logPath = meta.log;
      paintDefault(meta);
    }
    syncEngine();
    syncChips();
    dirty = false;
    status("All changes saved");
  };

  // Rust only says "saved" after config.toml was written *and re-read*.
  window.__gtxAck = json => {
    if (json) { try { applied = j(json); } catch (e) { /* keep baseline */ } }
    dirty = false;
    syncChips();
    status("All changes saved");
  };

  window.__gtxErr = e => {
    const m = (e && e.err) || (typeof e === "string" ? e : "unknown error");
    dirty = true;
    status("Not saved - " + m, "bad");
  };

  // Rust confirms the reset *after* the defaults landed on disk.
  window.__gtxReset = json => {
    if (json) { try { applied = j(json); } catch (e) { /* keep baseline */ } }
    dirty = false;
    syncChips();
    status("Reset to defaults", "ok");
    clearTimeout(window._resetT);
    window._resetT = setTimeout(() => { if (!dirty) status("All changes saved"); }, 4000);
  };

  // Default browser: Rust asks Windows (registration + the one sanctioned
  // API) and reports back either "it is yours now" or "the page is open".
  function paintDefault(state, msg, bad) {
    const isDef = !!(state && state.default_browser);
    const el = $("#default-status");
    el.textContent = msg || (isDef ? "GTX is your default browser" : "Not the default browser");
    el.className = "status" + (bad ? " bad" : isDef ? " ok" : "");
    $("#set-default").disabled = isDef;
    $("#set-default").textContent = isDef ? "Default \u{2713}" : "Set as default";
  }

  window.__gtxDefault = r => paintDefault(r, r && r.msg, !!(r && r.err));

  function collect() {
    if (!cfg) return;
    for (const el of document.querySelectorAll("[data-k]")) {
      const k = el.dataset.k;
      if (el.type === "checkbox") cfg[k] = el.checked;
      else if (el.type === "color") cfg[k] = el.value;
      else if (el.dataset.num !== undefined) cfg[k] = Number(el.value);
      else cfg[k] = el.value;
    }
    document.documentElement.style.setProperty("--accent", cfg.accent);
    syncEngine();
    syncChips();
    dirty = true;
    status("Saving...");
    post({ a: "save", cfg });
  }

  // Flush a pending debounced edit right now (the Save settings button).
  function saveNow() {
    clearTimeout(deb);
    collect();
  }

  // Navigation: chips switch sections.
  document.querySelectorAll("nav button").forEach(b => b.addEventListener("click", () => {
    document.querySelectorAll("nav button").forEach(x => x.classList.toggle("on", x === b));
    document.querySelectorAll("section").forEach(s => s.classList.toggle("on", s.id === "s-" + b.dataset.s));
  }));

  // Controls: everything auto-saves.
  document.addEventListener("change", e => {
    if (e.target.id === "engine") {
      const v = e.target.value;
      if (ENGINES[v]) document.querySelector('[data-k="search_template"]').value = ENGINES[v];
    }
    collect();
  });

  let deb;
  document.addEventListener("input", e => {
    if (e.target.type === "text" || e.target.tagName === "TEXTAREA") {
      dirty = true;
      status("Unsaved changes...");
      clearTimeout(deb);
      deb = setTimeout(collect, 350);
    }
    if (e.target.type === "color") {
      document.documentElement.style.setProperty("--accent", e.target.value);
      dirty = true;
      status("Unsaved changes...");
      clearTimeout(deb);
      deb = setTimeout(collect, 350);
    }
  });

  document.querySelectorAll(".presets button").forEach(b =>
    b.addEventListener("click", () => {
      document.querySelector('[data-k="accent"]').value = b.dataset.accent;
      collect();
    }));

  // Footer actions.
  $("#save").addEventListener("click", saveNow);
  $("#reset").addEventListener("click", () => post({ a: "reset" }));

  // Downloads helpers.
  $("#use-downloads").addEventListener("click", () => {
    document.querySelector('[data-k="download_dir"]').value = "";
    collect();
  });
  $("#open-downloads").addEventListener("click", () => post({ a: "opendir" }));

  // Default browser: hand it to Rust, then show whatever Windows answered.
  $("#set-default").addEventListener("click", () => {
    $("#default-status").textContent = "Asking Windows...";
    $("#default-status").className = "status";
    $("#set-default").disabled = true;
    post({ a: "defaultbrowser" });
  });
  $("#copy-log").addEventListener("click", () => {
    status("Copying...", "");
    post({ a: "copylog" });
  });
  // Rust owns the clipboard: the Web clipboard API is not dependable from a
  // data: document, and the reply always comes back through this callback.
  window.__gtxCopied = ok => status(ok ? "Log path copied" : "Could not copy", ok ? "ok" : "bad");

  post({ a: "get" });
</script>
</body>
</html>
"##;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    /// Every `data-k` the page writes back must be a real `Config` field, and
    /// **every** `Config` field must have a control — otherwise `serde`
    /// rejects the save, or a knob exists on disk that the UI silently drops.
    #[test]
    fn every_config_field_is_bound_and_every_binding_is_real() {
        let json = Config::default().to_json();
        let fields: Vec<String> = serde_json::from_str::<serde_json::Value>(&json)
            .expect("config json")
            .as_object()
            .expect("config json object")
            .keys()
            .cloned()
            .collect();
        assert!(fields.len() >= 19, "expected the full field set: {fields:?}");

        let mut keys: Vec<&str> = Vec::new();
        let mut rest = HTML;
        while let Some(i) = rest.find("data-k=\"") {
            rest = &rest[i + 8..];
            let end = rest.find('"').expect("quoted data-k");
            keys.push(&rest[..end]);
        }
        // Template literals in the script produce `[data-k="${k}"]`, which is
        // a placeholder rather than a key.
        keys.retain(|k| !k.contains('$') && !k.contains('{'));
        keys.sort_unstable();
        keys.dedup();

        for k in &keys {
            assert!(
                json.contains(&format!("\"{k}\"")),
                "settings control `{k}` has no matching Config field"
            );
        }
        for f in &fields {
            assert!(
                keys.contains(&f.as_str()),
                "Config field `{f}` has no control on the settings page"
            );
        }
    }

    #[test]
    fn page_speaks_the_full_protocol() {
        assert!(HTML.starts_with("<!doctype html>"));
        for marker in [
            "window.__gtxApply",
            "window.__gtxAck",
            "window.__gtxErr",
            "window.__gtxReset",
            "a: \"get\"",
            "a: \"save\"",
            "a: \"reset\"",
            "a: \"copylog\"",
            "a: \"opendir\"",
            "a: \"defaultbrowser\"",
            "window.__gtxDefault",
            "window.__gtxCopied",
            "const j = v =>",
            "id=\"set-default\"",
            "id=\"default-status\"",
            "id=\"restart\"",
            "id=\"save\"",
            "Reset all settings",
            "Save settings",
        ] {
            assert!(HTML.contains(marker), "settings page is missing `{marker}`");
        }
    }

    /// The contract that silently broke: Rust interpolates raw JSON into the
    /// call (`__gtxApply({...}, {...})`), so the handlers must accept
    /// *objects*. Parsing an object threw, `cfg` stayed null, `collect()`
    /// early-returned, and the whole page became decorative.
    #[test]
    fn the_callbacks_take_the_objects_rust_sends() {
        let apply = HTML
            .split("window.__gtxApply =")
            .nth(1)
            .and_then(|s| s.split("window.__gtxAck").next())
            .expect("__gtxApply handler");
        assert!(
            apply.contains("j(json)"),
            "__gtxApply must go through the tolerant parser"
        );
        assert!(
            !apply.contains("JSON.parse(json)"),
            "JSON.parse on an object argument throws and kills the page"
        );

        const APP: &str = include_str!("app.rs");
        assert!(APP.contains("__gtxApply({}, {})"), "Rust sends raw objects");
        assert!(APP.contains("__gtxAck({})"), "Rust sends raw objects");
        assert!(APP.contains("__gtxReset({})"), "Rust sends raw objects");
    }

    /// "Copy log path" must go through Rust: navigator.clipboard is not
    /// dependable from a data: document, and a dead button is exactly the
    /// kind of whole-page breakage this page has had before.
    #[test]
    fn the_copy_log_button_is_answered_by_rust() {
        assert!(HTML.contains("a: \"copylog\""));
        assert!(HTML.contains("window.__gtxCopied ="));
        assert!(
            !HTML.contains("navigator.clipboard"),
            "the page must not own the clipboard"
        );

        const APP: &str = include_str!("app.rs");
        let arm = APP
            .split("\"copylog\" =>")
            .nth(1)
            .and_then(|s| s.split("_ =>").next())
            .expect("the copylog arm of handle_ipc");
        assert!(arm.contains("copy_to_clipboard"), "Rust must actually copy");
        assert!(arm.contains("__gtxCopied"), "the button needs its reply");
    }

    /// The redesign must track the browser's theme rather than shipping its
    /// own palette: system colours + `color-scheme: light dark`, and no
    /// hardcoded dark background left over from the old card layout.
    #[test]
    fn colours_follow_the_browser_theme() {
        assert!(HTML.contains("color-scheme: light dark"));
        assert!(HTML.contains("background: Canvas"));
        assert!(HTML.contains("color: CanvasText"));
        for stale in ["#14161a", "#1b1e24", "#23272f", "#f5f6f8"] {
            assert!(
                !HTML.contains(stale),
                "hardcoded palette colour {stale} survived the redesign"
            );
        }
    }

    /// Reset must never be acknowledged with the same callback as a save —
    /// that is what made "Reset all settings" look like it saved everything.
    /// (The Rust side of the guarantee is tested next to `reset_ack_js`.)
    #[test]
    fn reset_has_its_own_acknowledgement() {
        assert!(HTML.contains("window.__gtxReset"));
        assert!(HTML.contains("Reset to defaults"));
        // The save path must not be the one that reports a reset: the first
        // status a reset produces has to be its own, not "All changes saved".
        let reset_handler = HTML
            .split("window.__gtxReset =")
            .nth(1)
            .expect("reset handler");
        let first_status = reset_handler
            .find("status(")
            .map(|i| &reset_handler[i..i + 60])
            .expect("the reset handler reports a status");
        assert!(
            first_status.starts_with("status(\"Reset to defaults\""),
            "a reset must announce itself, got {first_status:?}"
        );
    }

    /// `include_str!` so this file can check what the Rust handler sends back.
    #[test]
    fn app_sends_the_reset_ack_not_the_save_ack() {
        const APP: &str = include_str!("app.rs");
        let arm = APP
            .split("\"reset\" =>")
            .nth(1)
            .and_then(|s| s.split("\"opendir\"").next())
            .expect("the reset arm of handle_ipc");
        assert!(
            arm.contains("reset_ack_js("),
            "reset must acknowledge with __gtxReset"
        );
        assert!(
            !arm.contains("window.__gtxAck && window.__gtxAck"),
            "reset must not claim the previous settings were saved"
        );
    }

    /// The default-browser button is only half the feature: Rust must answer
    /// it with its own callback, carrying both the new status and a sentence.
    #[test]
    fn the_default_browser_button_is_answered_by_rust() {
        assert!(HTML.contains("post({ a: \"defaultbrowser\" })"));
        assert!(HTML.contains("window.__gtxDefault ="));

        const APP: &str = include_str!("app.rs");
        let arm = APP
            .split("\"defaultbrowser\" =>")
            .nth(1)
            .and_then(|s| s.split("_ =>").next())
            .expect("the defaultbrowser arm of handle_ipc");
        assert!(
            arm.contains("window.__gtxDefault && window.__gtxDefault"),
            "the button must be answered with __gtxDefault"
        );
        assert!(
            arm.contains("status_after_attempt()"),
            "Rust must actually ask Windows"
        );
    }
}
