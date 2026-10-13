//! The supplied Material SVG toolbar icons, tinted to match the active theme.
//!
//! The editable SVG sources live in `assets/toolbar-icons/`; pre-rasterized
//! 128 × 128 RGBA rasters are bundled in the executable for the native GDI
//! renderer (regenerate them with `tools/make-toolbar-rasters.py`). At draw
//! time the alpha mask is box-filtered down to the *exact* pixel size the row
//! needs and tinted, so the bitmap is always blitted 1:1 — no GDI stretching,
//! which is what made the old fixed 24 px rasters look blocky on HiDPI.

use windows::Win32::Foundation::COLORREF;
use windows::Win32::Graphics::Gdi::{DeleteObject, HBITMAP, HDC, HGDIOBJ};

use crate::favicon;

/// Side of the bundled source rasters (see `tools/make-toolbar-rasters.py`).
const SRC_SIDE: i32 = 128;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Icon {
    Back,
    Forward,
    Reload,
    Home,
    Search,
    Plus,
    Download,
}

impl Icon {
    const fn rgba(self) -> &'static [u8] {
        match self {
            Self::Back => include_bytes!("../assets/toolbar-icons/back.rgba"),
            Self::Forward => include_bytes!("../assets/toolbar-icons/forward.rgba"),
            Self::Reload => include_bytes!("../assets/toolbar-icons/reload.rgba"),
            Self::Home => include_bytes!("../assets/toolbar-icons/home.rgba"),
            Self::Search => include_bytes!("../assets/toolbar-icons/search.rgba"),
            Self::Plus => include_bytes!("../assets/toolbar-icons/plus.rgba"),
            Self::Download => include_bytes!("../assets/toolbar-icons/download.rgba"),
        }
    }

    #[cfg(test)]
    const fn original_svg(self) -> &'static str {
        match self {
            Self::Back => include_str!("../assets/toolbar-icons/back.svg"),
            Self::Forward => include_str!("../assets/toolbar-icons/forward.svg"),
            Self::Reload => include_str!("../assets/toolbar-icons/reload.svg"),
            Self::Home => include_str!("../assets/toolbar-icons/home.svg"),
            Self::Search => include_str!("../assets/toolbar-icons/search.svg"),
            Self::Plus => include_str!("../assets/toolbar-icons/plus.svg"),
            Self::Download => include_str!("../assets/toolbar-icons/download.svg"),
        }
    }
}

/// Seven small DIBs, one per icon, cached at the regular tint and the size
/// they were last drawn at (they are re-tinted when either changes). Other
/// tints — hover, disabled — are rendered on demand, keeping GDI handle
/// usage minimal.
pub(crate) struct Set {
    /// `(drawn size, bitmap)` per icon; size 0 means "not built yet".
    bitmaps: [(i32, HBITMAP); 7],
    regular: COLORREF,
}

impl Set {
    pub(crate) fn new(regular: COLORREF) -> Self {
        Self {
            bitmaps: std::array::from_fn(|_| (0, HBITMAP(core::ptr::null_mut()))),
            regular,
        }
    }

    /// Rebuild the cached DIBs if the theme's icon color changes.
    pub(crate) fn set_colors(&mut self, regular: COLORREF) {
        if self.regular == regular {
            return;
        }
        self.clear();
        self.regular = regular;
    }

