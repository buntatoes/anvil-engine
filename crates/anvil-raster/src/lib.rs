// SPDX-License-Identifier: LicenseRef-Proprietary
// Copyright (c) 2026 Buntato
//! CPU RGBA framebuffer and signed-distance rasterizer.
//!
//! `anvil-browser` calls [`rasterize`] with a `anvil-paint` display list, then
//! blits the pixels into the window. The buffer is top-down RGBA8 in this
//! `rasterize_quad_on_device` shades one quad through the anvil-gpu tile device.
//!
//! Rounded boxes, box shadows, and linear gradients call the C++20 scanline
//! and compositor helpers when `anvil_cpp_accel` is linked, or fall back to the
//! CPU helpers in `anvil-gpu` (`sdf_rounded_box`, `sdf_box_shadow_alpha`, and
//! `sample_linear_gradient`). Images are `anvil-image` buffers. `draw_image`
//! resizes with that crate's bilinear filter and samples the result. This
//! crate does not call `anvil-video`. A video frame is drawn only after the
//! browser has already copied the surface into an `Image` on the display list.
//!
//! Display list layers: a sticky layer is moved by its constraint for the
//! current scroll and the viewport size, then drawn like any other items.
//! A transform layer that only moves and evenly scales is drawn directly.
//! Any other transform (rotation, skew, uneven or negative scale) is drawn
//! into a layer buffer and resampled with bilinear filtering, so its edges
//! are smoothed. Clips inside the layer turn with it; clips from outside it
//! stay screen-aligned. A transform that cannot be inverted draws nothing.
//! Fixed boxes inside a transformed box move with it. An opacity layer is
//! drawn the same way with no transform and blended at its opacity; at 0 it
//! draws nothing.
//!
//! Text on Windows tries Win32 GDI in `font` and, if that returns false, the
//! built-in 5x7 bitmap. Text on Linux loads DejaVu Sans or Liberation Sans
//! through `anvil-ttf` when one of those files parses. Otherwise the same
//! bitmap font is used.

// Windows only. GDI text. Linux does not compile this module.
#[cfg(windows)]
mod font;

#[cfg(not(anvil_cpp_accel))]
use anvil_gpu::{GpuColor, sample_linear_gradient, sdf_box_shadow_alpha, sdf_rounded_box};
use anvil_image::Image;
use anvil_paint::{DisplayItem, DisplayList, Layer, LayerEffect, Transform2D};
pub use anvil_style::Color;
pub use anvil_ttf::{TtfFont, interpret_type2_charstring, interpret_type2_charstring_with_subrs};

// Scanline and compositor entry points from anvil_raster_accel.cpp and
// anvil_raster_compositor.cpp. Present only when build.rs set anvil_cpp_accel.
#[cfg(anvil_cpp_accel)]
unsafe extern "C" {
    // Must return 16 or is_cpp_raster_kernel_active reports the kernel as off.
    fn anvil_cpp_raster_kernel_version() -> u32;
    // Top-down RGBA to bottom-up BGRA. Used by Framebuffer::to_bgra_bottom_up.
    fn anvil_cpp_rgba_to_bgra_bottom_up(
        src_rgba: *const u8,
        dst_bgra: *mut u8,
        width: u32,
        height: u32,
    );
    // Opaque or blended axis-aligned fill. The clip stack is applied in Rust first.
    fn anvil_cpp_fill_rect_rgba(
        pixels: *mut u8,
        fb_width: u32,
        fb_height: u32,
        x0: i32,
        y0: i32,
        x1: i32,
        y1: i32,
        r: u8,
        g: u8,
        b: u8,
        a: u8,
    );
    // Rounded fill plus optional border. Its SDF is local to the C++ file.
    fn anvil_cpp_fill_rounded_rect_sdf(
        pixels: *mut u8,
        fb_width: u32,
        fb_height: u32,
        x0: i32,
        y0: i32,
        x1: i32,
        y1: i32,
        x: f32,
        y: f32,
        width: f32,
        height: f32,
        radius: f32,
        border_width: f32,
        fill_r: u8,
        fill_g: u8,
        fill_b: u8,
        fill_a: u8,
        border_r: u8,
        border_g: u8,
        border_b: u8,
        border_a: u8,
    );
    // Analytical SDF box-shadow scanline pass.
    fn anvil_cpp_fill_box_shadow_sdf(
        pixels: *mut u8,
        fb_width: u32,
        fb_height: u32,
        x0: i32,
        y0: i32,
        x1: i32,
        y1: i32,
        cx: f32,
        cy: f32,
        half_w: f32,
        half_h: f32,
        radius: f32,
        blur: f32,
        r: u8,
        g: u8,
        b: u8,
        a: u8,
    );
    // Rounded-rect two-stop linear gradient scanline pass.
    fn anvil_cpp_fill_linear_gradient_sdf(
        pixels: *mut u8,
        fb_width: u32,
        fb_height: u32,
        x0: i32,
        y0: i32,
        x1: i32,
        y1: i32,
        x: f32,
        y: f32,
        width: f32,
        height: f32,
        radius: f32,
        border_width: f32,
        angle_deg: f32,
        start_r: u8,
        start_g: u8,
        start_b: u8,
        start_a: u8,
        end_r: u8,
        end_g: u8,
        end_b: u8,
        end_a: u8,
        border_r: u8,
        border_g: u8,
        border_b: u8,
        border_a: u8,
    );
    // 8-bit coverage glyph bitmap blender.
    fn anvil_cpp_blend_glyph_coverage(
        pixels: *mut u8,
        fb_width: u32,
        fb_height: u32,
        clip_x0: i32,
        clip_y0: i32,
        clip_x1: i32,
        clip_y1: i32,
        dst_x: i32,
        dst_y: i32,
        coverage: *const u8,
        glyph_w: u32,
        glyph_h: u32,
        r: u8,
        g: u8,
        b: u8,
        a: u8,
    );
    // Clipped RGBA image blitter with optional rounded-corner SDF mask.
    fn anvil_cpp_blit_rgba_clipped(
        dst_pixels: *mut u8,
        fb_width: u32,
        fb_height: u32,
        clip_x0: i32,
        clip_y0: i32,
        clip_x1: i32,
        clip_y1: i32,
        base_x: i32,
        base_y: i32,
        src_rgba: *const u8,
        src_w: u32,
        src_h: u32,
        border_radius: f32,
    );
    // Semi-planar NV12 (Y + UV) to RGBA8 BT.709 color conversion.
    fn anvil_cpp_simd_width_bytes() -> u32;
    fn anvil_cpp_blend_subpixel_lcd(
        pixels: *mut u8,
        fb_width: u32,
        fb_height: u32,
        clip_x0: i32,
        clip_y0: i32,
        clip_x1: i32,
        clip_y1: i32,
        dst_x: i32,
        dst_y: i32,
        rgb_coverage: *const u8,
        glyph_w: u32,
        glyph_h: u32,
        r: u8,
        g: u8,
        b: u8,
        a: u8,
    );
    fn anvil_cpp_box_blur_rgba(
        pixels: *mut u8,
        scratch: *mut u8,
        fb_width: u32,
        fb_height: u32,
        x0: i32,
        y0: i32,
        x1: i32,
        y1: i32,
        radius: u32,
    );
    fn anvil_cpp_nv12_to_rgba_bt709(
        y_plane: *const u8,
        uv_plane: *const u8,
        dst_rgba: *mut u8,
        width: u32,
        height: u32,
    );
}

/// `true` only when `anvil_raster_accel.cpp` is linked and its version symbol is 16.
/// A build that skipped the compiler returns `false`.
/// Tile dimension in pixels used by incremental dirty-tile tracking and parallel strip rasterization.
pub const TILE_SIZE: u32 = 64;

/// Active SIMD span width in bytes reported by the C++20 raster kernel (`16` for 128-bit SSE2 on x86_64).
pub fn raster_simd_width_bytes() -> u32 {
    #[cfg(anvil_cpp_accel)]
    {
        unsafe { anvil_cpp_simd_width_bytes() }
    }
    #[cfg(not(anvil_cpp_accel))]
    {
        4
    }
}

pub fn is_cpp_raster_kernel_active() -> bool {
    #[cfg(anvil_cpp_accel)]
    {
        unsafe { anvil_cpp_raster_kernel_version() == 16 }
    }
    #[cfg(not(anvil_cpp_accel))]
    {
        false
    }
}

/// Convert a semi-planar `NV12` (`Y` plane + interleaved `UV` plane) surface into top-down `RGBA8`
/// using BT.709 coefficients (accelerated via C++20 when `anvil_cpp_accel` is linked).
pub fn convert_nv12_to_rgba_bt709(
    y_plane: &[u8],
    uv_plane: &[u8],
    width: u32,
    height: u32,
) -> Vec<u8> {
    let pixel_count = (width as usize).saturating_mul(height as usize);
    let uv_stride = ((width as usize) + 1) & !1;
    let uv_required = ((height as usize).div_ceil(2)).saturating_mul(uv_stride);
    if width == 0 || height == 0 || y_plane.len() < pixel_count || uv_plane.len() < uv_required {
        return Vec::new();
    }
    let mut out = vec![0u8; pixel_count * 4];
    #[cfg(anvil_cpp_accel)]
    unsafe {
        anvil_cpp_nv12_to_rgba_bt709(
            y_plane.as_ptr(),
            uv_plane.as_ptr(),
            out.as_mut_ptr(),
            width,
            height,
        );
        return out;
    }
    #[cfg(not(anvil_cpp_accel))]
    {
        for py in 0..(height as usize) {
            let uv_row = (py / 2) * uv_stride;
            for px in 0..(width as usize) {
                let y_val = i32::from(y_plane[py * (width as usize) + px]);
                let uv_idx = uv_row + (px & !1);
                let u_val = i32::from(uv_plane[uv_idx]) - 128;
                let v_val = i32::from(uv_plane[uv_idx + 1]) - 128;
                let c = (y_val - 16).max(0) * 298;
                let r = ((c + 459 * v_val + 128) >> 8).clamp(0, 255) as u8;
                let g = ((c - 55 * u_val - 136 * v_val + 128) >> 8).clamp(0, 255) as u8;
                let b = ((c + 541 * u_val + 128) >> 8).clamp(0, 255) as u8;
                let idx = (py * (width as usize) + px) * 4;
                out[idx] = r;
                out[idx + 1] = g;
                out[idx + 2] = b;
                out[idx + 3] = 255;
            }
        }
        out
    }
}

/// Clear color when the display list does not start with a full-width background rect.
const PAPER: Color = Color {
    r: 255,
    g: 255,
    b: 255,
    a: 255,
};

/// Top-down RGBA8 buffer plus a clip stack.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Framebuffer {
    width: u32,
    height: u32,
    pixels: Vec<u8>,
    clips: Vec<(i32, i32, i32, i32)>,
}

impl Framebuffer {
    /// Allocate `width * height` pixels filled with `clear`. The clip stack starts empty.
    pub fn new(width: u32, height: u32, clear: Color) -> Self {
        let count = (width as usize).saturating_mul(height as usize);
        let mut pixels = Vec::with_capacity(count * 4);
        for _ in 0..count {
            pixels.extend_from_slice(&[clear.r, clear.g, clear.b, clear.a]);
        }
        Self {
            width,
            height,
            pixels,
            clips: Vec::new(),
        }
    }

    /// Intersect a new clip with the framebuffer and with the clip already on top.
    pub fn push_clip(&mut self, x: f32, y: f32, width: f32, height: f32) {
        let mut x0 = (x.floor() as i32).max(0);
        let mut y0 = (y.floor() as i32).max(0);
        let mut x1 = ((x + width.max(0.0)).ceil() as i32).min(self.width as i32);
        let mut y1 = ((y + height.max(0.0)).ceil() as i32).min(self.height as i32);
        if let Some(&(px0, py0, px1, py1)) = self.clips.last() {
            x0 = x0.max(px0);
            y0 = y0.max(py0);
            x1 = x1.min(px1);
            y1 = y1.min(py1);
        }
        self.clips.push((x0, y0, x1, y1));
    }

    /// Drop the top clip. A pop on an empty stack does nothing.
    pub fn pop_clip(&mut self) {
        self.clips.pop();
    }

    /// Buffer width in pixels.
    pub fn width(&self) -> u32 {
        self.width
    }

    /// Buffer height in pixels.
    pub fn height(&self) -> u32 {
        self.height
    }

