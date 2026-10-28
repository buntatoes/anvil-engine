// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0-OR-Commercial
// Copyright (c) 2026 Buntato
//! Windows text path. This module is compiled only under `#[cfg(windows)]`.
//!
//! `Framebuffer::draw_text` calls [`draw_system_text`] before the built-in
//! bitmap font. The draw goes through Win32 GDI (`gdi32`): a memory DC, a
//! Segoe UI font, and a top-down 32-bit DIB. Coverage is the brightness of
//! white text on a black background. This is not a GPU text pass and it does
//! not call `anvil-ttf`. A `false` return lets the bitmap font in `lib.rs` run.

use std::ffi::c_void;
use std::ptr::{null, null_mut};

use crate::{Color, Framebuffer};

/// GDI device context (`HDC`).
type Hdc = *mut c_void;
/// GDI font (`HFONT`).
type Hfont = *mut c_void;
/// GDI bitmap (`HBITMAP`).
type Hbitmap = *mut c_void;
/// Any GDI object selected into a DC.
type HgdioBJ = *mut c_void;

/// `FW_NORMAL`. Regular stroke weight, not bold.
const FW_NORMAL: i32 = 400;
/// `DEFAULT_CHARSET`. Let GDI pick the character set for Segoe UI.
const DEFAULT_CHARSET: u32 = 1;
/// `OUT_DEFAULT_PRECIS`.
const OUT_DEFAULT_PRECIS: u32 = 0;
/// `CLIP_DEFAULT_PRECIS`.
const CLIP_DEFAULT_PRECIS: u32 = 0;
/// `ANTIALIASED_QUALITY`. Grayscale edges, not ClearType.
const ANTIALIASED_QUALITY: u32 = 4;
/// `DEFAULT_PITCH`. No family bits; the face name selects the font.
const DEFAULT_PITCH: u32 = 0;
/// `BI_RGB`. Uncompressed DIB. 32-bit pixels are stored B, G, R, reserved.
const BI_RGB: u32 = 0;
/// `DIB_RGB_COLORS`. The color table is not used for a 32-bit DIB.
const DIB_RGB_COLORS: u32 = 0;
/// `OPAQUE`. GDI fills the text background instead of leaving it untouched.
const OPAQUE: i32 = 2;

/// `SIZE`. Pixel width and height from `GetTextExtentPoint32W`.
#[repr(C)]
#[derive(Default)]
struct Size {
    cx: i32,
    cy: i32,
}

/// `BITMAPINFOHEADER`. `bi_height` is negative so the DIB is top-down.
#[repr(C)]
struct BitmapInfoHeader {
    bi_size: u32,
    bi_width: i32,
    bi_height: i32,
    bi_planes: u16,
    bi_bit_count: u16,
    bi_compression: u32,
    bi_size_image: u32,
    bi_x_pels_per_meter: i32,
    bi_y_pels_per_meter: i32,
    bi_clr_used: u32,
    bi_clr_important: u32,
}

/// `BITMAPINFO`. One unused color entry keeps the layout GDI expects.
#[repr(C)]
struct BitmapInfo {
    bmi_header: BitmapInfoHeader,
    bmi_colors: [u32; 1],
}

// `gdi32` entry points used to measure and draw one string. No other GDI calls.
#[link(name = "gdi32")]
unsafe extern "system" {
    /// Memory DC compatible with the screen. `null` asks for a display-compatible DC.
    fn CreateCompatibleDC(hdc: Hdc) -> Hdc;
    /// Releases a DC from `CreateCompatibleDC`.
    fn DeleteDC(hdc: Hdc) -> i32;
    /// Creates the Segoe UI font. A negative height is the character height in pixels.
    fn CreateFontW(
        c_height: i32,
        c_width: i32,
        c_escapement: i32,
        c_orientation: i32,
        c_weight: i32,
        b_italic: u32,
        b_underline: u32,
        b_strike_out: u32,
        i_char_set: u32,
        i_out_precision: u32,
        i_clip_precision: u32,
        i_quality: u32,
        i_pitch_and_family: u32,
        psz_face_name: *const u16,
    ) -> Hfont;
    /// Allocates the top-down 32-bit DIB and returns its pixel pointer.
    fn CreateDIBSection(
        hdc: Hdc,
        pbmi: *const BitmapInfo,
        usage: u32,
        ppv_bits: *mut *mut c_void,
        h_section: *mut c_void,
        offset: u32,
    ) -> Hbitmap;
    /// Selects a font or bitmap into the DC and returns the previous object.
    fn SelectObject(hdc: Hdc, h: HgdioBJ) -> HgdioBJ;
    /// Destroys a font or bitmap that is no longer selected.
    fn DeleteObject(ho: HgdioBJ) -> i32;
    /// `OPAQUE` so unpainted texels stay black and read as zero coverage.
    fn SetBkMode(hdc: Hdc, mode: i32) -> i32;
    /// Background `COLORREF`. `0x00000000` is black.
    fn SetBkColor(hdc: Hdc, color: u32) -> u32;
    /// Text `COLORREF` in `0x00BBGGRR`. `0x00FFFFFF` is white.
    fn SetTextColor(hdc: Hdc, color: u32) -> u32;
    /// Measures the UTF-16 string in pixels for the selected font.
    fn GetTextExtentPoint32W(hdc: Hdc, lp_string: *const u16, c: i32, psizl: *mut Size) -> i32;
    /// Draws the string at the origin of the memory DC.
    fn TextOutW(hdc: Hdc, x: i32, y: i32, lp_string: *const u16, c: i32) -> i32;
    /// Finishes pending GDI work before the DIB bytes are read.
    fn GdiFlush() -> i32;
}

