#version 460
// A quad as a strip of four vertices, its corner from gl_VertexIndex. What
// the fragments need goes out flat, so no fragment reads the buffer.

#extension GL_GOOGLE_include_directive : require
#include "quad.glsl"

layout(location = 0) out vec2 uv;
layout(location = 1) flat out vec4 color;
layout(location = 2) flat out vec2 texel;
layout(location = 3) flat out uint texture;
layout(location = 4) flat out uint flags;
layout(location = 5) flat out vec4 clip;
layout(location = 6) flat out vec2 rounding;
layout(location = 7) flat out vec4 bottom;

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
}
