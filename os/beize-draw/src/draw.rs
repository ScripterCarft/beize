//! What a window shows, as quads for beize-vulkan's pipelines. Windows
//! draw into sRGB images, so the GPU blends in linear light (ADR 0027 §9,
//! gamma-correct): colours are given in sRGB, as designers name them, and
//! turned linear and premultiplied here.
//!
//! A scene is data only: text adds its glyph runs, and `quads` turns each
//! glyph into a quad when it is drawn (`glyphs`, the atlas).

use beize_vulkan::pipeline::{EXACT, MASK, Quad, SOLID};
use parley::{FontData, Layout, PositionedLayoutItem, Run};
use tracing::debug;

use crate::glyphs::{ATLAS_SIZE, Face, Full, Glyphs};
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
    quads: Vec<Quad>,
    /// Per quad: drawn with blending.
    blend: Vec<bool>,
    /// The fonts of the glyph runs.
    runs: Vec<(FontData, Face)>,
    /// Glyphs whose quads `quads` fills in.
    glyphs: Vec<Glyph>,
    /// Symbols, likewise.
    symbols: Vec<Symbol>,
}

/// A symbol (ADR 0028 §8) in a square of `size` pixels, its top left at
/// `x`, `y` (whole pixels).
struct Symbol {
    pub quad: usize,
    pub name: String,
    pub size: u32,
    pub x: f32,
    pub y: f32,
}

/// A glyph of a run, at its pen position: `x` in pixels with fraction,
/// `y` the baseline on a whole pixel.
struct Glyph {
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

    /// The symbol `name` (Phosphor's, ADR 0028 §8) in `color`, a square of
    /// `size` pixels with its top left at `x`, `y`, on whole pixels.
    pub fn symbol(&mut self, name: &str, x: f32, y: f32, size: u32, color: Color) {
        self.symbols.push(Symbol {
            quad: self.quads.len(),
            name: name.to_owned(),
            size,
            x: x.round(),
            y: y.round(),
        });
        self.quads.push(Quad {
            color: color.linear(),
            flags: MASK | EXACT,
            ..Quad::default()
        });
        self.blend.push(true);
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

    /// `other`'s quads after (above) this scene's, moved down by `dy`, a
    /// whole number of pixels: baselines stay on whole pixels.
    pub fn append(&mut self, other: &Scene, dy: f32) {
        let (quads, runs) = (self.quads.len(), self.runs.len());
        self.quads.extend(other.quads.iter().map(|q| Quad {
            pos: [q.pos[0], q.pos[1] + dy],
            ..*q
        }));
        self.blend.extend_from_slice(&other.blend);
        self.runs.extend(other.runs.iter().cloned());
        self.glyphs.extend(other.glyphs.iter().map(|g| Glyph {
            quad: g.quad + quads,
            run: g.run + runs,
            id: g.id,
            x: g.x,
            y: g.y + dy,
        }));
        self.symbols.extend(other.symbols.iter().map(|s| Symbol {
            quad: s.quad + quads,
            name: s.name.clone(),
            size: s.size,
            x: s.x,
            y: s.y + dy,
        }));
    }

    pub fn len(&self) -> usize {
        self.quads.len()
    }

    /// Per quad: drawn with blending.
    pub fn blend(&self) -> &[bool] {
        &self.blend
    }

    /// Glyphs and symbols to place: the scene needs the atlas.
    pub fn has_glyphs(&self) -> bool {
        !self.glyphs.is_empty() || !self.symbols.is_empty()
    }

    /// The scene's quads with its glyphs and symbols placed in `glyphs`'
    /// atlas – rasterized where new – which is `slot` in the texture array.
    /// A full atlas starts over once; None if the scene's glyphs do not fit
    /// even then.
    pub fn quads(&self, glyphs: &mut Glyphs, slot: u32) -> Option<Vec<Quad>> {
        let mut quads = self.quads.clone();
        if !self.has_glyphs() {
            return Some(quads);
        }
        for attempt in 0..2 {
            match self.place(glyphs, &mut quads, slot) {
                Ok(()) => return Some(quads),
                Err(Full) if attempt == 0 => {
                    debug!("the glyph atlas is full: starting over");
                    glyphs.clear();
                }
                Err(Full) => break,
            }
        }
        None
    }

    fn place(&self, glyphs: &mut Glyphs, quads: &mut [Quad], slot: u32) -> Result<(), Full> {
        let texel = 1.0 / ATLAS_SIZE as f32;
        for glyph in &self.glyphs {
            let (font, face) = &self.runs[glyph.run];
            let (placed, whole) = glyphs.get(font, face, glyph.id, glyph.x)?;
            let quad = &mut quads[glyph.quad];
            let (w, h) = (placed.w as f32, placed.h as f32);
            quad.pos = [(whole + placed.left) as f32, glyph.y + placed.top as f32];
            // Without ink: nothing covered.
            quad.size = [w, h];
            quad.uv = [placed.x as f32, placed.y as f32];
            quad.uv_x = [w, 0.0];
            quad.uv_y = [0.0, h];
            quad.texel = [texel, texel];
            quad.texture = slot;
        }
        for symbol in &self.symbols {
            let placed = glyphs.symbol(&symbol.name, symbol.size)?;
            let quad = &mut quads[symbol.quad];
            let (w, h) = (placed.w as f32, placed.h as f32);
            quad.pos = [symbol.x, symbol.y];
            quad.size = [w, h];
            quad.uv = [placed.x as f32, placed.y as f32];
            quad.uv_x = [w, 0.0];
            quad.uv_y = [0.0, h];
            quad.texel = [texel, texel];
            quad.texture = slot;
        }
        Ok(())
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
    fn an_appended_scene_moves_down_with_its_glyphs() {
        let rect = Rect {
            x: 1.0,
            y: 2.0,
            w: 3.0,
            h: 4.0,
        };
        let mut below = Scene::new();
        below.fill(rect, Color::rgb(0x202020));
        below.symbol("x", 5.0, 6.0, 16, Color::rgb(0xffffff));
        let mut scene = Scene::new();
        scene.fill(rect, Color::rgb(0x101010));
        scene.append(&below, 32.0);
        assert_eq!(scene.len(), 3);
        assert_eq!(scene.quads[1].pos, [1.0, 34.0]);
        assert_eq!(scene.blend, [false, false, true]);
        let symbol = &scene.symbols[0];
        assert_eq!((symbol.quad, symbol.x, symbol.y), (2, 5.0, 38.0));
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