    /// Raw top-down RGBA bytes. Length is `width * height * 4`.
    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }

    /// Read one pixel. `None` when `x` or `y` is outside the buffer. Clips are ignored.
    pub fn pixel(&self, x: u32, y: u32) -> Option<Color> {
        if x >= self.width || y >= self.height {
            return None;
        }
        let offset = ((y as usize) * (self.width as usize) + (x as usize)) * 4;
        Some(Color {
            r: self.pixels[offset],
            g: self.pixels[offset + 1],
            b: self.pixels[offset + 2],
            a: self.pixels[offset + 3],
        })
    }

    /// Blend `color` at an integer pixel.
    pub fn set_pixel(&mut self, x: i32, y: i32, color: Color) {
        if x < 0 || y < 0 || color.a == 0 {
            return;
        }
        if let Some(&(cx0, cy0, cx1, cy1)) = self.clips.last() {
            if x < cx0 || x >= cx1 || y < cy0 || y >= cy1 {
                return;
            }
        }
        let (x, y) = (x as u32, y as u32);
        if x >= self.width || y >= self.height {
            return;
        }
        let offset = ((y as usize) * (self.width as usize) + (x as usize)) * 4;
        if color.a == 255 {
            self.pixels[offset] = color.r;
            self.pixels[offset + 1] = color.g;
            self.pixels[offset + 2] = color.b;
            self.pixels[offset + 3] = 255;
            return;
        }
        let alpha = color.a as u16;
        let inv = 255 - alpha;
        self.pixels[offset] =
            ((color.r as u16 * alpha + self.pixels[offset] as u16 * inv) / 255) as u8;
        self.pixels[offset + 1] =
            ((color.g as u16 * alpha + self.pixels[offset + 1] as u16 * inv) / 255) as u8;
        self.pixels[offset + 2] =
            ((color.b as u16 * alpha + self.pixels[offset + 2] as u16 * inv) / 255) as u8;
        self.pixels[offset + 3] = 255;
    }

    /// Current clip, or the whole buffer when the clip stack is empty.
    fn active_clip_bounds(&self) -> (i32, i32, i32, i32) {
        self.clips
            .last()
            .copied()
            .unwrap_or((0, 0, self.width as i32, self.height as i32))
    }

    /// Inclusive-exclusive pixel span for a rectangle after buffer and clip limits.
    fn clipped_span(
        &self,
        x: f32,
        y: f32,
        width: f32,
        height: f32,
    ) -> Option<(i32, i32, i32, i32)> {
        if width <= 0.0 || height <= 0.0 {
            return None;
        }
        let mut x0 = (x.floor() as i32).max(0);
        let mut y0 = (y.floor() as i32).max(0);
        let mut x1 = ((x + width).ceil() as i32).min(self.width as i32);
        let mut y1 = ((y + height).ceil() as i32).min(self.height as i32);
        if let Some(&(cx0, cy0, cx1, cy1)) = self.clips.last() {
            x0 = x0.max(cx0);
            y0 = y0.max(cy0);
            x1 = x1.min(cx1);
            y1 = y1.min(cy1);
        }
        (x0 < x1 && y0 < y1).then_some((x0, y0, x1, y1))
    }

    /// Fill one clipped span on the CPU. Tests use this to compare with the C++ fill.
    #[cfg(any(test, not(anvil_cpp_accel)))]
    fn fill_span_cpu(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, color: Color) {
        for py in y0..y1 {
            for px in x0..x1 {
                self.set_pixel(px, py, color);
            }
        }
    }

    /// Fill an axis-aligned rectangle, clipped to the buffer and to the top clip.
    /// Blend quads from the anvil-gpu draw device into this buffer.
    fn paint_device_quads(&mut self, quads: &[anvil_gpu::GpuQuad]) {
        if quads.is_empty() {
            return;
        }
        let (cx0, cy0, cx1, cy1) = self.active_clip_bounds();
        let mut min_x = cx1;
        let mut min_y = cy1;
        let mut max_x = cx0;
        let mut max_y = cy0;
        for quad in quads {
            let x0 = quad.clip.x.floor() as i32;
            let y0 = quad.clip.y.floor() as i32;
            let x1 = (quad.clip.x + quad.clip.width).ceil() as i32;
            let y1 = (quad.clip.y + quad.clip.height).ceil() as i32;
            min_x = min_x.min(x0);
            min_y = min_y.min(y0);
            max_x = max_x.max(x1);
            max_y = max_y.max(y1);
        }
        min_x = min_x.max(cx0).max(0);
        min_y = min_y.max(cy0).max(0);
        max_x = max_x.min(cx1).min(self.width as i32);
        max_y = max_y.min(cy1).min(self.height as i32);
        if min_x >= max_x || min_y >= max_y {
            return;
        }
        let origin_x = min_x as f32;
        let origin_y = min_y as f32;
        let tile_w = (max_x - min_x) as u32;
        let tile_h = (max_y - min_y) as u32;
        let shifted: Vec<anvil_gpu::GpuQuad> = quads
            .iter()
            .map(|quad| {
                let mut next = *quad;
                next.rect.x -= origin_x;
                next.rect.y -= origin_y;
                next.clip.x -= origin_x;
                next.clip.y -= origin_y;
                next
            })
            .collect();
        let pixels = anvil_gpu::GpuDevice::open().render_quads(
            tile_w,
            tile_h,
            anvil_gpu::GpuColor::rgba(0, 0, 0, 0),
            &shifted,
            None,
        );
        for py in 0..tile_h {
            for px in 0..tile_w {
                let index = ((py * tile_w + px) * 4) as usize;
                let alpha = pixels[index + 3];
                if alpha == 0 {
                    continue;
                }
                self.set_pixel(
                    min_x + px as i32,
                    min_y + py as i32,
                    Color {
                        r: pixels[index],
                        g: pixels[index + 1],
                        b: pixels[index + 2],
                        a: alpha,
                    },
                );
            }
        }
    }

    pub fn fill_rect(&mut self, x: f32, y: f32, width: f32, height: f32, color: Color) {
        if color.a == 0 {
            return;
        }
        let Some((x0, y0, x1, y1)) = self.clipped_span(x, y, width, height) else {
            return;
        };
        #[cfg(anvil_cpp_accel)]
        unsafe {
            anvil_cpp_fill_rect_rgba(
                self.pixels.as_mut_ptr(),
                self.width,
                self.height,
                x0,
                y0,
                x1,
                y1,
                color.r,
                color.g,
                color.b,
                color.a,
            );
            return;
        }
        #[cfg(not(anvil_cpp_accel))]
        self.fill_span_cpu(x0, y0, x1, y1, color);
    }

    /// Same rectangle as [`Self::fill_rect`], always on the CPU loop.
    #[cfg(test)]
    fn fill_rect_cpu(&mut self, x: f32, y: f32, width: f32, height: f32, color: Color) {
        if color.a == 0 {
            return;
        }
        let Some((x0, y0, x1, y1)) = self.clipped_span(x, y, width, height) else {
            return;
        };
        self.fill_span_cpu(x0, y0, x1, y1, color);
    }

    /// Fill a rounded rectangle and blend a border when `border_width` is positive.
    #[allow(clippy::too_many_arguments, unreachable_code)]
    pub fn fill_rounded_rect(
        &mut self,
        x: f32,
        y: f32,
        width: f32,
        height: f32,
        radius: f32,
        border_width: f32,
        fill_color: Color,
        border_color: Color,
    ) {
        if width <= 0.0 || height <= 0.0 {
            return;
        }
        let mut quad = anvil_gpu::GpuQuad::solid(
            anvil_gpu::GpuRect::new(x, y, width, height),
            anvil_gpu::GpuColor::rgba(fill_color.r, fill_color.g, fill_color.b, fill_color.a),
        );
        if radius > 0.0 || border_width > 0.0 {
            quad = quad.with_rounded_border(
                radius,
                border_width,
                anvil_gpu::GpuColor::rgba(
                    border_color.r,
                    border_color.g,
                    border_color.b,
                    border_color.a,
                ),
            );
        }
        self.paint_device_quads(&[quad]);
        return;
        let mut x0 = (x.floor() as i32).max(0);
        let mut y0 = (y.floor() as i32).max(0);
        let mut x1 = ((x + width).ceil() as i32).min(self.width as i32);
        let mut y1 = ((y + height).ceil() as i32).min(self.height as i32);
        if let Some(&(cx0, cy0, cx1, cy1)) = self.clips.last() {
            x0 = x0.max(cx0);
            y0 = y0.max(cy0);
            x1 = x1.min(cx1);
            y1 = y1.min(cy1);
        }
        if x0 >= x1 || y0 >= y1 {
            return;
        }
        #[cfg(anvil_cpp_accel)]
        unsafe {
            anvil_cpp_fill_rounded_rect_sdf(
                self.pixels.as_mut_ptr(),
                self.width,
                self.height,
                x0,
                y0,
                x1,
                y1,
                x,
                y,
                width,
                height,
                radius,
                border_width,
                fill_color.r,
                fill_color.g,
                fill_color.b,
                fill_color.a,
                border_color.r,
                border_color.g,
                border_color.b,
                border_color.a,
            );
            return;
        }
        #[cfg(not(anvil_cpp_accel))]
        {
            let half_w = width * 0.5;
            let half_h = height * 0.5;
            let cx = x + half_w;
            let cy = y + half_h;

            for py in y0..y1 {
                for px in x0..x1 {
                    let sx = px as f32 + 0.5;
                    let sy = py as f32 + 0.5;
                    let dist = sdf_rounded_box(sx - cx, sy - cy, half_w, half_h, radius);
                    let outer_alpha = (0.5 - dist).clamp(0.0, 1.0);
                    if outer_alpha <= 0.0 {
                        continue;
                    }
                    let mut c = fill_color;
                    if border_width > 0.0 {
                        let inner_dist = dist + border_width;
                        let border_mix = (0.5 + inner_dist).clamp(0.0, 1.0);
                        let inv = 1.0 - border_mix;
                        c = Color {
                            r: ((c.r as f32) * inv + (border_color.r as f32) * border_mix).round()
                                as u8,
                            g: ((c.g as f32) * inv + (border_color.g as f32) * border_mix).round()
                                as u8,
                            b: ((c.b as f32) * inv + (border_color.b as f32) * border_mix).round()
                                as u8,
                            a: ((c.a as f32) * inv + (border_color.a as f32) * border_mix).round()
                                as u8,
                        };
                    }
                    c.a = ((c.a as f32) * outer_alpha).round() as u8;
                    self.set_pixel(px, py, c);
                }
            }
        }
    }

    /// Draw a box shadow through the anvil-gpu draw device.
    #[allow(clippy::too_many_arguments, unreachable_code)]
    pub fn fill_box_shadow(
        &mut self,
        x: f32,
        y: f32,
        width: f32,
        height: f32,
        offset_x: f32,
        offset_y: f32,
        blur_radius: f32,
        spread_radius: f32,
        radius: f32,
        color: Color,
    ) {
        if width <= 0.0 || height <= 0.0 || color.a == 0 {
            return;
        }
        let quad = anvil_gpu::GpuQuad::box_shadow(
            anvil_gpu::GpuRect::new(x, y, width, height),
            offset_x,
            offset_y,
            blur_radius,
            spread_radius,
            radius,
            anvil_gpu::GpuColor::rgba(color.r, color.g, color.b, color.a),
        );
        self.paint_device_quads(&[quad]);
        return;
        let blur = blur_radius.max(0.0);
        let sx_rect = x + offset_x - spread_radius;
        let sy_rect = y + offset_y - spread_radius;
        let sw = (width + spread_radius * 2.0).max(1.0);
        let sh = (height + spread_radius * 2.0).max(1.0);
        let sr = (radius + spread_radius).max(0.0);

        let mut x0 = ((sx_rect - blur).floor() as i32).max(0);
        let mut y0 = ((sy_rect - blur).floor() as i32).max(0);
        let mut x1 = ((sx_rect + sw + blur).ceil() as i32).min(self.width as i32);
        let mut y1 = ((sy_rect + sh + blur).ceil() as i32).min(self.height as i32);
        if let Some(&(cx0, cy0, cx1, cy1)) = self.clips.last() {
            x0 = x0.max(cx0);
            y0 = y0.max(cy0);
            x1 = x1.min(cx1);
            y1 = y1.min(cy1);
        }
        if x0 >= x1 || y0 >= y1 {
            return;
        }
        let half_w = sw * 0.5;
        let half_h = sh * 0.5;
        let cx = sx_rect + half_w;
        let cy = sy_rect + half_h;

        #[cfg(anvil_cpp_accel)]
        unsafe {
            anvil_cpp_fill_box_shadow_sdf(
                self.pixels.as_mut_ptr(),
                self.width,
                self.height,
                x0,
                y0,
                x1,
                y1,
                cx,
                cy,
                half_w,
                half_h,
                sr,
                blur,
                color.r,
                color.g,
                color.b,
                color.a,
            );
            return;
        }
        #[cfg(not(anvil_cpp_accel))]
        for py in y0..y1 {
            for px in x0..x1 {
                let sample_x = px as f32 + 0.5;
                let sample_y = py as f32 + 0.5;
                let dist = sdf_rounded_box(sample_x - cx, sample_y - cy, half_w, half_h, sr);
                let alpha = sdf_box_shadow_alpha(dist, blur);
                if alpha <= 0.0 {
                    continue;
                }
                let mut c = color;
                c.a = ((c.a as f32) * alpha).round() as u8;
                self.set_pixel(px, py, c);
            }
        }
    }

    /// Fill a rounded rectangle with a two-stop linear gradient through the draw device.
    #[allow(clippy::too_many_arguments, unreachable_code)]
    pub fn fill_linear_gradient_rect(
        &mut self,
        x: f32,
        y: f32,
        width: f32,
        height: f32,
        radius: f32,
        border_width: f32,
        angle_deg: f32,
        start_color: Color,
        end_color: Color,
        border_color: Color,
    ) {
        if width <= 0.0 || height <= 0.0 {
            return;
        }
        let mut quad = anvil_gpu::GpuQuad::linear_gradient(
            anvil_gpu::GpuRect::new(x, y, width, height),
            anvil_gpu::GpuColor::rgba(start_color.r, start_color.g, start_color.b, start_color.a),
            anvil_gpu::GpuColor::rgba(end_color.r, end_color.g, end_color.b, end_color.a),
            angle_deg,
        );
        if radius > 0.0 || border_width > 0.0 {
            quad = quad.with_rounded_border(
                radius,
                border_width,
                anvil_gpu::GpuColor::rgba(
                    border_color.r,
                    border_color.g,
                    border_color.b,
                    border_color.a,
                ),
            );
        }
        self.paint_device_quads(&[quad]);
        return;
        let mut x0 = (x.floor() as i32).max(0);
        let mut y0 = (y.floor() as i32).max(0);
        let mut x1 = ((x + width).ceil() as i32).min(self.width as i32);
        let mut y1 = ((y + height).ceil() as i32).min(self.height as i32);
        if let Some(&(cx0, cy0, cx1, cy1)) = self.clips.last() {
            x0 = x0.max(cx0);
            y0 = y0.max(cy0);
            x1 = x1.min(cx1);
            y1 = y1.min(cy1);
        }
        if x0 >= x1 || y0 >= y1 {
            return;
        }

        #[cfg(anvil_cpp_accel)]
        unsafe {
            anvil_cpp_fill_linear_gradient_sdf(
                self.pixels.as_mut_ptr(),
                self.width,
                self.height,
                x0,
                y0,
                x1,
                y1,
                x,
                y,
                width,
                height,
                radius,
                border_width,
                angle_deg,
                start_color.r,
                start_color.g,
                start_color.b,
                start_color.a,
                end_color.r,
                end_color.g,
                end_color.b,
                end_color.a,
                border_color.r,
                border_color.g,
                border_color.b,
                border_color.a,
            );
            return;
        }
        #[cfg(not(anvil_cpp_accel))]
        {
            let half_w = width * 0.5;
            let half_h = height * 0.5;
            let cx = x + half_w;
            let cy = y + half_h;
            let gpu_start =
                GpuColor::rgba(start_color.r, start_color.g, start_color.b, start_color.a);
            let gpu_end = GpuColor::rgba(end_color.r, end_color.g, end_color.b, end_color.a);

            for py in y0..y1 {
                for px in x0..x1 {
                    let sx = px as f32 + 0.5;
                    let sy = py as f32 + 0.5;
                    let rel_x = sx - cx;
                    let rel_y = sy - cy;
                    let dist = sdf_rounded_box(rel_x, rel_y, half_w, half_h, radius);
                    let outer_alpha = (0.5 - dist).clamp(0.0, 1.0);
                    if outer_alpha <= 0.0 {
                        continue;
                    }
                    let grad = sample_linear_gradient(
                        rel_x, rel_y, half_w, half_h, angle_deg, gpu_start, gpu_end,
                    );
                    let mut c = Color {
                        r: grad.r,
                        g: grad.g,
                        b: grad.b,
                        a: grad.a,
                    };
                    if border_width > 0.0 {
                        let inner_dist = dist + border_width;
                        let border_mix = (0.5 + inner_dist).clamp(0.0, 1.0);
                        let inv = 1.0 - border_mix;
                        c = Color {
                            r: ((c.r as f32) * inv + (border_color.r as f32) * border_mix).round()
                                as u8,
                            g: ((c.g as f32) * inv + (border_color.g as f32) * border_mix).round()
                                as u8,
                            b: ((c.b as f32) * inv + (border_color.b as f32) * border_mix).round()
                                as u8,
                            a: ((c.a as f32) * inv + (border_color.a as f32) * border_mix).round()
                                as u8,
                        };
                    }
                    c.a = ((c.a as f32) * outer_alpha).round() as u8;
                    self.set_pixel(px, py, c);
                }
            }
        }
    }

    /// Stamp one `anvil-ttf` glyph and return its advance in pixels.
    pub fn draw_ttf_glyph(
        &mut self,
        x: f32,
        y: f32,
        ch: char,
        font_size: f32,
        color: Color,
        font: &TtfFont,
    ) -> Option<f32> {
        let bmp = font.rasterize_glyph(ch, font_size)?;
        let baseline_y = y + (font.ascender as f32) * font_size / (font.units_per_em.max(1) as f32);
        let gx0 = (x.round() as i32) + bmp.bearing_x;
        let gy0 = (baseline_y.round() as i32) - bmp.bearing_y;

        #[cfg(anvil_cpp_accel)]
        unsafe {
            let (cx0, cy0, cx1, cy1) = self.active_clip_bounds();
            anvil_cpp_blend_glyph_coverage(
                self.pixels.as_mut_ptr(),
                self.width,
                self.height,
                cx0,
                cy0,
                cx1,
                cy1,
                gx0,
                gy0,
                bmp.coverage.as_ptr(),
                bmp.width,
                bmp.height,
                color.r,
                color.g,
                color.b,
                color.a,
            );
            return Some(bmp.advance_px);
        }
        #[cfg(not(anvil_cpp_accel))]
        {
            for py in 0..bmp.height {
                for px in 0..bmp.width {
                    let cov = bmp.coverage[(py * bmp.width + px) as usize];
                    if cov == 0 {
                        continue;
                    }
                    let mut c = color;
                    c.a = ((u16::from(c.a) * u16::from(cov) + 127) / 255) as u8;
                    self.set_pixel(gx0 + px as i32, gy0 + py as i32, c);
                }
            }
            Some(bmp.advance_px)
        }
    }

    /// Sample a `anvil-image` buffer into the rectangle.
    /// Stamp one `anvil-ttf` glyph using 3-channel RGB horizontal LCD subpixel coverage.
    pub fn draw_subpixel_glyph(
        &mut self,
        x: f32,
        y: f32,
        ch: char,
        font_size: f32,
        color: Color,
        font: &TtfFont,
    ) -> Option<f32> {
        let bmp = font.rasterize_glyph_subpixel_lcd(ch, font_size)?;
        let baseline_y = y + (font.ascender as f32) * font_size / (font.units_per_em.max(1) as f32);
        let gx0 = (x.round() as i32) + bmp.bearing_x;
        let gy0 = (baseline_y.round() as i32) - bmp.bearing_y;

        #[cfg(anvil_cpp_accel)]
        unsafe {
            let (cx0, cy0, cx1, cy1) = self.active_clip_bounds();
            anvil_cpp_blend_subpixel_lcd(
                self.pixels.as_mut_ptr(),
                self.width,
                self.height,
                cx0,
                cy0,
                cx1,
                cy1,
                gx0,
                gy0,
                bmp.rgb_coverage.as_ptr(),
                bmp.width,
                bmp.height,
                color.r,
                color.g,
                color.b,
                color.a,
            );
            return Some(bmp.advance_px);
        }
        #[cfg(not(anvil_cpp_accel))]
        {
            let (cx0, cy0, cx1, cy1) = self.active_clip_bounds();
            for py in 0..bmp.height {
                let sy = gy0 + py as i32;
                if sy < cy0 || sy >= cy1 || sy < 0 || sy >= self.height as i32 {
                    continue;
                }
                for px in 0..bmp.width {
                    let sx = gx0 + px as i32;
                    if sx < cx0 || sx >= cx1 || sx < 0 || sx >= self.width as i32 {
                        continue;
                    }
                    let c_off = ((py * bmp.width + px) * 3) as usize;
                    let cr = bmp.rgb_coverage[c_off];
                    let cg = bmp.rgb_coverage[c_off + 1];
                    let cb = bmp.rgb_coverage[c_off + 2];
                    if cr == 0 && cg == 0 && cb == 0 {
                        continue;
                    }
                    let ar = (u16::from(color.a) * u16::from(cr) + 127) / 255;
                    let ag = (u16::from(color.a) * u16::from(cg) + 127) / 255;
                    let ab = (u16::from(color.a) * u16::from(cb) + 127) / 255;
                    let off = ((sy as usize) * (self.width as usize) + (sx as usize)) * 4;
                    self.pixels[off] = ((u16::from(color.r) * ar
                        + u16::from(self.pixels[off]) * (255 - ar))
                        / 255) as u8;
                    self.pixels[off + 1] = ((u16::from(color.g) * ag
                        + u16::from(self.pixels[off + 1]) * (255 - ag))
                        / 255) as u8;
                    self.pixels[off + 2] = ((u16::from(color.b) * ab
                        + u16::from(self.pixels[off + 2]) * (255 - ab))
                        / 255) as u8;
                    self.pixels[off + 3] = 255;
                }
            }
            Some(bmp.advance_px)
        }
    }

    /// Apply a 3-pass separable horizontal/vertical sliding-window box blur inside a clipped rect.
    pub fn blur_rect(&mut self, x: f32, y: f32, width: f32, height: f32, radius: u32) {
        if radius == 0 {
            return;
        }
        let Some((x0, y0, x1, y1)) = self.clipped_span(x, y, width, height) else {
            return;
        };
        let rw = (x1 - x0) as usize;
        let rh = (y1 - y0) as usize;
        if rw == 0 || rh == 0 {
            return;
        }
        let mut scratch = vec![0u8; rw * rh * 4];
        #[cfg(anvil_cpp_accel)]
        unsafe {
            anvil_cpp_box_blur_rgba(
                self.pixels.as_mut_ptr(),
                scratch.as_mut_ptr(),
                self.width,
                self.height,
                x0,
                y0,
                x1,
                y1,
                radius,
            );
            return;
        }
        #[cfg(not(anvil_cpp_accel))]
        {
            let rad = radius.min(64) as isize;
            let kernel = (2 * rad + 1) as u32;
            let fb_w = self.width as usize;
            for _ in 0..3 {
                for ry in 0..rh {
                    for rx in 0..rw {
                        let mut sr = 0u32;
                        let mut sg = 0u32;
                        let mut sb = 0u32;
                        let mut sa = 0u32;
                        for k in -rad..=rad {
                            let sx = (rx as isize + k).clamp(0, rw as isize - 1) as usize;
                            let off = ((y0 as usize + ry) * fb_w + (x0 as usize + sx)) * 4;
                            sr += u32::from(self.pixels[off]);
                            sg += u32::from(self.pixels[off + 1]);
                            sb += u32::from(self.pixels[off + 2]);
                            sa += u32::from(self.pixels[off + 3]);
                        }
                        let d_off = (ry * rw + rx) * 4;
                        scratch[d_off] = ((sr + kernel / 2) / kernel) as u8;
                        scratch[d_off + 1] = ((sg + kernel / 2) / kernel) as u8;
                        scratch[d_off + 2] = ((sb + kernel / 2) / kernel) as u8;
                        scratch[d_off + 3] = ((sa + kernel / 2) / kernel) as u8;
                    }
                }
                for rx in 0..rw {
                    for ry in 0..rh {
                        let mut sr = 0u32;
                        let mut sg = 0u32;
                        let mut sb = 0u32;
                        let mut sa = 0u32;
                        for k in -rad..=rad {
                            let sy = (ry as isize + k).clamp(0, rh as isize - 1) as usize;
                            let off = (sy * rw + rx) * 4;
                            sr += u32::from(scratch[off]);
                            sg += u32::from(scratch[off + 1]);
                            sb += u32::from(scratch[off + 2]);
                            sa += u32::from(scratch[off + 3]);
                        }
                        let d_off = ((y0 as usize + ry) * fb_w + (x0 as usize + rx)) * 4;
                        self.pixels[d_off] = ((sr + kernel / 2) / kernel) as u8;
                        self.pixels[d_off + 1] = ((sg + kernel / 2) / kernel) as u8;
                        self.pixels[d_off + 2] = ((sb + kernel / 2) / kernel) as u8;
                        self.pixels[d_off + 3] = ((sa + kernel / 2) / kernel) as u8;
                    }
                }
            }
        }
    }

    pub fn draw_image(
        &mut self,
        x: f32,
        y: f32,
        width: f32,
        height: f32,
        border_radius: f32,
        image: &Image,
    ) {
        if width <= 0.0 || height <= 0.0 || image.width == 0 || image.height == 0 {
            return;
        }
        let dst_w = width.round().max(1.0) as u32;
        let dst_h = height.round().max(1.0) as u32;
        let scaled = image.resize_bilinear(dst_w, dst_h);
        let base_x = x.round() as i32;
        let base_y = y.round() as i32;

        #[cfg(anvil_cpp_accel)]
        unsafe {
            let (cx0, cy0, cx1, cy1) = self.active_clip_bounds();
            anvil_cpp_blit_rgba_clipped(
                self.pixels.as_mut_ptr(),
                self.width,
                self.height,
                cx0,
                cy0,
                cx1,
                cy1,
                base_x,
                base_y,
                scaled.pixels.as_ptr(),
                dst_w,
                dst_h,
                border_radius,
            );
            return;
        }
        #[cfg(not(anvil_cpp_accel))]
        {
            let half_w = (dst_w as f32) * 0.5;
            let half_h = (dst_h as f32) * 0.5;

            for iy in 0..dst_h {
                for ix in 0..dst_w {
                    let Some(rgba) = scaled.pixel(ix, iy) else {
                        continue;
                    };
                    let mut alpha = rgba[3] as f32;
                    if border_radius > 0.0 {
                        let dist = sdf_rounded_box(
                            (ix as f32 + 0.5) - half_w,
                            (iy as f32 + 0.5) - half_h,
                            half_w,
                            half_h,
                            border_radius,
                        );
                        let mask = (0.5 - dist).clamp(0.0, 1.0);
                        alpha *= mask;
                    }
                    if alpha <= 0.5 {
                        continue;
                    }
                    self.set_pixel(
                        base_x + ix as i32,
                        base_y + iy as i32,
                        Color {
                            r: rgba[0],
                            g: rgba[1],
                            b: rgba[2],
                            a: alpha.round() as u8,
                        },
                    );
                }
            }
        }
    }

    /// Draw a run of text. Non-positive size or zero alpha returns immediately.
    pub fn draw_text(&mut self, x: f32, y: f32, text: &str, font_size: f32, color: Color) {
        if font_size <= 0.0 || color.a == 0 {
            return;
        }
        #[cfg(windows)]
        if font::draw_system_text(self, x, y, text, font_size, color) {
            return;
        }
        #[cfg(target_os = "linux")]
        if let Some(sys_font) = linux_system_ttf_font() {
            self.draw_ttf_text(x, y, text, font_size, color, sys_font);
            return;
        }
        self.draw_bitmap_text(x, y, text, font_size, color);
    }

    /// Walk `text` with a parsed `anvil-ttf` font.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    fn draw_ttf_text(
        &mut self,
        x: f32,
        y: f32,
        text: &str,
        font_size: f32,
        color: Color,
        font: &TtfFont,
    ) {
        if font.units_per_em == 0 {
            self.draw_bitmap_text(x, y, text, font_size, color);
            return;
        }
        let scale = font_size / f32::from(font.units_per_em);
        let baseline = y + f32::from(font.ascender) * scale;
        let mut pen = x;
        for ch in text.chars() {
            if let Some(bmp) = font.rasterize_glyph(ch, font_size) {
                // Saturating math: a run placed far off the canvas must not overflow.
                let origin_x = (pen.round() as i32).saturating_add(bmp.bearing_x);
                let width = bmp.width as usize;
                for row in 0..bmp.height {
                    let screen_y = (baseline.round() as i32)
                        .saturating_add(bmp.bearing_y)
                        .saturating_sub(row as i32);
                    for col in 0..bmp.width {
                        let cov = bmp.coverage[row as usize * width + col as usize];
                        if cov == 0 {
                            continue;
                        }
                        let a = ((u16::from(color.a) * u16::from(cov)) / 255) as u8;
                        self.set_pixel(
                            origin_x.saturating_add(col as i32),
                            screen_y,
                            Color {
                                r: color.r,
                                g: color.g,
                                b: color.b,
                                a,
                            },
                        );
                    }
                }
            }
            pen += font.glyph_advance_px(ch, font_size);
        }
    }

    /// Draw with the built-in 5x7 bitmap from `glyph`.
    pub fn draw_bitmap_text(&mut self, x: f32, y: f32, text: &str, font_size: f32, color: Color) {
        if font_size <= 0.0 || color.a == 0 {
            return;
        }
        let scale = ((font_size / 12.0).floor() as i32).max(1);
        let advance = font_size * 0.5;
        let glyph_height = 7 * scale;
        let line_height = (font_size * 1.25).round() as i32;
        let pad_y = ((line_height - glyph_height) / 2).max(0);
        let base_y = (y.round() as i32).saturating_add(pad_y);

        for (index, ch) in text.chars().enumerate() {
            let char_x = (x + index as f32 * advance).round() as i32;
            let rows = glyph(ch);
            for (row_idx, row_bits) in rows.iter().copied().enumerate() {
                for col_idx in 0..5 {
                    if (row_bits >> (4 - col_idx)) & 1 == 1 {
                        let gx = char_x.saturating_add(col_idx * scale);
                        let gy = base_y.saturating_add((row_idx as i32) * scale);
                        for sy in 0..scale {
                            for sx in 0..scale {
                                self.set_pixel(gx.saturating_add(sx), gy.saturating_add(sy), color);
                            }
                        }
                    }
                }
            }
        }
    }

    /// Convert top-down RGBA scanlines into bottom-up BGRA scanlines for GDI.
    pub fn to_bgra_bottom_up(&self) -> Vec<u8> {
        let mut out = vec![0_u8; self.pixels.len()];
        #[cfg(anvil_cpp_accel)]
        unsafe {
            anvil_cpp_rgba_to_bgra_bottom_up(
                self.pixels.as_ptr(),
                out.as_mut_ptr(),
                self.width,
                self.height,
            );
            return out;
        }
        #[cfg(not(anvil_cpp_accel))]
        {
            let stride = (self.width as usize) * 4;
            for row in 0..(self.height as usize) {
                let src_row = (self.height as usize - 1 - row) * stride;
                let dst_row = row * stride;
                for col in 0..(self.width as usize) {
                    let s = src_row + col * 4;
                    let d = dst_row + col * 4;
                    out[d] = self.pixels[s + 2];
                    out[d + 1] = self.pixels[s + 1];
                    out[d + 2] = self.pixels[s];
                    out[d + 3] = self.pixels[s + 3];
                }
            }
            out
        }
    }

    /// Convert top-down RGBA scanlines into top-down BGRA scanlines for X11 (`ZPixmap` 32-bit).
    pub fn to_bgra_top_down(&self) -> Vec<u8> {
        let mut out = vec![0_u8; self.pixels.len()];
        for (dst, src) in out.chunks_exact_mut(4).zip(self.pixels.chunks_exact(4)) {
            dst[0] = src[2];
            dst[1] = src[1];
            dst[2] = src[0];
            dst[3] = src[3];
        }
        out
    }

    /// Encode the framebuffer as a binary PPM (`P6`) image.
    pub fn to_ppm(&self) -> Vec<u8> {
        let header = format!("P6\n{} {}\n255\n", self.width, self.height);
        let mut out =
            Vec::with_capacity(header.len() + (self.width as usize) * (self.height as usize) * 3);
        out.extend_from_slice(header.as_bytes());
        for chunk in self.pixels.chunks_exact(4) {
            out.push(chunk[0]);
            out.push(chunk[1]);
            out.push(chunk[2]);
        }
        out
    }
}

