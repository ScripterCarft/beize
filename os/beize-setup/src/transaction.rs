//! The setup is all or nothing. It has the account service create the owner's account
//! and then writes /var/lib/beize/owner; a power cut in between would leave
//! an account the next setup does not know – it asks again, and the name
//! "exists already".
//!
//! So the name is written durably before the account is created. A setup
//! that finds such a name without having finished removes that account
//! first and starts from the state before.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

/// Notes that the account `name` is about to be created.
pub fn begin(pending: &Path, name: &str) -> Result<(), String> {
    write(pending, &format!("{name}\n"), 0o600)
}

/// The account an earlier setup began to create and did not finish.
pub fn unfinished(pending: &Path) -> Result<Option<String>, String> {
    match fs::read_to_string(pending) {
        Ok(name) => Ok(Some(name.trim().to_owned()).filter(|name| !name.is_empty())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!("reading {}: {error}", pending.display())),
    }
}

/// The account is made (or removed again): nothing to undo any more.
pub fn finish(pending: &Path) -> Result<(), String> {
    match fs::remove_file(pending) {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
            Err(format!("removing {}: {error}", pending.display()))
        }
        _ => sync_parent(pending),
    }
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

    #[test]
    fn remembers_an_unfinished_account_until_finished() {
        let dir = std::env::temp_dir().join(format!("beize-setup-pending-{}", std::process::id()));
        let pending = dir.join("setup-pending");
        assert_eq!(unfinished(&pending).unwrap(), None);
        begin(&pending, "joe").unwrap();
        assert_eq!(unfinished(&pending).unwrap().as_deref(), Some("joe"));
        finish(&pending).unwrap();
        assert_eq!(unfinished(&pending).unwrap(), None);
        // Finishing twice (a cut right after finishing) is fine.
        finish(&pending).unwrap();
        fs::remove_dir_all(dir).unwrap();
    }
}
