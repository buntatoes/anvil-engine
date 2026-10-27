// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0-OR-Commercial
// Copyright (c) 2026 Buntato
// Shared freestanding C++20 definitions and math helpers for anvil-raster.
// Author: Buntato

#ifndef ANVIL_RASTER_ACCEL_HPP
#define ANVIL_RASTER_ACCEL_HPP

using u8  = unsigned char;
using u16 = unsigned short;
using u32 = unsigned int;
using i32 = int;
using u64 = unsigned long long;

namespace anvil_raster_cpp {

inline float f_abs(float v) noexcept {
    return v < 0.0f ? -v : v;
}

inline float f_max(float a, float b) noexcept {
    return a > b ? a : b;
}

inline float f_min(float a, float b) noexcept {
    return a < b ? a : b;
}

inline float f_clamp(float v, float lo, float hi) noexcept {
    return v < lo ? lo : (v > hi ? hi : v);
}

inline float f_sqrt(float x) noexcept {
    if (x <= 0.0f) {
        return 0.0f;
    }
    float guess = x > 1.0f ? x * 0.5f : 1.0f;
    for (int i = 0; i < 6; ++i) {
        guess = 0.5f * (guess + x / guess);
    }
    return guess;
}

// Polynomial sine/cosine approximations (freestanding, no libm dependency) for CSS gradient angles.
inline void f_sincos_deg(float angle_deg, float* out_sin, float* out_cos) noexcept {
    float deg = angle_deg;
    while (deg >= 360.0f) deg -= 360.0f;
    while (deg < 0.0f) deg += 360.0f;

    const float rad = deg * (3.1415926535f / 180.0f);
    // Reduce to [-pi, pi]
    const float x = (rad > 3.1415926535f) ? (rad - 6.283185307f) : rad;
    const float x2 = x * x;
    const float s = x * (1.0f - x2 * (1.0f / 6.0f - x2 * (1.0f / 120.0f - x2 * (1.0f / 5040.0f))));
    const float c = 1.0f - x2 * (0.5f - x2 * (1.0f / 24.0f - x2 * (1.0f / 720.0f)));
    *out_sin = f_clamp(s, -1.0f, 1.0f);
    *out_cos = f_clamp(c, -1.0f, 1.0f);
}

inline float sdf_rounded_box_cpp(
    float px,
    float py,
    float half_w,
    float half_h,
    float radius
) noexcept {
    const float r = f_clamp(radius, 0.0f, f_min(half_w, half_h));
    const float qx = f_abs(px) - (half_w - r);
    const float qy = f_abs(py) - (half_h - r);
    const float ox = f_max(qx, 0.0f);
    const float oy = f_max(qy, 0.0f);
    const float outside = f_sqrt(ox * ox + oy * oy);
    const float inside = f_min(f_max(qx, qy), 0.0f);
    return outside + inside - r;
}

inline float sdf_box_shadow_alpha_cpp(float dist, float blur_radius) noexcept {
    if (blur_radius <= 0.5f) {
        return f_clamp(0.5f - dist, 0.0f, 1.0f);
    }
    const float t = f_clamp(1.0f - (dist / blur_radius), 0.0f, 1.0f);
    return t * t * (3.0f - 2.0f * t);
}

inline void blend_pixel_rgba(
    u8* pixels,
    u32 fb_width,
    u32 px,
    u32 py,
    u8 r,
    u8 g,
    u8 b,
    u8 a
) noexcept {
    if (a == 0) {
        return;
    }
    const u32 offset = (py * fb_width + px) * 4u;
    if (a == 255) {
        pixels[offset] = r;
        pixels[offset + 1] = g;
        pixels[offset + 2] = b;
        pixels[offset + 3] = 255;
        return;
    }
    const u16 alpha = static_cast<u16>(a);
    const u16 inv = static_cast<u16>(255u - alpha);
    pixels[offset] = static_cast<u8>((static_cast<u16>(r) * alpha + static_cast<u16>(pixels[offset]) * inv) / 255u);
    pixels[offset + 1] = static_cast<u8>((static_cast<u16>(g) * alpha + static_cast<u16>(pixels[offset + 1]) * inv) / 255u);
    pixels[offset + 2] = static_cast<u8>((static_cast<u16>(b) * alpha + static_cast<u16>(pixels[offset + 2]) * inv) / 255u);
    pixels[offset + 3] = 255;
}

inline void blend_subpixel_lcd_rgba(
    u8* pixels,
    u32 fb_width,
    u32 px,
    u32 py,
    u8 r,
    u8 g,
    u8 b,
    u8 a,
    u8 cov_r,
    u8 cov_g,
    u8 cov_b
) noexcept {
    if (a == 0 || (cov_r == 0 && cov_g == 0 && cov_b == 0)) {
        return;
    }
    const u32 offset = (py * fb_width + px) * 4u;
    const u16 ar = static_cast<u16>((static_cast<u16>(a) * static_cast<u16>(cov_r) + 127u) / 255u);
    const u16 ag = static_cast<u16>((static_cast<u16>(a) * static_cast<u16>(cov_g) + 127u) / 255u);
    const u16 ab = static_cast<u16>((static_cast<u16>(a) * static_cast<u16>(cov_b) + 127u) / 255u);
    pixels[offset]     = static_cast<u8>((static_cast<u16>(r) * ar + static_cast<u16>(pixels[offset])     * (255u - ar)) / 255u);
    pixels[offset + 1] = static_cast<u8>((static_cast<u16>(g) * ag + static_cast<u16>(pixels[offset + 1]) * (255u - ag)) / 255u);
    pixels[offset + 2] = static_cast<u8>((static_cast<u16>(b) * ab + static_cast<u16>(pixels[offset + 2]) * (255u - ab)) / 255u);
    pixels[offset + 3] = 255u;
}

} // namespace anvil_raster_cpp

#endif // ANVIL_RASTER_ACCEL_HPP
