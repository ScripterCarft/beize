//! Glyphs as the GPU draws them: rasterized once each – outlines from
//! Skrifa with light hinting, coverage from zeno – and kept in the atlas,
//! one 8-bit texture (ADR 0027 §9). Text is then a quad per glyph, its
//! texels 1:1 on whole pixels. Symbols (ADR 0028 §8) share the atlas: an
//! SVG rasterized by resvg once per size, drawn like a glyph – as SF
//! Symbols are glyphs on macOS. Images in colour, the icons of other apps,
//! go to an atlas of their own, 4 bytes a texel, as Skia keeps colour
//! glyphs apart from its 8-bit ones.
//!
//! Hinting is light, as FreeType's "light": vertical only, so baselines and
//! horizontal stems sit on the pixel grid while Inter keeps its shapes and
//! Parley's advances stay exact (`preserve_linear_metrics`). Horizontally a
//! glyph lands on a quarter pixel, as in Chrome: four rasterizations at
//! most, and even spacing.

use std::collections::HashMap;
use std::path::{Path as FilePath, PathBuf};

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

use crate::draw::decode;

/// Beize's symbols, one SVG each by name (recipes/phosphor-icons.toml).
const SYMBOLS: &str = "/usr/share/beize/symbols";

/// The atlas' side in texels: 1 MiB, room for thousands of glyphs at text
/// sizes. When it is full it starts over (`Atlas::clear`).
pub const ATLAS_SIZE: u32 = 1024;
/// The colour atlas' side in texels: 1 MiB at 4 bytes a texel, room for a
/// hundred icons of the dock's size.
pub const COLOUR_ATLAS_SIZE: u32 = 512;
/// The largest image file read: an icon's PNG at hicolor's largest size,
/// 512 px, has well under 1 MiB.
const IMAGE_BYTES: u64 = 4 << 20;
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
    /// Images in colour by file and size, their atlas and what it lacks.
    images: HashMap<(PathBuf, u32), Placed>,
    colour: Atlas,
    colour_uploads: Vec<(Placed, Vec<u8>)>,
    /// How often the atlas started over.
    generation: u64,
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
            images: HashMap::new(),
            colour: Atlas::new(COLOUR_ATLAS_SIZE),
            colour_uploads: Vec::new(),
            generation: 0,
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
        self.place_svg(name.to_owned(), size, || read_symbol(name))
    }

    fn place_svg(
        &mut self,
        name: String,
        size: u32,
        read: impl FnOnce() -> Result<Vec<u8>, String>,
    ) -> Result<Placed, Full> {
        let key = (name, size);
        if let Some(placed) = self.symbols.get(&key) {
            return Ok(*placed);
        }
        let name = &key.0;
        let placed = match read().and_then(|svg| rasterize_svg(&svg, size)) {
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

    /// The image file `path` – an app's icon, PNG or SVG – scaled into a
    /// square of `size` pixels in the colour atlas: rasterized if new. One
    /// that cannot be read draws nothing, and says so once.
    pub fn image(&mut self, path: &FilePath, size: u32) -> Result<Placed, Full> {
        let key = (path.to_owned(), size);
        if let Some(placed) = self.images.get(&key) {
            return Ok(*placed);
        }
        let placed = match read_image(path, size) {
            Ok(texels) if size > 0 => {
                let (x, y) = self.colour.place(size, size).ok_or(Full)?;
                let placed = Placed {
                    x,
                    y,
                    w: size,
                    h: size,
                    left: 0,
                    top: 0,
                };
                self.colour_uploads.push((placed, texels));
                placed
            }
            Ok(_) => Placed::default(),
            Err(e) => {
                warn!("the image {}: {e}", path.display());
                Placed::default()
            }
        };
        self.images.insert(key, placed);
        Ok(placed)
    }

    /// Empties both atlases: everything is rasterized again when next used.
    pub fn clear(&mut self) {
        self.placed.clear();
        self.symbols.clear();
        self.uploads.clear();
        self.atlas = Atlas::new(ATLAS_SIZE);
        self.images.clear();
        self.colour_uploads.clear();
        self.colour = Atlas::new(COLOUR_ATLAS_SIZE);
        self.generation += 1;
    }

    /// Changes each time the atlas starts over: what was placed before is
    /// gone from it.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// The glyphs the GPU's atlas has yet to receive.
    pub fn take_uploads(&mut self) -> Vec<(Placed, Vec<u8>)> {
        std::mem::take(&mut self.uploads)
    }

    /// The images the GPU's colour atlas has yet to receive, 4 bytes a
    /// texel: red, green, blue, alpha, sRGB (`premultiplied_in_linear_light`).
    pub fn take_colour_uploads(&mut self) -> Vec<(Placed, Vec<u8>)> {
        std::mem::take(&mut self.colour_uploads)
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

/// The texels of the image file `path` scaled into a square of `size`
/// pixels, centred, as the colour atlas holds them. PNG, or SVG by its
/// extension, as the Icon Theme Specification names them.
fn read_image(path: &FilePath, size: u32) -> Result<Vec<u8>, String> {
    let len = std::fs::metadata(path).map_err(|e| e.to_string())?.len();
    if len > IMAGE_BYTES {
        return Err(format!("{len} bytes, more than an icon has"));
    }
    let data = std::fs::read(path).map_err(|e| e.to_string())?;
    let mut pixmap = tiny_skia::Pixmap::new(size, size).ok_or("an empty size")?;
    // Scaled to fit, centred.
    let fit = |w: f32, h: f32| {
        let scale = size as f32 / w.max(h);
        tiny_skia::Transform::from_scale(scale, scale).post_translate(
            (size as f32 - w * scale) / 2.0,
            (size as f32 - h * scale) / 2.0,
        )
    };
    if path.extension().is_some_and(|e| e == "svg") {
        let tree =
            usvg::Tree::from_data(&data, &usvg::Options::default()).map_err(|e| e.to_string())?;
        let at = fit(tree.size().width(), tree.size().height());
        resvg::render(&tree, at, &mut pixmap.as_mut());
    } else {
        let image = tiny_skia::Pixmap::decode_png(&data).map_err(|e| e.to_string())?;
        let paint = tiny_skia::PixmapPaint {
            quality: tiny_skia::FilterQuality::Bicubic,
            ..tiny_skia::PixmapPaint::default()
        };
        let at = fit(image.width() as f32, image.height() as f32);
        pixmap.draw_pixmap(0, 0, image.as_ref(), &paint, at, None);
    }
    Ok(pixmap
        .pixels()
        .iter()
        .flat_map(|&p| premultiplied_in_linear_light(p))
        .collect())
}

/// A pixel of tiny-skia's – premultiplied as encoded – as the colour atlas
/// holds it: the GPU decodes its sRGB to linear light, where the shaders
/// blend premultiplied, so each channel is the encoding of its linear value
/// times alpha. Taken as it is, a half covered edge would come out darker.
fn premultiplied_in_linear_light(p: tiny_skia::PremultipliedColorU8) -> [u8; 4] {
    let c = p.demultiply();
    let alpha = f32::from(c.alpha()) / 255.0;
    let channel = |v: u8| encode(decode(v) * alpha);
    [
        channel(c.red()),
        channel(c.green()),
        channel(c.blue()),
        c.alpha(),
    ]
}

/// A linear value, 0 to 1, in sRGB, as the standard encodes it.
fn encode(linear: f32) -> u8 {
    let c = if linear <= 0.003_130_8 {
        linear * 12.92
    } else {
        1.055 * linear.powf(1.0 / 2.4) - 0.055
    };
    (c * 255.0).round().clamp(0.0, 255.0) as u8
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
    fn colour_is_premultiplied_in_linear_light() {
        // White half covered: half the light, encoded 188 – not 128.
        let half = tiny_skia::PremultipliedColorU8::from_rgba(128, 128, 128, 128).unwrap();
        assert_eq!(premultiplied_in_linear_light(half), [188, 188, 188, 128]);
        let red = tiny_skia::PremultipliedColorU8::from_rgba(255, 0, 0, 255).unwrap();
        assert_eq!(premultiplied_in_linear_light(red), [255, 0, 0, 255]);
    }

    #[test]
    fn an_icon_is_scaled_into_its_square_and_centred() {
        let dir = std::env::temp_dir().join(format!("beize-draw-icons-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // A red PNG twice as wide as high: fitted, with room above and below.
        let mut wide = tiny_skia::Pixmap::new(64, 32).unwrap();
        wide.fill(tiny_skia::Color::from_rgba8(255, 0, 0, 255));
        let png = dir.join("wide.png");
        wide.save_png(&png).unwrap();
        let texels = read_image(&png, 16).unwrap();
        assert_eq!(texels.len(), 16 * 16 * 4);
        let at = |x: usize, y: usize| &texels[(y * 16 + x) * 4..][..4];
        assert_eq!(at(8, 8), [255, 0, 0, 255]);
        assert_eq!(at(8, 1), [0, 0, 0, 0]);
        // An SVG by its extension.
        let svg = dir.join("square.svg");
        std::fs::write(
            &svg,
            r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 4 4"><rect width="4" height="4" fill="#0000ff"/></svg>"##,
        )
        .unwrap();
        assert_eq!(&read_image(&svg, 8).unwrap()[..4], [0, 0, 255, 255]);
        // Not an image: an error, no panic.
        std::fs::write(dir.join("bad.png"), b"not a png").unwrap();
        assert!(read_image(&dir.join("bad.png"), 16).is_err());
        assert!(read_image(&dir.join("missing.png"), 16).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_full_atlas_says_so() {
        let mut atlas = Atlas::new(16);
        assert_eq!(atlas.place(16, 16), Some((0, 0)));
        assert_eq!(atlas.place(1, 1), None);
        assert_eq!(atlas.place(17, 1), None);
    }
}
