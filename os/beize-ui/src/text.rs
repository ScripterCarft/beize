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
    Alignment, AlignmentOptions, FontContext, FontFamily, FontFamilyName, FontWeight, Layout,
    LayoutContext, StyleProperty,
};

use crate::draw::Color;

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
            .map(|f| std::fs::read(f).context(format!("reading {}", f.display())))
            .collect::<Result<Vec<_>>>()?;
        Text::with_fonts(fonts)
    }

    /// These fonts only (tests).
    pub fn with_fonts(fonts: impl IntoIterator<Item = Vec<u8>>) -> Result<Text> {
        let mut collection = Collection::new(CollectionOptions {
            shared: false,
            system_fonts: false,
        });
        for data in fonts {
            collection.register_fonts(Blob::new(Arc::new(data)), None);
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
}

/// The font files under `dir`, recursively.
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
