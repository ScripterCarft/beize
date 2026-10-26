//! Symbols and images (feature `icons`): Phosphor's SVGs, rasterized by
//! resvg once per size into the glyph atlas, and apps' icons, PNG or SVG,
//! in a colour atlas of their own (ADR 0028 §8). Beize UI and its apps draw
//! them; the compositor does not – its title bars are text and shapes – so
//! it is built without them, and without resvg.

use std::collections::HashMap;
use std::path::{Path as FilePath, PathBuf};

use resvg::{tiny_skia, usvg};
use tracing::warn;

use super::{Atlas, Full, Glyphs, Placed};
use crate::draw::decode;

/// Beize's symbols, one SVG each by name (recipes/phosphor-icons.toml).
const SYMBOLS: &str = "/usr/share/beize/symbols";

/// The colour atlas' side in texels: 1 MiB at 4 bytes a texel, room for a
/// hundred icons of the dock's size.
pub const COLOUR_ATLAS_SIZE: u32 = 512;
/// The largest image file read: an icon's PNG at hicolor's largest size,
/// 512 px, has well under 1 MiB.
const IMAGE_BYTES: u64 = 4 << 20;

/// The symbols in the glyph atlas, and the images in the colour atlas.
pub(super) struct Icons {
    /// Symbols by name and size in pixels.
    symbols: HashMap<(String, u32), Placed>,
    /// Images in colour by file and size, their atlas and what it lacks.
    images: HashMap<(PathBuf, u32), Placed>,
    colour: Atlas,
    colour_uploads: Vec<(Placed, Vec<u8>)>,
}

impl Icons {
    pub(super) fn new() -> Icons {
        Icons {
            symbols: HashMap::new(),
            images: HashMap::new(),
            colour: Atlas::new(COLOUR_ATLAS_SIZE),
            colour_uploads: Vec::new(),
        }
    }
}

impl Glyphs {
    /// The symbol `name` in a square of `size` pixels: rasterized if new.
    /// One that cannot be read draws nothing, and says so once.
    pub fn symbol(&mut self, name: &str, size: u32) -> Result<Placed, Full> {
        let key = (name.to_owned(), size);
        if let Some(placed) = self.icons.symbols.get(&key) {
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
        self.icons.symbols.insert(key, placed);
        Ok(placed)
    }

    /// The image file `path` – an app's icon, PNG or SVG – scaled into a
    /// square of `size` pixels in the colour atlas: rasterized if new. One
    /// that cannot be read draws nothing, and says so once.
    pub fn image(&mut self, path: &FilePath, size: u32) -> Result<Placed, Full> {
        let key = (path.to_owned(), size);
        if let Some(placed) = self.icons.images.get(&key) {
            return Ok(*placed);
        }
        let placed = match read_image(path, size) {
            Ok(texels) if size > 0 => {
                let (x, y) = self.icons.colour.place(size, size).ok_or(Full)?;
                let placed = Placed {
                    x,
                    y,
                    w: size,
                    h: size,
                    left: 0,
                    top: 0,
                };
                self.icons.colour_uploads.push((placed, texels));
                placed
            }
            Ok(_) => Placed::default(),
            Err(e) => {
                warn!("the image {}: {e}", path.display());
                Placed::default()
            }
        };
        self.icons.images.insert(key, placed);
        Ok(placed)
    }

    /// The images the GPU's colour atlas has yet to receive, 4 bytes a
    /// texel: red, green, blue, alpha, sRGB (`premultiplied_in_linear_light`).
    pub fn take_colour_uploads(&mut self) -> Vec<(Placed, Vec<u8>)> {
        std::mem::take(&mut self.icons.colour_uploads)
    }
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