/// Draw `text` with GDI and composite it into `fb`.
///
/// Returns `true` when the string was drawn or when there was nothing to draw
/// (empty text, non-positive size, or zero alpha). Returns `false` when the
/// DC, font, extent, or DIB cannot be created, so the caller can use the
/// bitmap font. The glyph height is `font_size * 0.90`, at least 8 pixels.
/// The DIB is clamped to 4096 by 512. Coverage is the average of the B, G,
/// and R bytes, multiplied by `color.a`. The line box is `font_size * 1.25`
/// and the bitmap is centered in it.
pub fn draw_system_text(
    fb: &mut Framebuffer,
    x: f32,
    y: f32,
    text: &str,
    font_size: f32,
    color: Color,
) -> bool {
    if text.is_empty()
        || font_size <= 0.0
        || color.a == 0
        || y + font_size * 1.5 < 0.0
        || y > fb.height() as f32
    {
        return true;
    }
    let utf16: Vec<u16> = text.encode_utf16().collect();
    if utf16.is_empty() {
        return true;
    }
    let height_px = (font_size * 0.90).round().max(8.0) as i32;
    let face: Vec<u16> = "Segoe UI\0".encode_utf16().collect();

    unsafe {
        let hdc = CreateCompatibleDC(null_mut());
        if hdc.is_null() {
            return false;
        }
        let hfont = CreateFontW(
            -height_px,
            0,
            0,
            0,
            FW_NORMAL,
            0,
            0,
            0,
            DEFAULT_CHARSET,
            OUT_DEFAULT_PRECIS,
            CLIP_DEFAULT_PRECIS,
            ANTIALIASED_QUALITY,
            DEFAULT_PITCH,
            face.as_ptr(),
        );
        if hfont.is_null() {
            DeleteDC(hdc);
            return false;
        }
        let old_font = SelectObject(hdc, hfont);

        let mut extent = Size::default();
        if GetTextExtentPoint32W(hdc, utf16.as_ptr(), utf16.len() as i32, &mut extent) == 0
            || extent.cx <= 0
            || extent.cy <= 0
        {
            SelectObject(hdc, old_font);
            DeleteObject(hfont);
            DeleteDC(hdc);
            return false;
        }

        let w = extent.cx.min(4096);
        let h = extent.cy.min(512);
        let info = BitmapInfo {
            bmi_header: BitmapInfoHeader {
                bi_size: size_of::<BitmapInfoHeader>() as u32,
                bi_width: w,
                bi_height: -h, // top-down DIB
                bi_planes: 1,
                bi_bit_count: 32,
                bi_compression: BI_RGB,
                bi_size_image: 0,
                bi_x_pels_per_meter: 0,
                bi_y_pels_per_meter: 0,
                bi_clr_used: 0,
                bi_clr_important: 0,
            },
            bmi_colors: [0],
        };
        let mut bits: *mut c_void = null_mut();
        let hbmp = CreateDIBSection(hdc, &info, DIB_RGB_COLORS, &mut bits, null_mut(), 0);
        if hbmp.is_null() || bits.is_null() {
            if !hbmp.is_null() {
                DeleteObject(hbmp);
            }
            SelectObject(hdc, old_font);
            DeleteObject(hfont);
            DeleteDC(hdc);
            return false;
        }

        let old_bmp = SelectObject(hdc, hbmp);
        SetBkMode(hdc, OPAQUE);
        SetBkColor(hdc, 0x0000_0000);
        SetTextColor(hdc, 0x00FF_FFFF);
        TextOutW(hdc, 0, 0, utf16.as_ptr(), utf16.len() as i32);
        GdiFlush();

        let slice = std::slice::from_raw_parts(bits.cast::<u8>(), (w as usize) * (h as usize) * 4);
        let base_x = x.round() as i32;
        let line_h = (font_size * 1.25).round() as i32;
        let pad_y = ((line_h - h) / 2).max(0);
        let base_y = y.round() as i32 + pad_y;

        for py in 0..h {
            let row_offset = (py as usize) * (w as usize) * 4;
            for px in 0..w {
                let idx = row_offset + (px as usize) * 4;
                let cov =
                    ((slice[idx] as u16 + slice[idx + 1] as u16 + slice[idx + 2] as u16) / 3) as u8;
                if cov > 0 {
                    fb.set_glyph_pixel_gamma_correct(base_x + px, base_y + py, color, cov);
                }
            }
        }

        SelectObject(hdc, old_bmp);
        SelectObject(hdc, old_font);
        DeleteObject(hbmp);
        DeleteObject(hfont);
        DeleteDC(hdc);
        // Keeps the `null` import referenced. It is not a GDI call.
        let _ = null::<c_void>();
    }
    true
}
