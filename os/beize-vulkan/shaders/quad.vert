#version 460
// A quad as a strip of four vertices, its corner from gl_VertexIndex. What
// the fragments need goes out flat, so no fragment reads the buffer.

#extension GL_GOOGLE_include_directive : require
#extension GL_EXT_nonuniform_qualifier : require
#include "quad.glsl"

layout(set = 0, binding = 0) uniform texture2D textures[];
layout(set = 0, binding = 1) uniform sampler linear;

layout(location = 0) out vec2 uv;
layout(location = 1) flat out vec4 color;
layout(location = 2) flat out vec2 texel;
layout(location = 3) flat out uint texture;
layout(location = 4) flat out uint flags;
layout(location = 5) flat out vec4 clip;
layout(location = 6) flat out vec2 rounding;
layout(location = 7) flat out vec4 bottom;
// Liquid glass's tone (`liquid_tone`): what it scales by, its chroma's.
layout(location = 8) flat out vec2 tone;

float decoded(float encoded) {
    return encoded <= 0.04045 ? encoded / 12.92 : pow((encoded + 0.055) / 1.055, 2.4);
}

// Liquid glass's one tone for the whole shape, from the mean behind it (its
// parts in `quad.frag`, `clear_glass`): read here, once a quad, not for every
// pixel. With white ink on it (`bottom.z` 1) unchanged over dark, eased
// towards CAP over light so the ink keeps about 3.4:1 (Apple's, measured),
// its colour deeper rather than grey where it is pulled down; without ink
// (the dock) only the appearance's. Nine reads of the frosted texture
// spelled out: NVK has misread a table indexed in a loop (quad.frag, EXTENT).
const float KNEE = 0.22;
const float CAP = 0.27;
const float EASE = 0.15;
const float DEEPEN = 0.6;
const float DEEP_MOST = 1.3;
// Dark: the tone times DARK_TONE.
const float DARK_TONE = 0.72;

// The relative luminance (linear light) of the frost at `f` across the shape.
float behind_at(Quad q, vec2 f) {
    vec2 at = q.uv * q.texel + (q.clip.xy + q.clip.zw * f - q.pos) * q.bottom.xy;
    vec3 c = textureLod(sampler2D(textures[nonuniformEXT(q.texture)], linear), at, 0.0).rgb;
    return dot(vec3(decoded(c.r), decoded(c.g), decoded(c.b)), vec3(0.2126, 0.7152, 0.0722));
}

vec2 liquid_tone(Quad q) {
    float behind = (behind_at(q, vec2(0.2, 0.2)) + behind_at(q, vec2(0.2, 0.5))
        + behind_at(q, vec2(0.2, 0.8)) + behind_at(q, vec2(0.5, 0.2))
        + behind_at(q, vec2(0.5, 0.5)) + behind_at(q, vec2(0.5, 0.8))
        + behind_at(q, vec2(0.8, 0.2)) + behind_at(q, vec2(0.8, 0.5))
        + behind_at(q, vec2(0.8, 0.8))) / 9.0;
    behind = max(behind, 1e-4);
    float target = behind;
    if (q.bottom.z > 0.5 && behind > KNEE) {
        target = KNEE + (CAP - KNEE) * (1.0 - exp(-(behind - KNEE) / EASE));
    }
    float pulled = target / behind;
    float gain = pulled * (q.bottom.w > 0.5 ? DARK_TONE : 1.0);
    float deeper = pulled < 1.0 ? min(pow(pulled, -DEEPEN), DEEP_MOST) : 1.0;
    return vec2(gain, deeper);
}

void main() {
    Quad q = push.quads.quads[gl_InstanceIndex];
    vec2 corner = vec2(gl_VertexIndex & 1, gl_VertexIndex >> 1);
    gl_Position = vec4((q.pos + corner * q.size) * push.scale - 1.0, 0.0, 1.0);
    uv = q.uv + corner.x * q.uv_x + corner.y * q.uv_y;
    color = q.color;
    texel = q.texel;
    texture = q.texture;
    flags = q.flags;
    clip = q.clip;
    rounding = vec2(q.radius, q.sigma);
    bottom = q.bottom;
    tone = (q.flags & CLEAR) != 0u ? liquid_tone(q) : vec2(1.0);
}
