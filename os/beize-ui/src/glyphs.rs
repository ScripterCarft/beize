//! Glyphs as the GPU draws them: rasterized once each – outlines from
//! Skrifa with light hinting, coverage from zeno – and kept in the atlas,
//! one 8-bit texture (ADR 0027 §9). Text is then a quad per glyph, its
//! texels 1:1 on whole pixels. Symbols (ADR 0028 §8) share the atlas: an
//! SVG rasterized by resvg once per size, drawn like a glyph – as SF
//! Symbols are glyphs on macOS.
//!
//! Hinting is light, as FreeType's "light": vertical only, so baselines and
//! horizontal stems sit on the pixel grid while Inter keeps its shapes and
//! Parley's advances stay exact (`preserve_linear_metrics`). Horizontally a
//! glyph lands on a quarter pixel, as in Chrome: four rasterizations at
//! most, and even spacing.

use std::collections::HashMap;
use std::path::Path as FilePath;

use resvg::{tiny_skia, usvg};
use tracing::warn;

use parley::FontData;
use skrifa::instance::{LocationRef, NormalizedCoord, Size};
use skrifa::outline::{
    DrawSettings, Engine, HintingInstance, HintingOptions, OutlinePen, SmoothMode, Target,
};
use skrifa::raw::types::F2Dot14;
use skrifa::{FontRef, GlyphId, MetadataProvider};
use zeno::{Command, Mask, Origin, Point, Vector};

/// Beize's symbols, one SVG each by name (recipes/phosphor-icons.toml).
const SYMBOLS: &str = "/usr/share/beize/symbols";

/// The atlas' side in texels: 1 MiB, room for thousands of glyphs at text
/// sizes. When it is full it starts over (`Atlas::clear`).
pub const ATLAS_SIZE: u32 = 1024;
/// Horizontal positions per pixel.
const SUBPIXELS: f32 = 4.0;

/// A font at a size and a place in its variation space: what a hinting
/// instance is made for.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Face {
    /// The font file's blob and the font's index in it.
    pub blob: u64,
    pub index: u32,
    /// The size in pixels per em, as bits (exact, hashable).
    pub size: u32,
    pub coords: Vec<i16>,
}

/// One glyph as rasterized: a face, the glyph, the quarter pixel.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct Key {
    face: Face,
    glyph: u32,
    subpixel: u8,
}

/// Where a glyph lies in the atlas and how it sits on its pen position.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Placed {
    /// Its texels in the atlas; empty for glyphs without ink (a space).
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
    /// From the pen position (whole pixel, on the baseline) to its top
    /// left, downwards positive.
    pub left: i32,
    pub top: i32,
}

/// The glyphs in the atlas, and what the GPU's copy still lacks.
pub struct Glyphs {
    placed: HashMap<Key, Placed>,
    /// Symbols by name and size in pixels.
    symbols: HashMap<(String, u32), Placed>,
    hinting: HashMap<Face, Option<HintingInstance>>,
    atlas: Atlas,
    /// Rasterized since the last `take_uploads`: their place and coverage.
    uploads: Vec<(Placed, Vec<u8>)>,
}

/// The atlas is full: everything in it goes, and the frame starts over.
#[derive(Debug, PartialEq, Eq)]
pub struct Full;

impl Default for Glyphs {
    fn default() -> Glyphs {
        Glyphs::new()
    }
}

impl Glyphs {
    pub fn new() -> Glyphs {
        Glyphs {
            placed: HashMap::new(),
            symbols: HashMap::new(),
            hinting: HashMap::new(),
            atlas: Atlas::new(ATLAS_SIZE),
            uploads: Vec::new(),
        }
    }

    /// The glyph `glyph` of `font` with its pen at `x` (pixels, any
    /// fraction): rasterized if new. Returns where it lies and the whole
    /// pixel its placement counts from.
    pub fn get(
        &mut self,
        font: &FontData,
        face: &Face,
        glyph: u32,
        x: f32,
    ) -> Result<(Placed, i32), Full> {
        let (whole, subpixel) = quarter(x);
        let key = Key {
            face: face.clone(),
            glyph,
            subpixel,
        };
        if let Some(placed) = self.placed.get(&key) {
            return Ok((*placed, whole as i32));
        }
        let hinting = self
            .hinting
            .entry(face.clone())
            .or_insert_with(|| hinting(font, face));
        let placed = match rasterize(font, face, hinting.as_ref(), glyph, subpixel) {
            Some((coverage, mut placed)) if placed.w > 0 && placed.h > 0 => {
                let (x, y) = self.atlas.place(placed.w, placed.h).ok_or(Full)?;
                placed.x = x;
                placed.y = y;
                self.uploads.push((placed, coverage));
                placed
            }
            // No ink, or no outline: nothing to draw.
            _ => Placed::default(),
        };
        self.placed.insert(key, placed);
        Ok((placed, whole as i32))
    }

