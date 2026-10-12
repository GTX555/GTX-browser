# GTX Browser

An extremely lightweight, no-bloat web browser for Windows. One small native
process around the WebView2 engine Windows already ships — no Electron, no
bundled Chromium, no telemetry, no updater, no extension store.

Modern-but-minimal chrome: a **frameless single row** — back / forward /
reload, a rounded omnibox with "Search DuckDuckGo or type a URL", inline tab
pills, `+`, a kebab menu, and drawn caption buttons — over a dark charcoal
palette that follows the system theme.

## Measured on Windows 11

| Metric | Result | Budget |
|---|---|---|
| `gtx-browser.exe` (release, LTO + strip) | **986 KB** | ≤ 8 MB |
| `gtx-setup.exe` (installer + wizard) | 325 KB | ≤ 1 MB |
| GTX shell, private memory | **3.7 MB** | ≤ 15 MB |
| GTX shell, working set | ~26 MB (mostly shared engine DLLs) | — |
| Cold start → window visible | **46–161 ms** | ≤ 1 s |
| Threads | 10 | — |
| Runtime crates | 5 (`wry`, `windows`, `serde`, `serde_json`, `toml`) | ≤ 5 |
| Unit tests | 95 passing | — |

The page-rendering processes (`msedgewebview2.exe`, ~500 MB tree with a modern
site loaded) are Windows' WebView2 runtime — the same engine as Microsoft Edge.
GTX adds only the shell above instead of Electron's extra 100–300 MB.

## Features

**Chrome** — frameless window (custom `WM_NCCALCSIZE`, DWM rounded corners +
immersive dark mode, real edge/resize hit-testing), drawn caption buttons with
red close hover, drag-by-empty-space, maximized into the work area.

- Single-row child window: nav buttons, magnifier + cue-banner omnibox,
  inline tab pills (accent underline, hover ×), `+`, kebab menu
- Tabs: lazy creation, close with ×/middle-click/Ctrl+W, reopen closed tab,
  Ctrl+1–9 select, background tabs **hibernate** after `hibernate_secs`
- One instance per profile: a link opened from another app is handed to the
  window that is already running (WM_COPYDATA) instead of starting a second GTX
- Address bar resolves URL-vs-search (DuckDuckGo default; Google, Bing, Brave
  Search, Kagi, or a custom template), Ctrl+L focuses it, Esc returns to page;
  clicking it selects everything in it, a fresh tab hands it over ready to
  type into, and a loading bar runs along the pill's bottom edge
- Back / forward / reload / home + full keyboard surface (table below)
- Drop-down panels (suggestions + downloads) carry per-row icons — the site's
  own favicon when it is known, a vector glyph otherwise — and highlight the
  row under the cursor
- Favicons everywhere, fetched from the site's own origin only (no icon
  proxy ever learns what you read): cached in memory and under
  `%LOCALAPPDATA%\GTX Browser\icons\` so they survive restarts, with a
  letter tile as the fallback
- Settings → **General** sets GTX as the default browser: GTX publishes its
  per-user capabilities, asks Windows through the one sanctioned API, and
  when Windows keeps the choice for the user it opens *Default apps* straight
  at GTX

**Built-in pages** (`gtx://…`, no network):

- `gtx://newtab` — themed blank page (start typing to search/navigate)
- `gtx://settings` — full settings UI that hydrates from the config Rust
  hands it on open, applies live; saves are **verified**
  (write → re-read → re-parse) and acked back to the page before the UI says
  "Saved"; failures show red "Not saved"
- `gtx://history` — last 500 visited pages from `history.toml`, Ctrl+H,
  each row showing the site's own favicon

**Protection & data**

- Popup blocking: `window.open` only works right after a real user gesture
  (one small page-side init script, nothing else injected)
- Session restore (tabs + window position), download confirmation, compact
  mode, per-monitor DPI (v2), `prefers-color-scheme` themed pages, zoom
- Everything local: config, session, history, logs — nothing leaves the
  machine except the pages you visit

### Keyboard

| Keys | Action |
|---|---|
| Ctrl+T / Ctrl+N / Ctrl+W | New tab (Ctrl+N too) / close tab |
| Ctrl+Shift+T | Reopen closed tab (config `shortcut_reopen`) |
| Ctrl+Tab / Ctrl+Shift+Tab (or PgDn/PgUp) | Next / previous tab |
| Ctrl+1–9 | Jump to tab (config `shortcut_ctrl_numbers`) |
| Ctrl+L | Focus address bar (Enter goes, Esc returns to page) |
| Ctrl+R / F5 | Reload |
| Alt+← / Alt+→ / Alt+Home | Back / forward / home |
| Ctrl++ / Ctrl+- / Ctrl+0 | Zoom in / out / reset |
| Ctrl+H | History page |
| Ctrl+, | Settings |
| F11 | Fullscreen (config `shortcut_f11`) |
| F12 | Devtools (only with `devtools = true`) |

The kebab menu (⋮) mirrors these: new tab, reopen, address bar, reload,
history, downloads folder, zoom, settings, About, exit.

## Building

Requirements: Rust (stable, MSVC), Visual Studio Build Tools with the C++
workload + Windows SDK.

```sh
cargo build --release     # → target/release/gtx-browser.exe
cargo test                # 95 unit tests (layout, hit-testing, config, IPC…)
gtx-browser.exe           # or: gtx-browser.exe --smoke 3000
```

Flags: `--smoke [ms]` start → report → exit; `--no-restore`; a bare URL opens
in the first tab — or, when a GTX is already running, straight into that
window instead of beside it.

