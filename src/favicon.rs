//! Favicons on the tab pills.
//!
//! The engine gives us no favicon API, so the page itself draws its
//! `<link rel=icon>` onto a 32×32 canvas and posts the pixels over IPC (see
//! [`script`]). We keep one bitmap per host ([`Cache`]) and paint it inside
//! the pill; a page that exposes no readable icon just gets a letter tile.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::config;

use windows::Win32::Graphics::Gdi::{
    AlphaBlend, CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, SelectObject,
    AC_SRC_ALPHA, AC_SRC_OVER, BI_RGB, BLENDFUNCTION, DIB_RGB_COLORS, HBITMAP, HDC, HGDIOBJ,
    BITMAPINFO, BITMAPINFOHEADER,
};

/// Side of the square the page renders into (and of our bitmaps).
pub const SIZE: i32 = 32;
/// The site's own icon URLs, in the order everything tries them: the history
/// page's `<img>` chain and the background fetcher share this so they can
/// never drift apart.
pub const ICON_PATHS: [&str; 2] = ["/favicon.ico", "/apple-touch-icon.png"];

/// `SIZE × SIZE × RGBA`.
pub const RGBA_LEN: usize = (SIZE * SIZE * 4) as usize;
/// Cached hosts per session — a bound, so GDI handles can never pile up.
const MAX_ICONS: usize = 256;

// ------------------------------------------------------------------
// Page side
// ------------------------------------------------------------------

/// The initialization script appended to every page: find the declared icon,
/// rasterise it at 32×32 and hand the pixels to GTX.
///
/// Reading a cross-origin image needs CORS (`crossOrigin = "anonymous"`), so
/// icons served without it simply fail to load — we then fall back to the
/// letter tile rather than showing nothing.
pub fn script() -> &'static str {
    r#"
(() => {
  try {
    const pick = () => {
      const link = document.querySelector("link[rel~='icon'][href], link[rel='shortcut icon'][href], link[rel='apple-touch-icon'][href]");
      if (!link) return;
      const img = new Image();
      img.crossOrigin = 'anonymous';
      img.onload = () => {
        try {
          const c = document.createElement('canvas');
          c.width = 32; c.height = 32;
          const g = c.getContext('2d');
          g.clearRect(0, 0, 32, 32);
          g.drawImage(img, 0, 0, 32, 32);
          const d = g.getImageData(0, 0, 32, 32).data;
          let bin = '';
          for (let i = 0; i < d.length; i += 4) {
            bin += String.fromCharCode(d[i], d[i + 1], d[i + 2], d[i + 3]);
          }
          window.ipc.postMessage(JSON.stringify({ a: 'gtx-icon', b64: btoa(bin) }));
        } catch (e) { /* tainted canvas: no icon, use the letter tile */ }
      };
      img.src = link.href;
    };
    if (document.readyState === 'loading') {
      document.addEventListener('DOMContentLoaded', pick, { once: true });
    } else {
      pick();
    }
    // Some pages declare the icon late (or replace it).
    setTimeout(pick, 1500);
  } catch (e) {}
})();
"#
}

// ------------------------------------------------------------------
// Decoding
// ------------------------------------------------------------------

