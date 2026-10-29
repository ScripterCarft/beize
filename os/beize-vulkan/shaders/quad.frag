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
// A gradient's colour at the bottom.
layout(location = 7) flat in vec4 bottom;
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
// Template (iOS 27, vector): it reaches EXTENT radii along each edge. From
// the corner's centre, EXTENT radii in from both edges, the distance to
// the curve in EXTENT radii, measured every 45/16 degrees from the edge (1)
// to the diagonal (1.118093), about which the corner is symmetric: RHO, a
// polynomial in x = s², s = 1 - t, t the angle from the edge in eighths of
// a turn (1 on the diagonal, where it is flat). It meets the 17 measured
// points within 5e-5 (src/pipeline.rs, CORNER_EXTENT). A formula, not the
// table it was before: read with an index computed per pixel, NVK
// returned garbage from it at times – rays from every corner (24.10.).
const float EXTENT = 1.48744;
const float RHO0 = 1.1180579;
const float RHO1 = -0.2242271;
const float RHO2 = -0.0785316;
const float RHO3 = 0.5015202;
const float RHO4 = -0.4636726;
const float RHO5 = 0.1468606;
const float QUARTER_PI = 0.7853981633974483;

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
    float s = 1.0 - min(phi, 2.0 * QUARTER_PI - phi) / QUARTER_PI;
    float x = s * s;
    float rho = (RHO0 + x * (RHO1 + x * (RHO2 + x * (RHO3 + x * (RHO4 + x * RHO5))))) * extent;
    // d(rho)/d(phi): through x = s² and s = 1 - phi / (pi / 4).
    float drho_dx = RHO1 + x * (2.0 * RHO2 + x * (3.0 * RHO3 + x * (4.0 * RHO4 + x * 5.0 * RHO5)));
    float drho_dphi = -drho_dx * 2.0 * s / QUARTER_PI * extent;
    // Along the ray the distance is r - rho; the curve's slant to it
    // shortens the true distance: divide by the gradient's length.
    float slope = drho_dphi / max(r, 1e-3);
    return (r - rho) / sqrt(1.0 + slope * slope);
}

// How much of the pixel at `p` lies inside the rectangle `box` with
// corners of `radius`: a pixel wide antialiased.
float inside(vec2 p, vec4 box, float radius) {
    return clamp(0.5 - edge_distance(p, box, radius), 0.0, 1.0);
}

// The window buttons' marks (`STROKE`, `TRIANGLE`): computed per pixel, as
// the corners are, sharp at any size. Points are in the quad's pixels
// (`uv`).

// The signed distance of `p` to a stroke from `a` to `b`, `half_width` to
// each side: running `past` its ends (0 butt, half the width square, as
// SVG's caps), or with round ends.
float stroke_distance(vec2 p, vec2 a, vec2 b, float half_width, float past, bool round_ends) {
    float len = length(b - a);
    vec2 along = len > 0.0 ? (b - a) / len : vec2(1.0, 0.0);
    vec2 pa = p - a;
    float t = dot(pa, along);
    if (round_ends) {
        return length(pa - along * clamp(t, 0.0, len)) - half_width;
    }
    vec2 q = vec2(abs(t - 0.5 * len) - (0.5 * len + past),
                  abs(dot(pa, vec2(-along.y, along.x))) - half_width);
    return length(max(q, 0.0)) + min(max(q.x, q.y), 0.0);
}

