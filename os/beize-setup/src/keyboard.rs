//! The keyboard layout (ADR 0017), chosen from systemd's kbd-model-map: each
//! row pairs an XKB layout (what the compositor will use) with the console
//! keymap for it. Only rows whose console keymap exists are offered – the
//! table also names keymaps kbd does not ship. The names come from
//! xkeyboard-config's evdev.lst. "German", "de" and "de-latin1" all lead to
//! German; "German" alone asks which of its variants.

use std::collections::HashSet;
use std::fs;
use std::path::Path;

pub const MODEL_MAP: &str = "/usr/share/systemd/kbd-model-map";
pub const XKB_NAMES: &str = "/usr/share/xkeyboard-config-2/rules/evdev.lst";
pub const KEYMAPS: &str = "/usr/share/keymaps";
/// The firmware's keyboard layout as a language tag (e.g. "de-DE"), reported
/// by systemd-boot.
const FIRMWARE_LAYOUT: &str =
    "/sys/firmware/efi/efivars/LoaderKeyboardLayout-4a67b082-0a4c-41cf-b6c7-440b29bb8c4f";

/// One row of kbd-model-map.
#[derive(Debug, Clone, PartialEq)]
pub struct Layout {
    /// The console keymap.
    pub keymap: String,
    /// XKB layout, model, variant and options ("" for none).
    pub layout: String,
    pub model: String,
    pub variant: String,
    pub options: String,
    /// Language tags the row is meant for (the firmware's layout).
    tags: Vec<String>,
    pub description: String,
}

impl Layout {
    /// The XKB notation: "de", "de(nodeadkeys)".
    pub fn code(&self) -> String {
        if self.variant.is_empty() || self.variant.chars().all(|c| c == ',') {
            self.layout.clone()
        } else {
            format!("{}({})", self.layout, self.variant)
        }
    }
}

#[derive(Debug, PartialEq)]
pub enum Found {
    Layout(Layout),
    Several(Vec<Layout>),
    None,
}

/// The layouts to offer, in kbd-model-map's order; one per XKB layout and
/// variant (the first row with an existing console keymap).
pub fn offered(
    model_map: &str,
    xkb_names: &str,
    keymap_exists: impl Fn(&str) -> bool,
) -> Vec<Layout> {
    let names = Names::parse(xkb_names);
    let mut seen = HashSet::new();
    let mut layouts = Vec::new();
    for line in model_map.lines() {
        let fields: Vec<&str> = line.split_whitespace().collect();
        if line.starts_with('#') || fields.len() < 5 {
            continue;
        }
        let none = |field: &str| {
            if field == "-" {
                String::new()
            } else {
                field.to_owned()
            }
        };
        let (layout, variant) = (fields[1].to_owned(), none(fields[3]));
        if !keymap_exists(fields[0]) || !seen.insert((layout.clone(), variant.clone())) {
            continue;
        }
        let Some(description) = names.describe(&layout, &variant) else {
            continue;
        };
        layouts.push(Layout {
            keymap: fields[0].to_owned(),
            model: fields[2].to_owned(),
            options: none(fields[4]),
            tags: fields
                .get(5)
                .filter(|tags| **tags != "-")
                .map(|tags| tags.split(',').map(str::to_lowercase).collect())
                .unwrap_or_default(),
            layout,
            variant,
            description,
        });
    }
    layouts
}

/// Looks `answer` up by code ("de", "de(nodeadkeys)", "de nodeadkeys"),
/// console keymap or name; a name's words may also find several layouts.
/// Case does not matter.
pub fn find(answer: &str, layouts: &[Layout]) -> Found {
    let wanted = words(answer);
    if wanted.is_empty() {
        return Found::None;
    }
    let joined = wanted.join(" ");
    let exact = layouts.iter().find(|layout| {
        let code = layout.code().to_lowercase();
        joined == code
            || joined == code.replace(['(', ')'], " ").trim()
            || joined == layout.keymap.to_lowercase()
            || joined == words(&layout.description).join(" ")
    });
    if let Some(layout) = exact {
        return Found::Layout(layout.clone());
    }
    // By the name of the layout itself, not of the one to switch to:
    // "English" is not "Russian + English (US)".
    let mut matches: Vec<Layout> = layouts
        .iter()
        .filter(|layout| {
            let own = layout.description.split(" + ").next().unwrap_or_default();
            let described = words(own);
            wanted.iter().all(|word| described.contains(word))
        })
        .cloned()
        .collect();
    matches.sort_by_key(|layout| layout.description.to_lowercase());
    match matches.len() {
        0 => Found::None,
        1 => Found::Layout(matches.remove(0)),
        _ => Found::Several(matches),
    }
}