> **Note on locked-down machines:** Windows Application Control may deny the
> *first* execution of a freshly built binary (build scripts included) with
> error 4551. It is transient — re-run the command, or build outside a
> Downloads folder, e.g. `export CARGO_TARGET_DIR=$USERPROFILE/gtx-target`.
> If the deny sticks to one dependency ("can't find crate for X" though X
> clearly exists), `cargo clean -p <crate> && cargo build` regenerates it.

## Packaging & installer

```sh
powershell -File tools/package.ps1     # release build → dist/GTX-Browser/ + zip
powershell -File tools/roundtrip.ps1   # install → verify → smoke → uninstall
powershell -File tools/wizard-check.ps1   # drive the setup wizard page by page
```

`package.ps1` stages `gtx-browser.exe` + `gtx-setup.exe` together (the
installer expects them side by side) and zips the portable folder.

`gtx-setup.exe` is per-user (no elevation). Run it with no arguments and it
shows a five-page wizard — **Welcome → Terms of Use → Options → Progress →
Finish** — in one native Win32 window (no webview, no dialog template). The
licence page shows the full plain-language terms
(`src/bin/GTX-TERMS.txt`) in a scrollable read-only box and **Next stays
disabled until you accept them**; the options page chooses which shortcuts to
create; the progress page names each step as it happens and locks navigation
while it runs; the finish page shows the report with a ✓/✗ per step and offers
to start the browser.

Under the hood it closes a running GTX politely so the session is saved, copies
to `%LOCALAPPDATA%\Programs\GTX Browser`, creates the shortcuts you asked for,
registers an Add/Remove Programs entry under HKCU, then **verifies every step**.
Flags `--repair`, `--uninstall` and `--quiet` (scripted: no wizard, no dialogs,
exit 1 on failure) keep the non-GUI path, so `roundtrip.ps1` and `package.ps1`
are unaffected. Uninstall keeps your settings, and the install folder is
removed by a deferred helper after setup exits. You can also skip installation
entirely and run `gtx-browser.exe` straight from the folder.

## Configuration

`%LOCALAPPDATA%\GTX Browser\config.toml` (a commented template is written on
first run; the settings page is the primary UI and saves are verified):

```toml
homepage = "https://duckduckgo.com"          # home
new_tab_url = "gtx://newtab"                 # what a fresh tab loads
search_template = "https://duckduckgo.com/?q={}"
user_agent = ""                              # empty = WebView2 default
devtools = false                             # allow F12
browser_accelerator_keys = false             # false = GTX owns all shortcuts
hardware_acceleration = true                 # false passes --disable-gpu
block_popups = true                          # gesture-gated window.open
theme = "auto"                               # auto | light | dark
hibernate_secs = 900                         # unload idle background tabs (0 = off)
restore_session = true
```

A corrupt config is backed up to `config.toml.bad` rather than silently
overwritten. Session state lives in `session.toml`, history in
`history.toml`, the WebView2 profile in `User Data\`, and rotating logs
(512 KB, `gtx.log` / `gtx.log.old`) in the same folder — releases are
windowed binaries, so diagnostics go to the file, not a console.

## Architecture

```
src/
  main.rs        entry, frameless wnd_proc (WM_NCCALCSIZE/NCHITTEST),
                 accelerator-driven message loop
  app.rs         app state: chrome layout + painting, hit-testing, tabs,
                 navigation, settings IPC, toasts, wry wiring
  chrome.rs      the single-row chrome child window (paint + mouse)
  toast.rs       transient status/error toast window
  w32.rs         rects, edge hit-testing, DWM effects, DPI, registry helpers
  nav.rs         address-bar input → URL or search (unit-tested)
  config.rs      config.toml: load/defaults/template, verified save
  session.rs     session.toml save/restore
  history.rs     history.toml (500 entries) + history page rendering
  favicon.rs     site-icon cache: decode (WIC), draw, disk persistence
  icon_fetch.rs  background favicon fetch (WinHTTP), one request per host
  internal.rs    gtx:// pages: new tab, history; URL classification
  settings.rs    settings page HTML/CSS/JS (light + dark) and its IPC
  guard.rs       the only page-side script: gesture-gated popup guard
  default_browser.rs  HKCU registration + the "set as default" flow
  log.rs         rotating file log (windowed binaries have no stderr)
  bin/gtx-setup.rs  installer: setup wizard / repair / uninstall / quiet
  bin/GTX-TERMS.txt terms shown on the wizard's licence page
tools/           package.ps1, roundtrip.ps1, capture.ps1, framecheck.ps1,
                 inspect.ps1, codepoints.ps1, make-icon.ps1, ui-check.ps1,
                 wizard-check.ps1
```

Threading model: everything runs on the main thread. Win32 window procs reach
app state through a stashed raw pointer; wry's callbacks never touch app state
directly — they enqueue events into a thread-local queue that the message loop
drains between messages, which keeps re-entrancy out of the picture. Creation
and teardown while a view is busy is queued (`Defer`) and replayed, so a
failed WebView2 creation surfaces as an on-screen error page with retry
instead of a silent dead tab.

Build sizes are held down by `lto = "fat"`, `codegen-units = 1`,
`panic = "abort"`, `strip = "symbols"`; `build.rs` embeds the icon, version
info and application manifest (Comctl32 v6, per-monitor-v2 DPI, asInvoker)
when `rc.exe` is present and skips it gracefully otherwise.

The omnibox placeholder ("Search DuckDuckGo or type a URL") is painted by GTX
itself over the edit control's own repaints rather than via `EM_SETCUEBANNER`,
whose availability depends on a Comctl32 activation context — ours is derived
from `search_template`, tracks theme and DPI, and clears the moment text is
typed.
