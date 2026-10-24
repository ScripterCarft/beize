#version 460
// The backdrop's passes (ADR 0028 §6), drawn once per change, not per frame:
// Beize's background, computed, and its blur for the base glass – Dual
// Kawase (Marius Bjørge, ARM, SIGGRAPH 2015; as KWin blurs), its last pass
// lifting and tinting the colours. A shader of its own, so the one that
// draws every frame (quad.frag) stays small. Out through an sRGB view: in
// linear light, encoded on the way out.

#extension GL_GOOGLE_include_directive : require
#extension GL_EXT_nonuniform_qualifier : require
#include "quad.glsl"

layout(set = 0, binding = 0) uniform texture2D textures[];
layout(set = 0, binding = 1) uniform sampler linear;

layout(location = 0) in vec2 uv;
layout(location = 1) flat in vec4 color;
layout(location = 2) flat in vec2 texel;
layout(location = 3) flat in uint texture;
layout(location = 4) flat in uint flags;
layout(location = 5) flat in vec4 clip;
layout(location = 6) flat in vec2 rounding;
layout(location = 7) flat in vec4 bottom;
layout(location = 0) out vec4 result;

float encoded(float linear) {
    return linear <= 0.0031308 ? linear * 12.92 : 1.055 * pow(linear, 1.0 / 2.4) - 0.055;
}

float decoded(float encoded) {
    return encoded <= 0.04045 ? encoded / 12.92 : pow((encoded + 0.055) / 1.055, 2.4);
}

vec3 decoded3(vec3 c) {
    return vec3(decoded(c.r), decoded(c.g), decoded(c.b));
}

vec3 encoded3(vec3 c) {
    return vec3(encoded(c.r), encoded(c.g), encoded(c.b));
}

// Oklab (Björn Ottosson): mixed there, colours keep their lightness and hue
// instead of turning muddy between two.
vec3 oklab(vec3 rgb) {
    vec3 lms = rgb * mat3(
        0.4122214708, 0.5363325363, 0.0514459929,
        0.2119034982, 0.6806995451, 0.1073969566,
        0.0883024619, 0.2817188376, 0.6299787005);
    return pow(max(lms, 0.0), vec3(1.0 / 3.0)) * mat3(
        0.2104542553, 0.7936177850, -0.0040720468,
        1.9779984951, -2.4285922050, 0.4505937099,
        0.0259040371, 0.7827717662, -0.8086757660);
}

vec3 linear_rgb(vec3 lab) {
    vec3 lms = lab * mat3(
        1.0, 0.3963377774, 0.2158037573,
        1.0, -0.1055613458, -0.0638541728,
        1.0, -0.0894841775, -1.2914855480);
    return (lms * lms * lms) * mat3(
        4.0767416621, -3.3077115913, 0.2309699292,
        -1.2684380046, 2.6097574011, -0.3413193965,
        -0.0041960863, -0.7034186147, 1.7076147010);
}

// Beize's background: one motif, light and dark (the appearance, 0 or 1),
// the same places in other colours. Four points of colour, each fading as a
// Gaussian, under four bands that flow across – the upper edge of each
// crisp and lit, the lower fading out – so the glass above has something
// to blur. Positions by the screen's height (x of the points by its width),
// so it keeps its shape on any screen.
const vec3 POINT[8] = vec3[](
    // x (by the width), y, spread
    vec3(0.05, 0.00, 0.60), vec3(1.10, 0.05, 0.55),
    vec3(0.25, 1.00, 0.55), vec3(1.40, 1.00, 0.50),
    vec3(0.05, 0.00, 0.60), vec3(1.10, 0.05, 0.55),
    vec3(0.25, 1.00, 0.55), vec3(1.40, 1.00, 0.50));
const vec3 POINT_COLOR[8] = vec3[](
    vec3(0x8f, 0xb6, 0xf2), vec3(0xb3, 0xa4, 0xec),
    vec3(0x5d, 0x8f, 0xe6), vec3(0xf2, 0xb8, 0xcf),
    vec3(0x0f, 0x1d, 0x4a), vec3(0x2e, 0x24, 0x70),
    vec3(0x1d, 0x4f, 0xb0), vec3(0x7a, 0x3a, 0x86));
// y, amplitude, frequency, phase; thickness, opacity.
const vec4 BAND[4] = vec4[](
    vec4(0.12, 0.05, 1.6, 0.4), vec4(0.30, 0.09, 1.3, 2.1),
    vec4(0.58, 0.08, 1.1, 4.0), vec4(0.82, 0.07, 1.5, 1.2));
const vec2 BAND_BODY[4] = vec2[](
    vec2(0.16, 0.85), vec2(0.22, 0.80), vec2(0.24, 0.85), vec2(0.26, 0.80));
