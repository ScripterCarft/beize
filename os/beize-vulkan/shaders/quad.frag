#version 460
// Solids, textures and glyphs alike, premultiplied. All textures sit in one array
// (bindless), so a frame's quads go out in few draws whatever they show.

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
// The radius and a shadow's sigma.
layout(location = 6) flat in vec2 rounding;
layout(location = 0) out vec4 result;

// Skia's contrast for text (SkMaskGamma.cpp, its defaults: blending in
// linear light, SK_GAMMA_CONTRAST 0.5). Blended linearly, a glyph's partial
// coverage looks lighter than it is: dark text on light thins, light text on
// dark does not. The coverage is raised by `contrast * linear(1 - luminance)`
// of the text's colour: fully for black, not at all for white (ADR 0027 §9).
const float CONTRAST = 0.5;

float encoded(float linear) {
    return linear <= 0.0031308 ? linear * 12.92 : 1.055 * pow(linear, 1.0 / 2.4) - 0.055;
}

float decoded(float encoded) {
    return encoded <= 0.04045 ? encoded / 12.92 : pow((encoded + 0.055) / 1.055, 2.4);
}

float coverage(float raw, vec4 text) {
    vec3 rgb = text.a > 0.0 ? text.rgb / text.a : vec3(0.0);
    // Luminance of the colour as it is encoded (Rec. 709 weights), as
    // Skia's SkComputeLuminance.
    float luminance = dot(vec3(encoded(rgb.r), encoded(rgb.g), encoded(rgb.b)),
                          vec3(0.2126, 0.7152, 0.0722));
    float k = CONTRAST * decoded(1.0 - luminance);
    return raw + (1.0 - raw) * k * raw;
}

// Apple's continuous corner, as its icons, windows and Dock have it
// (ADR 0028 §2): an arc of `radius` in the middle, eased into each edge by
// a curve whose curvature rises from zero – no kink in curvature where a
// circle meets the edge. Measured from the mask of Apple's App Icon
// Template (iOS 27, vector): it reaches EXTENT radii along each edge. RHO:
// from the corner's centre, EXTENT radii in from both edges, the distance
// to the curve in EXTENT radii, every 45/16 degrees from the edge to the
// diagonal, about which the corner is symmetric. Linear between them it
// is off by less than 3e-4 of the extent (src/pipeline.rs, CORNER_EXTENT).
const float EXTENT = 1.48744;
const float RHO[17] = float[](
    1.000000, 1.001193, 1.004700, 1.010436, 1.018224, 1.027838,
    1.038913, 1.050913, 1.063232, 1.075213, 1.086245, 1.095866,
    1.103823, 1.110052, 1.114512, 1.117195, 1.118093
);
const float STEPS_PER_RADIAN = 16.0 / 0.7853981633974483;

// The signed distance of `p` to the rectangle `box` with corners of
// `radius`, negative inside. Where the curve does not fit – a capsule, a
// dot – the corners are circular, as SwiftUI's Capsule.
float edge_distance(vec2 p, vec4 box, float radius) {
    vec2 half_size = box.zw * 0.5;
    // From the nearest vertical and horizontal edge, positive inside.
    vec2 u = half_size - abs(p - (box.xy + half_size));
    float limit = min(half_size.x, half_size.y);
    float extent = radius * EXTENT;
    if (extent > limit) {
        float r = min(radius, limit);
        vec2 q = r - u;
        return length(max(q, 0.0)) + min(max(q.x, q.y), 0.0) - r;
    }
    vec2 w = u - extent;
    if (w.x >= 0.0 || w.y >= 0.0) {
        return -min(u.x, u.y);
    }
    // In the corner: from its centre, the curve's distance by angle.
    float r = length(w);
    float phi = atan(-w.x, -w.y);
    float a = min(phi, 1.5707963267948966 - phi) * STEPS_PER_RADIAN;
    int i = min(int(a), 15);
    float rho = mix(RHO[i], RHO[i + 1], a - float(i)) * extent;
    // Along the ray the distance is r - rho; the curve's slant to it
    // shortens the true distance: divide by the gradient's length.
    float slope = (RHO[i + 1] - RHO[i]) * STEPS_PER_RADIAN * extent / max(r, 1e-3);
    return (r - rho) / sqrt(1.0 + slope * slope);
}

// How much of the pixel at `p` lies inside the rectangle `box` with
// corners of `radius`: a pixel wide antialiased.
float inside(vec2 p, vec4 box, float radius) {
    return clamp(0.5 - edge_distance(p, box, radius), 0.0, 1.0);
}

// The shadow of a rounded rectangle, blurred by a Gaussian: Evan Wallace's
// closed form ("Fast rounded rectangle shadows", as Zed and Figma draw
// theirs). Across, the blur of each row is exact (erf); down, four samples.
// Its corners are circular of the same radius: the continuous corner lies
// within less than a pixel of that, far below a shadow's blur.
const float PI = 3.141592653589793;

vec2 erf2(vec2 x) {
    vec2 s = sign(x);
    vec2 a = abs(x);
    x = 1.0 + (0.278393 + (0.230389 + 0.078108 * (a * a)) * a) * a;
    x *= x;
    return s - s / (x * x);
}

float gaussian(float x, float sigma) {
    return exp(-(x * x) / (2.0 * sigma * sigma)) / (sqrt(2.0 * PI) * sigma);
}

float shadow_row(float x, float y, float sigma, float radius, vec2 half_size) {
    float delta = min(half_size.y - radius - abs(y), 0.0);
    float curved = half_size.x - radius + sqrt(max(0.0, radius * radius - delta * delta));
    vec2 integral = 0.5 + 0.5 * erf2((x + vec2(-curved, curved)) * (sqrt(0.5) / sigma));
    return integral.y - integral.x;
}

float shadow(vec2 p, vec4 box, float radius, float sigma) {
    vec2 half_size = box.zw * 0.5;
    p -= box.xy + half_size;
    float low = p.y - half_size.y;
    float high = p.y + half_size.y;
    float start = clamp(-3.0 * sigma, low, high);
    float end = clamp(3.0 * sigma, low, high);
    float step = (end - start) / 4.0;
    float y = start + step * 0.5;
    float value = 0.0;
    for (int i = 0; i < 4; i++) {
        value += shadow_row(p.x, p.y - y, sigma, radius, half_size) * gaussian(y, sigma) * step;
        y += step;
    }
    return value;
}

void main() {
    if ((flags & SHADOW) != 0u) {
        result = color * shadow(gl_FragCoord.xy, clip, rounding.x, rounding.y);
        return;
    }
    // Round corners (rounding.x > 0): only what lies inside the window.
    float shown = rounding.x > 0.0 ? inside(gl_FragCoord.xy, clip, rounding.x) : 1.0;
    if ((flags & SOLID) != 0u) {
        result = color * shown;
        return;
    }
    // nonuniformEXT: neighbouring quads may show different textures.
    uint t = nonuniformEXT(texture);
    vec4 sampled;
    if ((flags & EXACT) != 0u) {
        // One texel per pixel: the texel itself, never a blend of two.
        sampled = texelFetch(sampler2D(textures[t], linear), ivec2(floor(uv)), 0);
    } else {
        sampled = textureLod(sampler2D(textures[t], linear), uv * texel, 0.0);
    }
    if ((flags & MASK) != 0u) {
        // A glyph: the texture's red is its coverage, the colour is the
        // text's (premultiplied).
        result = color * coverage(sampled.r, color) * shown;
    } else {
        result = sampled * (color.a * shown);
    }
}
