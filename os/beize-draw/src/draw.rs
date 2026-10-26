//! What a window shows, as quads for beize-vulkan's pipelines. Windows
//! draw into sRGB images, so the GPU blends in linear light (ADR 0027 §9,
//! gamma-correct): colours are given in sRGB, as designers name them, and
//! turned linear and premultiplied here.
//!
//! A scene is data only: text adds its glyph runs, and `quads` turns each
//! glyph into a quad when it is drawn (`glyphs`, the atlas).

use beize_vulkan::pipeline::{EXACT, GRADIENT, MASK, Quad, ROUND, SOLID, STROKE, TRIANGLE};
use parley::{FontData, Layout, PositionedLayoutItem, Run};
use tracing::debug;

#[cfg(feature = "icons")]
use std::path::{Path, PathBuf};

#[cfg(feature = "icons")]
use crate::glyphs::COLOUR_ATLAS_SIZE;
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
pub(crate) fn decode(channel: u8) -> f32 {
    let c = f32::from(channel) / 255.0;
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// How a stroke ends (`Scene::stroke`), as SVG's line caps.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cap {
    /// At its end.
    Butt,
    /// Half its width past its end, square.
    Square,
    /// In a half circle.
    Round,
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
    #[cfg(feature = "icons")]
    symbols: Vec<Symbol>,
    /// Images in colour, likewise.
    #[cfg(feature = "icons")]
    images: Vec<Image>,
}

/// Where the atlases lie in the texture array.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Atlases {
    pub coverage: u32,
    /// None where nothing draws images (the compositor's title bars, a
    /// scene without them): an image then draws nothing. There without
    /// `icons` too, so the feature only adds.
    pub colour: Option<u32>,
}

/// An image file (`Scene::image`) in a square of `size` pixels, its top
/// left at `x`, `y` (whole pixels).
#[cfg(feature = "icons")]
struct Image {
    pub quad: usize,
    pub path: PathBuf,
    pub size: u32,
    pub x: f32,
    pub y: f32,
}