const vec3 BAND_COLOR[8] = vec3[](
    vec3(0xc9, 0xdc, 0xff), vec3(0xa7, 0xb8, 0xfa),
    vec3(0x7a, 0xa0, 0xf0), vec3(0xf3, 0xc2, 0xda),
    vec3(0x22, 0x30, 0x7a), vec3(0x3a, 0x2d, 0x8a),
    vec3(0x24, 0x56, 0xc0), vec3(0x8a, 0x3f, 0x95));

vec3 wallpaper(vec2 p, vec2 size, uint look) {
    vec2 at = p / size.y;
    uint first = look * 4u;
    vec3 sum = vec3(0.0);
    float total = 0.0;
    for (uint i = 0u; i < 4u; i++) {
        vec3 point = POINT[first + i];
        vec2 d = at - vec2(point.x * size.x / size.y, point.y);
        float w = exp(-dot(d, d) / (point.z * point.z));
        sum += w * oklab(decoded3(POINT_COLOR[first + i] / 255.0));
        total += w;
    }
    vec3 lab = sum / total;
    for (uint i = 0u; i < 4u; i++) {
        vec4 b = BAND[i];
        float top = b.x + b.y * sin(b.z * at.x + b.w)
            + 0.35 * b.y * sin(2.3 * b.z * at.x + 1.7 * b.w);
        float bottom_edge = top + BAND_BODY[i].x * (1.0 + 0.35 * sin(0.8 * b.z * at.x + b.w + 1.0));
        float a = smoothstep(top - 0.002, top + 0.002, at.y)
            * (1.0 - smoothstep(top, bottom_edge, at.y));
        vec3 band = oklab(decoded3(BAND_COLOR[first + i] / 255.0));
        // Lit along its upper edge.
        band.x += 0.10 * exp(-max(at.y - top, 0.0) / 0.015);
        lab = mix(lab, band, a * BAND_BODY[i].y);
    }
    return clamp(linear_rgb(lab), 0.0, 1.0);
}

// Uniform in [0, 1) for a pixel (as quad.frag's).
float hash(uvec2 p) {
    uint h = p.x * 0x8da6b343u ^ p.y * 0xd8163841u;
    h ^= h >> 16;
    h *= 0x7feb352du;
    h ^= h >> 15;
    h *= 0x846ca68bu;
    h ^= h >> 16;
    return float(h) * (1.0 / 4294967296.0);
}

// A texel of the source, linear: sources are encoded (the pictures and,
// later, the screen), the filter mixes them as stored.
vec3 tap(vec2 at) {
    return decoded3(textureLod(sampler2D(textures[nonuniformEXT(texture)], linear), at, 0.0).rgb);
}

void main() {
    if ((flags & WALLPAPER) != 0u) {
        vec3 c = wallpaper(gl_FragCoord.xy, clip.zw, texture);
        // Dithered against banding in the soft gradient: half an 8-bit
        // step of triangular noise, where the sRGB view will round.
        uvec2 p = uvec2(gl_FragCoord.xy);
        float n = (hash(p) - hash(p + uvec2(7919u, 104729u))) / 255.0;
        result = vec4(decoded3(clamp(encoded3(c) + n, 0.0, 1.0)), 1.0);
        return;
    }
    // Dual Kawase: `rounding.y` is the taps' distance in source texels.
    vec2 at = uv * texel;
    vec2 h = texel * rounding.y;
    vec3 c;
    if ((flags & BLUR_DOWN) != 0u) {
        c = (4.0 * tap(at) + tap(at - h) + tap(at + h)
            + tap(at + vec2(h.x, -h.y)) + tap(at - vec2(h.x, -h.y))) / 8.0;
    } else {
        c = (tap(at + vec2(-2.0 * h.x, 0.0)) + tap(at + vec2(2.0 * h.x, 0.0))
            + tap(at + vec2(0.0, -2.0 * h.y)) + tap(at + vec2(0.0, 2.0 * h.y))
            + 2.0 * (tap(at + vec2(-h.x, h.y)) + tap(at + vec2(h.x, h.y))
                + tap(at + vec2(h.x, -h.y)) + tap(at + vec2(-h.x, -h.y)))) / 12.0;
    }
    if ((flags & MATERIAL) != 0u) {
        // The glass's colour: saturation lifted by `bottom.x` around the
        // luminance, then the tint `color` (linear, premultiplied) over it.
        float luminance = dot(c, vec3(0.2126, 0.7152, 0.0722));
        c = clamp(luminance + (c - luminance) * bottom.x, 0.0, 1.0);
        c = c * (1.0 - color.a) + color.rgb;
    }
    result = vec4(c, 1.0);
}