// The signed distance of `p` to the triangle `p0`, `p1`, `p2`: Inigo
// Quilez's ("2D distance functions", sdTriangle).
float triangle_distance(vec2 p, vec2 p0, vec2 p1, vec2 p2) {
    vec2 e0 = p1 - p0;
    vec2 e1 = p2 - p1;
    vec2 e2 = p0 - p2;
    vec2 v0 = p - p0;
    vec2 v1 = p - p1;
    vec2 v2 = p - p2;
    vec2 pq0 = v0 - e0 * clamp(dot(v0, e0) / dot(e0, e0), 0.0, 1.0);
    vec2 pq1 = v1 - e1 * clamp(dot(v1, e1) / dot(e1, e1), 0.0, 1.0);
    vec2 pq2 = v2 - e2 * clamp(dot(v2, e2) / dot(e2, e2), 0.0, 1.0);
    float s = sign(e0.x * e2.y - e0.y * e2.x);
    vec2 d = min(min(vec2(dot(pq0, pq0), s * (v0.x * e0.y - v0.y * e0.x)),
                     vec2(dot(pq1, pq1), s * (v1.x * e1.y - v1.y * e1.x))),
                 vec2(dot(pq2, pq2), s * (v2.x * e2.y - v2.y * e2.x)));
    return -sqrt(d.x) * sign(d.y);
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

// A premultiplied linear colour as CSS interpolates it by default:
// premultiplied, in sRGB as encoded (CSS Color 4, "Interpolating with
// Alpha"), and back.
vec4 to_encoded(vec4 c) {
    vec3 rgb = c.a > 0.0 ? c.rgb / c.a : vec3(0.0);
    return vec4(vec3(encoded(rgb.r), encoded(rgb.g), encoded(rgb.b)) * c.a, c.a);
}

vec4 from_encoded(vec4 c) {
    vec3 rgb = c.a > 0.0 ? c.rgb / c.a : vec3(0.0);
    return vec4(vec3(decoded(rgb.r), decoded(rgb.g), decoded(rgb.b)) * c.a, c.a);
}

// From `color` at the top of `clip` to `bottom` at its bottom.
vec4 gradient(float y) {
    float t = clamp((y - clip.y) / max(clip.w, 1.0), 0.0, 1.0);
    return from_encoded(mix(to_encoded(color), to_encoded(bottom), t));
}

// Uniform in [0, 1) for a pixel: an integer hash (the "lowbias32" finaliser
// of Chris Wellons' hash prospector), the same every frame.
float hash(uvec2 p) {
    uint h = p.x * 0x8da6b343u ^ p.y * 0xd8163841u;
    h ^= h >> 16;
    h *= 0x7feb352du;
    h ^= h >> 15;
    h *= 0x846ca68bu;
    h ^= h >> 16;
    return float(h) * (1.0 / 4294967296.0);
}

// Base glass (ADR 0028 §6): the backdrop's blurred, lifted and tinted copy
// (backdrop.frag), with grain and a rim of light along its edge.
//
// The rim follows the light, as Apple's Liquid Glass: bright where the edge
// faces it (top left), weaker on the opposite side, all but gone between.
// RIM_FOLLOWS_LIGHT false: the same all round – a developer's choice, it
// costs the same.
const bool RIM_FOLLOWS_LIGHT = true;
const vec2 LIGHT = vec2(-0.6, -0.8);

// How much rim the pixel at `p` shows: a ring of about 1.5 px just inside
// the edge of `clip` with corners of `radius`.
float rim(vec2 p) {
    float d = edge_distance(p, clip, rounding.x);
    float ring = clamp(1.6 - abs(d + 0.8), 0.0, 1.0);
    if (ring <= 0.0 || !RIM_FOLLOWS_LIGHT) {
        return ring * 0.6;
    }
    // The edge's direction: the distance's gradient.
    vec2 n = vec2(
        edge_distance(p + vec2(0.5, 0.0), clip, rounding.x)
            - edge_distance(p - vec2(0.5, 0.0), clip, rounding.x),
        edge_distance(p + vec2(0.0, 0.5), clip, rounding.x)
            - edge_distance(p - vec2(0.0, 0.5), clip, rounding.x));
    float facing = dot(n / max(length(n), 1e-4), LIGHT);
    float light = facing > 0.0 ? facing * facing : 0.25 * facing * facing;
    return ring * (0.08 + 0.92 * light);
}

// `color` is the rim's (premultiplied), `rounding.y` the grain's strength.
vec4 glass(vec4 sampled) {
    uvec2 p = uvec2(gl_FragCoord.xy);
    // Triangular noise: steadier than uniform at the same strength.
    float grain = (hash(p) - hash(p + uvec2(7919u, 104729u))) * rounding.y;
    vec4 base = vec4(sampled.rgb + grain, 1.0);
    float r = color.a > 0.0 ? rim(gl_FragCoord.xy) : 0.0;
    return base * (1.0 - color.a * r) + color * r;
}

// Clear glass (`CLEAR`, the control center's tiles), its look as approved
// on mock-ups (29.10.): no grain; a lens – within LENS px of the edge it
// shows what lies further out along the edge's normal, the more the nearer
// the edge, by up to LENS_SHIFT px –, a faint white sheen at the top, an
// inner glow and the rim, both brighter where the edge faces the light (top
// left). All on the colours as encoded, as the mock-up mixed them.
const float LENS = 10.0;
const float LENS_SHIFT = 9.0;
const float SHEEN = 0.05;
const float SHEEN_DOWN_TO = 0.35;
const float GLOW = 0.16;
const float GLOW_WIDTH = 4.0;
const float RIM_WIDTH = 1.5;
const vec2 CLEAR_LIGHT = vec2(-0.70710678, -0.70710678);

vec4 clear_glass() {
    vec2 p = gl_FragCoord.xy;
    float d = edge_distance(p, clip, rounding.x);
    float shown = clamp(0.5 - d, 0.0, 1.0);
    float within = max(-d, 0.0);
    // The edge's direction, outwards: the distance's gradient; only where
    // the lens, the glow or the rim needs it.
    vec2 n = vec2(0.0);
    if (within < LENS) {
        n = vec2(
            edge_distance(p + vec2(0.5, 0.0), clip, rounding.x)
                - edge_distance(p - vec2(0.5, 0.0), clip, rounding.x),
            edge_distance(p + vec2(0.0, 0.5), clip, rounding.x)
                - edge_distance(p - vec2(0.0, 0.5), clip, rounding.x));
        n /= max(length(n), 1e-4);
    }
    float bend = clamp(1.0 - within / LENS, 0.0, 1.0);
    vec2 at = uv * texel + n * (bend * bend * LENS_SHIFT) * bottom.xy;
    vec3 c = textureLod(sampler2D(textures[nonuniformEXT(texture)], linear), at, 0.0).rgb;
    float down = (p.y - clip.y) / max(clip.w, 1.0);
    c = mix(c, vec3(1.0), clamp(1.0 - down / SHEEN_DOWN_TO, 0.0, 1.0) * SHEEN);
    float facing = dot(n, CLEAR_LIGHT);
    float toward = max(facing, 0.0) + 0.45 * max(-facing, 0.0);
    c = mix(c, vec3(1.0), clamp(1.0 - within / GLOW_WIDTH, 0.0, 1.0) * shown * toward * GLOW);
    // The rim in `color` (premultiplied), a ring RIM_WIDTH px inside.
    float ring = (shown - clamp(0.5 - (d + RIM_WIDTH), 0.0, 1.0)) * toward;
    c = c * (1.0 - color.a * ring) + color.rgb * ring;
    return vec4(c, 1.0) * shown;
}

void main() {
    if ((flags & SHADOW) != 0u) {
        result = color * shadow(gl_FragCoord.xy, clip, rounding.x, rounding.y);
        return;
    }
    if ((flags & CLEAR) != 0u) {
        result = clear_glass();
        return;
    }
    if ((flags & GLASS) != 0u) {
        vec4 sampled = textureLod(sampler2D(textures[nonuniformEXT(texture)], linear),
                                  uv * texel, 0.0);
        result = glass(sampled) * inside(gl_FragCoord.xy, clip, rounding.x);
        return;
    }
    if ((flags & (STROKE | TRIANGLE)) != 0u) {
        float d = (flags & TRIANGLE) != 0u
            ? triangle_distance(uv, bottom.xy, bottom.zw, texel)
            : stroke_distance(uv, bottom.xy, bottom.zw, rounding.x, rounding.y,
                              (flags & ROUND) != 0u);
        result = color * clamp(0.5 - d, 0.0, 1.0);
        return;
    }
    // Round corners (rounding.x > 0): only what lies inside the window.
    float shown = rounding.x > 0.0 ? inside(gl_FragCoord.xy, clip, rounding.x) : 1.0;
    if ((flags & SOLID) != 0u) {
        result = ((flags & GRADIENT) != 0u ? gradient(gl_FragCoord.y) : color) * shown;
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
    if ((flags & ENCODE) != 0u) {
        // Premultiplied linear to premultiplied as encoded (`ENCODE`).
        vec3 c = sampled.a > 0.0 ? clamp(sampled.rgb / sampled.a, 0.0, 1.0) : vec3(0.0);
        result = vec4(encoded(c.r), encoded(c.g), encoded(c.b), 1.0) * sampled.a;
        return;
    }
    if ((flags & MASK) != 0u) {
        // A glyph: the texture's red is its coverage, the colour is the
        // text's (premultiplied).
        result = color * coverage(sampled.r, color) * shown;
    } else {
        result = sampled * (color.a * shown);
    }
}
