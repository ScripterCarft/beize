//! What a window shows, as quads for beize-vulkan's pipelines. Windows
//! draw into sRGB images, so the GPU blends in linear light (ADR 0027 §9,
//! gamma-correct): colours are given in sRGB, as designers name them, and
//! turned linear and premultiplied here.

use beize_vulkan::pipeline::{Quad, SOLID};

/// A colour in sRGB, 8 bits a channel, alpha straight (not premultiplied).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Color {
    /// An opaque colour from its hex code, `0xRRGGBB`.
    pub const fn rgb(hex: u32) -> Color {
        Color {
            r: (hex >> 16) as u8,
            g: (hex >> 8) as u8,
            b: hex as u8,
            a: 0xff,
        }
    }

    /// Linear and premultiplied, as the shaders blend.
    pub fn linear(self) -> [f32; 4] {
        let a = f32::from(self.a) / 255.0;
        [
            decode(self.r) * a,
            decode(self.g) * a,
            decode(self.b) * a,
            a,
        ]
    }
}

/// sRGB's transfer function, inverted (IEC 61966-2-1).
fn decode(channel: u8) -> f32 {
    let c = f32::from(channel) / 255.0;
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// A rectangle in the window's pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

/// The quads of one frame, back to front.
#[derive(Default)]
pub struct Scene {
    pub(crate) quads: Vec<Quad>,
    /// Per quad: drawn with blending.
    pub(crate) blend: Vec<bool>,
}

impl Scene {
    pub fn new() -> Scene {
        Scene::default()
    }

    /// `rect` in `color`; blended only where the colour is translucent.
    pub fn fill(&mut self, rect: Rect, color: Color) {
        self.quads.push(Quad {
            pos: [rect.x, rect.y],
            size: [rect.w, rect.h],
            color: color.linear(),
            flags: SOLID,
            ..Quad::default()
        });
        self.blend.push(color.a < 0xff);
    }

    pub fn len(&self) -> usize {
        self.quads.len()
    }

    pub fn is_empty(&self) -> bool {
        self.quads.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn srgb_decodes_as_the_standard_says() {
        assert_eq!(decode(0), 0.0);
        assert_eq!(decode(255), 1.0);
        // The linear part ends at 10/255; 0.5 in sRGB is 21.4 % of the light.
        assert!((decode(10) - 10.0 / 255.0 / 12.92).abs() < 1e-7);
        assert!((decode(128) - 0.2158605).abs() < 1e-6);
    }

    #[test]
    fn colours_are_premultiplied_in_linear_light() {
        assert_eq!(Color::rgb(0xffffff).linear(), [1.0, 1.0, 1.0, 1.0]);
        let half = Color {
            a: 0x80,
            ..Color::rgb(0xff0000)
        };
        let [r, g, b, a] = half.linear();
        assert!((a - 128.0 / 255.0).abs() < 1e-7);
        assert_eq!((r, g, b), (a, 0.0, 0.0));
    }

    #[test]
    fn only_translucent_fills_blend() {
        let mut scene = Scene::new();
        let rect = Rect {
            x: 0.0,
            y: 0.0,
            w: 10.0,
            h: 10.0,
        };
        scene.fill(rect, Color::rgb(0x202020));
        scene.fill(
            rect,
            Color {
                a: 0x40,
                ..Color::rgb(0x202020)
            },
        );
        assert_eq!(scene.blend, [false, true]);
        assert_eq!(scene.quads[0].flags, SOLID);
    }
}
