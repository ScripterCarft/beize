//! Apps' desktop files (freedesktop's Desktop Entry Specification): found by
//! the app's id – the window's `app_id` – in `applications/` of the XDG
//! data directories, the user's first (XDG Base Directory Specification).
//! The shell reads an app's name and icon here, `apps` the command that
//! starts it (`Exec`).

use std::path::{Path, PathBuf};

/// What Beize reads of a desktop file's `[Desktop Entry]`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Entry {
    pub name: Option<String>,
    pub icon: Option<String>,
    pub exec: Option<String>,
}

/// The desktop file of `app_id` and what it says; none for an id that is
/// no file's name, without one, or one deleted (`Hidden`).
pub fn find(app_id: &str) -> Option<(PathBuf, Entry)> {
    find_in(app_id, &data_dirs())
}

/// `find` in the data directories `dirs`, the first first.
fn find_in(app_id: &str, dirs: &[PathBuf]) -> Option<(PathBuf, Entry)> {
    if app_id.is_empty() || app_id.contains('/') {
        return None;
    }
    dirs.iter()
        .map(|dir| dir.join("applications").join(format!("{app_id}.desktop")))
        .find_map(|path| Some((std::fs::read_to_string(&path).ok()?, path)))
        // The first file of that name counts, a hidden one as none at all
        // (the spec's `Hidden`; GLib's `GDesktopAppInfo`).
        .and_then(|(text, path)| Some((path, entry(&text)?)))
}

