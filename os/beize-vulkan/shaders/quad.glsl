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
const uint ENCODE = 1024u;
const uint STROKE = 2048u;
const uint ROUND = 4096u;
const uint TRIANGLE = 8192u;
const uint GAUSS_ACROSS = 16384u;
const uint GAUSS_DOWN = 32768u;
const uint CLEAR = 65536u;

// Oklab (Björn Ottosson), from and to linear light: mixed there, colours
// keep their lightness and hue instead of turning muddy between two; its
// chroma scaled, a colour gets deeper or paler at the same lightness.
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

layout(buffer_reference, scalar, buffer_reference_align = 4) readonly buffer Quads {
    Quad quads[];
};

layout(push_constant, scalar) uniform Push {
    Quads quads;
    // 2 / the framebuffer's size: pixels to clip space.
    vec2 scale;
} push;
