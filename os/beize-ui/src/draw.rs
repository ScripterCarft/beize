//! What a window shows, as quads for beize-vulkan's pipelines. Windows
//! draw into sRGB images, so the GPU blends in linear light (ADR 0027 §9,
//! gamma-correct): colours are given in sRGB, as designers name them, and
//! turned linear and premultiplied here.
//!
//! A scene is data only: text adds its glyph runs, and the renderer turns
//! each glyph into a quad when it draws (`glyphs`, the atlas).

use beize_vulkan::pipeline::{EXACT, MASK, Quad, SOLID};
use parley::{FontData, Layout, PositionedLayoutItem, Run};

use crate::glyphs::Face;
use crate::text::Cells;

/// The face a run of Parley's is set in.
pub(crate) fn face(run: &Run<'_, Color>) -> Face {
    Face {
        blob: run.font().data.id(),
        index: run.font().index,
        size: run.font_size().to_bits(),
        coords: run
            .normalized_coords()
            .iter()
            .map(|c| c.to_bits())
            .collect(),
    }
}

/// A colour in sRGB, 8 bits a channel, alpha straight (not premultiplied).
/// The default is transparent.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
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
    /// The fonts of the glyph runs.
    pub(crate) runs: Vec<(FontData, Face)>,
    /// Glyphs whose quads the renderer fills in.
    pub(crate) glyphs: Vec<Glyph>,
}

/// A glyph of a run, at its pen position: `x` in pixels with fraction,
/// `y` the baseline on a whole pixel.
pub(crate) struct Glyph {
    pub quad: usize,
    pub run: usize,
    pub id: u32,
    pub x: f32,
    pub y: f32,
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

    /// The text of `layout` with its top left at `x`, `y`.
    pub fn text(&mut self, layout: &Layout<Color>, x: f32, y: f32) {
        for line in layout.lines() {
            for item in line.items() {
                let PositionedLayoutItem::GlyphRun(glyphs) = item else {
                    continue;
                };
                let run = glyphs.run();
                let index = self.run(run.font(), face(run));
                let color = glyphs.style().brush;
                for glyph in glyphs.positioned_glyphs() {
                    self.glyph(index, glyph.id, x + glyph.x, y + glyph.y, color);
                }
            }
        }
    }

    /// The glyph `id` of a grid's font (a terminal's cell) with its pen at
    /// `x` and its baseline at `y`.
    pub fn cell_glyph(&mut self, cells: &Cells, id: u32, x: f32, y: f32, color: Color) {
        let index = self.run(&cells.font, cells.face.clone());
        self.glyph(index, id, x, y, color);
    }

    /// The index of the run of `font` in `face`, new if it is.
    fn run(&mut self, font: &FontData, face: Face) -> usize {
        match self.runs.iter().position(|(_, f)| *f == face) {
            Some(i) => i,
            None => {
                self.runs.push((font.clone(), face));
                self.runs.len() - 1
            }
        }
    }

    fn glyph(&mut self, run: usize, id: u32, x: f32, y: f32, color: Color) {
        self.glyphs.push(Glyph {
            quad: self.quads.len(),
            run,
            id,
            x,
            // Baselines on whole pixels, where hinting put the stems.
            y: y.round(),
        });
        self.quads.push(Quad {
            color: color.linear(),
            flags: MASK | EXACT,
            ..Quad::default()
        });
        self.blend.push(true);
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
