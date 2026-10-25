// SPDX-License-Identifier: LicenseRef-Proprietary
// Copyright (c) 2026 Buntato
// CPU scanline helpers for the anvil-raster framebuffer.
// Author: Buntato
//
// Compiled alongside anvil_raster_compositor.cpp by crates/anvil-raster/build.rs
// when a C++20 compiler is on PATH, setting cfg(anvil_cpp_accel).

#include "anvil_raster_accel.hpp"
#if !defined(_MSC_VER) && defined(__SSE2__)
#include <emmintrin.h>
#endif

using namespace anvil_raster_cpp;

extern "C" {

// Rust accepts this object file only when the result is 16.
u32 anvil_cpp_simd_width_bytes() noexcept {
#if defined(_M_X64) || defined(__x86_64__) || defined(__SSE2__)
    return 16u;
#else
    return 4u;
#endif
}

u32 anvil_cpp_raster_kernel_version() noexcept {
    return 16u;
}

// Top-down RGBA to bottom-up BGRA for a GDI DIB. Row 0 of dst is the last src row.
void anvil_cpp_rgba_to_bgra_bottom_up(
    const u8* src_rgba,
    u8* dst_bgra,
    u32 width,
    u32 height
) noexcept {
    if (!src_rgba || !dst_bgra || width == 0 || height == 0) {
        return;
    }
    const u32 stride = width * 4u;
    for (u32 row = 0; row < height; ++row) {
        const u32 src_row = (height - 1u - row) * stride;
        const u32 dst_row = row * stride;
        u32 col = 0u;
#if !defined(_MSC_VER) && defined(__SSE2__)
        const __m128i mask_ga = _mm_set1_epi32(static_cast<int>(0xFF00FF00u));
        const __m128i mask_r  = _mm_set1_epi32(0x000000FF);
        const __m128i mask_b  = _mm_set1_epi32(0x00FF0000);
        for (; col + 4u <= width; col += 4u) {
            const __m128i v = _mm_loadu_si128(reinterpret_cast<const __m128i*>(src_rgba + src_row + col * 4u));
            const __m128i ga = _mm_and_si128(v, mask_ga);
            const __m128i r_shifted = _mm_slli_epi32(_mm_and_si128(v, mask_r), 16);
            const __m128i b_shifted = _mm_srli_epi32(_mm_and_si128(v, mask_b), 16);
            const __m128i bgra = _mm_or_si128(ga, _mm_or_si128(r_shifted, b_shifted));
            _mm_storeu_si128(reinterpret_cast<__m128i*>(dst_bgra + dst_row + col * 4u), bgra);
        }
#else
        for (; col + 4u <= width; col += 4u) {
            for (u32 lane = 0u; lane < 4u; ++lane) {
                const u32 s = src_row + (col + lane) * 4u;
                const u32 d = dst_row + (col + lane) * 4u;
                dst_bgra[d]     = src_rgba[s + 2];
                dst_bgra[d + 1] = src_rgba[s + 1];
                dst_bgra[d + 2] = src_rgba[s];
                dst_bgra[d + 3] = src_rgba[s + 3];
            }
        }
#endif
        for (; col < width; ++col) {
            const u32 s = src_row + col * 4u;
            const u32 d = dst_row + col * 4u;
            dst_bgra[d] = src_rgba[s + 2];
            dst_bgra[d + 1] = src_rgba[s + 1];
            dst_bgra[d + 2] = src_rgba[s];
            dst_bgra[d + 3] = src_rgba[s + 3];
        }
    }
}

// Fill [x0, x1) by [y0, y1) in the RGBA buffer.
void anvil_cpp_fill_rect_rgba(
    u8* pixels,
    u32 fb_width,
    u32 fb_height,
    i32 x0,
    i32 y0,
    i32 x1,
    i32 y1,
    u8 r,
    u8 g,
    u8 b,
    u8 a
) noexcept {
    if (!pixels || a == 0 || fb_width == 0 || fb_height == 0) {
        return;
    }
    if (a == 255u) {
        const i32 ox0 = x0 < 0 ? 0 : x0;
        const i32 oy0 = y0 < 0 ? 0 : y0;
        const i32 ox1 = x1 > static_cast<i32>(fb_width) ? static_cast<i32>(fb_width) : x1;
        const i32 oy1 = y1 > static_cast<i32>(fb_height) ? static_cast<i32>(fb_height) : y1;
        if (ox0 >= ox1 || oy0 >= oy1) {
            return;
        }
        const u32 packed = static_cast<u32>(r)
            | (static_cast<u32>(g) << 8u)
            | (static_cast<u32>(b) << 16u)
            | 0xFF000000u;
#if !defined(_MSC_VER) && defined(__SSE2__)
        const __m128i vec4 = _mm_set1_epi32(static_cast<int>(packed));
#endif
        for (i32 py = oy0; py < oy1; ++py) {
            i32 px = ox0;
            u8* row_ptr = pixels + static_cast<u32>(py) * fb_width * 4u;
#if !defined(_MSC_VER) && defined(__SSE2__)
            for (; px + 4 <= ox1; px += 4) {
                _mm_storeu_si128(reinterpret_cast<__m128i*>(row_ptr + static_cast<u32>(px) * 4u), vec4);
            }
#else
            for (; px + 4 <= ox1; px += 4) {
                for (i32 lane = 0; lane < 4; ++lane) {
                    const u32 off = static_cast<u32>(px + lane) * 4u;
                    row_ptr[off]     = r;
                    row_ptr[off + 1] = g;
                    row_ptr[off + 2] = b;
                    row_ptr[off + 3] = 255u;
                }
            }
#endif
            for (; px < ox1; ++px) {
                const u32 off = static_cast<u32>(px) * 4u;
                row_ptr[off]     = r;
                row_ptr[off + 1] = g;
                row_ptr[off + 2] = b;
                row_ptr[off + 3] = 255u;
            }
        }
        return;
    }
    if (false) {
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
        for (i32 px = cx0; px < cx1; ++px) {
            blend_pixel_rgba(
                pixels,
                fb_width,
                static_cast<u32>(px),
                static_cast<u32>(py),
                r,
                g,
                b,
                a
            );
        }
    }
}

// Fill a rounded rectangle, optionally mixing a border, into the RGBA buffer.
void anvil_cpp_fill_rounded_rect_sdf(
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
    u8 fill_r,
    u8 fill_g,
    u8 fill_b,
    u8 fill_a,
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
    const float center_x = x + half_w;
    const float center_y = y + half_h;

    for (i32 py = cy0; py < cy1; ++py) {
        const float sy = static_cast<float>(py) + 0.5f;
        for (i32 px = cx0; px < cx1; ++px) {
            const float sx = static_cast<float>(px) + 0.5f;
            const float dist = sdf_rounded_box_cpp(sx - center_x, sy - center_y, half_w, half_h, radius);
            const float outer_alpha = f_clamp(0.5f - dist, 0.0f, 1.0f);
            if (outer_alpha <= 0.0f) {
                continue;
            }
            float cr = static_cast<float>(fill_r);
            float cg = static_cast<float>(fill_g);
            float cb = static_cast<float>(fill_b);
            float ca = static_cast<float>(fill_a);

            if (border_width > 0.0f) {
                const float inner_dist = dist + border_width;
                const float border_mix = f_clamp(0.5f + inner_dist, 0.0f, 1.0f);
                const float inv = 1.0f - border_mix;
                cr = cr * inv + static_cast<float>(border_r) * border_mix;
                cg = cg * inv + static_cast<float>(border_g) * border_mix;
                cb = cb * inv + static_cast<float>(border_b) * border_mix;
                ca = ca * inv + static_cast<float>(border_a) * border_mix;
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

} // extern "C"
