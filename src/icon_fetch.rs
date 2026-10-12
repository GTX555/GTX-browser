//! Background favicon fetching.
//!
//! The page-side capture (see [`crate::favicon::script`]) can only read an
//! icon the page itself may read - same origin, or CORS. The downloads panel
//! and the suggest rows must still show something real for a host that was
//! never captured, so this module asks the site itself over WinHTTP (the
//! system's own TLS stack - no new dependency) and decodes the bytes with
//! WIC, which speaks PNG, JPEG, GIF and ICO natively. Results queue for the
//! main thread; a failed host is remembered so a 404 is never asked twice.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

use windows::core::{w, Interface, PCWSTR};
use windows::Win32::Foundation::{GENERIC_READ, S_FALSE, S_OK};
use windows::Win32::Graphics::Imaging::{
    CLSID_WICImagingFactory, GUID_WICPixelFormat32bppRGBA, IWICBitmapSource, IWICImagingFactory,
    IWICPalette, WICBitmapDitherTypeNone, WICBitmapInterpolationModeFant, WICBitmapPaletteTypeCustom,
    WICDecodeMetadataCacheOnDemand,
};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED,
};

use crate::{favicon, log};

/// Whole responses are bounded: an icon is 4 KB, so anything near a
/// megabyte is somebody answering with a page instead.
const MAX_BYTES: usize = 1_048_576;

static IN_FLIGHT: Mutex<Option<Vec<String>>> = Mutex::new(None);
static FAILED: Mutex<Option<Vec<String>>> = Mutex::new(None);
static DONE: Mutex<Vec<(String, Vec<u8>)>> = Mutex::new(Vec::new());
static TEMP: AtomicUsize = AtomicUsize::new(0);

/// A mutex nobody can poison for long: even if a caller panics between lock
/// and unlock, the next caller still gets the guard.
fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Fetch `host`'s icon unless it is cached, claimed, or already given up on.
/// The claim is taken synchronously, so a burst of repaints costs one thread.
pub fn request(host: &str) {
    let host = host.to_ascii_lowercase();
    if host.is_empty() || !begin(&host) {
        return;
    }
    let h = host.clone();
    let _ = std::thread::Builder::new()
        .name("gtx-favicon".into())
        .spawn(move || finish(&h, fetch(&h)));
}

/// Claim `host` for this request; false when it is already spoken for.
fn begin(host: &str) -> bool {
    let mut inflight = lock(&IN_FLIGHT);
    let list = inflight.get_or_insert_with(Vec::new);
    if list.iter().any(|h| h == host) {
        return false;
    }
    let failed = lock(&FAILED);
    if failed.as_ref().is_some_and(|f| f.iter().any(|h| h == host)) {
        return false;
    }
    list.push(host.to_string());
    true
}

/// Hand the result over and release the claim.
fn finish(host: &str, got: Option<Vec<u8>>) {
    {
        let mut inflight = lock(&IN_FLIGHT);
        if let Some(list) = inflight.as_mut() {
            list.retain(|h| h != host);
        }
    }
    match got {
        Some(rgba) => lock(&DONE).push((host.to_string(), rgba)),
        None => {
            lock(&FAILED)
                .get_or_insert_with(Vec::new)
                .push(host.to_string());
            log::write(&format!("no favicon fetched for {host}"));
        }
    }
}

/// Icons fetched since the last drain - consumed on the main thread, where
/// the cache lives.
pub fn drain() -> Vec<(String, Vec<u8>)> {
    std::mem::take(&mut *lock(&DONE))
}


// ------------------------------------------------------------------
// The network and the decoder - all of it on the worker thread
// ------------------------------------------------------------------

/// `favicon.ico` first, then the touch icon; `None` marks the host done.
fn fetch(host: &str) -> Option<Vec<u8>> {
    for path in favicon::ICON_PATHS {
        if let Some(bytes) = get(host, path) {
            if let Some(rgba) = decode(&bytes) {
                return Some(rgba);
            }
        }
    }
    None
}