/// `$XDG_DATA_HOME` (`~/.local/share`), then `$XDG_DATA_DIRS`
/// (`/usr/local/share:/usr/share`).
pub fn data_dirs() -> Vec<PathBuf> {
    let var = |name| std::env::var_os(name).filter(|v| !v.is_empty());
    let home = var("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| var("HOME").map(|home| PathBuf::from(home).join(".local/share")));
    let system = var("XDG_DATA_DIRS")
        .map(|dirs| std::env::split_paths(&dirs).collect())
        .unwrap_or_else(|| vec!["/usr/local/share".into(), "/usr/share".into()]);
    home.into_iter().chain(system).collect()
}

/// `Name`, `Icon` and `Exec` of the `[Desktop Entry]` group, unescaped;
/// none for an entry that is `Hidden` (deleted, by the spec).
pub fn entry(text: &str) -> Option<Entry> {
    let mut in_entry = false;
    let mut found = Entry::default();
    for line in text.lines().map(str::trim) {
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
        let value = unescape(value.trim_start());
        match key.trim_end() {
            "Name" => found.name = Some(value),
            "Icon" => found.icon = Some(value),
            "Exec" => found.exec = Some(value),
            "Hidden" if value.trim() == "true" => return None,
            _ => {}
        }
    }
    let given = |v: Option<String>| v.filter(|v| !v.is_empty());
    Some(Entry {
        name: given(found.name),
        icon: given(found.icon),
        exec: given(found.exec),
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
            Some(other) => {
                // Kept for the quoting rule (`command`): `\"`, `` \` ``, `\$`.
                if matches!(other, '"' | '`' | '$') {
                    out.push('\\');
                }
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

/// The command `exec` (an entry's `Exec`, unescaped) runs with no files
/// to open, as the spec's "The Exec key" reads it: arguments apart by
/// spaces, or in double quotes with `\"`, `` \` ``, `\$` and `\\` inside;
/// the field codes for files and URLs dropped, `%i` the icon's
/// `--icon <Icon>`, `%c` the name, `%k` the desktop file (`path`), `%%` a
/// percent sign, the deprecated ones dropped. None for a malformed one.
pub fn command(exec: &str, entry: &Entry, path: &Path) -> Option<Vec<String>> {
    // Split first: a quoted argument's percent signs are field codes too.
    let mut words: Vec<String> = Vec::new();
    let mut chars = exec.chars().peekable();
    loop {
        while chars.next_if(|c| *c == ' ' || *c == '\t').is_some() {}
        let Some(&first) = chars.peek() else {
            break;
        };
        let mut word = String::new();
        if first == '"' {
            chars.next();
            loop {
                match chars.next()? {
                    '"' => break,
                    '\\' => match chars.next()? {
                        c @ ('"' | '`' | '$' | '\\') => word.push(c),
                        _ => return None,
                    },
                    c => word.push(c),
                }
            }
            // A quoted argument ends where it closes.
            if chars.peek().is_some_and(|c| *c != ' ' && *c != '\t') {
                return None;
            }
        } else {
            while let Some(c) = chars.next_if(|c| *c != ' ' && *c != '\t') {
                if c == '"' {
                    return None;
                }
                word.push(c);
            }
        }
        words.push(word);
    }
    let mut args = Vec::new();
    for word in words {
        match word.as_str() {
            "%f" | "%F" | "%u" | "%U" | "%d" | "%D" | "%n" | "%N" | "%v" | "%m" => continue,
            "%i" => {
                if let Some(icon) = &entry.icon {
                    args.push("--icon".to_owned());
                    args.push(icon.clone());
                }
                continue;
            }
            _ => {}
        }
        let mut arg = String::with_capacity(word.len());
        let mut chars = word.chars();
        while let Some(c) = chars.next() {
            if c != '%' {
                arg.push(c);
                continue;
            }
            match chars.next()? {
                '%' => arg.push('%'),
                'c' => arg.push_str(entry.name.as_deref().unwrap_or_default()),
                'k' => arg.push_str(&path.to_string_lossy()),
                'f' | 'F' | 'u' | 'U' | 'd' | 'D' | 'n' | 'N' | 'v' | 'm' | 'i' => {}
                _ => return None,
            }
        }
        args.push(arg);
    }
    (!args.is_empty()).then_some(args)
}

/// The program `name`: a path as it is, else the first executable of that
/// name in `$PATH`, as the spec asks of `Exec`'s first argument.
pub fn program(name: &str) -> Option<PathBuf> {
    if name.contains('/') {
        return Path::new(name).is_absolute().then(|| PathBuf::from(name));
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|file| {
            use std::os::unix::fs::PermissionsExt;
            file.metadata()
                .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn named(name: &str, icon: &str) -> Entry {
        Entry {
            name: Some(name.into()),
            icon: Some(icon.into()),
            exec: None,
        }
    }

    #[test]
    fn reads_name_icon_and_exec_of_the_desktop_entry() {
        let text = "# a comment\n[Desktop Entry]\nType=Application\nName[de]=Konsole\n\
                    Name = Terminal\\sApp\nIcon=utilities-terminal\nExec=x %U\n\
                    [Desktop Action new]\nName=New Window\nIcon=other\nExec=y\n";
        assert_eq!(
            entry(text),
            Some(Entry {
                name: Some("Terminal App".into()),
                icon: Some("utilities-terminal".into()),
                exec: Some("x %U".into()),
            })
        );
        assert_eq!(entry("[Desktop Entry]\nName=X\nHidden=true\n"), None);
        assert_eq!(entry("[Other]\nName=X\n"), Some(Entry::default()));
    }

    #[test]
    fn exec_drops_files_and_fills_in_the_rest() {
        let path = Path::new("/usr/share/applications/app.desktop");
        let app = named("My App", "app-icon");
        let run = |exec: &str| command(exec, &app, path);
        assert_eq!(run("/usr/bin/app"), Some(vec!["/usr/bin/app".into()]));
        assert_eq!(run("app %U"), Some(vec!["app".into()]));
        assert_eq!(
            run("app --name=%c %i -f %f"),
            Some(vec![
                "app".into(),
                "--name=My App".into(),
                "--icon".into(),
                "app-icon".into(),
                "-f".into()
            ])
        );
        assert_eq!(
            run("app --from %k 100%%"),
            Some(vec![
                "app".into(),
                "--from".into(),
                "/usr/share/applications/app.desktop".into(),
                "100%".into()
            ])
        );
    }

    #[test]
    fn exec_quotes_as_the_spec_does() {
        let (app, path) = (Entry::default(), Path::new("/a.desktop"));
        // `Exec="/opt/my app/run" "say \"hi\"" "\$HOME" "a\\b"`, as the
        // file writes it, a backslash written twice (the string escapes
        // come first, `entry`).
        let exec = unescape(r#""/opt/my app/run" "say \"hi\"" "\$HOME" "a\\\\b""#);
        assert_eq!(
            command(&exec, &app, path),
            Some(vec![
                "/opt/my app/run".into(),
                "say \"hi\"".into(),
                "$HOME".into(),
                "a\\b".into()
            ])
        );
        // Unclosed, or a quote inside a word: malformed.
        assert_eq!(command("\"app", &app, path), None);
        assert_eq!(command("ap\"p\"", &app, path), None);
        assert_eq!(command("app %z", &app, path), None);
        assert_eq!(command("%U", &app, path), None);
    }

    #[test]
    fn a_program_by_name_is_looked_up_in_path() {
        assert_eq!(program("/usr/lib/beize/x"), Some("/usr/lib/beize/x".into()));
        assert_eq!(program("rel/x"), None);
        assert!(program("sh").is_some_and(|p| p.is_absolute()));
        assert_eq!(program("beize-no-such-program"), None);
    }

    #[test]
    fn the_users_file_wins_and_hidden_hides_the_app() {
        let root = std::env::temp_dir().join(format!("beize-desktop-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (user, system) = (root.join("user"), root.join("system"));
        let put = |dir: &Path, text: &str| {
            let apps = dir.join("applications");
            std::fs::create_dir_all(&apps).unwrap();
            std::fs::write(apps.join("app.desktop"), text).unwrap();
        };
        let dirs = [user.clone(), system.clone()];
        put(&system, "[Desktop Entry]\nName=System\nExec=app\n");
        assert_eq!(
            find_in("app", &dirs).unwrap().1.name.as_deref(),
            Some("System")
        );
        put(&user, "[Desktop Entry]\nName=Mine\nExec=app\n");
        assert_eq!(
            find_in("app", &dirs).unwrap().1.name.as_deref(),
            Some("Mine")
        );
        // Hidden: as if no file of that name were anywhere.
        put(&user, "[Desktop Entry]\nName=Mine\nHidden=true\n");
        assert_eq!(find_in("app", &dirs), None);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn without_a_desktop_file_nothing_is_found() {
        assert_eq!(find("beize-no-such-app"), None);
        assert_eq!(find("../etc/passwd"), None);
        assert_eq!(find(""), None);
    }
}