/// Hand-rolled base64 decode (standard alphabet, padding and whitespace
/// tolerated) — no new dependency for one IPC message.
pub fn decode(b64: &str) -> Option<Vec<u8>> {
    fn value(c: u8) -> Option<u8> {
        match c {
            b'A'..=b'Z' => Some(c - b'A'),
            b'a'..=b'z' => Some(c - b'a' + 26),
            b'0'..=b'9' => Some(c - b'0' + 52),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    }
    let mut out = Vec::with_capacity(b64.len() / 4 * 3);
    let mut acc: u32 = 0;
    let mut bits = 0u32;
    for &c in b64.as_bytes() {
        if c == b'=' || c.is_ascii_whitespace() {
            continue;
        }
        let v = value(c)?;
        acc = (acc << 6) | v as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Some(out)
}

/// The tile letter for a host: its first letter, so `www.docs.rs` → `D`.
pub fn letter(host: &str) -> Option<char> {
    let bare = host.strip_prefix("www.").unwrap_or(host);
    bare.chars()
        .find(|c| c.is_ascii_alphabetic())
        .or_else(|| bare.chars().find(|c| !c.is_ascii_punctuation()))
}

// ------------------------------------------------------------------
// Bitmaps
// ------------------------------------------------------------------

/// 32×32 top-down premultiplied BGRA section — what `AlphaBlend` expects.
fn bitmap(rgba: &[u8]) -> Option<HBITMAP> {
    bitmap_rgba(rgba, SIZE)
}

/// Make a square top-down RGBA bitmap of any positive side length. Shared
/// with the bundled toolbar glyphs, which use the same transparent BGRA
/// `AlphaBlend` path but are authored at 24 × 24 instead of 32 × 32.
pub(crate) fn bitmap_rgba(rgba: &[u8], side: i32) -> Option<HBITMAP> {
    if side <= 0 {
        return None;
    }
    let side = usize::try_from(side).ok()?;
    let len = side.checked_mul(side)?.checked_mul(4)?;
    if rgba.len() != len {
        return None;
    }
    let mut bmi = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: core::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: side as i32,
            // Negative: top-down, so row 0 is the icon's top row.
            biHeight: -(side as i32),
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut ptr: *mut core::ffi::c_void = core::ptr::null_mut();
    let hbm = unsafe {
        CreateDIBSection(
            None,
            &mut bmi as *const BITMAPINFO,
            DIB_RGB_COLORS,
            &mut ptr,
            None,
            0,
        )
        .ok()?
    };
    if ptr.is_null() {
        return None;
    }
    let dst = unsafe { core::slice::from_raw_parts_mut(ptr as *mut u8, len) };
    for (src, out) in rgba.chunks_exact(4).zip(dst.chunks_exact_mut(4)) {
        let (r, g, b, a) = (src[0], src[1], src[2], src[3]);
        // Premultiply: AlphaBlend with AC_SRC_ALPHA wants it.
        let m = |c: u8| ((c as u32 * a as u32 + 127) / 255) as u8;
        out[0] = m(b);
        out[1] = m(g);
        out[2] = m(r);
        out[3] = a;
    }
    Some(hbm)
}

/// Paint a cached icon, stretched into `w × h` at `x, y`.
///
/// # Safety
/// `dst` must be a live device context.
pub unsafe fn draw(dst: HDC, hbm: HBITMAP, x: i32, y: i32, w: i32, h: i32) {
    unsafe {
        draw_sized(dst, hbm, x, y, w, h, SIZE);
    }
}

/// Paint a square bitmap of `side × side` into `w × h` at `x, y`.
///
/// # Safety
/// `dst` must be a live device context and `hbm` a compatible alpha bitmap.
pub(crate) unsafe fn draw_sized(
    dst: HDC,
    hbm: HBITMAP,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    side: i32,
) {
    if w <= 0 || h <= 0 || side <= 0 {
        return;
    }
    unsafe {
        let mem = CreateCompatibleDC(None);
        if mem.0.is_null() {
            return;
        }
        let prev = SelectObject(mem, HGDIOBJ(hbm.0));
        let _ = AlphaBlend(
            dst,
            x,
            y,
            w,
            h,
            mem,
            0,
            0,
            side,
            side,
            BLENDFUNCTION {
                BlendOp: AC_SRC_OVER as u8,
                BlendFlags: 0,
                SourceConstantAlpha: 255,
                AlphaFormat: AC_SRC_ALPHA as u8,
            },
        );
        SelectObject(mem, prev);
        let _ = DeleteDC(mem);
    }
}

// ------------------------------------------------------------------
// Per-host cache
// ------------------------------------------------------------------

// ------------------------------------------------------------------
// On disk
// ------------------------------------------------------------------

/// Where favicons survive restarts: raw `SIZE`x`SIZE`xRGBA files, one host
/// each - no encoder to write, no dependency to decode, 4 KB a piece.
fn dir() -> PathBuf {
    config::data_dir().join("icons")
}

/// The file a host maps to. Hostnames are ASCII already; anything else folds
/// to `_`, so a crafted URL can never climb out of [`dir`].
fn file_stem(host: &str) -> String {
    let mut out = String::with_capacity(80);
    for c in host.to_ascii_lowercase().chars().take(80) {
        out.push(if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') {
            c
        } else {
            '_'
        });
    }
    if out.is_empty() {
        out.push('_');
    }
    out
}

/// Write one icon out. Best effort: a full disk must not break the page.
fn save_to(dir: &Path, host: &str, rgba: &[u8]) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    std::fs::write(dir.join(format!("{}.icon", file_stem(host))), rgba)
}