/// One HTTPS GET, whole body, bounded. `None` on any failure - this runs on
/// a background thread where "try the next URL" is the entire error policy.
fn get(host: &str, path: &str) -> Option<Vec<u8>> {
    use windows::Win32::Networking::WinHttp::*;
    let host_w: Vec<u16> = host.encode_utf16().chain(std::iter::once(0)).collect();
    let path_w: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();
    unsafe {
        let session = WinHttpOpen(
            w!("GTX Browser"),
            WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
            PCWSTR::null(),
            PCWSTR::null(),
            0,
        );
        if session.is_null() {
            return None;
        }
        // Session-level timeouts: every request inherits them, so a hung
        // host can never keep a thread - or a repaint - waiting.
        let _ = WinHttpSetTimeouts(session, 2000, 2000, 3000, 3000);
        let conn = WinHttpConnect(session, PCWSTR(host_w.as_ptr()), INTERNET_DEFAULT_HTTPS_PORT, 0);
        if conn.is_null() {
            let _ = WinHttpCloseHandle(session);
            return None;
        }
        let req = WinHttpOpenRequest(
            conn,
            w!("GET"),
            PCWSTR(path_w.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            std::ptr::null(),
            WINHTTP_FLAG_SECURE,
        );
        if req.is_null() {
            let _ = WinHttpCloseHandle(conn);
            let _ = WinHttpCloseHandle(session);
            return None;
        }
        let body = exchange(req);
        let _ = WinHttpCloseHandle(req);
        let _ = WinHttpCloseHandle(conn);
        let _ = WinHttpCloseHandle(session);
        body
    }
}

/// Send, check the status, read the body - capped at [`MAX_BYTES`].
unsafe fn exchange(req: *mut core::ffi::c_void) -> Option<Vec<u8>> {
    use windows::Win32::Networking::WinHttp::*;
    unsafe {
        WinHttpSendRequest(req, None, None, 0, 0, 0).ok()?;
        WinHttpReceiveResponse(req, std::ptr::null_mut()).ok()?;
        let mut status = 0u32;
        let mut len = 4u32;
        let mut idx = 0u32;
        WinHttpQueryHeaders(
            req,
            WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
            PCWSTR::null(),
            Some(&mut status as *mut u32 as *mut core::ffi::c_void),
            &mut len,
            &mut idx,
        )
        .ok()?;
        if status != 200 {
            return None;
        }
        let mut body: Vec<u8> = Vec::new();
        loop {
            let mut avail = 0u32;
            WinHttpQueryDataAvailable(req, &mut avail).ok()?;
            if avail == 0 {
                break;
            }
            let want = (avail as usize).min(MAX_BYTES - body.len());
            if want == 0 {
                return None; // past the cap: not an icon, refuse the rest
            }
            let mut buf = vec![0u8; want];
            let mut read = 0u32;
            WinHttpReadData(req, buf.as_mut_ptr() as *mut core::ffi::c_void, want as u32, &mut read)
                .ok()?;
            if read == 0 {
                break;
            }
            body.extend_from_slice(&buf[..read as usize]);
        }
        (!body.is_empty()).then_some(body)
    }
}

// ------------------------------------------------------------------
// Decode: bytes in, SIZE x SIZE RGBA out
// ------------------------------------------------------------------

/// Bytes -> [`favicon::RGBA_LEN`] RGBA through WIC, which reads PNG, JPEG,
/// GIF and the ICO container without a single line of format code from us.
fn decode(bytes: &[u8]) -> Option<Vec<u8>> {
    if bytes.is_empty() || bytes.len() > MAX_BYTES {
        return None;
    }
    // WIC wants COM on this thread; a worker is free to pick its apartment,
    // and "already initialised differently" is not an error here.
    let hr = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
    let inited = hr == S_OK || hr == S_FALSE;
    let out = unsafe { decode_file(bytes) };
    if inited {
        unsafe { CoUninitialize() };
    }
    out
}

/// WIC reads files, so the bytes take a short detour through the temp dir.
unsafe fn decode_file(bytes: &[u8]) -> Option<Vec<u8>> {
    let name = std::env::temp_dir().join(format!(
        "gtx-fav-{}-{}.tmp",
        std::process::id(),
        TEMP.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::write(&name, bytes).ok()?;
    let out = unsafe { decode_path(&name) };
    let _ = std::fs::remove_file(&name);
    out
}

/// Whatever it turned out to be, scaled to `SIZE` x `SIZE` and converted to
/// straight (non-premultiplied) RGBA - exactly what the cache expects.
unsafe fn decode_path(name: &std::path::Path) -> Option<Vec<u8>> {
    unsafe {
        use std::os::windows::ffi::OsStrExt;
        let wide: Vec<u16> = name
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        let factory: IWICImagingFactory =
            CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER).ok()?;
        let decoder = factory
            .CreateDecoderFromFilename(
                PCWSTR(wide.as_ptr()),
                None,
                GENERIC_READ,
                WICDecodeMetadataCacheOnDemand,
            )
            .ok()?;
        let frame = decoder.GetFrame(0).ok()?;
        let src: IWICBitmapSource = frame.cast().ok()?;
        let scaler = factory.CreateBitmapScaler().ok()?;
        scaler
            .Initialize(
                &src,
                favicon::SIZE as u32,
                favicon::SIZE as u32,
                WICBitmapInterpolationModeFant,
            )
            .ok()?;
        let scaled: IWICBitmapSource = scaler.cast().ok()?;
        let conv = factory.CreateFormatConverter().ok()?;
        conv.Initialize(
            &scaled,
            &GUID_WICPixelFormat32bppRGBA,
            WICBitmapDitherTypeNone,
            None::<&IWICPalette>,
            0.0,
            WICBitmapPaletteTypeCustom,
        )
        .ok()?;
        let mut rgba = vec![0u8; favicon::RGBA_LEN];
        conv.CopyPixels(std::ptr::null(), (favicon::SIZE * 4) as u32, &mut rgba)
            .ok()?;
        Some(rgba)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One claim per host: while it is held nobody else starts a thread,
    /// and once a host has failed it is never asked again this session.
    #[test]
    fn a_host_is_only_ever_asked_once() {
        let host = "claim-test.example";
        assert!(begin(host), "the first claim wins");
        assert!(!begin(host), "a held claim is refused");
        finish(host, None);
        assert!(!begin(host), "a failed host is never retried");
    }

    /// Results wait in the queue exactly until the main thread drains them.
    #[test]
    fn drains_hand_over_each_icon_once() {
        let host = "drain-test.example";
        let rgba = vec![7u8; favicon::RGBA_LEN];
        finish(host, Some(rgba.clone()));
        let first = drain();
        assert!(first.iter().any(|(h, b)| h == host && b == &rgba));
        assert!(
            !drain().iter().any(|(h, _)| h == host),
            "drained icons are gone, not copied"
        );
    }

    /// Live check against the real internet - ignored like the other two
    /// network/profile tests: cargo test -- --ignored --nocapture
    #[test]
    #[ignore]
    fn fetches_a_real_favicon() {
        let rgba = fetch("duckduckgo.com").expect("DuckDuckGo serves a favicon");
        assert_eq!(rgba.len(), favicon::RGBA_LEN);
    }
}
