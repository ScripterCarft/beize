//! Beize's own fixes to upstream's code, sent upstream by the owner and
//! carried until upstream has them (CLAUDE.md §2): the kernel's in
//! `kernel/patches`, every other package's in `recipes/patches/<name>`
//! (`recipes/patches/README.md`). Nothing from third parties.
//!
//! Each patch must change the source, and one that upstream took – its
//! changes already there – stops the build, so it leaves with the update
//! that brings it.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};

use crate::paths::Paths;
use crate::recipe::Recipe;
use crate::{say, util};

/// What a package's source carries, by name and content, so that another
/// set unpacks it anew.
const APPLIED: &str = ".beize-own-patches";

/// The patches in `dir`, in name order; none if it does not exist.
pub fn list(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut patches = Vec::new();
    if !dir.is_dir() {
        return Ok(patches);
    }
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        if path.extension().is_some_and(|e| e == "patch") {
            patches.push(path);
        }
    }
    patches.sort_by_key(|p| p.file_name().map(ToOwned::to_owned));
    Ok(patches)
}

/// A package's own patches (`recipes/patches/<name>`).
pub fn of(paths: &Paths, recipe: &Recipe) -> Result<Vec<PathBuf>> {
    list(&paths.root.join("recipes/patches").join(&recipe.name))
}

/// The patches' names and contents in one digest; empty without any, so a
/// package's fingerprint stays what it was.
pub fn digest(patches: &[PathBuf]) -> Result<String> {
    if patches.is_empty() {
        return Ok(String::new());
    }
    let mut hasher = Sha256::new();
    for patch in patches {
        let name = patch.file_name().unwrap_or_default().to_string_lossy();
        hasher.update(format!("{name}\n"));
        hasher.update(fs::read(patch).with_context(|| patch.display().to_string())?);
    }
    Ok(util::hex(&hasher.finalize()))
}

/// The unpacked source of `recipe` with its own patches applied: as it is
/// if they are applied already, unpacked anew if it carries others.
pub fn apply_own(paths: &Paths, recipe: &Recipe, src: PathBuf) -> Result<PathBuf> {
    let patches = of(paths, recipe)?;
    let want = digest(&patches)?;
    let have = fs::read_to_string(src.join(APPLIED)).unwrap_or_default();
    if want == have {
        return Ok(src);
    }
    // From a fresh unpack, as rpmbuild starts: a run that stopped halfway
    // left some applied without the mark, and those would read as taken
    // upstream.
    fs::remove_file(src.join(".beize-unpacked"))?;
    let src = recipe.unpack(paths)?;
    let home = format!("recipes/patches/{}", recipe.name);
    for patch in &patches {
        say!("  applying {}", patch.display());
        git_apply(&src, patch, false, &home)?;
    }
    if !want.is_empty() {
        fs::write(src.join(APPLIED), want)?;
    }
    Ok(src)
}

/// `git apply` on the unpacked source. That tree lies inside Beize's own
/// repository, where git would take the patch's paths from the repository's
/// top and skip them silently: the ceiling keeps git out of it. The check
/// afterwards proves the patch is (or is no longer) in the tree. `home` is
/// where the patch lies, to say where it goes when upstream took it.
pub fn git_apply(src: &Path, patch: &Path, reverse: bool, home: &str) -> Result<()> {
    let git = |args: &[&str]| {
        let mut cmd = Command::new("git");
        cmd.current_dir(src)
            .env("GIT_CEILING_DIRECTORIES", src.parent().unwrap_or(src))
            .arg("apply")
            .args(args)
            .arg(patch);
        cmd
    };
    if let Err(e) = util::run(&mut git(if reverse { &["--reverse"] } else { &[] })) {
        // Already in the source: upstream took it, and it goes from here.
        if !reverse && util::run(&mut git(&["--reverse", "--check"])).is_ok() {
            bail!(
                "{} is in this source already (upstream took it): remove it from {home}",
                patch.display()
            );
        }
        return Err(e);
    }
    util::run(&mut git(if reverse {
        &["--check"]
    } else {
        &["--reverse", "--check"]
    }))
    .with_context(|| format!("{} did not change the source", patch.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_digest_is_empty_without_patches_and_follows_their_content() {
        assert_eq!(digest(&[]).unwrap(), "");
        let dir = std::env::temp_dir().join(format!("bz-patches-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let patch = dir.join("0001-a.patch");
        fs::write(&patch, "one").unwrap();
        let first = digest(std::slice::from_ref(&patch)).unwrap();
        fs::write(&patch, "two").unwrap();
        assert_ne!(digest(std::slice::from_ref(&patch)).unwrap(), first);
        fs::write(dir.join("notes.md"), "").unwrap();
        assert_eq!(list(&dir).unwrap(), vec![patch]);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn applies_once_and_knows_when_upstream_took_it() {
        let dir = std::env::temp_dir().join(format!("bz-apply-{}", std::process::id()));
        let src = dir.join("src");
        fs::create_dir_all(&src).unwrap();
        fs::write(src.join("f"), "a\n").unwrap();
        let patch = dir.join("0001.patch");
        fs::write(&patch, "--- a/f\n+++ b/f\n@@ -1 +1 @@\n-a\n+b\n").unwrap();
        git_apply(&src, &patch, false, "here").unwrap();
        assert_eq!(fs::read_to_string(src.join("f")).unwrap(), "b\n");
        let again = git_apply(&src, &patch, false, "here").unwrap_err();
        assert!(again.to_string().contains("upstream took it"), "{again:#}");
        fs::remove_dir_all(&dir).unwrap();
    }
}