/// Every icon on disk as `(host, rgba)`, newest names first, bounded by
/// [`MAX_ICONS`]. The host comes back from the file name, so a host the
/// sanitizer had to fold loads under its folded name - no icon rather than
/// the wrong one.
fn read_dir_icons(dir: &Path) -> Vec<(String, Vec<u8>)> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(dir) else {
        return out;
    };
    let mut paths: Vec<PathBuf> = rd.flatten().map(|e| e.path()).collect();
    paths.sort();
    for p in paths {
        if p.extension().and_then(|x| x.to_str()) != Some("icon") {
            continue;
        }
        let Some(host) = p.file_stem().and_then(|x| x.to_str()).map(str::to_owned) else {
            continue;
        };
        let Ok(bytes) = std::fs::read(&p) else {
            continue;
        };
        if bytes.len() == RGBA_LEN {
            out.push((host, bytes));
        }
        if out.len() >= MAX_ICONS {
            break;
        }
    }
    out
}

/// One bitmap per host, bounded and self-cleaning.
#[derive(Default)]
pub struct Cache {
    map: HashMap<String, HBITMAP>,
}

impl Cache {
    pub fn new() -> Self {
        Self::default()
    }

    /// Rebuild from disk at startup: the downloads panel and the suggest
    /// rows show the icons of sites visited in *earlier* sessions too.
    pub fn load() -> Self {
        let mut c = Self::new();
        for (host, rgba) in read_dir_icons(&dir()) {
            c.put(&host, &rgba);
        }
        c
    }

    pub fn get(&self, host: &str) -> Option<HBITMAP> {
        self.map.get(host).copied()
    }

    /// Store `rgba` (exactly [`RGBA_LEN`] bytes) for `host`, replacing any
    /// icon that host had, and write it out for the next run. Over the bound
    /// the whole cache is dropped: stale pills fall back to letter tiles
    /// until their pages send again.
    pub fn insert(&mut self, host: &str, rgba: &[u8]) {
        if rgba.len() != RGBA_LEN {
            return;
        }
        self.put(host, rgba);
        let _ = save_to(&dir(), host, rgba);
    }

    /// The in-memory half of [`Cache::insert`] - no disk traffic, so the
    /// startup replay does not rewrite every file it just read.
    fn put(&mut self, host: &str, rgba: &[u8]) {
        let Some(hbm) = bitmap(rgba) else {
            return;
        };
        let host = host.to_ascii_lowercase();
        if self.map.len() >= MAX_ICONS && !self.map.contains_key(&host) {
            self.clear();
        }
        if let Some(old) = self.map.insert(host, hbm) {
            unsafe {
                let _ = DeleteObject(HGDIOBJ(old.0));
            }
        }
    }

    fn clear(&mut self) {
        for hbm in self.map.values() {
            unsafe {
                let _ = DeleteObject(HGDIOBJ(hbm.0));
            }
        }
        self.map.clear();
    }
}

