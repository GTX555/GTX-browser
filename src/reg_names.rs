// Names the browser and its installer must agree on: GTX's per-user
// registration, which is what puts it in *Settings -> Apps -> Default apps*
// and what an uninstall has to take back out.
//
// Shared verbatim: gtx-browser includes it as `mod reg_names`, gtx-setup
// pulls the same file in with `include!`, so renaming a key here can never
// leave the installer deleting paths the browser no longer writes.
//
// Plain `//` comments on purpose: `include!` drops this file into the middle
// of gtx-setup, where inner doc comments would not parse.

/// Handles `.htm`/`.html` documents.
pub const PROGID_FILE: &str = "GTXBrowserHTM";
/// Handles `http:`/`https:` links.
pub const PROGID_URL: &str = "GTXBrowserURL";
/// The name GTX is registered under — also the label Windows shows in
/// *Default apps* and the value the `registeredAppUser` deep link matches.
pub const APP_NAME: &str = "GTX Browser";
/// Capabilities key, relative to `HKCU\`.
pub const CAPS_KEY: &str = "Software\\GTX Browser\\Capabilities";
/// Where per-user applications announce themselves, relative to `HKCU\`.
pub const REGISTERED_KEY: &str = "Software\\RegisteredApplications";