/// The layout for a language tag ("de-DE"): the row listing the tag, else the
/// first listing its language ("de"), as systemd-vconsole-setup matches it.
pub fn for_tag<'a>(tag: &str, layouts: &'a [Layout]) -> Option<&'a Layout> {
    let tag = tag.trim().to_lowercase();
    let language = tag.split('-').next().filter(|l| !l.is_empty())?;
    layouts
        .iter()
        .find(|layout| layout.tags.contains(&tag))
        .or_else(|| {
            layouts.iter().find(|layout| {
                layout
                    .tags
                    .iter()
                    .any(|t| t.split('-').next() == Some(language))
            })
        })
}

/// The firmware's keyboard layout, if systemd-boot reported one: an EFI
/// variable, 4 attribute bytes and a NUL-terminated UTF-16 string.
pub fn firmware_tag() -> Option<String> {
    let raw = fs::read(FIRMWARE_LAYOUT).ok()?;
    let units: Vec<u16> = raw
        .get(4..)?
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| u16::from_le_bytes(*pair))
        .take_while(|&unit| unit != 0)
        .collect();
    String::from_utf16(&units)
        .ok()
        .filter(|tag| !tag.is_empty())
}

/// The console keymaps there are, by name: loadkeys finds a keymap anywhere
/// below its directory.
pub fn keymap_names(dir: &Path) -> HashSet<String> {
    let mut names = HashSet::new();
    let mut pending = vec![dir.to_path_buf()];
    while let Some(dir) = pending.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
            } else if let Some(name) = path.file_name().and_then(|n| n.to_str())
                && let Some(stem) = name.strip_suffix(".map")
            {
                names.insert(stem.to_owned());
            }
        }
    }
    names
}

/// Lowercase words, without punctuation: "German (no dead keys)" →
/// ["german", "no", "dead", "keys"].
fn words(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric() && c != '-')
        .filter(|word| !word.is_empty())
        .map(str::to_lowercase)
        .collect()
}

/// The layout and variant names from evdev.lst.
struct Names {
    layouts: Vec<(String, String)>,
    /// (layout, variant, name)
    variants: Vec<(String, String, String)>,
}

impl Names {
    fn parse(lst: &str) -> Self {
        let (mut layouts, mut variants) = (Vec::new(), Vec::new());
        let mut section = "";
        for line in lst.lines() {
            if let Some(name) = line.strip_prefix("! ") {
                section = name.trim();
                continue;
            }
            let Some((code, rest)) = line.trim().split_once(char::is_whitespace) else {
                continue;
            };
            let rest = rest.trim();
            match section {
                "layout" => layouts.push((code.to_owned(), rest.to_owned())),
                "variant" => {
                    if let Some((layout, name)) = rest.split_once(": ") {
                        variants.push((layout.to_owned(), code.to_owned(), name.to_owned()));
                    }
                }
                _ => {}
            }
        }
        Self { layouts, variants }
    }

