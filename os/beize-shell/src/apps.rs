//! What the shell knows of an app: its name, from its desktop file
//! (freedesktop's Desktop Entry Specification), found by its id – the
//! window's `app_id` – in `applications/` of the XDG data directories, the
//! user's first (XDG Base Directory Specification). Without a desktop file
//! the id stands in for the name. Icons come with the symbols (ADR 0028 d).

use std::path::PathBuf;

/// The name of the app `app_id`, as its desktop file has it.
pub fn name(app_id: &str) -> String {
    if !app_id.is_empty()
        && !app_id.contains('/')
        && let Some(name) = data_dirs()
            .into_iter()
            .map(|dir| dir.join("applications").join(format!("{app_id}.desktop")))
            .find_map(|path| std::fs::read_to_string(path).ok())
            .and_then(|entry| entry_name(&entry))
    {
        return name;
    }
    app_id.to_owned()
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

/// `Name` of the `[Desktop Entry]` group, unescaped; none for an entry that
/// is `Hidden` (deleted, by the spec).
fn entry_name(entry: &str) -> Option<String> {
    let mut in_entry = false;
    let mut name = None;
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
            "Hidden" if value.trim() == "true" => return None,
            _ => {}
        }
    }
    name.filter(|n| !n.is_empty())
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
    fn reads_the_name_of_the_desktop_entry() {
        let entry = "# a comment\n[Desktop Entry]\nType=Application\nName[de]=Konsole\n\
                     Name = Terminal\\sApp\nExec=x\n[Desktop Action new]\nName=New Window\n";
        assert_eq!(entry_name(entry).as_deref(), Some("Terminal App"));
        assert_eq!(entry_name("[Desktop Entry]\nName=X\nHidden=true\n"), None);
        assert_eq!(entry_name("[Other]\nName=X\n"), None);
    }

    #[test]
    fn without_a_desktop_file_the_id_is_the_name() {
        assert_eq!(name("beize-no-such-app"), "beize-no-such-app");
        assert_eq!(name("../etc/passwd"), "../etc/passwd");
    }
}
