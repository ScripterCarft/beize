//! Text: the system's fonts and Parley's layout (ADR 0027 §9). The fonts
//! are Beize's own, under /usr/share/fonts, registered with Fontique
//! directly – no fontconfig, as CoreText knows macOS's system fonts by
//! itself. Inter is the sans-serif and the interface's face, JetBrains Mono
//! the monospace.

use std::path::Path;
use std::sync::Arc;

use beize_vulkan::error::{Context, Result, error};
use parley::fontique::{Blob, Collection, CollectionOptions, GenericFamily, SourceCache};
use parley::{
    Alignment, AlignmentOptions, FontContext, FontData, FontFamily, FontFamilyName, FontWeight,
    Layout, LayoutContext, PositionedLayoutItem, StyleProperty,
};
use skrifa::{FontRef, MetadataProvider};

use crate::draw::{Color, face};
use crate::glyphs::Face;

/// Where the image keeps its fonts.
pub const FONTS: &str = "/usr/share/fonts";

/// The families the generic names stand for.
const SANS: &str = "Inter Variable";
const MONO: &str = "JetBrains Mono";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Family {
    /// Inter: the interface's face.
    Sans,
    /// JetBrains Mono: code and the terminal.
    Mono,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Style {
    pub family: Family,
    /// Pixels per em.
    pub size: f32,
    /// 100 to 900, 400 regular.
    pub weight: f32,
    pub color: Color,
}

/// The fonts and the layout machinery; one per app.
pub struct Text {
    fonts: FontContext,
    layouts: LayoutContext<Color>,
}

impl Text {
    /// The system's fonts (`FONTS`).
    pub fn new() -> Result<Text> {
        let mut files = Vec::new();
        collect(Path::new(FONTS), &mut files)?;
        files.sort();
        let fonts = files
            .iter()
            .map(|f| Ok(Blob::new(Arc::new(Mapped::open(f)?))))
            .collect::<Result<Vec<_>>>()?;
        Text::with_blobs(fonts)
    }

    /// These fonts only (tests).
    pub fn with_fonts(fonts: impl IntoIterator<Item = Vec<u8>>) -> Result<Text> {
        Text::with_blobs(fonts.into_iter().map(|data| Blob::new(Arc::new(data))))
    }

    fn with_blobs(fonts: impl IntoIterator<Item = Blob<u8>>) -> Result<Text> {
        let mut collection = Collection::new(CollectionOptions {
            shared: false,
            system_fonts: false,
        });
        for data in fonts {
            collection.register_fonts(data, None);
        }
        for (name, generics) in [
            (
                SANS,
                &[
                    GenericFamily::SansSerif,
                    GenericFamily::SystemUi,
                    GenericFamily::UiSansSerif,
                ][..],
            ),
            (
                MONO,
                &[GenericFamily::Monospace, GenericFamily::UiMonospace][..],
            ),
        ] {
            let id = collection
                .family_id(name)
                .ok_or_else(|| error(format!("the font {name} is missing")))?;
            for &generic in generics {
                collection.set_generic_families(generic, std::iter::once(id));
            }
        }
        Ok(Text {
            fonts: FontContext {
                collection,
                source_cache: SourceCache::default(),
            },
            layouts: LayoutContext::new(),
        })
    }

    /// `text` set in `style`, broken into lines no wider than `width`.
    pub fn layout(&mut self, text: &str, style: &Style, width: Option<f32>) -> Layout<Color> {
        let generic = match style.family {
            Family::Sans => GenericFamily::SansSerif,
            Family::Mono => GenericFamily::Monospace,
        };
        // Fractional positions: the glyphs land on quarter pixels
        // (`glyphs`), the baselines on whole ones (`Scene::text`).
        let mut builder = self
            .layouts
            .ranged_builder(&mut self.fonts, text, 1.0, false);
        builder.push_default(StyleProperty::FontFamily(FontFamily::Single(
            FontFamilyName::Generic(generic),
        )));
        builder.push_default(StyleProperty::FontSize(style.size));
        builder.push_default(StyleProperty::FontWeight(FontWeight::new(style.weight)));
        builder.push_default(StyleProperty::Brush(style.color));
        let mut layout = builder.build(text);
        layout.break_all_lines(width);
        layout.align(Alignment::Start, AlignmentOptions::default());
        layout
    }

    /// The monospace face in `size` and `weight`, measured for a grid.
    pub fn cells(&mut self, size: f32, weight: f32) -> Result<Cells> {
        let style = Style {
            family: Family::Mono,
            size,
            weight,
            color: Color::default(),
        };
        let layout = self.layout("M", &style, None);
        let line = layout.lines().next().context("no line for a cell")?;
        let glyphs = line
            .items()
            .find_map(|item| match item {
                PositionedLayoutItem::GlyphRun(glyphs) => Some(glyphs),
                PositionedLayoutItem::InlineBox(_) => None,
            })
            .context("no glyph for a cell")?;
        let advance = glyphs
            .positioned_glyphs()
            .next()
            .context("no glyph for a cell")?
            .advance;
        let run = glyphs.run();
        let metrics = line.metrics();
        Ok(Cells {
            font: run.font().clone(),
            face: face(run),
            // Whole pixels: backgrounds and the cursor fill cells edge to
            // edge; the glyphs keep their quarter pixels.
            width: advance.round().max(1.0),
            height: metrics.line_height.ceil().max(1.0),
            baseline: metrics.baseline.round(),
        })
    }
}

/// A monospace face for a grid of cells, as terminals draw it: one glyph
/// per character straight from the font's character map, unshaped, so each
/// stays in its cell – no ligatures, as in Alacritty, foot and VTE.
pub struct Cells {
    pub(crate) font: FontData,
    pub(crate) face: Face,
    /// A cell's size in whole pixels.
    pub width: f32,
    pub height: f32,
    /// From a cell's top to the baseline.
    pub baseline: f32,
}

impl Cells {
    /// The glyph of `c`; the font's box for missing characters (.notdef)
    /// where it has none, until there is a fallback font (ADR 0027 §9).
    pub fn glyph(&self, c: char) -> u32 {
        FontRef::from_index(self.font.data.as_ref(), self.font.index)
            .ok()
            .and_then(|font| font.charmap().map(c))
            .map_or(0, |id| id.to_u32())
    }
}

/// The font files under `dir`, recursively.
/// A font file mapped read-only: its pages are the page cache's, shared by
/// every app that maps it, not a copy in each app's heap (1.5 MB each).
/// The fonts lie on /usr, read-only and verified (dm-verity, ADR 0008): a
/// file there cannot change or shrink under the mapping.
struct Mapped {
    pointer: *mut std::ffi::c_void,
    len: usize,
}

// SAFETY: the mapping is read-only and stays until the value is dropped;
// any thread may read it.
unsafe impl Send for Mapped {}
// SAFETY: as above.
unsafe impl Sync for Mapped {}

impl Mapped {
    fn open(path: &Path) -> Result<Mapped> {
        let what = || format!("mapping {}", path.display());
        let file = std::fs::File::open(path).context(what())?;
        let len = file.metadata().context(what())?.len();
        let len = usize::try_from(len).context(what())?;
        if len == 0 {
            return Err(error(format!("{}: an empty font file", path.display())));
        }
        // SAFETY: a new private, read-only mapping of the whole file where
        // the kernel chooses; why the file stays as it is: `Mapped`.
        let pointer = unsafe {
            rustix::mm::mmap(
                std::ptr::null_mut(),
                len,
                rustix::mm::ProtFlags::READ,
                rustix::mm::MapFlags::PRIVATE,
                &file,
                0,
            )
        }
        .context(what())?;
        Ok(Mapped { pointer, len })
    }
}

impl AsRef<[u8]> for Mapped {
    fn as_ref(&self) -> &[u8] {
        // SAFETY: `len` readable bytes are mapped there until the value is
        // dropped, which the borrow cannot outlive.
        unsafe { std::slice::from_raw_parts(self.pointer.cast(), self.len) }
    }
}

impl Drop for Mapped {
    fn drop(&mut self) {
        // SAFETY: the mapping is ours and unmapped only here; no slice of
        // it outlives the value.
        let _ = unsafe { rustix::mm::munmap(self.pointer, self.len) };
    }
}

fn collect(dir: &Path, files: &mut Vec<std::path::PathBuf>) -> Result<()> {
    for entry in std::fs::read_dir(dir).context(format!("reading {}", dir.display()))? {
        let path = entry.context("reading the fonts")?.path();
        if path.is_dir() {
            collect(&path, files)?;
        } else if path
            .extension()
            .is_some_and(|e| e == "ttf" || e == "otf" || e == "ttc")
        {
            files.push(path);
        }
    }
    Ok(())
}
