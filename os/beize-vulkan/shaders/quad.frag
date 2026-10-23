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

void main() {
    if ((flags & SOLID) != 0u) {
        result = color;
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
        result = color * coverage(sampled.r, color);
    } else {
        result = sampled * color.a;
    }
}