/// Clear color: the first item when it is a large rect at the origin, else white.
fn resolve_canvas_bg(list: &DisplayList, width: u32) -> Color {
    match list.items.first() {
        Some(DisplayItem::Rect {
            x,
            y,
            width: rw,
            height: rh,
            color,
        }) if *x <= 0.0 && *y <= 0.0 && *rw >= (width as f32) * 0.9 && *rh >= 100.0 => *color,
        _ => PAPER,
    }
}

/// Paint `list` into `fb` with every non-fixed item moved up by `scroll_y`
/// and every item moved up by `band_y0`. `viewport_height` is the full
/// visible height, which sticky boxes need even when `fb` is one band.
fn paint_list_with_band_offset(
    fb: &mut Framebuffer,
    list: &DisplayList,
    scroll_y: f32,
    band_y0: f32,
    viewport_height: f32,
) {
    let viewport_width = fb.width as f32;
    let mut ctx = PaintCtx {
        scroll_y,
        band_y0,
        viewport_width,
        viewport_height,
        fixed_depth: 0,
        offscreen: false,
        s: 1.0,
        tx: 0.0,
        ty: 0.0,
        sticky_dx: 0.0,
        sticky_dy: 0.0,
    };
    let mut next_layer = 0usize;
    paint_span(fb, list, 0, list.items.len(), &mut next_layer, &mut ctx);
}