    /// Draw `icon` as a crisp `size × size` glyph at `x, y`, tinted `color`.
    /// The regular tint is cached per size (the painter reuses it every
    /// frame); any other tint — hover, disabled — is rendered on demand and
    /// freed again, exactly like the hover tint always worked.
    ///
    /// # Safety
    /// `hdc` must be a live device context.
    pub(crate) unsafe fn draw(
        &mut self,
        hdc: HDC,
        icon: Icon,
        x: i32,
        y: i32,
        size: i32,
        color: COLORREF,
    ) {
        if size <= 0 {
            return;
        }
        if color == self.regular {
            let idx = base_index(icon);
            let (cached_size, cached) = self.bitmaps[idx];
            if cached.0.is_null() || cached_size != size {
                // First draw at this size, or a DPI/mode change moved it.
                if !cached.0.is_null() {
                    unsafe {
                        let _ = DeleteObject(HGDIOBJ(cached.0));
                    }
                }
                self.bitmaps[idx] = (size, tint(icon.rgba(), color, size));
            }
            let bitmap = self.bitmaps[idx].1;
            if !bitmap.0.is_null() {
                unsafe {
                    favicon::draw_sized(hdc, bitmap, x, y, size, size, size);
                }
            }
            return;
        }
        let bitmap = tint(icon.rgba(), color, size);
        if !bitmap.0.is_null() {
            unsafe {
                favicon::draw_sized(hdc, bitmap, x, y, size, size, size);
                let _ = DeleteObject(HGDIOBJ(bitmap.0));
            }
        }
    }

    fn clear(&mut self) {
        for slot in &mut self.bitmaps {
            if !slot.1 .0.is_null() {
                unsafe {
                    let _ = DeleteObject(HGDIOBJ(slot.1 .0));
                }
            }
            *slot = (0, HBITMAP(core::ptr::null_mut()));
        }
    }
}

impl Drop for Set {
    fn drop(&mut self) {
        self.clear();
    }
}

const fn base_index(icon: Icon) -> usize {
    match icon {
        Icon::Back => 0,
        Icon::Forward => 1,
        Icon::Reload => 2,
        Icon::Home => 3,
        Icon::Search => 4,
        Icon::Plus => 5,
        Icon::Download => 6,
    }
}

/// Box-filter the source alpha mask down to `dst × dst` and tint it.
///
/// The source is a flat shape carried by its alpha channel alone, so the
/// resample only ever touches alpha; RGB stays the requested tint. Averaging
/// the mask before tinting is what antialiases the downscale — GDI's own
/// StretchBlt would use nearest-neighbour and leave the "bumps".
fn tint(source: &[u8], color: COLORREF, dst: i32) -> HBITMAP {
    if dst <= 0 || source.len() != (SRC_SIDE * SRC_SIDE * 4) as usize {
        return HBITMAP(core::ptr::null_mut());
    }
    let alpha = resample_alpha(source, SRC_SIDE, dst);
    let (r, g, b) = (
        (color.0 & 0xff) as u8,
        ((color.0 >> 8) & 0xff) as u8,
        ((color.0 >> 16) & 0xff) as u8,
    );
    let mut rgba = Vec::with_capacity((dst * dst * 4) as usize);
    for a in alpha {
        rgba.extend_from_slice(&[r, g, b, a]);
    }
    favicon::bitmap_rgba(&rgba, dst).unwrap_or(HBITMAP(core::ptr::null_mut()))
}

