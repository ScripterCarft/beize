// The quads of a frame (src/pipeline.rs, `Quad`): one instance
// each, read through their buffer's device address (push constant).

#extension GL_EXT_buffer_reference : require
#extension GL_EXT_scalar_block_layout : require

struct Quad {
    vec2 pos;
    vec2 size;
    vec2 uv;
    vec2 uv_x;
    vec2 uv_y;
    vec2 texel;
    vec4 color;
    uint texture;
    uint flags;
    vec4 clip;
    float radius;
    float sigma;
    vec4 bottom;
};

const uint SOLID = 1u;
const uint EXACT = 2u;
const uint MASK = 4u;
const uint SHADOW = 8u;
const uint GRADIENT = 16u;
const uint GLASS = 32u;
// The backdrop's passes (backdrop.frag).
const uint WALLPAPER = 64u;
const uint BLUR_DOWN = 128u;
const uint BLUR_UP = 256u;
const uint MATERIAL = 512u;

layout(buffer_reference, scalar, buffer_reference_align = 4) readonly buffer Quads {
    Quad quads[];
};

layout(push_constant, scalar) uniform Push {
    Quads quads;
    // 2 / the framebuffer's size: pixels to clip space.
    vec2 scale;
} push;
