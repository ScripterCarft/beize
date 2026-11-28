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
// A row of a mesh (`STRIP`, the genie): a strip across it, two vertices per
// column edge – top, bottom – between its top and its bottom line, the
// texels along each line spread as evenly as the points.
void band(Quad q) {
    float t = float(gl_VertexIndex >> 1) / max(q.radius, 1.0);
    float lower = float(gl_VertexIndex & 1);
    vec2 top = vec2(q.pos.x + t * q.size.x, q.pos.y);
    vec2 low = vec2(q.bottom.x + t * q.bottom.y, q.pos.y + q.size.y);
    gl_Position = vec4(mix(top, low, lower) * push.scale - 1.0, 0.0, 1.0);
    uv = mix(vec2(mix(q.uv.x, q.uv_x.x, t), q.uv.y),
             vec2(mix(q.bottom.z, q.bottom.w, t), q.uv_x.y), lower);
    color = q.color;
    texel = q.texel;
    texture = q.texture;
    flags = q.flags;
    clip = q.clip;
    rounding = vec2(0.0);
    bottom = vec4(0.0);
}

void main() {
    Quad q = push.quads.quads[gl_InstanceIndex];
    if ((q.flags & STRIP) != 0u) {
        band(q);
        return;
    }
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
}
