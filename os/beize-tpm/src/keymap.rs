//! The owner's console keymap in an EFI variable (ADR 0017), as macOS keeps
//! the keyboard layout in NVRAM for the FileVault prompt: the initrd is the
//! same for every computer, yet the recovery key is typed there with the
//! owner's keyboard.
//!
//! `beize-tpm keymap save` (system, whenever /etc/vconsole.conf changes)
//! copies its KEYMAP= into the variable, or removes the variable when there
//! is none. `beize-tpm keymap load` (initrd, before systemd-vconsole-setup)
//! writes the variable's keymap into the initrd's /etc/vconsole.conf.
//!
//! The variable is not measured and anyone who can write EFI variables can
//! change it, so the initrd takes only the name of a keymap it has (letters,
//! digits, `.`, `_`, `-`); anything else leaves the kernel's US keymap. The
//! worst a forged value does is a keyboard that types wrong characters.

use std::ffi::c_int;
use std::fs::{self, File, OpenOptions};
use std::io::{ErrorKind, Write};
use std::os::fd::AsRawFd;
use std::path::Path;

/// BeizeKeymap under Beize's own vendor GUID.
const VARIABLE: &str = "/sys/firmware/efi/efivars/BeizeKeymap-e6e221d0-db73-4fa8-99da-ba41e1b78888";
/// Non-volatile, readable at boot and from the running system.
const ATTRIBUTES: u32 = 0x1 | 0x2 | 0x4;
const VCONSOLE_CONF: &str = "/etc/vconsole.conf";
const KEYMAPS: &str = "/usr/share/keymaps";

pub fn run(command: Option<&str>) -> Result<(), String> {
    match command {
        Some("save") => save(),
        Some("load") => load(),
        _ => Err("usage: beize-tpm keymap save | load".into()),
    }
}

fn save() -> Result<(), String> {
    let wanted = match fs::read_to_string(VCONSOLE_CONF) {
        Ok(conf) => keymap_of(&conf),
        Err(e) if e.kind() == ErrorKind::NotFound => None,
        Err(e) => return Err(format!("reading {VCONSOLE_CONF}: {e}")),
    };
    let wanted = wanted.filter(|name| valid(name));
    let stored = read_variable()?;
    if stored == wanted {
        return Ok(());
    }
    match &wanted {
        Some(name) => {
            write_variable(name)?;
            println!("The console keymap {name} is used in the initrd from the next start on.");
        }
        None => {
            remove_variable()?;
            println!(
                "No console keymap set: the initrd uses the US keymap from the next start on."
            );
        }
    }
    Ok(())
}

fn load() -> Result<(), String> {
    let Some(name) = read_variable()? else {
        return Ok(());
    };
    if !valid(&name) || !has_keymap(Path::new(KEYMAPS), &name) {
        println!("Ignoring the stored console keymap: there is no keymap {name:?}.");
        return Ok(());
    }
    fs::write(VCONSOLE_CONF, format!("KEYMAP={name}\n"))
        .map_err(|e| format!("writing {VCONSOLE_CONF}: {e}"))?;
    println!("Console keymap: {name}.");
    Ok(())
}

/// KEYMAP= of a vconsole.conf (the last one counts, as for systemd).
fn keymap_of(conf: &str) -> Option<String> {
    conf.lines()
        .rev()
        .find_map(|line| line.trim().strip_prefix("KEYMAP="))
        .map(|value| value.trim().trim_matches('"').to_owned())
        .filter(|value| !value.is_empty())
}

/// A keymap name as kbd names them; never a path.
fn valid(name: &str) -> bool {
    (1..=64).contains(&name.len())
        && !name.starts_with(['.', '-'])
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

/// Whether `<name>.map` is anywhere below `dir`, where loadkeys looks.
fn has_keymap(dir: &Path, name: &str) -> bool {
    let file = format!("{name}.map");
    let mut pending = vec![dir.to_path_buf()];
    while let Some(dir) = pending.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
            } else if entry.file_name().to_str() == Some(&file) {
                return true;
            }
        }
    }
    false
}

