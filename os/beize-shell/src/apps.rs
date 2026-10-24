//! What the shell knows of an app: its name and icon, from its desktop
//! file (freedesktop's Desktop Entry Specification), found by its id – the
//! window's `app_id` – in `applications/` of the XDG data directories, the
//! user's first (XDG Base Directory Specification). Without a desktop file
//! the id stands in for the name.
//!
//! The icon (`Icon=`) is a file, or a name looked up in the hicolor theme as
//! the Icon Theme Specification does – Beize has no other theme – and then
//! in `/usr/share/pixmaps`. Beize's own apps draw their symbols themselves
//! (`AppIcon`, ADR 0028 §8).

use std::path::{Path, PathBuf};

/// What the shell knows of an app.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct App {
    pub name: String,
    /// Its icon's file, PNG or SVG, the best for `size` pixels.
    pub icon: Option<PathBuf>,
}

/// The app `app_id`, as its desktop file has it; its icon chosen for `size`
/// pixels.
pub fn app(app_id: &str, size: u32) -> App {
    let dirs = data_dirs();
    let entry = (!app_id.is_empty() && !app_id.contains('/'))
        .then(|| {
            dirs.iter()
                .map(|dir| dir.join("applications").join(format!("{app_id}.desktop")))
                .find_map(|path| std::fs::read_to_string(path).ok())
                .and_then(|entry| desktop_entry(&entry))
        })
        .flatten()
        .unwrap_or_default();
    App {
        name: entry.name.unwrap_or_else(|| app_id.to_owned()),
        icon: entry.icon.and_then(|icon| find_icon(&icon, size, &dirs)),
    }
}

/// An icon by its desktop file's `Icon=`: a file as it is; a name in the
/// hicolor theme of each data directory, then in `/usr/share/pixmaps`. Of
/// hicolor's sizes the exact one wins, then a scalable SVG, then the
/// nearest larger, then the nearest smaller – scaled down rather than up;
/// on a tie, the earlier data directory.
fn find_icon(icon: &str, size: u32, dirs: &[PathBuf]) -> Option<PathBuf> {
    let path = Path::new(icon);
    if path.is_absolute() {
        return path.is_file().then(|| path.to_owned());
    }
    if icon.is_empty() || icon.contains('/') {
        return None;
    }
    // (rank, path): lower is better.
    let mut best: Option<((u8, u32), PathBuf)> = None;
    for dir in dirs {
        let theme = dir.join("icons/hicolor");
        let Ok(sizes) = std::fs::read_dir(&theme) else {
            continue;
        };
        for sized in sizes.flatten() {
            let Some(pixels) = sized.file_name().to_str().and_then(pixels) else {
                continue;
            };
            for extension in ["png", "svg"] {
                let file = sized
                    .path()
                    .join("apps")
                    .join(format!("{icon}.{extension}"));
                if !file.is_file() {
                    continue;
                }
                let rank = match pixels {
                    Some(p) if p == size => (0, 0),
                    None => (1, 0),
                    Some(p) if p > size => (2, p - size),
                    Some(p) => (3, size - p),
                };
                if best.as_ref().is_none_or(|(r, _)| rank < *r) {
                    best = Some((rank, file));
                }
            }
        }
    }
    best.map(|(_, file)| file).or_else(|| {
        ["png", "svg"]
            .iter()
            .map(|extension| Path::new("/usr/share/pixmaps").join(format!("{icon}.{extension}")))
            .find(|file| file.is_file())
    })
}

/// The pixels of a hicolor directory: `48x48` 48, `24x24@2` 48,
/// `scalable` any (None); others are not sizes.
fn pixels(name: &str) -> Option<Option<u32>> {
    if name == "scalable" {
        return Some(None);
    }
    let (square, scale) = match name.split_once('@') {
        Some((square, scale)) => (square, scale.parse::<u32>().ok()?),
        None => (name, 1),
    };
    let (w, h) = square.split_once('x')?;
    let (w, h): (u32, u32) = (w.parse().ok()?, h.parse().ok()?);
    if w != h {
        return None;
    }
    Some(Some(w.checked_mul(scale)?))
}