    /// The symbol `name` in a square of `size` pixels: rasterized if new.
    /// One that cannot be read draws nothing, and says so once.
    pub fn symbol(&mut self, name: &str, size: u32) -> Result<Placed, Full> {
        let key = (name.to_owned(), size);
        if let Some(placed) = self.symbols.get(&key) {
            return Ok(*placed);
        }
        let placed = match read_symbol(name).and_then(|svg| rasterize_svg(&svg, size)) {
            Ok(coverage) if size > 0 => {
                let (x, y) = self.atlas.place(size, size).ok_or(Full)?;
                let placed = Placed {
                    x,
                    y,
                    w: size,
                    h: size,
                    left: 0,
                    top: 0,
                };
                self.uploads.push((placed, coverage));
                placed
            }
            Ok(_) => Placed::default(),
            Err(e) => {
                warn!("the symbol {name}: {e}");
                Placed::default()
            }
        };
        self.symbols.insert(key, placed);
        Ok(placed)
    }

    /// Empties the atlas: every glyph is rasterized again when next used.
    pub fn clear(&mut self) {
        self.placed.clear();
        self.symbols.clear();
        self.uploads.clear();
        self.atlas = Atlas::new(ATLAS_SIZE);
    }

    /// The glyphs the GPU's atlas has yet to receive.
    pub fn take_uploads(&mut self) -> Vec<(Placed, Vec<u8>)> {
        std::mem::take(&mut self.uploads)
    }
}

/// `x` as a whole pixel and the nearest quarter after it; a fraction that
/// rounds to a whole quarter more is the next pixel.
fn quarter(x: f32) -> (f32, u8) {
    let whole = x.floor();
    let quarters = ((x - whole) * SUBPIXELS).round();
    if quarters >= SUBPIXELS {
        (whole + 1.0, 0)
    } else {
        (whole, quarters as u8)
    }
}

/// The hinting instance for a face; none where the font cannot be read or
/// hinted (its glyphs are then drawn unhinted).
fn hinting(font: &FontData, face: &Face) -> Option<HintingInstance> {
    let font = FontRef::from_index(font.data.as_ref(), font.index).ok()?;
    let coords = coords(face);
    let options = HintingOptions {
        engine: Engine::AutoFallback,
        target: Target::Smooth {
            mode: SmoothMode::Light,
            symmetric_rendering: true,
            preserve_linear_metrics: true,
        },
    };
    HintingInstance::new(
        &font.outline_glyphs(),
        Size::new(f32::from_bits(face.size)),
        LocationRef::new(&coords),
        options,
    )
    .ok()
}

fn coords(face: &Face) -> Vec<NormalizedCoord> {
    face.coords.iter().map(|&c| F2Dot14::from_bits(c)).collect()
}

/// The glyph's coverage and placement (`x`, `y` still 0); none if the font
/// has no outline for it.
fn rasterize(
    font: &FontData,
    face: &Face,
    hinting: Option<&HintingInstance>,
    glyph: u32,
    subpixel: u8,
) -> Option<(Vec<u8>, Placed)> {
    let font = FontRef::from_index(font.data.as_ref(), font.index).ok()?;
    let outlines = font.outline_glyphs();
    let outline = outlines.get(GlyphId::new(glyph))?;
    let coords = coords(face);
    let settings = match hinting {
        Some(instance) => DrawSettings::hinted(instance, false),
        None => DrawSettings::unhinted(
            Size::new(f32::from_bits(face.size)),
            LocationRef::new(&coords),
        ),
    };
    let mut path = Path(Vec::new());
    outline.draw(settings, &mut path).ok()?;
    if path.0.is_empty() {
        return Some((Vec::new(), Placed::default()));
    }
    // Outlines are y-up from the baseline (BottomLeft). The glyph moves to
    // its quarter pixel with its bounds: `offset` moves only the bounds
    // (alone it left the ink where it was, measured), `render_offset` only
    // the outline (alone, ink past the old bounds would be cut).
    let shift = Vector::new(f32::from(subpixel) / SUBPIXELS, 0.0);
    let (coverage, placement) = Mask::new(&path.0)
        .origin(Origin::BottomLeft)
        .offset(shift)
        .render_offset(shift)
        .render();
    Some((
        coverage,
        Placed {
            x: 0,
            y: 0,
            w: placement.width,
            h: placement.height,
            left: placement.left,
            // zeno names the bottom edge (y-up); the top is above it.
            top: -(placement.top + placement.height as i32),
        },
    ))
}

/// The SVG of the symbol `name` (lower case and dashes, as Phosphor names
/// them).
fn read_symbol(name: &str) -> Result<Vec<u8>, String> {
    if name.is_empty() || !name.bytes().all(|b| b.is_ascii_lowercase() || b == b'-') {
        return Err("not a symbol's name".into());
    }
    let file = FilePath::new(SYMBOLS).join(format!("{name}.svg"));
    std::fs::read(&file).map_err(|e| format!("{}: {e}", file.display()))
}