/// A symbol (ADR 0028 §8) in a square of `size` pixels, its top left at
/// `x`, `y` (whole pixels).
#[cfg(feature = "icons")]
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

    /// `rect` transparent, whatever was there: where a translucent panel
    /// shows what is below. Not blended – blending nothing over an image's
    /// undefined start would leave it undefined.
    pub fn clear(&mut self, rect: Rect) {
        self.quads.push(Quad {
            pos: [rect.x, rect.y],
            size: [rect.w, rect.h],
            flags: SOLID,
            ..Quad::default()
        });
        self.blend.push(false);
    }

    /// `rect` with corners of `radius` in `color`, its edge antialiased –
    /// so always blended.
    pub fn rounded(&mut self, rect: Rect, radius: f32, color: Color) {
        self.quads.push(Quad {
            pos: [rect.x, rect.y],
            size: [rect.w, rect.h],
            color: color.linear(),
            flags: SOLID,
            clip: [rect.x, rect.y, rect.w, rect.h],
            radius,
            ..Quad::default()
        });
        self.blend.push(true);
    }

    /// `rect` with corners of `radius`, from `top` at its top to `bottom`
    /// at its bottom; always blended, as `rounded`.
    pub fn gradient(&mut self, rect: Rect, radius: f32, top: Color, bottom: Color) {
        self.quads.push(Quad {
            pos: [rect.x, rect.y],
            size: [rect.w, rect.h],
            color: top.linear(),
            bottom: bottom.linear(),
            flags: SOLID | GRADIENT,
            clip: [rect.x, rect.y, rect.w, rect.h],
            radius,
            ..Quad::default()
        });
        self.blend.push(true);
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
    #[cfg(feature = "icons")]
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

    /// A stroke from `a` to `b`, `width` wide, its ends `cap`, in `color`:
    /// the window buttons' marks, drawn for their size as AppKit and
    /// Windows draw theirs – computed per pixel, sharp at any size, no atlas.
    pub fn stroke(&mut self, a: [f32; 2], b: [f32; 2], width: f32, cap: Cap, color: Color) {
        let half = width / 2.0;
        let (flags, past) = match cap {
            Cap::Butt => (STROKE, 0.0),
            Cap::Square => (STROKE, half),
            Cap::Round => (STROKE | ROUND, 0.0),
        };
        let mut quad = self.shape_quad(&[a, b], half + past, color);
        let [x, y] = quad.pos;
        quad.flags = flags;
        quad.bottom = [a[0] - x, a[1] - y, b[0] - x, b[1] - y];
        quad.radius = half;
        quad.sigma = past;
        self.quads.push(quad);
        self.blend.push(true);
    }

    /// A disc of `radius` around `centre`, in `color`: a rounded rectangle
    /// as round as it can be, its quad on whole pixels around it, so the
    /// antialiased edge is drawn where the disc does not start on one.
    pub fn circle(&mut self, centre: [f32; 2], radius: f32, color: Color) {
        let mut quad = self.shape_quad(&[centre], radius, color);
        quad.flags = SOLID;
        quad.clip = [
            centre[0] - radius,
            centre[1] - radius,
            2.0 * radius,
            2.0 * radius,
        ];
        quad.radius = radius;
        self.quads.push(quad);
        self.blend.push(true);
    }

    /// The triangle with `corners`, in `color`, as `stroke` draws.
    pub fn triangle(&mut self, corners: [[f32; 2]; 3], color: Color) {
        let mut quad = self.shape_quad(&corners, 0.0, color);
        let [x, y] = quad.pos;
        let [a, b, c] = corners;
        quad.flags = TRIANGLE;
        quad.bottom = [a[0] - x, a[1] - y, b[0] - x, b[1] - y];
        quad.texel = [c[0] - x, c[1] - y];
        self.quads.push(quad);
        self.blend.push(true);
    }

    /// The quad around `points`, `reach` beyond them and a pixel more for
    /// the antialiased edge, on whole pixels; `uv` runs over its pixels.
    fn shape_quad(&self, points: &[[f32; 2]], reach: f32, color: Color) -> Quad {
        let (mut low, mut high) = ([f32::MAX; 2], [f32::MIN; 2]);
        for p in points {
            for i in 0..2 {
                low[i] = low[i].min(p[i]);
                high[i] = high[i].max(p[i]);
            }
        }
        let x = (low[0] - reach - 1.0).floor();
        let y = (low[1] - reach - 1.0).floor();
        let w = (high[0] + reach + 1.0).ceil() - x;
        let h = (high[1] + reach + 1.0).ceil() - y;
        Quad {
            pos: [x, y],
            size: [w, h],
            uv_x: [w, 0.0],
            uv_y: [0.0, h],
            color: color.linear(),
            ..Quad::default()
        }
    }

    /// The image file `path` – an app's icon, PNG or SVG – scaled into a
    /// square of `size` pixels with its top left at `x`, `y`, on whole
    /// pixels, texel for pixel.
    #[cfg(feature = "icons")]
    pub fn image(&mut self, path: &Path, x: f32, y: f32, size: u32) {
        self.images.push(Image {
            quad: self.quads.len(),
            path: path.to_owned(),
            size,
            x: x.round(),
            y: y.round(),
        });
        self.quads.push(Quad {
            color: [1.0; 4],
            flags: EXACT,
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
        // A rounded quad's rectangle moves with it.
        self.quads.extend(other.quads.iter().map(|q| Quad {
            pos: [q.pos[0], q.pos[1] + dy],
            clip: [q.clip[0], q.clip[1] + dy, q.clip[2], q.clip[3]],
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
        #[cfg(feature = "icons")]
        self.symbols.extend(other.symbols.iter().map(|s| Symbol {
            quad: s.quad + quads,
            name: s.name.clone(),
            size: s.size,
            x: s.x,
            y: s.y + dy,
        }));
        #[cfg(feature = "icons")]
        self.images.extend(other.images.iter().map(|i| Image {
            quad: i.quad + quads,
            path: i.path.clone(),
            size: i.size,
            x: i.x,
            y: i.y + dy,
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
        #[cfg(feature = "icons")]
        if !self.symbols.is_empty() {
            return true;
        }
        !self.glyphs.is_empty()
    }

    /// Images to place: the scene needs the colour atlas.
    #[cfg(feature = "icons")]
    pub fn has_images(&self) -> bool {
        !self.images.is_empty()
    }

    /// The scene's quads with its glyphs, symbols and images placed in
    /// `glyphs`' atlases – rasterized where new – which lie at `atlases` in
    /// the texture array. A full atlas starts over once; None if the
    /// scene's do not fit even then.
    pub fn quads(&self, glyphs: &mut Glyphs, atlases: Atlases) -> Option<Vec<Quad>> {
        let mut quads = self.quads.clone();
        #[cfg(feature = "icons")]
        let pictures = self.has_images();
        #[cfg(not(feature = "icons"))]
        let pictures = false;
        if !self.has_glyphs() && !pictures {
            return Some(quads);
        }
        for attempt in 0..2 {
            match self.place(glyphs, &mut quads, atlases) {
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

    fn place(&self, glyphs: &mut Glyphs, quads: &mut [Quad], atlases: Atlases) -> Result<(), Full> {
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
            quad.texture = atlases.coverage;
        }
        #[cfg(feature = "icons")]
        self.place_icons(glyphs, quads, atlases)?;
        Ok(())
    }

    /// `place` for the symbols and images.
    #[cfg(feature = "icons")]
    fn place_icons(
        &self,
        glyphs: &mut Glyphs,
        quads: &mut [Quad],
        atlases: Atlases,
    ) -> Result<(), Full> {
        let slot = atlases.coverage;
        let texel = 1.0 / ATLAS_SIZE as f32;
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
        let texel = 1.0 / COLOUR_ATLAS_SIZE as f32;
        for image in &self.images {
            // Without a colour atlas: empty, nothing drawn.
            let Some(slot) = atlases.colour else {
                continue;
            };
            let placed = glyphs.image(&image.path, image.size)?;
            let quad = &mut quads[image.quad];
            let (w, h) = (placed.w as f32, placed.h as f32);
            quad.pos = [image.x, image.y];
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
        below.glyph(0, 1, 5.0, 6.0, Color::rgb(0xffffff));
        let mut scene = Scene::new();
        scene.fill(rect, Color::rgb(0x101010));
        scene.append(&below, 32.0);
        assert_eq!(scene.len(), 3);
        assert_eq!(scene.quads[1].pos, [1.0, 34.0]);
        assert_eq!(scene.blend, [false, false, true]);
        let glyph = &scene.glyphs[0];
        assert_eq!((glyph.quad, glyph.x, glyph.y), (2, 5.0, 38.0));
    }

    #[test]
    fn a_shape_lies_in_its_quads_pixels_and_moves_with_it() {
        let mut marks = Scene::new();
        marks.stroke([10.5, 20.0], [16.5, 20.0], 1.0, Cap::Square, Color::rgb(0));
        marks.triangle([[3.0, 3.0], [8.0, 3.0], [3.0, 8.0]], Color::rgb(0));
        let stroke = marks.quads[0];
        // Half a pixel past each end and a pixel for the edge, on whole
        // pixels; its ends counted from the quad's top left.
        assert_eq!((stroke.pos, stroke.size), ([8.0, 18.0], [11.0, 4.0]));
        assert_eq!(
            (stroke.uv, stroke.uv_x, stroke.uv_y),
            ([0.0; 2], [11.0, 0.0], [0.0, 4.0])
        );
        assert_eq!(stroke.bottom, [2.5, 2.0, 8.5, 2.0]);
        assert_eq!(
            (stroke.flags, stroke.radius, stroke.sigma),
            (STROKE, 0.5, 0.5)
        );
        let triangle = marks.quads[1];
        assert_eq!((triangle.pos, triangle.size), ([2.0, 2.0], [7.0, 7.0]));
        assert_eq!(
            (triangle.bottom, triangle.texel),
            ([1.0, 1.0, 6.0, 1.0], [1.0, 6.0])
        );
        assert_eq!(marks.blend, [true, true]);
        let mut scene = Scene::new();
        scene.append(&marks, 32.0);
        assert_eq!(scene.quads[0].pos, [8.0, 50.0]);
        assert_eq!(scene.quads[0].bottom, stroke.bottom);
    }

    #[test]
    fn a_circle_off_the_pixel_grid_keeps_its_edge_in_its_quad() {
        let mut scene = Scene::new();
        scene.circle([16.0, 16.0], 6.25, Color::rgb(0));
        let quad = scene.quads[0];
        // The pixel from 9 to 10 is partly covered: in the quad.
        assert_eq!((quad.pos, quad.size), ([8.0, 8.0], [16.0, 16.0]));
        assert_eq!(quad.clip, [9.75, 9.75, 12.5, 12.5]);
        assert_eq!((quad.flags, quad.radius), (SOLID, 6.25));
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

    #[test]
    fn clearing_overwrites_and_rounded_rects_keep_their_corners_when_moved() {
        let rect = Rect {
            x: 2.0,
            y: 3.0,
            w: 40.0,
            h: 20.0,
        };
        let mut panel = Scene::new();
        panel.clear(rect);
        panel.rounded(rect, 8.0, Color::rgb(0xf5f5f5));
        // Transparent, written as it is; the rounded one blends its edge.
        assert_eq!(panel.blend, [false, true]);
        assert_eq!(panel.quads[0].color, [0.0; 4]);
        let mut scene = Scene::new();
        scene.append(&panel, 28.0);
        let moved = &scene.quads[1];
        assert_eq!(moved.pos, [2.0, 31.0]);
        assert_eq!(moved.clip, [2.0, 31.0, 40.0, 20.0]);
        assert_eq!(moved.radius, 8.0);
    }
}
