//! The account setup changes three files – /etc/shadow, /etc/group and
//! /etc/passwd – which cannot be replaced together in one step. A power cut
//! between them would leave a half-made account: on the next boot the setup
//! asks again, but the name "exists already" or gets a second entry.
//!
//! So the setup is a transaction: before the first change, the three files
//! are saved durably, completed by a marker. A setup that finds a complete
//! copy without having finished (no /var/lib/beize/owner) puts the files
//! back first and starts from the state before. Copies without the marker
//! are from a save that never finished, before anything was changed.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

/// The files the setup changes.
pub const FILES: [&str; 3] = ["shadow", "group", "passwd"];
/// Written last into the copy: it is complete.
const COMPLETE: &str = "complete";

/// Saves the files of `etc` into `saved`, durably, before they change.
pub fn save(etc: &Path, saved: &Path) -> Result<(), String> {
    discard(saved)?;
    for name in FILES {
        copy(&etc.join(name), &saved.join(name))?;
    }
    write(&saved.join(COMPLETE), "", 0o644)
}

/// Puts the files saved in `saved` back into `etc` if an earlier setup did
/// not finish; returns whether it did.
pub fn roll_back(etc: &Path, saved: &Path) -> Result<bool, String> {
    if !saved.join(COMPLETE).exists() {
        discard(saved)?;
        return Ok(false);
    }
    for name in FILES {
        copy(&saved.join(name), &etc.join(name))?;
    }
    discard(saved)?;
    Ok(true)
}

/// Removes the copy once the setup has finished (or was never begun).
pub fn discard(saved: &Path) -> Result<(), String> {
    match fs::remove_dir_all(saved) {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
            Err(format!("removing {}: {error}", saved.display()))
        }
        _ => sync_parent(saved),
    }
}

/// Copies `from` to `to` with its mode (shadow: 000), durably.
fn copy(from: &Path, to: &Path) -> Result<(), String> {
    let mode = fs::metadata(from)
        .map_err(|e| format!("reading {}: {e}", from.display()))?
        .permissions()
        .mode()
        & 0o7777;
    write(to, &read(from)?, mode)
}

pub fn read(path: &Path) -> Result<String, String> {
    fs::read_to_string(path).map_err(|e| format!("reading {}: {e}", path.display()))
}

/// Replaces `path` atomically and durably: a temporary file in the same
/// directory, synced, renamed over it, and the directory synced – only then
/// does the new name survive a power cut.
pub fn write(path: &Path, content: &str, mode: u32) -> Result<(), String> {
    let dir = path.parent().ok_or("a path without a directory")?;
    fs::create_dir_all(dir).map_err(|e| format!("creating {}: {e}", dir.display()))?;
    let temporary = path.with_extension("beize-setup");
    fs::write(&temporary, content).map_err(|e| format!("writing {}: {e}", temporary.display()))?;
    fs::set_permissions(&temporary, fs::Permissions::from_mode(mode))
        .map_err(|e| format!("protecting {}: {e}", temporary.display()))?;
    fs::File::open(&temporary)
        .and_then(|f| f.sync_all())
        .map_err(|e| format!("syncing {}: {e}", temporary.display()))?;
    fs::rename(&temporary, path).map_err(|e| format!("replacing {}: {e}", path.display()))?;
    sync_parent(path)
}

fn sync_parent(path: &Path) -> Result<(), String> {
    let dir = path.parent().ok_or("a path without a directory")?;
    match fs::File::open(dir).and_then(|d| d.sync_all()) {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
            Err(format!("syncing {}: {error}", dir.display()))
        }
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn system(name: &str) -> (std::path::PathBuf, std::path::PathBuf) {
        let root = std::env::temp_dir().join(format!("beize-setup-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let etc = root.join("etc");
        // Readable without root (the real shadow is 000; root reads it anyway).
        for file in FILES {
            write(&etc.join(file), &format!("{file} before\n"), 0o600).unwrap();
        }
        (etc, root.join("var/lib/beize/setup-rollback"))
    }

    #[test]
    fn a_setup_cut_off_halfway_is_undone() {
        let (etc, saved) = system("cut");
        save(&etc, &saved).unwrap();
        // Power cut after shadow and group, before passwd.
        write(&etc.join("shadow"), "shadow after\n", 0o600).unwrap();
        write(&etc.join("group"), "group after\n", 0o644).unwrap();
        assert!(roll_back(&etc, &saved).unwrap());
        // The mode came back with the content (group was 644 after the cut).
        for file in FILES {
            assert_eq!(read(&etc.join(file)).unwrap(), format!("{file} before\n"));
        }
        assert!(!saved.exists());
        assert_eq!(
            fs::metadata(etc.join("group"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        fs::remove_dir_all(etc.parent().unwrap()).unwrap();
    }

    #[test]
    fn an_unfinished_copy_restores_nothing() {
        let (etc, saved) = system("unfinished");
        // Power cut while saving: no marker, and nothing was changed yet.
        write(&saved.join("shadow"), "half\n", 0o600).unwrap();
        write(&etc.join("group"), "group now\n", 0o644).unwrap();
        assert!(!roll_back(&etc, &saved).unwrap());
        assert_eq!(read(&etc.join("group")).unwrap(), "group now\n");
        assert!(!saved.exists());
        fs::remove_dir_all(etc.parent().unwrap()).unwrap();
    }
}