impl Drop for Cache {
    fn drop(&mut self) {
        self.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Test-side encoder so `decode` round-trips against real output.
    fn encode(bytes: &[u8]) -> String {
        const ALPHABET: &[u8; 64] =
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = String::new();
        for chunk in bytes.chunks(3) {
            let b = [
                chunk[0],
                *chunk.get(1).unwrap_or(&0),
                *chunk.get(2).unwrap_or(&0),
            ];
            let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
            out.push(ALPHABET[(n >> 18) as usize & 63] as char);
            out.push(ALPHABET[(n >> 12) as usize & 63] as char);
            out.push(if chunk.len() > 1 {
                ALPHABET[(n >> 6) as usize & 63] as char
            } else {
                '='
            });
            out.push(if chunk.len() > 2 {
                ALPHABET[n as usize & 63] as char
            } else {
                '='
            });
        }
        out
    }

    #[test]
    fn decode_handles_the_real_alphabet() {
        assert_eq!(decode("AQID").unwrap(), vec![1, 2, 3]);
        assert_eq!(decode("AQ==").unwrap(), vec![1]);
        assert_eq!(decode("QQ==").unwrap(), vec![65], "ASCII 'A'");
        assert_eq!(decode("QUJD").unwrap(), b"ABC");
        assert_eq!(decode("").unwrap(), Vec::<u8>::new());
        assert_eq!(decode("AQI D\n").unwrap(), vec![1, 2, 3], "whitespace tolerated");
        assert_eq!(decode("!!!!"), None, "not base64 at all");
    }

    #[test]
    fn decode_round_trips_an_icon_sized_payload() {
        let data: Vec<u8> = (0..RGBA_LEN).map(|i| (i % 251) as u8).collect();
        let back = decode(&encode(&data)).expect("decodes");
        assert_eq!(back.len(), RGBA_LEN);
        assert_eq!(back, data);
    }

    #[test]
    fn the_tile_letter_reads_like_a_domain() {
        assert_eq!(letter("www.docs.rs"), Some('d'));
        assert_eq!(letter("example.com"), Some('e'));
        assert_eq!(letter("34.56.78.90"), Some('3'), "an IP has no name");
        assert_eq!(letter(""), None);
    }

    #[test]
    fn the_page_script_reports_over_ipc() {
        let js = script();
        assert!(js.contains("gtx-icon"), "the action GTX routes on");
        assert!(js.contains("window.ipc.postMessage"));
        assert!(js.contains("rel~='icon'"));
        assert!(js.contains("crossOrigin"), "CORS is required to read pixels");
        assert!(js.contains("btoa"));
    }

    /// The whole persistence contract: one 4 KB file per host, readable back
    /// with its host name intact, so downloads listed after a restart still
    /// show the site's real icon.
    #[test]
    fn icons_survive_a_restart() {
        let dir = std::env::temp_dir().join(format!("gtx-icons-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let rgba: Vec<u8> = (0..RGBA_LEN).map(|i| (i % 251) as u8).collect();
        save_to(&dir, "Example.COM", &rgba).expect("write icon");
        let back = read_dir_icons(&dir);
        assert_eq!(back.len(), 1);
        assert_eq!(back[0].0, "example.com", "hosts load back lowercased");
        assert_eq!(back[0].1, rgba);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A URL can name any host it likes: the file name is the only thing
    /// that host can influence, and it cannot contain a path separator.
    #[test]
    fn a_host_cannot_climb_out_of_the_icon_directory() {
        assert_eq!(file_stem("../../evil"), ".._.._evil");
        assert_eq!(file_stem("a\\b"), "a_b");
        assert_eq!(file_stem("x:y"), "x_y");
        assert_eq!(file_stem(""), "_");
    }

    /// Half a download is not an icon: wrong-sized files never load.
    #[test]
    fn truncated_files_are_never_loaded_as_icons() {
        let dir = std::env::temp_dir().join(format!("gtx-icons-bad-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        std::fs::write(dir.join("short.icon"), vec![0u8; 10]).expect("write");
        assert!(read_dir_icons(&dir).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