/// The variable's value: 4 attribute bytes, then the keymap name.
fn read_variable() -> Result<Option<String>, String> {
    match fs::read(VARIABLE) {
        Ok(raw) => Ok(raw
            .get(4..)
            .and_then(|name| String::from_utf8(name.to_vec()).ok())
            .filter(|name| !name.is_empty())),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("reading {VARIABLE}: {e}")),
    }
}

/// efivarfs takes attributes and value in a single write.
fn write_variable(name: &str) -> Result<(), String> {
    if Path::new(VARIABLE).exists() {
        make_mutable(VARIABLE)?;
    }
    let mut value = ATTRIBUTES.to_le_bytes().to_vec();
    value.extend_from_slice(name.as_bytes());
    OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(VARIABLE)
        .and_then(|mut file| file.write_all(&value))
        .map_err(|e| format!("writing {VARIABLE}: {e}"))
}

fn remove_variable() -> Result<(), String> {
    if !Path::new(VARIABLE).exists() {
        return Ok(());
    }
    make_mutable(VARIABLE)?;
    fs::remove_file(VARIABLE).map_err(|e| format!("removing {VARIABLE}: {e}"))
}

/// efivarfs marks existing variables immutable, so that `rm -rf` cannot
/// delete firmware settings by accident; changing one takes clearing that
/// flag first (as systemd and efivar do).
fn make_mutable(path: &str) -> Result<(), String> {
    const FS_IOC_GETFLAGS: u64 = 0x8008_6601;
    const FS_IOC_SETFLAGS: u64 = 0x4008_6602;
    const FS_IMMUTABLE_FL: c_int = 0x10;
    unsafe extern "C" {
        fn ioctl(fd: c_int, request: u64, ...) -> c_int;
    }
    let file = File::open(path).map_err(|e| format!("opening {path}: {e}"))?;
    let mut flags: c_int = 0;
    // SAFETY: both requests read or write one int through a valid pointer.
    unsafe {
        if ioctl(file.as_raw_fd(), FS_IOC_GETFLAGS, &raw mut flags) != 0 {
            return Err(format!(
                "reading the flags of {path}: {}",
                std::io::Error::last_os_error()
            ));
        }
        if flags & FS_IMMUTABLE_FL != 0 {
            flags &= !FS_IMMUTABLE_FL;
            if ioctl(file.as_raw_fd(), FS_IOC_SETFLAGS, &raw const flags) != 0 {
                return Err(format!(
                    "making {path} writable: {}",
                    std::io::Error::last_os_error()
                ));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_keymap_from_vconsole_conf() {
        let conf = "# Written by systemd-localed(8)\nKEYMAP=de-latin1-nodeadkeys\nXKBLAYOUT=de\n";
        assert_eq!(keymap_of(conf).as_deref(), Some("de-latin1-nodeadkeys"));
        assert_eq!(keymap_of("KEYMAP=\"us\"\n").as_deref(), Some("us"));
        assert_eq!(keymap_of("XKBLAYOUT=de\n"), None);
        assert_eq!(keymap_of("KEYMAP=\n"), None);
    }

    #[test]
    fn takes_only_keymap_names() {
        for name in [
            "de",
            "de-latin1-nodeadkeys",
            "fr_CH-latin1",
            "br-abnt2",
            "mac-us",
        ] {
            assert!(valid(name), "{name}");
        }
        for name in [
            "",
            "../etc/passwd",
            "/usr/share/keymaps/de",
            "de map",
            ".hidden",
            "-q",
            "dé",
        ] {
            assert!(!valid(name), "{name}");
        }
        assert!(!valid(&"a".repeat(65)));
    }

    #[test]
    fn finds_keymaps_below_the_directory() {
        let dir = std::env::temp_dir().join(format!("beize-keymaps-{}", std::process::id()));
        fs::create_dir_all(dir.join("i386/qwertz")).unwrap();
        fs::write(dir.join("i386/qwertz/de-latin1.map"), "").unwrap();
        assert!(has_keymap(&dir, "de-latin1"));
        assert!(!has_keymap(&dir, "de"));
        fs::remove_dir_all(&dir).unwrap();
    }
}