/// Drawing state while walking a display list.
///
/// Items are drawn at `p * s + (tx, ty)` and then moved up by [`Self::dy`].
/// `sticky_dx` / `sticky_dy` add up the sticky shifts of enclosing layers so
/// a nested sticky box compares against the same view. `offscreen` is set
/// while drawing into a layer buffer, where scroll and band offsets are
/// already part of the map.
#[derive(Clone, Copy, Debug)]
struct PaintCtx {
    scroll_y: f32,
    band_y0: f32,
    viewport_width: f32,
    viewport_height: f32,
    fixed_depth: usize,
    offscreen: bool,
    s: f32,
    tx: f32,
    ty: f32,
    sticky_dx: f32,
    sticky_dy: f32,
}

impl PaintCtx {
    /// Vertical offset subtracted from mapped positions.
    fn dy(&self) -> f32 {
        if self.offscreen {
            0.0
        } else if self.fixed_depth > 0 {
            self.band_y0
        } else {
            self.scroll_y + self.band_y0
        }
    }

    /// The viewport in page space for sticky boxes, moved back by the
    /// shifts of enclosing sticky layers. Fixed content does not scroll.
    fn sticky_port(&self) -> anvil_paint::Rect {
        let scroll = if self.fixed_depth > 0 {
            0.0
        } else {
            self.scroll_y
        };
        anvil_paint::Rect {
            x: -self.sticky_dx,
            y: scroll - self.sticky_dy,
            width: self.viewport_width,
            height: self.viewport_height,
        }
    }
}

/// Draw items `start..end`, handing each layer that starts in the span to
/// its effect. `next_layer` walks [`DisplayList::layers`] in order; layers
/// that are malformed (empty, out of range, or not nested) are skipped and
/// their items are drawn plainly.
fn paint_span(
    fb: &mut Framebuffer,
    list: &DisplayList,
    start: usize,
    end: usize,
    next_layer: &mut usize,
    ctx: &mut PaintCtx,
) {
    let end = end.min(list.items.len());
    let mut i = start;
    while i < end {
        while list
            .layers
            .get(*next_layer)
            .is_some_and(|layer| layer.start < i || layer.start >= layer.end)
        {
            *next_layer += 1;
        }
        let starts_here = |layer: &Layer| layer.start == i && layer.end <= end;
        if let Some(layer) = list.layers.get(*next_layer).copied().filter(starts_here) {
            *next_layer += 1;
            paint_layer(fb, list, layer, next_layer, ctx);
            i = layer.end;
            continue;
        }
        paint_item(fb, &list.items[i], ctx);
        i += 1;
    }
}

/// Draw one layer's range with its effect applied.
fn paint_layer(
    fb: &mut Framebuffer,
    list: &DisplayList,
    layer: Layer,
    next_layer: &mut usize,
    ctx: &mut PaintCtx,
) {
    match layer.effect {
        LayerEffect::Sticky(constraint) => {
            let (dx, dy) = constraint.offset(ctx.sticky_port());
            let mut inner = PaintCtx {
                tx: ctx.tx + dx * ctx.s,
                ty: ctx.ty + dy * ctx.s,
                sticky_dx: ctx.sticky_dx + dx,
                sticky_dy: ctx.sticky_dy + dy,
                ..*ctx
            };
            paint_span(fb, list, layer.start, layer.end, next_layer, &mut inner);
            ctx.fixed_depth = inner.fixed_depth;
        }
        LayerEffect::Transform(m) => {
            paint_mapped_layer(fb, list, layer, m, 1.0, next_layer, ctx);
        }
        LayerEffect::Opacity(opacity) => {
            let opacity = if opacity.is_nan() { 1.0 } else { opacity };
            paint_mapped_layer(
                fb,
                list,
                layer,
                Transform2D::IDENTITY,
                opacity,
                next_layer,
                ctx,
            );
        }
    }
}

/// Largest side, in pixels, of the buffer a transformed layer is drawn into.
/// Content past it is not drawn.
const MAX_LAYER_SIDE: i32 = 2048;

