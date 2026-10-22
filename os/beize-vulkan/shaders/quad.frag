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
        result = color * sampled.r;
    } else {
        result = sampled * color.a;
    }
}
