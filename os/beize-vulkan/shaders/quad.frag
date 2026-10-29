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

// Liquid glass (`CLEAR`: the control center's tiles and the dock; ADR 0028
// §6): clear glass, a breath of milk, a curved edge that bends and catches
// light. Measured on macOS 26's dock on a Mac's screen (2 px per pt, the
// owner's, 29.10.), Apple's glass buttons over a white window (macOS 27
// press image) and its control center (macOS 26 press image), in linear
// light; here 1.18 px per pt, the light a little below Apple's (the owner:
// subtle). The glass texture is what lies behind, frosted (a Gaussian at
// half size, `backdrop.frag`). Per pixel:
// - a rounded edge: over BEVEL px the surface curves down to the rim, its
//   normal tilting out; there the glass shows what lies further out along
//   the normal (refraction);
// - one tone for the whole shape from the mean behind it (TONE_AT, nine
//   reads of the frosted texture, the same for every pixel of the shape):
//   with white ink on it (`bottom.z` 1) unchanged over dark, eased towards
//   CAP over light so the ink keeps about 3.4:1 (Apple's, measured), its
//   colour deeper rather than grey where it is pulled down; without ink
//   (the dock) only the appearance's;
// - chroma times SATURATION (Apple's tiles x1.1 to 1.3 of what is around);
// - milk: MILK of the way towards a light grey VEIL – dark behind comes out
//   lighter (Apple's dock: 47 -> 83), midtones barely (172 -> 175), white a
//   touch grey (its buttons: 245 of 253); no lift towards white (0.5.119's
//   made the dock vanish over a white window);
// - the outermost pixel darker at the sides, as a shadow (Apple's dock: 47
//   -> 12 over dark, 172 -> 100 over light, none at the top and bottom), and
//   all around the lighter what lies just outside it is (Apple's over white:
//   240 between 252 outside and 242 inside);
// - a line of light one pixel in, at the top and the bottom, none at the
//   sides, and the light gathered inside the top and the bottom edge,
//   fading inwards within a few pt (Apple's dock over dark: 68 outside, 175
//   on the line, 140, 113, 106 … back to the glass's 96).
// `bottom.xy` is the glass texture's step per pixel (normalised), `bottom.w`
// 1 in the dark appearance.
const float BEVEL = 5.0;
const float REFRACT = 0.9;
const float SATURATION = 1.25;
const float KNEE = 0.22;
const float CAP = 0.27;
const float EASE = 0.15;
const float DEEPEN = 0.6;
const float DEEP_MOST = 1.3;
// Dark: the tone times DARK_TONE, the light added times DARK_LIGHT.
const float DARK_TONE = 0.72;
const float DARK_LIGHT = 0.30;
// The milk, and its grey in either appearance.
const float MILK = 0.10;
const float VEIL = 0.60;
const float VEIL_DARK = 0.25;
// The outermost pixel: darker by SIDE at the sides (by the normal's x to
// the fourth: the corners turn from light to dark), and by EDGE over a
// white outside, from nothing below EDGE_FROM to all of it at EDGE_TO
// (relative luminance), read EDGE_OUTSIDE px out.
const float SIDE = 0.35;
const float EDGE = 0.10;
const float EDGE_FROM = 0.45;
const float EDGE_TO = 0.9;
const float EDGE_OUTSIDE = 2.0;
// The line of light: where it lies inside the edge and how wide, the
// light's direction (outward normal up, a little left) and its reflection's
// (down), what each adds and how fast it falls off around them.
const float LINE_AT = 1.3;
const float LINE_WIDTH = 0.6;
const vec2 LINE_KEY = vec2(-0.17365, -0.98481);     // -100 degrees
const vec2 LINE_BACK = vec2(0.17365, 0.98481);      // 80 degrees
const float LINE_KEY_LIGHT = 0.4;
const float LINE_BACK_LIGHT = 0.35;
const float LINE_FALL = 2.0;
// Where the outermost pixel and the line reach, in px from the edge.
const float LINE_REACH = 3.0;
// The light gathered inside the top and the bottom edge, fading over
// GLOW_WIDTH px.
const float GLOW_TOP = 0.12;
const float GLOW_BOTTOM = 0.06;
const float GLOW_WIDTH = 2.5;
// How far in the edge's direction is needed: the curve and the glow.
const float EDGE_REACH = 12.0;
// Where the tone is read: three by three across the shape.
const float TONE_AT[3] = float[](0.2, 0.5, 0.8);

vec3 decoded3(vec3 c) {
    return vec3(decoded(c.r), decoded(c.g), decoded(c.b));
}

float relative_luminance(vec3 c) {
    return dot(c, vec3(0.2126, 0.7152, 0.0722));
}

// The frosted texture, linear, at the screen's pixel `q` (the fragment at
// `p` reads `at`).
vec3 frosted(vec2 at, vec2 p, vec2 q) {
    vec2 there = at + (q - p) * bottom.xy;
    return decoded3(textureLod(sampler2D(textures[nonuniformEXT(texture)], linear), there, 0.0).rgb);
}