/// Draw a layer's range through `m` (a page-space map), at `opacity`.
///
/// A plain move plus an even, positive scale is drawn directly by folding
/// it into the context's map. Anything else is drawn twice into a small
/// buffer, once over black and once over white, which gives back each
/// pixel's color and coverage. Every screen pixel in the layer's bounds and
/// the current clip then samples that buffer through the inverse map. The
/// buffer is aligned to a fixed grid in layer space, so bands and tiles
/// produce the same pixels. A map that cannot be inverted draws nothing.
fn paint_mapped_layer(
    fb: &mut Framebuffer,
    list: &DisplayList,
    layer: Layer,
    m: Transform2D,
    opacity: f32,
    next_layer: &mut usize,
    ctx: &mut PaintCtx,
) {
    if !m.is_finite() || opacity <= 0.0 {
        return;
    }
    let opacity = opacity.min(1.0);
    if opacity >= 1.0 && m.b == 0.0 && m.c == 0.0 && m.a == m.d && m.a > 0.0 {
        let mut inner = PaintCtx {
            s: ctx.s * m.a,
            tx: ctx.tx + ctx.s * m.e,
            ty: ctx.ty + ctx.s * m.f,
            ..*ctx
        };
        paint_span(fb, list, layer.start, layer.end, next_layer, &mut inner);
        ctx.fixed_depth = inner.fixed_depth;
        return;
    }
    let dy = ctx.dy();
    let total = Transform2D {
        a: ctx.s * m.a,
        b: ctx.s * m.b,
        c: ctx.s * m.c,
        d: ctx.s * m.d,
        e: ctx.s * m.e + ctx.tx,
        f: ctx.s * m.f + ctx.ty - dy,
    };
    let Some(inverse) = total.inverse() else {
        return;
    };
    let local_bounds = match layer_local_bounds(list, layer) {
        LayerBounds::Empty => return,
        LayerBounds::Unknown => None,
        LayerBounds::Known(bounds) => Some(bounds),
    };
    let (cx0, cy0, cx1, cy1) = fb.active_clip_bounds();
    let (mut x0, mut y0, mut x1, mut y1) = (cx0, cy0, cx1, cy1);
    if let Some(bounds) = local_bounds {
        let screen = total.map_rect(bounds);
        x0 = x0.max(floor_px(screen.x));
        y0 = y0.max(floor_px(screen.y));
        x1 = x1.min(ceil_px(screen.x + screen.width));
        y1 = y1.min(ceil_px(screen.y + screen.height));
    }
    if x0 >= x1 || y0 >= y1 {
        return;
    }
    let mut region = inverse.map_rect(anvil_paint::Rect {
        x: x0 as f32,
        y: y0 as f32,
        width: (x1 - x0) as f32,
        height: (y1 - y0) as f32,
    });
    if let Some(bounds) = local_bounds {
        let rx0 = region.x.max(bounds.x);
        let ry0 = region.y.max(bounds.y);
        let rx1 = (region.x + region.width).min(bounds.x + bounds.width);
        let ry1 = (region.y + region.height).min(bounds.y + bounds.height);
        region = anvil_paint::Rect {
            x: rx0,
            y: ry0,
            width: rx1 - rx0,
            height: ry1 - ry0,
        };
    }
    if !(region.width > 0.0 && region.height > 0.0) {
        return;
    }
    // Draw at about the screen density so scaled-up content stays sharp.
    let density = (total.a.hypot(total.b))
        .max(total.c.hypot(total.d))
        .clamp(1.0, 4.0);
    let i0 = floor_px(region.x * density) - 2;
    let j0 = floor_px(region.y * density) - 2;
    let w = (ceil_px((region.x + region.width) * density) + 2 - i0).clamp(1, MAX_LAYER_SIDE);
    let h = (ceil_px((region.y + region.height) * density) + 2 - j0).clamp(1, MAX_LAYER_SIDE);
    let mut inner = PaintCtx {
        offscreen: true,
        s: density,
        tx: -(i0 as f32),
        ty: -(j0 as f32),
        ..*ctx
    };
    let black = Color {
        r: 0,
        g: 0,
        b: 0,
        a: 255,
    };
    let mut over_black = Framebuffer::new(w as u32, h as u32, black);
    let mut over_white = Framebuffer::new(w as u32, h as u32, Color::WHITE);
    let mut white_next = *next_layer;
    let mut white_ctx = inner;
    paint_span(
        &mut over_black,
        list,
        layer.start,
        layer.end,
        next_layer,
        &mut inner,
    );
    paint_span(
        &mut over_white,
        list,
        layer.start,
        layer.end,
        &mut white_next,
        &mut white_ctx,
    );
    ctx.fixed_depth = inner.fixed_depth;
    let to_buffer = Transform2D {
        a: density,
        b: 0.0,
        c: 0.0,
        d: density,
        e: -(i0 as f32) - 0.5,
        f: -(j0 as f32) - 0.5,
    }
    .then_inner(&inverse);
    for py in y0..y1 {
        for px in x0..x1 {
            let (u, v) = to_buffer.apply(px as f32 + 0.5, py as f32 + 0.5);
            let dark = sample_bilinear(&over_black, u, v, 0.0);
            let light = sample_bilinear(&over_white, u, v, 255.0);
            let cover =
                255.0 - ((light[0] - dark[0]) + (light[1] - dark[1]) + (light[2] - dark[2])) / 3.0;
            let alpha = (cover.clamp(0.0, 255.0) / 255.0) * opacity;
            if alpha <= 0.0 {
                continue;
            }
            blend_premultiplied(fb, px, py, dark, opacity, alpha);
        }
    }
}

/// `x.floor()` as a pixel index, saturating far off the canvas.
fn floor_px(x: f32) -> i32 {
    x.floor().clamp(-1e9, 1e9) as i32
}

/// `x.ceil()` as a pixel index, saturating far off the canvas.
fn ceil_px(x: f32) -> i32 {
    x.ceil().clamp(-1e9, 1e9) as i32
}

/// RGB at `(u, v)` in pixel-center coordinates, blending the four nearest
/// pixels. Outside the buffer reads as `outside` on every channel.
fn sample_bilinear(fb: &Framebuffer, u: f32, v: f32, outside: f32) -> [f32; 3] {
    if !(u.is_finite() && v.is_finite()) {
        return [outside; 3];
    }
    let fu = u.floor();
    let fv = v.floor();
    let (tu, tv) = (u - fu, v - fv);
    let (iu, iv) = (fu.clamp(-2.0, 1e7) as i64, fv.clamp(-2.0, 1e7) as i64);
    let read = |x: i64, y: i64| -> [f32; 3] {
        if x < 0 || y < 0 || x >= i64::from(fb.width) || y >= i64::from(fb.height) {
            return [outside; 3];
        }
        let offset = ((y as usize) * (fb.width as usize) + (x as usize)) * 4;
        [
            f32::from(fb.pixels[offset]),
            f32::from(fb.pixels[offset + 1]),
            f32::from(fb.pixels[offset + 2]),
        ]
    };
    let (p00, p10, p01, p11) = (
        read(iu, iv),
        read(iu + 1, iv),
        read(iu, iv + 1),
        read(iu + 1, iv + 1),
    );
    let mut out = [0.0f32; 3];
    for (ch, slot) in out.iter_mut().enumerate() {
        let top = p00[ch] + (p10[ch] - p00[ch]) * tu;
        let bottom = p01[ch] + (p11[ch] - p01[ch]) * tu;
        *slot = top + (bottom - top) * tv;
    }
    out
}

/// Put premultiplied `color` (already scaled by its own coverage) over the
/// pixel at `opacity`, where `alpha` is coverage times opacity.
fn blend_premultiplied(
    fb: &mut Framebuffer,
    x: i32,
    y: i32,
    color: [f32; 3],
    opacity: f32,
    alpha: f32,
) {
    if x < 0 || y < 0 || x as u32 >= fb.width || y as u32 >= fb.height {
        return;
    }
    let offset = ((y as usize) * (fb.width as usize) + (x as usize)) * 4;
    for (ch, value) in color.iter().enumerate() {
        let dst = f32::from(fb.pixels[offset + ch]);
        let out = value * opacity + dst * (1.0 - alpha);
        fb.pixels[offset + ch] = out.round().clamp(0.0, 255.0) as u8;
    }
    fb.pixels[offset + 3] = 255;
}

/// What is known about where a layer's items draw, in page space.
enum LayerBounds {
    /// A nested layer can move items past their listed positions.
    Unknown,
    /// Nothing in the range has a size.
    Empty,
    Known(anvil_paint::Rect),
}

/// Page-space bounds of a layer's items, generous for text and shadows.
fn layer_local_bounds(list: &DisplayList, layer: Layer) -> LayerBounds {
    let nested = list
        .layers
        .iter()
        .any(|other| *other != layer && other.start >= layer.start && other.end <= layer.end);
    let Some(items) = list.items.get(layer.start..layer.end).filter(|_| !nested) else {
        return LayerBounds::Unknown;
    };
    let mut bounds: Option<(f32, f32, f32, f32)> = None;
    for item in items {
        let (x, y, w, h) = match item {
            DisplayItem::Text {
                x,
                y,
                text,
                font_size,
                ..
            } => {
                let size = font_size.abs();
                (
                    *x - size,
                    *y - size,
                    (text.chars().count() as f32 + 2.0) * size,
                    size * 3.0,
                )
            }
            DisplayItem::BoxShadow { blur_radius, .. } => {
                let Some((x, y, w, h)) = item_screen_bounds(item, 0.0, false) else {
                    continue;
                };
                // The blur can reach past the listed pad.
                let pad = blur_radius.abs() + 4.0;
                (x - pad, y - pad, w + pad * 2.0, h + pad * 2.0)
            }
            other => match item_screen_bounds(other, 0.0, false) {
                Some(b) => b,
                None => continue,
            },
        };
        if !(x.is_finite() && y.is_finite() && w.is_finite() && h.is_finite()) {
            return LayerBounds::Unknown;
        }
        let (x1, y1) = (x + w.max(0.0), y + h.max(0.0));
        bounds = Some(match bounds {
            None => (x, y, x1, y1),
            Some((bx0, by0, bx1, by1)) => (bx0.min(x), by0.min(y), bx1.max(x1), by1.max(y1)),
        });
    }
    let Some((x0, y0, x1, y1)) = bounds else {
        return LayerBounds::Empty;
    };
    LayerBounds::Known(anvil_paint::Rect {
        x: x0 - 1.0,
        y: y0 - 1.0,
        width: x1 - x0 + 2.0,
        height: y1 - y0 + 2.0,
    })
}

/// Draw one item through the context's map.
fn paint_item(fb: &mut Framebuffer, item: &DisplayItem, ctx: &mut PaintCtx) {
    let s = ctx.s;
    let dy = ctx.dy();
    let px = |x: f32| x * s + ctx.tx;
    let py = |y: f32| y * s + ctx.ty - dy;
    match item {
        DisplayItem::PushClip {
            x,
            y,
            width,
            height,
        } => {
            fb.push_clip(px(*x), py(*y), *width * s, *height * s);
        }
        DisplayItem::PopClip => {
            fb.pop_clip();
        }
        DisplayItem::PushFixed => {
            ctx.fixed_depth += 1;
        }
        DisplayItem::PopFixed => {
            ctx.fixed_depth = ctx.fixed_depth.saturating_sub(1);
        }
        DisplayItem::BoxShadow {
            x,
            y,
            width,
            height,
            offset_x,
            offset_y,
            blur_radius,
            spread_radius,
            radius,
            color,
        } => {
            fb.fill_box_shadow(
                px(*x),
                py(*y),
                *width * s,
                *height * s,
                *offset_x * s,
                *offset_y * s,
                *blur_radius * s,
                *spread_radius * s,
                *radius * s,
                *color,
            );
        }
        DisplayItem::LinearGradientRect {
            x,
            y,
            width,
            height,
            radius,
            border_width,
            angle_deg,
            start_color,
            end_color,
            border_color,
        } => {
            fb.fill_linear_gradient_rect(
                px(*x),
                py(*y),
                *width * s,
                *height * s,
                *radius * s,
                *border_width * s,
                *angle_deg,
                *start_color,
                *end_color,
                *border_color,
            );
        }
        DisplayItem::Rect {
            x,
            y,
            width,
            height,
            color,
        } => {
            fb.fill_rect(px(*x), py(*y), *width * s, *height * s, *color);
        }
        DisplayItem::RoundedRect {
            x,
            y,
            width,
            height,
            radius,
            border_width,
            fill_color,
            border_color,
        } => {
            fb.fill_rounded_rect(
                px(*x),
                py(*y),
                *width * s,
                *height * s,
                *radius * s,
                *border_width * s,
                *fill_color,
                *border_color,
            );
        }
        DisplayItem::Image {
            x,
            y,
            width,
            height,
            border_radius,
            image,
        } => {
            fb.draw_image(
                px(*x),
                py(*y),
                *width * s,
                *height * s,
                *border_radius * s,
                image,
            );
        }
        DisplayItem::Text {
            x,
            y,
            text,
            color,
            font_size,
        } => {
            fb.draw_text(px(*x), py(*y), text, *font_size * s, *color);
        }
    }
}

/// Paint `list` into a new framebuffer in parallel across horizontal 64px tile strips.
///
/// Produces bit-exact output matching single-threaded [`rasterize`].
pub fn rasterize_parallel(
    list: &DisplayList,
    width: u32,
    height: u32,
    scroll_y: f32,
) -> Framebuffer {
    if width == 0 || height <= TILE_SIZE {
        return rasterize(list, width, height, scroll_y);
    }
    let canvas_bg = resolve_canvas_bg(list, width);
    let max_workers = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(2)
        .clamp(2, 8) as u32;
    let total_tile_rows = height.div_ceil(TILE_SIZE);
    let workers = max_workers.min(total_tile_rows).max(1);
    if workers <= 1 {
        return rasterize(list, width, height, scroll_y);
    }
    let tile_rows_per_worker = total_tile_rows.div_ceil(workers);
    let strip_height = tile_rows_per_worker * TILE_SIZE;

    let mut strips = Vec::new();
    let mut y0 = 0u32;
    while y0 < height {
        let y1 = (y0 + strip_height).min(height);
        strips.push((y0, y1 - y0));
        y0 = y1;
    }

    let band_buffers: Vec<Vec<u8>> = std::thread::scope(|s| {
        let mut handles = Vec::with_capacity(strips.len());
        for &(band_y0, band_h) in &strips {
            handles.push(s.spawn(move || {
                let mut band_fb = Framebuffer::new(width, band_h, canvas_bg);
                paint_list_with_band_offset(
                    &mut band_fb,
                    list,
                    scroll_y,
                    band_y0 as f32,
                    height as f32,
                );
                band_fb.pixels
            }));
        }
        handles
            .into_iter()
            .map(|h| h.join().unwrap_or_default())
            .collect()
    });

    let total_bytes = (width as usize).saturating_mul(height as usize) * 4;
    let mut pixels = Vec::with_capacity(total_bytes);
    for mut chunk in band_buffers {
        pixels.append(&mut chunk);
    }
    Framebuffer {
        width,
        height,
        pixels,
        clips: Vec::new(),
    }
}