/// The coverage of `svg` scaled into a square of `size` pixels, one byte a
/// pixel.
fn rasterize_svg(svg: &[u8], size: u32) -> Result<Vec<u8>, String> {
    let tree = usvg::Tree::from_data(svg, &usvg::Options::default()).map_err(|e| e.to_string())?;
    let mut pixmap = tiny_skia::Pixmap::new(size, size).ok_or("an empty size")?;
    let scale = size as f32 / tree.size().width().max(tree.size().height());
    resvg::render(
        &tree,
        tiny_skia::Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );
    Ok(pixmap.pixels().iter().map(|p| p.alpha()).collect())
}

/// An outline as zeno's path commands.
struct Path(Vec<Command>);

impl OutlinePen for Path {
    fn move_to(&mut self, x: f32, y: f32) {
        self.0.push(Command::MoveTo(Point::new(x, y)));
    }
    fn line_to(&mut self, x: f32, y: f32) {
        self.0.push(Command::LineTo(Point::new(x, y)));
    }
    fn quad_to(&mut self, cx0: f32, cy0: f32, x: f32, y: f32) {
        self.0
            .push(Command::QuadTo(Point::new(cx0, cy0), Point::new(x, y)));
    }
    fn curve_to(&mut self, cx0: f32, cy0: f32, cx1: f32, cy1: f32, x: f32, y: f32) {
        self.0.push(Command::CurveTo(
            Point::new(cx0, cy0),
            Point::new(cx1, cy1),
            Point::new(x, y),
        ));
    }
    fn close(&mut self) {
        self.0.push(Command::Close);
    }
}

/// Shelves of glyphs, filled left to right and top to bottom: glyphs of a
/// text size differ little in height, so little is wasted.
struct Atlas {
    size: u32,
    /// Top, height and the next free column of each shelf.
    shelves: Vec<(u32, u32, u32)>,
}

impl Atlas {
    fn new(size: u32) -> Atlas {
        Atlas {
            size,
            shelves: Vec::new(),
        }
    }

    /// A free place of `w` × `h`, or none: the atlas is full.
    fn place(&mut self, w: u32, h: u32) -> Option<(u32, u32)> {
        if w > self.size || h > self.size {
            return None;
        }
        // The lowest shelf that fits it without wasting more than a
        // quarter of its height.
        let fits = self
            .shelves
            .iter_mut()
            .filter(|(_, height, next)| {
                *height >= h && h * 4 >= *height * 3 && self.size - *next >= w
            })
            .min_by_key(|(_, height, _)| *height);
        if let Some((top, _, next)) = fits {
            let at = (*next, *top);
            *next += w;
            return Some(at);
        }
        let bottom = self
            .shelves
            .last()
            .map_or(0, |(top, height, _)| top + height);
        if self.size - bottom < h {
            return None;
        }
        self.shelves.push((bottom, h, w));
        Some((0, bottom))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shelves_take_glyphs_of_a_height_and_open_new_ones() {
        let mut atlas = Atlas::new(64);
        assert_eq!(atlas.place(10, 16), Some((0, 0)));
        assert_eq!(atlas.place(10, 14), Some((10, 0)));
        // Far smaller: a shelf of its own.
        assert_eq!(atlas.place(10, 8), Some((0, 16)));
        // Too wide for what is left of the first: below the others.
        assert_eq!(atlas.place(50, 16), Some((0, 24)));
    }

    #[test]
    fn pens_land_on_the_nearest_quarter_pixel() {
        assert_eq!(quarter(10.0), (10.0, 0));
        assert_eq!(quarter(10.3), (10.0, 1));
        assert_eq!(quarter(10.5), (10.0, 2));
        assert_eq!(quarter(10.8), (10.0, 3));
        assert_eq!(quarter(10.9), (11.0, 0));
        assert_eq!(quarter(-0.1), (0.0, 0));
    }

    #[test]
    fn a_symbol_covers_its_shape_and_nothing_else() {
        // Phosphor's "square", regular: a frame 16/256 of its size wide.
        let svg = br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 256 256" fill="currentColor"><path d="M208,32H48A16,16,0,0,0,32,48V208a16,16,0,0,0,16,16H208a16,16,0,0,0,16-16V48A16,16,0,0,0,208,32Zm0,176H48V48H208V208Z"/></svg>"#;
        let coverage = rasterize_svg(svg, 32).unwrap();
        assert_eq!(coverage.len(), 32 * 32);
        let at = |x: usize, y: usize| coverage[y * 32 + x];
        // The frame from 4 to 6 px, its inside and outside empty.
        assert_eq!(at(16, 4), 255);
        assert_eq!(at(5, 16), 255);
        assert_eq!(at(16, 16), 0);
        assert_eq!(at(1, 16), 0);
        assert!(rasterize_svg(b"<svg", 32).is_err());
        assert!(read_symbol("../../etc/passwd").is_err());
    }

    #[test]
    fn a_full_atlas_says_so() {
        let mut atlas = Atlas::new(16);
        assert_eq!(atlas.place(16, 16), Some((0, 0)));
        assert_eq!(atlas.place(1, 1), None);
        assert_eq!(atlas.place(17, 1), None);
    }
}