/// `$XDG_DATA_HOME` (`~/.local/share`), then `$XDG_DATA_DIRS`
/// (`/usr/local/share:/usr/share`).
fn data_dirs() -> Vec<PathBuf> {
    let var = |name| std::env::var_os(name).filter(|v| !v.is_empty());
    let home = var("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| var("HOME").map(|home| PathBuf::from(home).join(".local/share")));
    let system = var("XDG_DATA_DIRS")
        .map(|dirs| std::env::split_paths(&dirs).collect())
        .unwrap_or_else(|| vec!["/usr/local/share".into(), "/usr/share".into()]);
    home.into_iter().chain(system).collect()
}

/// What the shell reads of a desktop file.
#[derive(Debug, Default, PartialEq, Eq)]
struct Entry {
    name: Option<String>,
    icon: Option<String>,
}

/// `Name` and `Icon` of the `[Desktop Entry]` group, unescaped; none for an
/// entry that is `Hidden` (deleted, by the spec).
fn desktop_entry(entry: &str) -> Option<Entry> {
    let mut in_entry = false;
    let mut name = None;
    let mut icon = None;
    for line in entry.lines().map(str::trim) {
        if line.starts_with('[') {
            in_entry = line == "[Desktop Entry]";
            continue;
        }
        if !in_entry || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        match key.trim_end() {
            "Name" => name = Some(unescape(value.trim_start())),
            "Icon" => icon = Some(unescape(value.trim_start())),
            "Hidden" if value.trim() == "true" => return None,
            _ => {}
        }
    }
    Some(Entry {
        name: name.filter(|n| !n.is_empty()),
        icon: icon.filter(|i| !i.is_empty()),
    })
}

/// The spec's escapes in string values: `\s` `\n` `\t` `\r` `\\`.
fn unescape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('s') => out.push(' '),
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some(other) => out.push(other),
            None => out.push('\\'),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_name_and_icon_of_the_desktop_entry() {
        let entry = "# a comment\n[Desktop Entry]\nType=Application\nName[de]=Konsole\n\
                     Name = Terminal\\sApp\nIcon=utilities-terminal\nExec=x\n\
                     [Desktop Action new]\nName=New Window\nIcon=other\n";
        assert_eq!(
            desktop_entry(entry),
            Some(Entry {
                name: Some("Terminal App".into()),
                icon: Some("utilities-terminal".into()),
            })
        );
        assert_eq!(
            desktop_entry("[Desktop Entry]\nName=X\nHidden=true\n"),
            None
        );
        assert_eq!(desktop_entry("[Other]\nName=X\n"), Some(Entry::default()));
    }

    #[test]
    fn without_a_desktop_file_the_id_is_the_name() {
        assert_eq!(app("beize-no-such-app", 48).name, "beize-no-such-app");
        assert_eq!(app("../etc/passwd", 48).name, "../etc/passwd");
    }

    #[test]
    fn hicolor_directories_name_their_pixels() {
        assert_eq!(pixels("48x48"), Some(Some(48)));
        assert_eq!(pixels("24x24@2"), Some(Some(48)));
        assert_eq!(pixels("scalable"), Some(None));
        assert_eq!(pixels("48x32"), None);
        assert_eq!(pixels("symbolic"), None);
        assert_eq!(pixels("4294967295x4294967295@2"), None);
    }

    #[test]
    fn the_icon_of_the_size_wins_then_svg_then_larger_then_smaller() {
        let root = std::env::temp_dir().join(format!("beize-shell-icons-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (user, system) = (root.join("user"), root.join("system"));
        let put = |dir: &Path, sized: &str, file: &str| {
            let apps = dir.join("icons/hicolor").join(sized).join("apps");
            std::fs::create_dir_all(&apps).unwrap();
            std::fs::write(apps.join(file), b"").unwrap();
            apps.join(file)
        };
        let dirs = [user.clone(), system.clone()];
        let small = put(&system, "32x32", "app.png");
        assert_eq!(find_icon("app", 48, &dirs), Some(small));
        let large = put(&system, "64x64", "app.png");
        assert_eq!(find_icon("app", 48, &dirs), Some(large));
        let svg = put(&system, "scalable", "app.svg");
        assert_eq!(find_icon("app", 48, &dirs), Some(svg));
        // A tie: the user's.
        let exact = put(&system, "48x48", "app.png");
        assert_eq!(find_icon("app", 48, &dirs), Some(exact.clone()));
        let users = put(&user, "24x24@2", "app.png");
        assert_eq!(find_icon("app", 48, &dirs), Some(users));
        // A file as it is; nothing outside the theme by a name.
        assert_eq!(find_icon(exact.to_str().unwrap(), 48, &dirs), Some(exact));
        assert_eq!(find_icon("../app", 48, &dirs), None);
        assert_eq!(find_icon("none", 48, &dirs), None);
        std::fs::remove_dir_all(&root).unwrap();
    }
}