/// Average the alpha channel of a `src × src` RGBA mask into `dst × dst`.
///
/// Each destination pixel averages every source pixel its area covers, with
/// fractional edge weights, so the result is a true area (box) downscale.
fn resample_alpha(source: &[u8], src: i32, dst: i32) -> Vec<u8> {
    debug_assert!(source.len() >= (src * src * 4) as usize);
    let mut out = Vec::with_capacity((dst * dst) as usize);
    let scale = src as f64 / dst as f64;
    for dy in 0..dst {
        let y0 = dy as f64 * scale;
        let y1 = y0 + scale;
        for dx in 0..dst {
            let x0 = dx as f64 * scale;
            let x1 = x0 + scale;
            let mut acc = 0.0;
            let mut weight = 0.0;
            for sy in y0.floor() as i32..(y1.ceil() as i32).min(src) {
                let wy = (y1.min(sy as f64 + 1.0) - y0.max(sy as f64)).max(0.0);
                if wy <= 0.0 {
                    continue;
                }
                for sx in x0.floor() as i32..(x1.ceil() as i32).min(src) {
                    let wx = (x1.min(sx as f64 + 1.0) - x0.max(sx as f64)).max(0.0);
                    if wx <= 0.0 {
                        continue;
                    }
                    acc += source[((sy * src + sx) * 4 + 3) as usize] as f64 * wx * wy;
                    weight += wx * wy;
                }
            }
            let v = if weight > 0.0 { acc / weight } else { 0.0 };
            out.push((v + 0.5).clamp(0.0, 255.0) as u8);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [Icon; 7] = [
        Icon::Back,
        Icon::Forward,
        Icon::Reload,
        Icon::Home,
        Icon::Search,
        Icon::Plus,
        Icon::Download,
    ];

    #[test]
    fn each_supplied_svg_has_a_nonempty_square_rgba_raster() {
        for icon in ALL {
            let svg = icon.original_svg();
            assert!(svg.contains("<svg ") && svg.contains("viewBox=\"0 -960 960 960\""));
            assert!(svg.contains("<path ") && svg.contains("#e3e3e3"));
            let rgba = icon.rgba();
            assert_eq!(rgba.len(), (SRC_SIDE * SRC_SIDE * 4) as usize);
            assert!(
                rgba.chunks_exact(4).any(|px| px[3] == 255),
                "{icon:?} has fully drawn pixels"
            );
            assert!(
                rgba.chunks_exact(4).any(|px| px[3] == 0),
                "{icon:?} preserves transparency"
            );
        }
    }

    #[test]
    fn each_toolbar_action_uses_the_correct_supplied_icon() {
        for (i, icon) in ALL.into_iter().enumerate() {
            assert_eq!(base_index(icon), i, "slot {i}");
        }
    }

    #[test]
    fn downscaling_the_rasters_keeps_the_shape_and_its_soft_edges() {
        for icon in ALL {
            let src = icon.rgba();
            let full = resample_alpha(src, SRC_SIDE, SRC_SIDE);
            let ink_full: f64 = full.iter().map(|&a| a as f64).sum();
            for size in [16, 18, 20, 24, 30] {
                let small = resample_alpha(src, SRC_SIDE, size);
                assert_eq!(small.len(), (size * size) as usize, "{icon:?} @{size}");
                let ink: f64 = small.iter().map(|&a| a as f64).sum();
                // Box filtering preserves total ink scaled by the area
                // ratio, so undo that to compare coverage directly.
                let ratio = ink / ink_full * (SRC_SIDE * SRC_SIDE) as f64 / (size * size) as f64;
                assert!(
                    (0.90..=1.10).contains(&ratio),
                    "{icon:?} @{size} keeps its coverage (ratio {ratio:.3})"
                );
                assert!(
                    small.iter().any(|&a| a > 0 && a < 255),
                    "{icon:?} @{size} has antialiased edges"
                );
                assert!(
                    small.iter().any(|&a| a == 0),
                    "{icon:?} @{size} keeps transparent areas"
                );
            }
        }
    }

    /// The box filter must average, not pick: a checkerboard downsizes to a
    /// uniform mid-tone rather than one of the two phases.
    #[test]
    fn the_resampler_averages_the_area_it_covers() {
        // 4x4 RGBA checkerboard of black/white alpha corners.
        let mut src = vec![0u8; 4 * 4 * 4];
        for y in 0..4 {
            for x in 0..4 {
                let a = if (x + y) % 2 == 0 { 255 } else { 0 };
                src[(y * 4 + x) * 4 + 3] = a;
            }
        }
        let out = resample_alpha(&src, 4, 2);
        assert_eq!(out, vec![128, 128, 128, 128]);
        // Halving each axis halves the weight of one checker phase: 2x1
        // collapses the two columns of a row pair into one average.
        let out = resample_alpha(&src, 4, 4);
        assert_eq!(out, src[3..].iter().step_by(4).copied().collect::<Vec<_>>());
    }
}