vec4 clear_glass() {
    vec2 p = gl_FragCoord.xy;
    float d = edge_distance(p, clip, rounding.x);
    float shown = clamp(0.5 - d, 0.0, 1.0);
    float within = max(-d, 0.0);
    bool ink = bottom.z > 0.5;
    bool dark = bottom.w > 0.5;
    // The edge's direction, outwards: the distance's gradient; only where
    // the curve or the light reach.
    vec2 n = vec2(0.0);
    if (within < EDGE_REACH) {
        n = vec2(
            edge_distance(p + vec2(0.5, 0.0), clip, rounding.x)
                - edge_distance(p - vec2(0.5, 0.0), clip, rounding.x),
            edge_distance(p + vec2(0.0, 0.5), clip, rounding.x)
                - edge_distance(p - vec2(0.0, 0.5), clip, rounding.x));
        n /= max(length(n), 1e-4);
    }
    float tilt = pow(clamp(1.0 - within / BEVEL, 0.0, 1.0), 1.3);
    vec2 at = uv * texel;
    vec3 c = frosted(at, p, p + n * (tilt * BEVEL * REFRACT));

    // The tone: the same for all of the shape.
    float behind = 0.0;
    for (int i = 0; i < 3; i++) {
        for (int j = 0; j < 3; j++) {
            vec2 q = clip.xy + clip.zw * vec2(TONE_AT[i], TONE_AT[j]);
            behind += relative_luminance(frosted(at, p, q));
        }
    }
    behind = max(behind / 9.0, 1e-4);
    float target = behind;
    if (ink && behind > KNEE) {
        target = KNEE + (CAP - KNEE) * (1.0 - exp(-(behind - KNEE) / EASE));
    }
    float pulled = target / behind;
    float gain = pulled * (dark ? DARK_TONE : 1.0);
    float deeper = pulled < 1.0 ? min(pow(pulled, -DEEPEN), DEEP_MOST) : 1.0;
    vec3 lab = oklab(c * gain);
    lab.yz *= SATURATION * deeper;
    c = max(linear_rgb(lab), 0.0);
    c = mix(c, vec3(dark ? VEIL_DARK : VEIL), MILK);

    // The edge: the light gathered inside it, the outermost pixel darker,
    // the line of light.
    if (within < EDGE_REACH) {
        float up = max(dot(n, LINE_KEY), 0.0);
        float down = max(dot(n, LINE_BACK), 0.0);
        float light = dark ? DARK_LIGHT : 1.0;
        float gathered = (GLOW_TOP * up * up + GLOW_BOTTOM * down * down)
            * exp(-within / GLOW_WIDTH);
        c = min(c, 1.0);
        c += gathered * light * (1.0 - c);
        if (within < LINE_REACH) {
            vec3 outside = frosted(at, p, p + n * EDGE_OUTSIDE);
            float side = n.x * n.x;
            float outermost = clamp(1.0 - within, 0.0, 1.0) * shown;
            // At the sides darker than the darker of the glass and what lies
            // just outside it: over dark the milky glass is lighter than
            // what is around it, and darkened alone it would stay above it
            // (Apple's dock: 47 outside, 12 on the edge, 83 inside).
            vec3 darker = relative_luminance(outside) < relative_luminance(c) ? outside : c;
            c = mix(c, darker, side * side * outermost);
            c *= 1.0 - (SIDE * side * side
                + EDGE * smoothstep(EDGE_FROM, EDGE_TO, relative_luminance(outside)))
                * outermost;
            // At the top and the bottom the line lies on the lighter of the
            // glass and what lies just outside it: an edge catching the
            // light is never darker than either side – on glass pulled down
            // for its ink it would come out between the two, a grey stroke
            // (Apple's dock over light: 142 outside, 225 on the line).
            float line = exp(-pow((within - LINE_AT) / LINE_WIDTH, 2.0));
            float facing = min(pow(up, LINE_FALL) + pow(down, LINE_FALL), 1.0);
            vec3 lighter = relative_luminance(outside) > relative_luminance(c) ? outside : c;
            vec3 under = mix(c, lighter, line * facing);
            float lit = LINE_KEY_LIGHT * pow(up, LINE_FALL)
                + LINE_BACK_LIGHT * pow(down, LINE_FALL);
            c = under + line * lit * light * (1.0 - under);
        }
    }
    c = clamp(c, 0.0, 1.0);
    return vec4(encoded(c.r), encoded(c.g), encoded(c.b), 1.0) * shown;
}

void main() {
    if ((flags & SHADOW) != 0u) {
        // Never under the shape it falls from (`bottom`): drawn in front
        // of glass, outside it only.
        result = color * shadow(gl_FragCoord.xy, clip, rounding.x, rounding.y)
            * (1.0 - inside(gl_FragCoord.xy, bottom, rounding.x));
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