    /// The name of a layout and variant; with a second layout to switch to
    /// ("ru,us"), both names.
    fn describe(&self, layout: &str, variant: &str) -> Option<String> {
        let mut variants = variant.split(',');
        let names: Option<Vec<String>> = layout
            .split(',')
            .map(|layout| {
                let variant = variants.next().unwrap_or("");
                if variant.is_empty() {
                    self.layouts
                        .iter()
                        .find(|(code, _)| code == layout)
                        .map(|(_, name)| name.clone())
                } else {
                    self.variants
                        .iter()
                        .find(|(l, v, _)| l == layout && v == variant)
                        .map(|(_, _, name)| name.clone())
                }
            })
            .collect();
        Some(names?.join(" + "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MODEL_MAP: &str = "# consolelayout\txlayout\txmodel\txvariant\txoptions\tbcp47\n\
        sg\t\tch\tpc105\tde_nodeadkeys\tterminate:ctrl_alt_bksp\t\tde-CH\n\
        ru\t\tru,us\tpc105\t-\tterminate:ctrl_alt_bksp,grp:shifts_toggle\tru-RU,ru\n\
        de\t\tde\tpc105\t-\tterminate:ctrl_alt_bksp\t\tde-DE,de-AT,de\n\
        de-latin1\tde\tpc105\t-\tterminate:ctrl_alt_bksp\t-\n\
        de-latin1-nodeadkeys\tde\tpc105\tnodeadkeys\tterminate:ctrl_alt_bksp\n\
        us\t\tus\tpc105+inet\t-\tterminate:ctrl_alt_bksp\t\ten-US,en\n\
        ko\t\tkr\tpc105\t-\tterminate:ctrl_alt_bksp\t\tko-KR,ko\n";
    const XKB_NAMES: &str = "! model\n  pc105           Generic 105-key PC\n\n\
        ! layout\n  us              English (US)\n  de              German\n  \
        ch              German (Switzerland)\n  ru              Russian\n  kr              Korean\n\n\
        ! variant\n  nodeadkeys      de: German (no dead keys)\n  \
        de_nodeadkeys   ch: German (Switzerland, no dead keys)\n";

    fn layouts() -> Vec<Layout> {
        // kbd ships no "ko".
        offered(MODEL_MAP, XKB_NAMES, |keymap| keymap != "ko")
    }

    fn code(found: Found) -> String {
        match found {
            Found::Layout(layout) => layout.code(),
            Found::Several(layouts) => layouts
                .iter()
                .map(Layout::code)
                .collect::<Vec<_>>()
                .join(" "),
            Found::None => "none".to_owned(),
        }
    }

    #[test]
    fn offers_one_row_per_layout_with_an_existing_keymap() {
        let offered: Vec<(String, String)> = layouts()
            .iter()
            .map(|layout| (layout.code(), layout.keymap.clone()))
            .collect();
        let expected = [
            ("ch(de_nodeadkeys)", "sg"),
            ("ru,us", "ru"),
            ("de", "de"),
            ("de(nodeadkeys)", "de-latin1-nodeadkeys"),
            ("us", "us"),
        ];
        assert_eq!(offered, expected.map(|(c, k)| (c.to_owned(), k.to_owned())));
        let ru = &layouts()[1];
        assert_eq!(ru.description, "Russian + English (US)");
        assert_eq!(ru.options, "terminate:ctrl_alt_bksp,grp:shifts_toggle");
    }

    #[test]
    fn finds_layouts_by_code_keymap_and_name() {
        let layouts = layouts();
        assert_eq!(code(find("de", &layouts)), "de");
        assert_eq!(code(find(" DE ", &layouts)), "de");
        assert_eq!(code(find("de(nodeadkeys)", &layouts)), "de(nodeadkeys)");
        assert_eq!(code(find("de nodeadkeys", &layouts)), "de(nodeadkeys)");
        assert_eq!(
            code(find("de-latin1-nodeadkeys", &layouts)),
            "de(nodeadkeys)"
        );
        assert_eq!(code(find("German", &layouts)), "de");
        assert_eq!(code(find("english", &layouts)), "us");
        assert_eq!(code(find("Switzerland", &layouts)), "ch(de_nodeadkeys)");
        assert_eq!(
            code(find("dead keys", &layouts)),
            "de(nodeadkeys) ch(de_nodeadkeys)"
        );
    }

    #[test]
    fn several_layouts_ask_back() {
        // The name of a layout also matches its variants.
        assert_eq!(
            code(find("german no", &layouts())),
            "de(nodeadkeys) ch(de_nodeadkeys)"
        );
    }

    #[test]
    fn unknown_or_unusable_layouts_find_nothing() {
        let layouts = layouts();
        assert_eq!(code(find("", &layouts)), "none");
        assert_eq!(code(find("Klingon", &layouts)), "none");
        assert_eq!(code(find("Korean", &layouts)), "none");
        assert_eq!(code(find("Germ", &layouts)), "none");
    }

    #[test]
    fn firmware_tags_choose_a_layout() {
        let layouts = layouts();
        let tag = |t| for_tag(t, &layouts).map(Layout::code);
        assert_eq!(tag("de-DE").as_deref(), Some("de"));
        assert_eq!(tag("de-CH").as_deref(), Some("ch(de_nodeadkeys)"));
        assert_eq!(tag("de-LU").as_deref(), Some("ch(de_nodeadkeys)"));
        assert_eq!(tag("en-GB").as_deref(), Some("us"));
        assert_eq!(tag("ko-KR"), None);
        assert_eq!(tag(""), None);
    }
}