/// 64x64 dirty-tile damage tracker for incremental display-list rasterization.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DirtyTileGrid {
    /// Number of 64px tile columns covering the framebuffer width.
    pub cols: u32,
    /// Number of 64px tile rows covering the framebuffer height.
    pub rows: u32,
    /// Per-tile dirty flag (`rows * cols` entries).
    pub dirty: Vec<bool>,
}

impl DirtyTileGrid {
    /// Create a clean 64x64 tile grid for `width` by `height` pixels.
    pub fn new(width: u32, height: u32) -> Self {
        let cols = width.div_ceil(TILE_SIZE).max(1);
        let rows = height.div_ceil(TILE_SIZE).max(1);
        Self {
            cols,
            rows,
            dirty: vec![false; (cols * rows) as usize],
        }
    }

    /// Mark every tile overlapping `[x, x + w)` by `[y, y + h)` dirty.
    pub fn mark_rect(&mut self, x: f32, y: f32, w: f32, h: f32) {
        if w <= 0.0 || h <= 0.0 {
            return;
        }
        let tx0 = ((x.floor().max(0.0) as u32) / TILE_SIZE).min(self.cols - 1);
        let ty0 = ((y.floor().max(0.0) as u32) / TILE_SIZE).min(self.rows - 1);
        let tx1 = (((x + w).ceil().max(0.0) as u32).div_ceil(TILE_SIZE)).min(self.cols);
        let ty1 = (((y + h).ceil().max(0.0) as u32).div_ceil(TILE_SIZE)).min(self.rows);
        for ty in ty0..ty1 {
            for tx in tx0..tx1 {
                self.dirty[(ty * self.cols + tx) as usize] = true;
            }
        }
    }

    /// Total number of dirty tiles in the grid.
    pub fn dirty_count(&self) -> usize {
        self.dirty.iter().filter(|&&d| d).count()
    }
}

fn item_screen_bounds(
    item: &DisplayItem,
    scroll_y: f32,
    fixed: bool,
) -> Option<(f32, f32, f32, f32)> {
    let dy = if fixed { 0.0 } else { scroll_y };
    match item {
        DisplayItem::Rect {
            x,
            y,
            width,
            height,
            ..
        }
        | DisplayItem::RoundedRect {
            x,
            y,
            width,
            height,
            ..
        }
        | DisplayItem::LinearGradientRect {
            x,
            y,
            width,
            height,
            ..
        }
        | DisplayItem::Image {
            x,
            y,
            width,
            height,
            ..
        } => Some((*x, *y - dy, *width, *height)),
        DisplayItem::BoxShadow {
            x,
            y,
            width,
            height,
            offset_x,
            offset_y,
            blur_radius,
            spread_radius,
            ..
        } => {
            let pad = blur_radius.max(0.0) + spread_radius.abs();
            Some((
                *x + *offset_x - pad,
                *y - dy + *offset_y - pad,
                *width + pad * 2.0,
                *height + pad * 2.0,
            ))
        }
        DisplayItem::Text {
            x,
            y,
            text,
            font_size,
            ..
        } => Some((
            *x,
            *y - dy,
            (text.chars().count() as f32) * (*font_size * 0.65).max(8.0),
            (*font_size * 1.4).max(12.0),
        )),
        _ => None,
    }
}

/// Incrementally re-rasterize only the 64x64 tiles damaged between `prev_list` and `next_list`.
///
/// Returns the number of dirty 64x64 tiles repainted.
pub fn rasterize_incremental(
    prev_list: &DisplayList,
    next_list: &DisplayList,
    prev_scroll_y: f32,
    next_scroll_y: f32,
    fb: &mut Framebuffer,
) -> usize {
    let mut grid = DirtyTileGrid::new(fb.width, fb.height);
    let viewport_h = fb.height as f32;
    // A layer can move or reshape everything in its range, so any change to
    // a list with layers repaints the whole frame.
    let layered = !prev_list.layers.is_empty() || !next_list.layers.is_empty();
    if (prev_scroll_y - next_scroll_y).abs() > 0.01 || (layered && prev_list != next_list) {
        grid.dirty.fill(true);
    } else {
        let max_len = prev_list.items.len().max(next_list.items.len());
        let mut fixed_prev = 0usize;
        let mut fixed_next = 0usize;
        for idx in 0..max_len {
            let p_item = prev_list.items.get(idx);
            let n_item = next_list.items.get(idx);
            if let Some(DisplayItem::PushFixed) = p_item {
                fixed_prev += 1;
            } else if let Some(DisplayItem::PopFixed) = p_item {
                fixed_prev = fixed_prev.saturating_sub(1);
            }
            if let Some(DisplayItem::PushFixed) = n_item {
                fixed_next += 1;
            } else if let Some(DisplayItem::PopFixed) = n_item {
                fixed_next = fixed_next.saturating_sub(1);
            }
            if p_item != n_item {
                if let Some(item) = p_item {
                    if let Some((x, y, w, h)) =
                        item_screen_bounds(item, prev_scroll_y, fixed_prev > 0)
                    {
                        grid.mark_rect(x, y, w, h);
                    }
                }
                if let Some(item) = n_item {
                    if let Some((x, y, w, h)) =
                        item_screen_bounds(item, next_scroll_y, fixed_next > 0)
                    {
                        grid.mark_rect(x, y, w, h);
                    }
                }
            }
        }
    }

    let dirty_total = grid.dirty_count();
    if dirty_total == 0 {
        return 0;
    }
    let canvas_bg = resolve_canvas_bg(next_list, fb.width);
    for ty in 0..grid.rows {
        for tx in 0..grid.cols {
            if !grid.dirty[(ty * grid.cols + tx) as usize] {
                continue;
            }
            let x0 = (tx * TILE_SIZE) as f32;
            let y0 = (ty * TILE_SIZE) as f32;
            let tw = TILE_SIZE as f32;
            let th = TILE_SIZE as f32;
            fb.clips.clear();
            fb.push_clip(x0, y0, tw, th);
            fb.fill_rect(x0, y0, tw, th, canvas_bg);
            paint_list_with_band_offset(fb, next_list, next_scroll_y, 0.0, viewport_h);
            fb.clips.clear();
        }
    }
    dirty_total
}

/// Paint `list` into a new `width` by `height` framebuffer, scrolled down by `scroll_y`.
///
/// The clear color is the first item when it is a large rect at the origin,
/// otherwise white. Fixed items ignore the scroll. Layers are applied as
/// described on [`anvil_paint::Layer`].
pub fn rasterize(list: &DisplayList, width: u32, height: u32, scroll_y: f32) -> Framebuffer {
    let canvas_bg = resolve_canvas_bg(list, width);
    let mut fb = Framebuffer::new(width, height, canvas_bg);
    paint_list_with_band_offset(&mut fb, list, scroll_y, 0.0, height as f32);
    fb
}

#[cfg(target_os = "linux")]
fn linux_system_ttf_font() -> Option<&'static TtfFont> {
    use std::sync::OnceLock;
    static SYSTEM_FONT: OnceLock<Option<TtfFont>> = OnceLock::new();
    SYSTEM_FONT
        .get_or_init(|| {
            const CANDIDATES: &[&str] = &[
                "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
                "/usr/share/fonts/TTF/DejaVuSans.ttf",
                "/usr/share/fonts/liberation/LiberationSans-Regular.ttf",
                "/usr/share/fonts/truetype/liberation/LiberationSans-Regular.ttf",
            ];
            for path in CANDIDATES {
                if let Ok(bytes) = std::fs::read(path) {
                    if let Ok(font) = TtfFont::parse(&bytes) {
                        return Some(font);
                    }
                }
            }
            None
        })
        .as_ref()
}

