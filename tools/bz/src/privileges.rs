//! No file in an image grants privileges by itself (ADR 0018): no setuid or
//! setgid bit. Privileges come from the service manager (run0, units), never
//! from the program a user starts.
//!
//! File capabilities need no check of their own: they live in the
//! `security.capability` extended attribute, and neither image carries
//! extended attributes – the /usr image is written without them (`mkfs.erofs
//! -x-1`), the initrd is a newc cpio archive, which has no place for them.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use anyhow::{Result, ensure};

use crate::say;

const SETUID: u32 = 0o4000;
const SETGID: u32 = 0o2000;

/// Fails if a file under `root` is setuid or setgid; `what` names the tree.
pub fn check_tree(root: &Path, what: &str) -> Result<()> {
    let mut found = Vec::new();
    let mut count = 0;
    collect(root, root, &mut found, &mut count)?;
    ensure!(
        found.is_empty(),
        "{} files in {what} are setuid or setgid (ADR 0018: privileges come from run0 \
         or the unit, not from the file); install them without the bit:\n  {}",
        found.len(),
        found.join("\n  ")
    );
    say!("  ok: none of the {count} files in {what} is setuid or setgid");
    Ok(())
}

fn collect(root: &Path, dir: &Path, found: &mut Vec<String>, count: &mut usize) -> Result<()> {
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        let meta = fs::symlink_metadata(&path)?;
        if meta.is_dir() {
            collect(root, &path, found, count)?;
        } else if !meta.is_symlink() {
            *count += 1;
            if let Some(bits) = privileged(meta.permissions().mode()) {
                found.push(format!("/{} ({bits})", path.strip_prefix(root)?.display()));
            }
        }
        // Directories may carry setgid (group inheritance), which grants nothing.
    }
    Ok(())
}

fn privileged(mode: u32) -> Option<&'static str> {
    match (mode & SETUID != 0, mode & SETGID != 0) {
        (true, true) => Some("setuid, setgid"),
        (true, false) => Some("setuid"),
        (false, true) => Some("setgid"),
        (false, false) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_setuid_and_setgid_files() {
        let dir = std::env::temp_dir().join(format!("bz-privileges-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("usr/bin")).unwrap();
        for (name, mode) in [("plain", 0o755), ("su", 0o4755), ("wall", 0o2755)] {
            let file = dir.join("usr/bin").join(name);
            fs::write(&file, "").unwrap();
            fs::set_permissions(&file, fs::Permissions::from_mode(mode)).unwrap();
        }
        let err = check_tree(&dir, "test").unwrap_err().to_string();
        assert!(err.contains("/usr/bin/su (setuid)"), "{err}");
        assert!(err.contains("/usr/bin/wall (setgid)"), "{err}");
        assert!(!err.contains("plain"), "{err}");

        fs::remove_file(dir.join("usr/bin/su")).unwrap();
        fs::remove_file(dir.join("usr/bin/wall")).unwrap();
        check_tree(&dir, "test").unwrap();
        fs::remove_dir_all(&dir).unwrap();
    }
}
