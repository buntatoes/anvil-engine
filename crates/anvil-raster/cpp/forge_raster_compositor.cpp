// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0-OR-Commercial
// Copyright (c) 2026 Buntato
// C++20 SDF Box-Shadow, Linear-Gradient, Clipped RGBA Blitter, Glyph Coverage, and NV12 Kernels.
// Author: Buntato

#include "forge_raster_accel.hpp"

using namespace forge_raster_cpp;

extern "C" {

// Analytical SDF box-shadow scanline pass.
void anvil_cpp_fill_box_shadow_sdf(
    u8* pixels,
    u32 fb_width,
    u32 fb_height,
    i32 x0,
    i32 y0,
    i32 x1,
    i32 y1,
    float cx,
    float cy,
    float half_w,
    float half_h,
    float radius,
    float blur,
    u8 r,
    u8 g,
    u8 b,
    u8 a
) noexcept {
    if (!pixels || a == 0 || fb_width == 0 || fb_height == 0) {
        return;
    }
    const i32 cx0 = x0 < 0 ? 0 : x0;
    const i32 cy0 = y0 < 0 ? 0 : y0;
    const i32 cx1 = x1 > static_cast<i32>(fb_width) ? static_cast<i32>(fb_width) : x1;
    const i32 cy1 = y1 > static_cast<i32>(fb_height) ? static_cast<i32>(fb_height) : y1;
    if (cx0 >= cx1 || cy0 >= cy1) {
        return;
    }

    for (i32 py = cy0; py < cy1; ++py) {
        const float sy = static_cast<float>(py) + 0.5f;
        for (i32 px = cx0; px < cx1; ++px) {
            const float sx = static_cast<float>(px) + 0.5f;
            const float dist = sdf_rounded_box_cpp(sx - cx, sy - cy, half_w, half_h, radius);
            const float alpha = sdf_box_shadow_alpha_cpp(dist, blur);
            if (alpha <= 0.0f) {
                continue;
            }
            const u8 final_a = static_cast<u8>(static_cast<float>(a) * alpha + 0.5f);
            blend_pixel_rgba(
                pixels,
                fb_width,
                static_cast<u32>(px),
                static_cast<u32>(py),
                r,
                g,
                b,
                final_a
            );
        }
    }
}

// Rounded-rect two-stop linear gradient scanline pass with optional SDF border mix.
void anvil_cpp_fill_linear_gradient_sdf(
    u8* pixels,
    u32 fb_width,
    u32 fb_height,
    i32 x0,
    i32 y0,
    i32 x1,
    i32 y1,
    float x,
    float y,
    float width,
    float height,
    float radius,
    float border_width,
    float angle_deg,
    u8 start_r,
    u8 start_g,
    u8 start_b,
    u8 start_a,
    u8 end_r,
    u8 end_g,
    u8 end_b,
    u8 end_a,
    u8 border_r,
    u8 border_g,
    u8 border_b,
    u8 border_a
) noexcept {
    if (!pixels || width <= 0.0f || height <= 0.0f || fb_width == 0 || fb_height == 0) {
        return;
    }
    const i32 cx0 = x0 < 0 ? 0 : x0;
    const i32 cy0 = y0 < 0 ? 0 : y0;
    const i32 cx1 = x1 > static_cast<i32>(fb_width) ? static_cast<i32>(fb_width) : x1;
    const i32 cy1 = y1 > static_cast<i32>(fb_height) ? static_cast<i32>(fb_height) : y1;
    if (cx0 >= cx1 || cy0 >= cy1) {
        return;
    }

    const float half_w = width * 0.5f;
    const float half_h = height * 0.5f;
    const float cx = x + half_w;
    const float cy = y + half_h;

    float sin_a = 0.0f;
    float cos_a = 1.0f;
    f_sincos_deg(angle_deg, &sin_a, &cos_a);
    const float dir_x = sin_a;
    const float dir_y = -cos_a;
    const float max_proj = f_max(f_abs(dir_x) * half_w + f_abs(dir_y) * half_h, 1.0f);

    for (i32 py = cy0; py < cy1; ++py) {
        const float sy = static_cast<float>(py) + 0.5f;
        const float rel_y = sy - cy;
        for (i32 px = cx0; px < cx1; ++px) {
            const float sx = static_cast<float>(px) + 0.5f;
            const float rel_x = sx - cx;
            const float dist = sdf_rounded_box_cpp(rel_x, rel_y, half_w, half_h, radius);
            const float outer_alpha = f_clamp(0.5f - dist, 0.0f, 1.0f);
            if (outer_alpha <= 0.0f) {
                continue;
            }

            const float proj = rel_x * dir_x + rel_y * dir_y;
            const float t = f_clamp((proj / max_proj) * 0.5f + 0.5f, 0.0f, 1.0f);
            const float inv_t = 1.0f - t;

            float cr = static_cast<float>(start_r) * inv_t + static_cast<float>(end_r) * t;
            float cg = static_cast<float>(start_g) * inv_t + static_cast<float>(end_g) * t;
            float cb = static_cast<float>(start_b) * inv_t + static_cast<float>(end_b) * t;
            float ca = static_cast<float>(start_a) * inv_t + static_cast<float>(end_a) * t;

            if (border_width > 0.0f) {
                const float inner_dist = dist + border_width;
                const float border_mix = f_clamp(0.5f + inner_dist, 0.0f, 1.0f);
                const float inv_b = 1.0f - border_mix;
                cr = cr * inv_b + static_cast<float>(border_r) * border_mix;
                cg = cg * inv_b + static_cast<float>(border_g) * border_mix;
                cb = cb * inv_b + static_cast<float>(border_b) * border_mix;
                ca = ca * inv_b + static_cast<float>(border_a) * border_mix;
            }

            const u8 final_a = static_cast<u8>(ca * outer_alpha + 0.5f);
            blend_pixel_rgba(
                pixels,
                fb_width,
                static_cast<u32>(px),
                static_cast<u32>(py),
                static_cast<u8>(cr + 0.5f),
                static_cast<u8>(cg + 0.5f),
                static_cast<u8>(cb + 0.5f),
                final_a
            );
        }
    }
}

// 8-bit coverage glyph bitmap blender with clip rect enforcement.
void anvil_cpp_blend_glyph_coverage(
    u8* pixels,
    u32 fb_width,
    u32 fb_height,
    i32 clip_x0,
    i32 clip_y0,
    i32 clip_x1,
    i32 clip_y1,
    i32 dst_x,
    i32 dst_y,
    const u8* coverage,
    u32 glyph_w,
    u32 glyph_h,
    u8 r,
    u8 g,
    u8 b,
    u8 a
) noexcept {
    if (!pixels || !coverage || a == 0 || glyph_w == 0 || glyph_h == 0) {
        return;
    }
    for (u32 gy = 0; gy < glyph_h; ++gy) {
        const i32 py = dst_y + static_cast<i32>(gy);
        if (py < clip_y0 || py >= clip_y1 || py < 0 || py >= static_cast<i32>(fb_height)) {
            continue;
        }
        for (u32 gx = 0; gx < glyph_w; ++gx) {
            const i32 px = dst_x + static_cast<i32>(gx);
            if (px < clip_x0 || px >= clip_x1 || px < 0 || px >= static_cast<i32>(fb_width)) {
                continue;
            }
            const u8 cov = coverage[gy * glyph_w + gx];
            if (cov == 0) {
                continue;
            }
            const u8 eff_a = static_cast<u8>((static_cast<u16>(a) * static_cast<u16>(cov) + 127u) / 255u);
            blend_pixel_rgba(
                pixels,
                fb_width,
                static_cast<u32>(px),
                static_cast<u32>(py),
                r,
                g,
                b,
                eff_a
            );
        }
    }
}

// Clipped RGBA image blitter with optional rounded-corner SDF mask.
void anvil_cpp_blit_rgba_clipped(
    u8* dst_pixels,
    u32 fb_width,
    u32 fb_height,
    i32 clip_x0,
    i32 clip_y0,
    i32 clip_x1,
    i32 clip_y1,
    i32 base_x,
    i32 base_y,
    const u8* src_rgba,
    u32 src_w,
    u32 src_h,
    float border_radius
) noexcept {
    if (!dst_pixels || !src_rgba || src_w == 0 || src_h == 0) {
        return;
    }
    const float half_w = static_cast<float>(src_w) * 0.5f;
    const float half_h = static_cast<float>(src_h) * 0.5f;

    for (u32 iy = 0; iy < src_h; ++iy) {
        const i32 py = base_y + static_cast<i32>(iy);
        if (py < clip_y0 || py >= clip_y1 || py < 0 || py >= static_cast<i32>(fb_height)) {
            continue;
        }
        for (u32 ix = 0; ix < src_w; ++ix) {
            const i32 px = base_x + static_cast<i32>(ix);
            if (px < clip_x0 || px >= clip_x1 || px < 0 || px >= static_cast<i32>(fb_width)) {
                continue;
            }
            const u32 s_off = (iy * src_w + ix) * 4u;
            float alpha = static_cast<float>(src_rgba[s_off + 3]);
            if (border_radius > 0.0f) {
                const float dist = sdf_rounded_box_cpp(
                    (static_cast<float>(ix) + 0.5f) - half_w,
                    (static_cast<float>(iy) + 0.5f) - half_h,
                    half_w,
                    half_h,
                    border_radius
                );
                const float mask = f_clamp(0.5f - dist, 0.0f, 1.0f);
                alpha *= mask;
            }
            if (alpha <= 0.5f) {
                continue;
            }
            blend_pixel_rgba(
                dst_pixels,
                fb_width,
                static_cast<u32>(px),
                static_cast<u32>(py),
                src_rgba[s_off],
                src_rgba[s_off + 1],
                src_rgba[s_off + 2],
                static_cast<u8>(alpha + 0.5f)
            );
        }
    }
}

// Semi-planar NV12 (Y plane + interleaved UV plane) to RGBA8 BT.709 color conversion.
void anvil_cpp_nv12_to_rgba_bt709(
    const u8* y_plane,
    const u8* uv_plane,
    u8* dst_rgba,
    u32 width,
    u32 height
) noexcept {
    if (!y_plane || !uv_plane || !dst_rgba || width == 0 || height == 0) {
        return;
    }
    const u32 uv_stride = (width + 1u) & ~1u;
    for (u32 py = 0; py < height; ++py) {
        const u32 uv_row = (py / 2u) * uv_stride;
        for (u32 px = 0; px < width; ++px) {
            const i32 y_val = static_cast<i32>(y_plane[py * width + px]);
            const u32 uv_idx = uv_row + (px & ~1u);
            const i32 u_val = static_cast<i32>(uv_plane[uv_idx]) - 128;
            const i32 v_val = static_cast<i32>(uv_plane[uv_idx + 1u]) - 128;

            const i32 c = (y_val > 16) ? (y_val - 16) * 298 : 0;
            const i32 r = (c + 459 * v_val + 128) >> 8;
            const i32 g = (c - 55 * u_val - 136 * v_val + 128) >> 8;
            const i32 b = (c + 541 * u_val + 128) >> 8;

            const u32 out = (py * width + px) * 4u;
            dst_rgba[out]     = static_cast<u8>(r < 0 ? 0 : (r > 255 ? 255 : r));
            dst_rgba[out + 1] = static_cast<u8>(g < 0 ? 0 : (g > 255 ? 255 : g));
            dst_rgba[out + 2] = static_cast<u8>(b < 0 ? 0 : (b > 255 ? 255 : b));
            dst_rgba[out + 3] = 255u;
        }
    }
}

// 3-channel RGB subpixel LCD coverage glyph blender with clip rect enforcement.
void anvil_cpp_blend_subpixel_lcd(
    u8* pixels,
    u32 fb_width,
    u32 fb_height,
    i32 clip_x0,
    i32 clip_y0,
    i32 clip_x1,
    i32 clip_y1,
    i32 dst_x,
    i32 dst_y,
    const u8* rgb_coverage,
    u32 glyph_w,
    u32 glyph_h,
    u8 r,
    u8 g,
    u8 b,
    u8 a
) noexcept {
    if (!pixels || !rgb_coverage || a == 0 || glyph_w == 0 || glyph_h == 0) {
        return;
    }
    for (u32 gy = 0; gy < glyph_h; ++gy) {
        const i32 py = dst_y + static_cast<i32>(gy);
        if (py < clip_y0 || py >= clip_y1 || py < 0 || py >= static_cast<i32>(fb_height)) {
            continue;
        }
        for (u32 gx = 0; gx < glyph_w; ++gx) {
            const i32 px = dst_x + static_cast<i32>(gx);
            if (px < clip_x0 || px >= clip_x1 || px < 0 || px >= static_cast<i32>(fb_width)) {
                continue;
            }
            const u32 c_off = (gy * glyph_w + gx) * 3u;
            blend_subpixel_lcd_rgba(
                pixels,
                fb_width,
                static_cast<u32>(px),
                static_cast<u32>(py),
                r,
                g,
                b,
                a,
                rgb_coverage[c_off],
                rgb_coverage[c_off + 1u],
                rgb_coverage[c_off + 2u]
            );
        }
    }
}

// 3-pass separable horizontal + vertical sliding-window box blur approximating a Gaussian kernel.
void anvil_cpp_box_blur_rgba(
    u8* pixels,
    u8* scratch,
    u32 fb_width,
    u32 fb_height,
    i32 x0,
    i32 y0,
    i32 x1,
    i32 y1,
    u32 radius
) noexcept {
    if (!pixels || !scratch || radius == 0 || fb_width == 0 || fb_height == 0) {
        return;
    }
    const i32 cx0 = x0 < 0 ? 0 : x0;
    const i32 cy0 = y0 < 0 ? 0 : y0;
    const i32 cx1 = x1 > static_cast<i32>(fb_width) ? static_cast<i32>(fb_width) : x1;
    const i32 cy1 = y1 > static_cast<i32>(fb_height) ? static_cast<i32>(fb_height) : y1;
    if (cx0 >= cx1 || cy0 >= cy1) {
        return;
    }
    const i32 rw = cx1 - cx0;
    const i32 rh = cy1 - cy0;
    const i32 rad = static_cast<i32>(radius > 64u ? 64u : radius);
    const u32 kernel = static_cast<u32>(2 * rad + 1);

    for (int pass = 0; pass < 3; ++pass) {
        // Horizontal sliding-window pass: pixels -> scratch
        for (i32 ry = 0; ry < rh; ++ry) {
            u32 sum_r = 0u, sum_g = 0u, sum_b = 0u, sum_a = 0u;
            const u32 fb_row = static_cast<u32>(cy0 + ry) * fb_width;
            for (i32 k = -rad; k <= rad; ++k) {
                const i32 sx = (k < 0) ? 0 : (k >= rw ? (rw - 1) : k);
                const u32 off = (fb_row + static_cast<u32>(cx0 + sx)) * 4u;
                sum_r += pixels[off];
                sum_g += pixels[off + 1];
                sum_b += pixels[off + 2];
                sum_a += pixels[off + 3];
            }
            for (i32 rx = 0; rx < rw; ++rx) {
                const u32 d_off = static_cast<u32>(ry * rw + rx) * 4u;
                scratch[d_off]     = static_cast<u8>((sum_r + kernel / 2u) / kernel);
                scratch[d_off + 1] = static_cast<u8>((sum_g + kernel / 2u) / kernel);
                scratch[d_off + 2] = static_cast<u8>((sum_b + kernel / 2u) / kernel);
                scratch[d_off + 3] = static_cast<u8>((sum_a + kernel / 2u) / kernel);

                const i32 out_x = (rx - rad < 0) ? 0 : (rx - rad);
                const i32 in_x  = (rx + rad + 1 >= rw) ? (rw - 1) : (rx + rad + 1);
                const u32 out_off = (fb_row + static_cast<u32>(cx0 + out_x)) * 4u;
                const u32 in_off  = (fb_row + static_cast<u32>(cx0 + in_x)) * 4u;
                sum_r = sum_r + pixels[in_off]     - pixels[out_off];
                sum_g = sum_g + pixels[in_off + 1] - pixels[out_off + 1];
                sum_b = sum_b + pixels[in_off + 2] - pixels[out_off + 2];
                sum_a = sum_a + pixels[in_off + 3] - pixels[out_off + 3];
            }
        }

        // Vertical sliding-window pass: scratch -> pixels
        for (i32 rx = 0; rx < rw; ++rx) {
            u32 sum_r = 0u, sum_g = 0u, sum_b = 0u, sum_a = 0u;
            for (i32 k = -rad; k <= rad; ++k) {
                const i32 sy = (k < 0) ? 0 : (k >= rh ? (rh - 1) : k);
                const u32 off = static_cast<u32>(sy * rw + rx) * 4u;
                sum_r += scratch[off];
                sum_g += scratch[off + 1];
                sum_b += scratch[off + 2];
                sum_a += scratch[off + 3];
            }
            for (i32 ry = 0; ry < rh; ++ry) {
                const u32 d_off = (static_cast<u32>(cy0 + ry) * fb_width + static_cast<u32>(cx0 + rx)) * 4u;
                pixels[d_off]     = static_cast<u8>((sum_r + kernel / 2u) / kernel);
                pixels[d_off + 1] = static_cast<u8>((sum_g + kernel / 2u) / kernel);
                pixels[d_off + 2] = static_cast<u8>((sum_b + kernel / 2u) / kernel);
                pixels[d_off + 3] = static_cast<u8>((sum_a + kernel / 2u) / kernel);

                const i32 out_y = (ry - rad < 0) ? 0 : (ry - rad);
                const i32 in_y  = (ry + rad + 1 >= rh) ? (rh - 1) : (ry + rad + 1);
                const u32 out_off = static_cast<u32>(out_y * rw + rx) * 4u;
                const u32 in_off  = static_cast<u32>(in_y * rw + rx) * 4u;
                sum_r = sum_r + scratch[in_off]     - scratch[out_off];
                sum_g = sum_g + scratch[in_off + 1] - scratch[out_off + 1];
                sum_b = sum_b + scratch[in_off + 2] - scratch[out_off + 2];
                sum_a = sum_a + scratch[in_off + 3] - scratch[out_off + 3];
            }
        }
    }
}

} // extern "C"