fn glyph(ch: char) -> [u8; 7] {
    match ch {
        ' ' => [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
        '!' => [0x04, 0x04, 0x04, 0x04, 0x04, 0x00, 0x04],
        '"' => [0x0a, 0x0a, 0x0a, 0x00, 0x00, 0x00, 0x00],
        '#' => [0x0a, 0x1f, 0x0a, 0x0a, 0x1f, 0x0a, 0x00],
        '$' => [0x04, 0x0f, 0x14, 0x0e, 0x05, 0x1e, 0x04],
        '%' => [0x18, 0x19, 0x02, 0x04, 0x08, 0x13, 0x03],
        '&' => [0x0c, 0x12, 0x14, 0x08, 0x15, 0x12, 0x0d],
        '\'' => [0x04, 0x04, 0x08, 0x00, 0x00, 0x00, 0x00],
        '(' => [0x02, 0x04, 0x08, 0x08, 0x08, 0x04, 0x02],
        ')' => [0x08, 0x04, 0x02, 0x02, 0x02, 0x04, 0x08],
        '*' => [0x00, 0x04, 0x15, 0x0e, 0x15, 0x04, 0x00],
        '+' => [0x00, 0x04, 0x04, 0x1f, 0x04, 0x04, 0x00],
        ',' => [0x00, 0x00, 0x00, 0x00, 0x04, 0x04, 0x08],
        '-' => [0x00, 0x00, 0x00, 0x1f, 0x00, 0x00, 0x00],
        '.' => [0x00, 0x00, 0x00, 0x00, 0x00, 0x04, 0x04],
        '/' => [0x01, 0x02, 0x02, 0x04, 0x08, 0x08, 0x10],
        '0' => [0x0e, 0x11, 0x13, 0x15, 0x19, 0x11, 0x0e],
        '1' => [0x04, 0x0c, 0x04, 0x04, 0x04, 0x04, 0x0e],
        '2' => [0x0e, 0x11, 0x01, 0x06, 0x08, 0x10, 0x1f],
        '3' => [0x1f, 0x02, 0x04, 0x06, 0x01, 0x11, 0x0e],
        '4' => [0x02, 0x06, 0x0a, 0x12, 0x1f, 0x02, 0x02],
        '5' => [0x1f, 0x10, 0x1e, 0x01, 0x01, 0x11, 0x0e],
        '6' => [0x06, 0x08, 0x10, 0x1e, 0x11, 0x11, 0x0e],
        '7' => [0x1f, 0x01, 0x02, 0x04, 0x08, 0x08, 0x08],
        '8' => [0x0e, 0x11, 0x11, 0x0e, 0x11, 0x11, 0x0e],
        '9' => [0x0e, 0x11, 0x11, 0x0f, 0x01, 0x02, 0x0c],
        ':' => [0x00, 0x04, 0x04, 0x00, 0x04, 0x04, 0x00],
        ';' => [0x00, 0x04, 0x04, 0x00, 0x04, 0x04, 0x08],
        '<' => [0x02, 0x04, 0x08, 0x10, 0x08, 0x04, 0x02],
        '=' => [0x00, 0x00, 0x1f, 0x00, 0x1f, 0x00, 0x00],
        '>' => [0x08, 0x04, 0x02, 0x01, 0x02, 0x04, 0x08],
        '?' => [0x0e, 0x11, 0x01, 0x02, 0x04, 0x00, 0x04],
        '@' => [0x0e, 0x11, 0x17, 0x15, 0x17, 0x10, 0x0e],
        'A' => [0x0e, 0x11, 0x11, 0x1f, 0x11, 0x11, 0x11],
        'B' => [0x1e, 0x11, 0x11, 0x1e, 0x11, 0x11, 0x1e],
        'C' => [0x0e, 0x11, 0x10, 0x10, 0x10, 0x11, 0x0e],
        'D' => [0x1c, 0x12, 0x11, 0x11, 0x11, 0x12, 0x1c],
        'E' => [0x1f, 0x10, 0x10, 0x1e, 0x10, 0x10, 0x1f],
        'F' => [0x1f, 0x10, 0x10, 0x1e, 0x10, 0x10, 0x10],
        'G' => [0x0e, 0x11, 0x10, 0x17, 0x11, 0x11, 0x0f],
        'H' => [0x11, 0x11, 0x11, 0x1f, 0x11, 0x11, 0x11],
        'I' => [0x0e, 0x04, 0x04, 0x04, 0x04, 0x04, 0x0e],
        'J' => [0x07, 0x02, 0x02, 0x02, 0x02, 0x12, 0x0c],
        'K' => [0x11, 0x12, 0x14, 0x18, 0x14, 0x12, 0x11],
        'L' => [0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x1f],
        'M' => [0x11, 0x1b, 0x15, 0x15, 0x11, 0x11, 0x11],
        'N' => [0x11, 0x19, 0x15, 0x13, 0x11, 0x11, 0x11],
        'O' => [0x0e, 0x11, 0x11, 0x11, 0x11, 0x11, 0x0e],
        'P' => [0x1e, 0x11, 0x11, 0x1e, 0x10, 0x10, 0x10],
        'Q' => [0x0e, 0x11, 0x11, 0x11, 0x15, 0x12, 0x0d],
        'R' => [0x1e, 0x11, 0x11, 0x1e, 0x14, 0x12, 0x11],
        'S' => [0x0f, 0x10, 0x10, 0x0e, 0x01, 0x01, 0x1e],
        'T' => [0x1f, 0x04, 0x04, 0x04, 0x04, 0x04, 0x04],
        'U' => [0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x0e],
        'V' => [0x11, 0x11, 0x11, 0x11, 0x11, 0x0a, 0x04],
        'W' => [0x11, 0x11, 0x11, 0x15, 0x15, 0x15, 0x0a],
        'X' => [0x11, 0x11, 0x0a, 0x04, 0x0a, 0x11, 0x11],
        'Y' => [0x11, 0x11, 0x0a, 0x04, 0x04, 0x04, 0x04],
        'Z' => [0x1f, 0x01, 0x02, 0x04, 0x08, 0x10, 0x1f],
        '[' => [0x0e, 0x08, 0x08, 0x08, 0x08, 0x08, 0x0e],
        '\\' => [0x10, 0x08, 0x08, 0x04, 0x02, 0x02, 0x01],
        ']' => [0x0e, 0x02, 0x02, 0x02, 0x02, 0x02, 0x0e],
        '^' => [0x04, 0x0a, 0x11, 0x00, 0x00, 0x00, 0x00],
        '_' => [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x1f],
        '`' => [0x08, 0x04, 0x02, 0x00, 0x00, 0x00, 0x00],
        'a' => [0x00, 0x00, 0x0e, 0x01, 0x0f, 0x11, 0x0f],
        'b' => [0x10, 0x10, 0x1e, 0x11, 0x11, 0x11, 0x1e],
        'c' => [0x00, 0x00, 0x0e, 0x10, 0x10, 0x11, 0x0e],
        'd' => [0x01, 0x01, 0x0f, 0x11, 0x11, 0x11, 0x0f],
        'e' => [0x00, 0x00, 0x0e, 0x11, 0x1f, 0x10, 0x0e],
        'f' => [0x06, 0x08, 0x1e, 0x08, 0x08, 0x08, 0x08],
        'g' => [0x00, 0x0f, 0x11, 0x11, 0x0f, 0x01, 0x0e],
        'h' => [0x10, 0x10, 0x1e, 0x11, 0x11, 0x11, 0x11],
        'i' => [0x04, 0x00, 0x0c, 0x04, 0x04, 0x04, 0x0e],
        'j' => [0x02, 0x00, 0x06, 0x02, 0x02, 0x12, 0x0c],
        'k' => [0x10, 0x10, 0x12, 0x14, 0x18, 0x14, 0x12],
        'l' => [0x0c, 0x04, 0x04, 0x04, 0x04, 0x04, 0x0e],
        'm' => [0x00, 0x00, 0x1a, 0x15, 0x15, 0x11, 0x11],
        'n' => [0x00, 0x00, 0x1e, 0x11, 0x11, 0x11, 0x11],
        'o' => [0x00, 0x00, 0x0e, 0x11, 0x11, 0x11, 0x0e],
        'p' => [0x00, 0x1e, 0x11, 0x11, 0x1e, 0x10, 0x10],
        'q' => [0x00, 0x0f, 0x11, 0x11, 0x0f, 0x01, 0x01],
        'r' => [0x00, 0x00, 0x16, 0x19, 0x10, 0x10, 0x10],
        's' => [0x00, 0x00, 0x0f, 0x10, 0x0e, 0x01, 0x1e],
        't' => [0x08, 0x08, 0x1e, 0x08, 0x08, 0x09, 0x06],
        'u' => [0x00, 0x00, 0x11, 0x11, 0x11, 0x13, 0x0d],
        'v' => [0x00, 0x00, 0x11, 0x11, 0x11, 0x0a, 0x04],
        'w' => [0x00, 0x00, 0x11, 0x11, 0x15, 0x15, 0x0a],
        'x' => [0x00, 0x00, 0x11, 0x0a, 0x04, 0x0a, 0x11],
        'y' => [0x00, 0x11, 0x11, 0x11, 0x0f, 0x01, 0x0e],
        'z' => [0x00, 0x00, 0x1f, 0x02, 0x04, 0x08, 0x1f],
        '{' => [0x02, 0x04, 0x04, 0x08, 0x04, 0x04, 0x02],
        '|' => [0x04, 0x04, 0x04, 0x04, 0x04, 0x04, 0x04],
        '}' => [0x08, 0x04, 0x04, 0x02, 0x04, 0x04, 0x08],
        '~' => [0x00, 0x00, 0x08, 0x15, 0x02, 0x00, 0x00],
        _ => [0x1f, 0x11, 0x11, 0x11, 0x11, 0x11, 0x1f],
    }
}

/// Shade one solid quad through the anvil-gpu tile device.
/// The pixels match a full-frame draw of the same quad.
pub fn rasterize_quad_on_device(
    width: u32,
    height: u32,
    color: Color,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
) -> Vec<u8> {
    let quad = anvil_gpu::GpuQuad::solid(
        anvil_gpu::GpuRect::new(x, y, w, h),
        anvil_gpu::GpuColor::rgba(color.r, color.g, color.b, color.a),
    );
    let clear = anvil_gpu::GpuColor::rgba(0, 0, 0, 255);
    anvil_gpu::GpuDevice::open().render_quads(width, height, clear, &[quad], None)
}

/// Fill, scroll, clip, fixed layers, shadows, gradients, blits, and NV12 conversion tests.
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fills_a_rectangle_and_leaves_outside_pixels_alone() {
        let red = Color {
            r: 220,
            g: 30,
            b: 40,
            a: 255,
        };
        let list = DisplayList {
            items: vec![DisplayItem::Rect {
                x: 2.0,
                y: 3.0,
                width: 4.0,
                height: 5.0,
                color: red,
            }],
            layers: Vec::new(),
        };
        let fb = rasterize(&list, 16, 16, 0.0);
        assert_eq!(fb.pixel(2, 3), Some(red));
        assert_eq!(fb.pixel(5, 7), Some(red));
        assert_eq!(fb.pixel(1, 3), Some(PAPER));
        assert_eq!(fb.pixel(6, 7), Some(PAPER));
    }

    #[test]
    fn device_quad_matches_a_filled_rectangle() {
        let color = Color {
            r: 20,
            g: 120,
            b: 220,
            a: 255,
        };
        let pixels = rasterize_quad_on_device(8, 8, color, 1.0, 1.0, 4.0, 3.0);
        let idx = ((2 * 8) + 2) * 4;
        assert_eq!(&pixels[idx..idx + 4], &[20, 120, 220, 255]);
        assert_eq!(&pixels[0..4], &[0, 0, 0, 255]);
    }

    #[test]
    fn draws_glyph_pixels_and_respects_vertical_scroll() {
        let ink = Color {
            r: 10,
            g: 20,
            b: 30,
            a: 255,
        };
        let list = DisplayList {
            items: vec![DisplayItem::Text {
                x: 4.0,
                y: 20.0,
                text: "Anvil".to_string(),
                color: ink,
                font_size: 16.0,
            }],
            layers: Vec::new(),
        };
        let scrolled = rasterize(&list, 64, 32, 16.0);
        let lit = (0..32)
            .flat_map(|y| (0..64).map(move |x| (x, y)))
            .any(|(x, y)| scrolled.pixel(x, y) == Some(ink));
        assert!(lit);

        let bgra = scrolled.to_bgra_bottom_up();
        assert_eq!(bgra.len(), 64 * 32 * 4);
    }

    #[test]
    fn clips_overflow_and_keeps_fixed_items_pinned_on_scroll() {
        let blue = Color {
            r: 20,
            g: 100,
            b: 220,
            a: 255,
        };
        let gold = Color {
            r: 232,
            g: 195,
            b: 122,
            a: 255,
        };
        let list = DisplayList {
            items: vec![
                DisplayItem::PushClip {
                    x: 0.0,
                    y: 0.0,
                    width: 4.0,
                    height: 4.0,
                },
                DisplayItem::Rect {
                    x: 0.0,
                    y: 0.0,
                    width: 10.0,
                    height: 10.0,
                    color: blue,
                },
                DisplayItem::PopClip,
                DisplayItem::PushFixed,
                DisplayItem::Rect {
                    x: 8.0,
                    y: 2.0,
                    width: 4.0,
                    height: 4.0,
                    color: gold,
                },
                DisplayItem::PopFixed,
            ],
            layers: Vec::new(),
        };
        let fb = rasterize(&list, 16, 16, 50.0);
        assert_eq!(fb.pixel(9, 3), Some(gold));
        let fb_no_scroll = rasterize(&list, 16, 16, 0.0);
        assert_eq!(fb_no_scroll.pixel(2, 2), Some(blue));
        assert_eq!(fb_no_scroll.pixel(6, 6), Some(PAPER));
    }

    #[test]
    fn renders_box_shadows_and_linear_gradients() {
        let list = DisplayList {
            items: vec![
                DisplayItem::BoxShadow {
                    x: 4.0,
                    y: 4.0,
                    width: 8.0,
                    height: 8.0,
                    offset_x: 0.0,
                    offset_y: 2.0,
                    blur_radius: 4.0,
                    spread_radius: 0.0,
                    radius: 2.0,
                    color: Color {
                        r: 0,
                        g: 0,
                        b: 0,
                        a: 180,
                    },
                },
                DisplayItem::LinearGradientRect {
                    x: 0.0,
                    y: 0.0,
                    width: 16.0,
                    height: 8.0,
                    radius: 0.0,
                    border_width: 0.0,
                    angle_deg: 90.0,
                    start_color: Color {
                        r: 255,
                        g: 0,
                        b: 0,
                        a: 255,
                    },
                    end_color: Color {
                        r: 0,
                        g: 0,
                        b: 255,
                        a: 255,
                    },
                    border_color: Color::BLACK,
                },
            ],
            layers: Vec::new(),
        };
        let fb = rasterize(&list, 20, 20, 0.0);
        let left = fb.pixel(1, 4).unwrap();
        let right = fb.pixel(14, 4).unwrap();
        assert!(left.r > right.r);
        assert!(right.b > left.b);
        let shadow_px = fb.pixel(8, 13).unwrap();
        assert!(shadow_px.r < 255);
    }

    #[test]
    fn converts_nv12_bt709_and_blits_clipped_images() {
        let y_plane = [200u8; 16];
        let uv_plane = [128u8; 8];
        let rgba = convert_nv12_to_rgba_bt709(&y_plane, &uv_plane, 4, 4);
        assert_eq!(rgba.len(), 4 * 4 * 4);
        assert!(rgba[0] > 180 && rgba[3] == 255);

        let img = Image::from_rgba(4, 4, rgba).unwrap();
        let mut fb = Framebuffer::new(8, 8, Color::BLACK);
        fb.push_clip(2.0, 2.0, 2.0, 2.0);
        fb.draw_image(0.0, 0.0, 4.0, 4.0, 0.0, &img);
        fb.pop_clip();
        assert_eq!(fb.pixel(1, 1), Some(Color::BLACK));
        assert!(fb.pixel(2, 2).unwrap().r > 180);
    }

    #[test]
    fn length_and_flex_rectangles_match_on_the_cpu_and_cpp_fills() {
        use anvil_dom::parse_html;
        use anvil_layout::layout_document;
        use anvil_paint::paint;
        use anvil_style::{compute_styles, parse_css};

        let html = "<style>
                html, body, div { margin: 0; padding: 0; }
                html { font-size: 20px; }
                #host { width: 200px; height: 100px; background: #111111; }
                #pct { width: 50%; height: 40%; background: #ff0000; }
                #em { font-size: 1.5rem; width: 2em; height: 10px; background: #00ff00; }
                #row { display: flex; flex-wrap: wrap; width: 100px; background: #0000ff; }
                #row div { width: 40px; height: 10px; }
                #a { background: #aa0000; }
                #b { background: #00aa00; }
                #c { background: #0000aa; }
                #grow { display: flex; width: 100px; height: 10px; }
                #g1 { width: 20px; height: 10px; flex-grow: 1; background: #abcdef; }
                #g2 { width: 20px; height: 10px; flex-grow: 2; background: #fedcba; }
             </style>
             <div id=\"host\"><div id=\"pct\"></div><div id=\"em\"></div></div>
             <div id=\"row\"><div id=\"a\"></div><div id=\"b\"></div><div id=\"c\"></div></div>
             <div id=\"grow\"><div id=\"g1\"></div><div id=\"g2\"></div></div>";
        let doc = parse_html(html);
        let styles = compute_styles(&doc, &parse_css(&doc.embedded_css()));
        let tree = layout_document(&doc, &styles, 200.0);
        let list = paint(&tree);
        let painted = rasterize(&list, 200, 160, 0.0);
        let mut cpu = Framebuffer::new(200, 160, PAPER);
        for item in &list.items {
            if let DisplayItem::Rect {
                x,
                y,
                width,
                height,
                color,
            } = item
            {
                cpu.fill_rect_cpu(*x, *y, *width, *height, *color);
            }
        }

        fn find<'a>(
            doc: &anvil_dom::Document,
            tree: &'a anvil_layout::LayoutBox,
            id: &str,
        ) -> &'a anvil_layout::LayoutBox {
            if doc.get(tree.node).data().attr("id") == Some(id) {
                return tree;
            }
            for child in &tree.children {
                if doc.get(child.node).data().attr("id") == Some(id) {
                    return child;
                }
                let nested = find(doc, child, id);
                if doc.get(nested.node).data().attr("id") == Some(id) {
                    return nested;
                }
            }
            tree
        }

        let expect = [
            ("pct", Color::parse("#ff0000").unwrap()),
            ("em", Color::parse("#00ff00").unwrap()),
            ("a", Color::parse("#aa0000").unwrap()),
            ("b", Color::parse("#00aa00").unwrap()),
            ("c", Color::parse("#0000aa").unwrap()),
            ("g1", Color::parse("#abcdef").unwrap()),
            ("g2", Color::parse("#fedcba").unwrap()),
        ];
        for (id, color) in expect {
            let rect = find(&doc, &tree, id).rect;
            let x = (rect.x + rect.width * 0.5).floor() as u32;
            let y = (rect.y + rect.height * 0.5).floor() as u32;
            assert_eq!(painted.pixel(x, y), Some(color), "{id}");
            assert_eq!(cpu.pixel(x, y), painted.pixel(x, y), "{id}");
        }
        let parallel = rasterize_parallel(&list, 200, 160, 0.0);
        assert_eq!(parallel.pixels(), painted.pixels());
        assert!(raster_simd_width_bytes() >= 4);
    }

    #[test]
    fn incremental_dirty_tiles_and_separable_box_blur_work() {
        let red = Color {
            r: 220,
            g: 30,
            b: 40,
            a: 255,
        };
        let green = Color {
            r: 30,
            g: 210,
            b: 80,
            a: 255,
        };
        let prev = DisplayList {
            items: vec![DisplayItem::Rect {
                x: 8.0,
                y: 8.0,
                width: 16.0,
                height: 16.0,
                color: red,
            }],
            layers: Vec::new(),
        };
        let next = DisplayList {
            items: vec![
                DisplayItem::Rect {
                    x: 8.0,
                    y: 8.0,
                    width: 16.0,
                    height: 16.0,
                    color: red,
                },
                DisplayItem::Rect {
                    x: 80.0,
                    y: 80.0,
                    width: 16.0,
                    height: 16.0,
                    color: green,
                },
            ],
            layers: Vec::new(),
        };
        {
            let mut fb = rasterize(&prev, 128, 128, 0.0);
            let dirty = rasterize_incremental(&prev, &next, 0.0, 0.0, &mut fb);
            assert_eq!(dirty, 1);
            let full = rasterize(&next, 128, 128, 0.0);
            assert_eq!(fb.pixels(), full.pixels());

            fb.blur_rect(76.0, 76.0, 24.0, 24.0, 2);
            let edge = fb.pixel(80, 80).unwrap();
            assert!(edge.g > 50 && edge.g < 255);
        }
    }

    #[test]
    fn sticky_layers_follow_scroll_in_every_raster_path() {
        use anvil_dom::parse_html;
        use anvil_layout::layout_document;
        use anvil_paint::paint;
        use anvil_style::{compute_styles, parse_css};

        let doc = parse_html(
            "<style>
                html, body, div { margin: 0; padding: 0; }
                #wrap { height: 400px; }
                #bar { position: sticky; top: 0; height: 20px; background: #ff0000; }
             </style>
             <div id=\"wrap\"><div style=\"height: 50px\"></div><div id=\"bar\"></div></div>
             <div style=\"height: 400px\"></div>",
        );
        let styles = compute_styles(&doc, &parse_css(&doc.embedded_css()));
        let list = paint(&layout_document(&doc, &styles, 40.0));
        assert_eq!(list.layers.len(), 1);
        let red = Color {
            r: 255,
            g: 0,
            b: 0,
            a: 255,
        };
        let at_top = rasterize(&list, 40, 150, 0.0);
        assert_eq!(at_top.pixel(5, 55), Some(red));
        assert_ne!(at_top.pixel(5, 5), Some(red));
        // Scrolled 100px: the bar sticks to the top edge.
        let scrolled = rasterize(&list, 40, 150, 100.0);
        assert_eq!(scrolled.pixel(5, 0), Some(red));
        assert_eq!(scrolled.pixel(5, 19), Some(red));
        assert_ne!(scrolled.pixel(5, 20), Some(red));
        // Past the end of #wrap it scrolls away with it: 380..400 is 0..20 at 380.
        let past = rasterize(&list, 40, 150, 390.0);
        assert_ne!(past.pixel(5, 10), Some(red));
        assert_eq!(past.pixel(5, 5), Some(red));
        assert_eq!(rasterize_parallel(&list, 40, 150, 100.0), scrolled);
        let mut fb = at_top.clone();
        rasterize_incremental(&list, &list, 0.0, 100.0, &mut fb);
        assert_eq!(fb, scrolled);
        // Zoom keeps the sticky edge: at 2x the bar is 40px tall at the top.
        let zoomed = rasterize(&list.scaled(2.0), 80, 150, 200.0);
        assert_eq!(zoomed.pixel(5, 39), Some(red));
        assert_ne!(zoomed.pixel(5, 40), Some(red));
    }

    /// Lay out and paint `html` (with its own `<style>`) `width` pixels wide.
    fn page_list(html: &str, width: f32) -> DisplayList {
        use anvil_dom::parse_html;
        use anvil_layout::layout_document;
        use anvil_paint::paint;
        use anvil_style::{compute_styles, parse_css};

        let doc = parse_html(html);
        let styles = compute_styles(&doc, &parse_css(&doc.embedded_css()));
        paint(&layout_document(&doc, &styles, width))
    }

    const RED: Color = Color {
        r: 255,
        g: 0,
        b: 0,
        a: 255,
    };

    #[test]
    fn moves_and_scales_transformed_boxes_directly() {
        // 10x10 at (10, 10); scale(2) about its center covers 5..25.
        let list = page_list(
            "<style>html, body, div { margin: 0; padding: 0; }
             #t { margin: 10px; width: 10px; height: 10px; background: #ff0000;
                  transform: translate(20px, 0) scale(2); }</style><div id=\"t\"></div>",
            60.0,
        );
        assert_eq!(list.layers.len(), 1);
        let fb = rasterize(&list, 60, 40, 0.0);
        assert_eq!(fb.pixel(25, 5), Some(RED));
        assert_eq!(fb.pixel(44, 24), Some(RED));
        assert_eq!(fb.pixel(24, 5), Some(Color::WHITE));
        assert_eq!(fb.pixel(45, 24), Some(Color::WHITE));
        assert_eq!(fb.pixel(25, 25), Some(Color::WHITE));
        assert_eq!(fb.pixel(10, 10), Some(Color::WHITE));
    }

    #[test]
    fn rotates_through_a_layer_buffer_with_exact_interior_pixels() {
        // 20x10 at (10, 10), turned 90 degrees about (20, 15): x 15..25, y 5..25.
        let list = page_list(
            "<style>html, body, div { margin: 0; padding: 0; }
             #t { margin: 10px; width: 20px; height: 10px; background: #ff0000;
                  transform: rotate(90deg); }</style><div id=\"t\"></div>",
            40.0,
        );
        let fb = rasterize(&list, 40, 40, 0.0);
        for (x, y) in [(15, 5), (24, 5), (15, 24), (24, 24), (20, 15)] {
            assert_eq!(fb.pixel(x, y), Some(RED), "inside at {x},{y}");
        }
        for (x, y) in [(14, 15), (25, 15), (20, 4), (20, 25), (11, 11), (28, 18)] {
            assert_eq!(fb.pixel(x, y), Some(Color::WHITE), "outside at {x},{y}");
        }
        // The same frame from bands, tiles, and a scroll.
        let tall = rasterize(&list, 40, 300, 0.0);
        assert_eq!(rasterize_parallel(&list, 40, 300, 0.0), tall);
        let mut fb = Framebuffer::new(40, 300, Color::WHITE);
        rasterize_incremental(&DisplayList::default(), &list, 0.0, 0.0, &mut fb);
        assert_eq!(fb, tall);
        let scrolled = rasterize(&list, 40, 40, 3.0);
        assert_eq!(scrolled.pixel(15, 2), Some(RED));
        assert_eq!(scrolled.pixel(15, 22), Some(Color::WHITE));
        // Zoom 2x doubles every edge.
        let zoomed = rasterize(&list.scaled(2.0), 80, 80, 0.0);
        assert_eq!(zoomed.pixel(30, 10), Some(RED));
        assert_eq!(zoomed.pixel(49, 49), Some(RED));
        assert_eq!(zoomed.pixel(29, 30), Some(Color::WHITE));
        assert_eq!(zoomed.pixel(50, 30), Some(Color::WHITE));
    }

    #[test]
    fn outer_clips_stay_square_and_own_clips_turn_with_the_box() {
        // The outer box clips at 0..30; the inner box is turned 45 degrees and
        // clips its own 40px wide child, so the child shows as a diamond.
        let list = page_list(
            "<style>html, body, div { margin: 0; padding: 0; }
             #clip { width: 30px; height: 30px; overflow: hidden; }
             #turn { width: 20px; height: 20px; margin: 5px; overflow: hidden;
                     transform: rotate(45deg); }
             #fill { width: 40px; height: 20px; background: #ff0000; }
             </style><div id=\"clip\"><div id=\"turn\"><div id=\"fill\"></div></div></div>",
            60.0,
        );
        let fb = rasterize(&list, 60, 60, 0.0);
        // Center of the diamond, and its top tip 14px above the center.
        assert_eq!(fb.pixel(15, 15), Some(RED));
        assert_eq!(fb.pixel(15, 3), Some(RED));
        // Corners of the unturned 5..25 square are outside the diamond.
        assert_eq!(fb.pixel(6, 6), Some(Color::WHITE));
        assert_eq!(fb.pixel(23, 23), Some(Color::WHITE));
        // Nothing passes the outer clip, even the diamond's right tip at 29.1.
        assert_eq!(fb.pixel(30, 15), Some(Color::WHITE));
    }

    #[test]
    fn flattened_or_broken_transforms_draw_nothing() {
        for transform in [
            "scale(0)",
            "scale(1, 0)",
            "matrix(1, 2, 2, 4, 0, 0)",
            "scale(1e30) scale(1e30)",
        ] {
            let list = page_list(
                &format!(
                    "<style>html, body, div {{ margin: 0; }}
                     #t {{ width: 20px; height: 20px; background: #ff0000;
                          transform: {transform}; }}</style><div id=\"t\"></div>"
                ),
                40.0,
            );
            let fb = rasterize(&list, 40, 40, 0.0);
            assert!(
                (0..40).all(|y| (0..40).all(|x| fb.pixel(x, y) == Some(Color::WHITE))),
                "{transform}"
            );
        }
    }

    #[test]
    fn opacity_blends_the_box_as_one_group() {
        // Two overlapping red boxes in a half-opaque parent: the overlap
        // matches the rest, and a zero-opacity box draws nothing.
        let list = page_list(
            "<style>html, body, div { margin: 0; padding: 0; }
             #group { opacity: 0.5; height: 20px; }
             #a { width: 20px; height: 20px; background: #ff0000; }
             #b { position: absolute; left: 10px; top: 0; width: 20px; height: 20px;
                  background: #ff0000; }
             #gone { opacity: 0; width: 20px; height: 20px; background: #0000ff; }
             </style><div id=\"group\"><div id=\"a\"></div><div id=\"b\"></div></div>
             <div id=\"gone\"></div>",
            40.0,
        );
        let fb = rasterize(&list, 40, 60, 0.0);
        let pink = Color {
            r: 255,
            g: 128,
            b: 128,
            a: 255,
        };
        assert_eq!(fb.pixel(5, 5), Some(pink));
        assert_eq!(fb.pixel(15, 5), Some(pink));
        assert_eq!(fb.pixel(25, 19), Some(pink));
        assert_eq!(fb.pixel(35, 5), Some(Color::WHITE));
        assert_eq!(fb.pixel(5, 25), Some(Color::WHITE));
        assert_eq!(
            rasterize_parallel(&list, 40, 300, 0.0).pixel(15, 5),
            Some(pink)
        );
    }

    #[test]
    fn a_frame_loop_moves_a_transition_across_the_canvas() {
        use anvil_dom::parse_html;
        use anvil_layout::layout_document;
        use anvil_paint::paint;
        use anvil_style::{Animator, ManualClock, compute_styles_animated, parse_css};

        let mut doc = parse_html(
            "<style>html, body, div { margin: 0; padding: 0; }
             #box { width: 10px; height: 10px; background: #ff0000;
                    transition: transform 200ms linear; }</style><div id=\"box\"></div>",
        );
        let sheet = parse_css(&doc.embedded_css());
        let clock = ManualClock::new();
        let mut animator = Animator::new();
        let draw = |doc: &anvil_dom::Document, animator: &mut Animator| {
            let more = animator.tick_clock(&clock);
            let styles = compute_styles_animated(doc, &sheet, 60.0, 20.0, animator);
            let fb = rasterize(&paint(&layout_document(doc, &styles, 60.0)), 60, 20, 0.0);
            let left = (0..60).find(|&x| fb.pixel(x, 5) == Some(RED));
            (more || animator.is_animating(), left)
        };
        assert_eq!(draw(&doc, &mut animator), (false, Some(0)));
        let id = doc.get_element_by_id("box").unwrap();
        doc.set_attribute(id, "style", "transform: translateX(40px)");
        assert_eq!(draw(&doc, &mut animator), (true, Some(0)));
        clock.advance(50.0);
        assert_eq!(draw(&doc, &mut animator), (true, Some(10)));
        clock.advance(100.0);
        assert_eq!(draw(&doc, &mut animator), (true, Some(30)));
        clock.advance(50.0);
        assert_eq!(draw(&doc, &mut animator), (false, Some(40)));
    }
}
