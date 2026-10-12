//! The supplied Material SVG toolbar icons, tinted to match the active theme.
//!
//! The editable SVG sources live in `assets/toolbar-icons/`; pre-rasterized
//! 24 × 24 RGBA pixels are bundled in the executable for the native GDI
//! renderer, so no runtime path or icon-font dependency is needed.

use windows::Win32::Foundation::COLORREF;
use windows::Win32::Graphics::Gdi::{DeleteObject, HBITMAP, HDC, HGDIOBJ};

use crate::favicon;

const SIDE: i32 = 24;

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

/// Seven small DIBs tinted to match the regular toolbar icon color. The one
/// hovered glyph is tinted on demand, keeping GDI handle usage minimal.
pub(crate) struct Set {
    bitmaps: [HBITMAP; 7],
    regular: COLORREF,
    hovered: COLORREF,
}

impl Set {
    pub(crate) fn new(regular: COLORREF, hovered: COLORREF) -> Self {
        Self {
            bitmaps: std::array::from_fn(|i| tint(icon_at(i).rgba(), regular)),
            regular,
            hovered,
        }
    }

    /// Rebuild the cached DIBs if the theme's icon colors change.
    pub(crate) fn set_colors(&mut self, regular: COLORREF, hovered: COLORREF) {
        if self.regular == regular && self.hovered == hovered {
            return;
        }
        for (i, bitmap) in self.bitmaps.iter_mut().enumerate() {
            if !bitmap.0.is_null() {
                unsafe {
                    let _ = DeleteObject(HGDIOBJ(bitmap.0));
                }
            }
            *bitmap = tint(icon_at(i).rgba(), regular);
        }
        self.regular = regular;
        self.hovered = hovered;
    }

    pub(crate) unsafe fn draw(&self, hdc: HDC, icon: Icon, x: i32, y: i32, size: i32, hover: bool) {
        let bitmap = if hover {
            tint(icon.rgba(), self.hovered)
        } else {
            self.bitmaps[base_index(icon)]
        };
        if !bitmap.0.is_null() {
            unsafe { favicon::draw_sized(hdc, bitmap, x, y, size, size, SIDE) };
            if hover {
                unsafe {
                    let _ = DeleteObject(HGDIOBJ(bitmap.0));
                }
            }
        }
    }
}

impl Drop for Set {
    fn drop(&mut self) {
        for bitmap in self.bitmaps {
            if !bitmap.0.is_null() {
                unsafe {
                    let _ = DeleteObject(HGDIOBJ(bitmap.0));
                }
            }
        }
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

const fn icon_at(i: usize) -> Icon {
    match i {
        0 => Icon::Back,
        1 => Icon::Forward,
        2 => Icon::Reload,
        3 => Icon::Home,
        4 => Icon::Search,
        5 => Icon::Plus,
        _ => Icon::Download,
    }
}

fn tint(source: &[u8], color: COLORREF) -> HBITMAP {
    if source.len() != (SIDE * SIDE * 4) as usize {
        return HBITMAP(core::ptr::null_mut());
    }
    let (r, g, b) = (
        (color.0 & 0xff) as u8,
        ((color.0 >> 8) & 0xff) as u8,
        ((color.0 >> 16) & 0xff) as u8,
    );
    let mut rgba = Vec::with_capacity(source.len());
    for pixel in source.chunks_exact(4) {
        // Keep the SVG's antialiased shape and replace its original gray tint.
        rgba.extend_from_slice(&[r, g, b, pixel[3]]);
    }
    favicon::bitmap_rgba(&rgba, SIDE).unwrap_or(HBITMAP(core::ptr::null_mut()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_supplied_svg_has_a_nonempty_square_rgba_raster() {
        for icon in [
            Icon::Back,
            Icon::Forward,
            Icon::Reload,
            Icon::Home,
            Icon::Search,
            Icon::Plus,
            Icon::Download,
        ] {
            let svg = icon.original_svg();
            assert!(svg.contains("<svg ") && svg.contains("viewBox=\"0 -960 960 960\""));
            assert!(svg.contains("<path ") && svg.contains("#e3e3e3"));
            let rgba = icon.rgba();
            assert_eq!(rgba.len(), (SIDE * SIDE * 4) as usize);
            assert!(
                rgba.chunks_exact(4).any(|px| px[3] != 0),
                "{icon:?} has visible pixels"
            );
            assert!(
                rgba.chunks_exact(4).any(|px| px[3] == 0),
                "{icon:?} preserves transparency"
            );
        }
    }

    #[test]
    fn each_toolbar_action_uses_the_correct_supplied_icon() {
        for (i, icon) in [
            Icon::Back,
            Icon::Forward,
            Icon::Reload,
            Icon::Home,
            Icon::Search,
            Icon::Plus,
            Icon::Download,
        ]
        .into_iter()
        .enumerate()
        {
            assert_eq!(base_index(icon), i);
            assert_eq!(icon_at(i), icon);
        }
    }
}
