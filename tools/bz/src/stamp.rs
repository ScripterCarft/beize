//! Stamps: a fingerprint of everything a build step reads, stored once the
//! step has succeeded. A step whose fingerprint matches its stamp and whose
//! outputs exist is skipped.
//!
//! bz itself is one of the inputs: a step is redone whenever the code that
//! does it may have changed.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};

use crate::paths::Paths;
use crate::util;

/// Collects the inputs of a build step.
pub struct Fingerprint(Sha256);

impl Fingerprint {
    /// Starts with the running bz binary.
    pub fn new() -> Result<Self> {
        let mut hasher = Sha256::new();
        hasher.update(format!("bz {}\n", bz_digest()?));
        Ok(Self(hasher))
    }

    pub fn text(mut self, text: &str) -> Self {
        self.0.update(format!("text {}\n", text.len()));
        self.0.update(text);
        self
    }

    /// A file's path and content.
    pub fn file(mut self, path: &Path) -> Result<Self> {
        self.0.update(format!(
            "file {} {}\n",
            path.display(),
            util::sha256_file(path)?
        ));
        Ok(self)
    }

    /// Every file under `dir` (paths relative to it, contents; symlinks by
    /// their target), in a stable order.
    pub fn tree(mut self, dir: &Path) -> Result<Self> {
        self.0.update(format!("tree {}\n", dir.display()));
        hash_tree(&mut self.0, dir, dir)?;
        Ok(self)
    }

    pub fn finish(self) -> String {
        hex(&self.0.finalize())
    }
}

/// Adds every file under `entry` to `hasher`: its path relative to `root`
/// and the SHA-256 of its content (of its target, for a symlink).
pub fn hash_tree(hasher: &mut Sha256, root: &Path, entry: &Path) -> Result<()> {
    let mut pending = vec![entry.to_owned()];
    let mut files = Vec::new();
    while let Some(path) = pending.pop() {
        let meta =
            fs::symlink_metadata(&path).with_context(|| format!("reading {}", path.display()))?;
        if meta.is_dir() {
            for child in fs::read_dir(&path)? {
                pending.push(child?.path());
            }
        } else {
            files.push((path, meta.is_symlink()));
        }
    }
    files.sort();
    for (file, is_symlink) in files {
        let rel = file.strip_prefix(root).unwrap_or(&file);
        hasher.update(format!("{}\n", rel.display()));
        if is_symlink {
            hasher.update(format!("-> {}", fs::read_link(&file)?.display()));
        } else {
            hasher.update(util::sha256_file(&file)?);
        }
    }
    Ok(())
}

/// Where the fingerprint of `step` is stored; a later step can take it as an
/// input that stands for everything the earlier one was built from.
pub fn file(paths: &Paths, step: &str) -> PathBuf {
    paths.work.join("stamps").join(format!("step-{step}"))
}

/// The stored fingerprint of a build step and the files it produces.
pub struct Stamp {
    path: PathBuf,
    fingerprint: String,
    outputs: Vec<PathBuf>,
}

impl Stamp {
    pub fn new(paths: &Paths, step: &str, fingerprint: Fingerprint, outputs: &[&Path]) -> Self {
        Self {
            path: file(paths, step),
            fingerprint: fingerprint.finish(),
            outputs: outputs.iter().map(|p| p.to_path_buf()).collect(),
        }
    }

    /// Whether the step ran with exactly these inputs and its outputs are
    /// still there.
    pub fn is_current(&self) -> bool {
        self.outputs.iter().all(|output| output.exists())
            && fs::read_to_string(&self.path).is_ok_and(|stored| stored == self.fingerprint)
    }

    /// Forgets the stamp before the step runs: a step that fails halfway
    /// must not count as done when its old inputs come back.
    pub fn clear(&self) -> Result<()> {
        match fs::remove_file(&self.path) {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(error.into()),
            _ => Ok(()),
        }
    }

    pub fn write(&self) -> Result<()> {
        if let Some(dir) = self.path.parent() {
            fs::create_dir_all(dir)?;
        }
        fs::write(&self.path, &self.fingerprint)?;
        Ok(())
    }
}

/// SHA-256 of the running bz binary, computed once.
fn bz_digest() -> Result<&'static str> {
    static DIGEST: OnceLock<String> = OnceLock::new();
    if let Some(digest) = DIGEST.get() {
        return Ok(digest);
    }
    let exe = std::env::current_exe().context("locating the bz binary")?;
    let digest = util::sha256_file(&exe)?;
    Ok(DIGEST.get_or_init(|| digest))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("bz-stamp-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn tree_digest(dir: &Path) -> String {
        Fingerprint::new().unwrap().tree(dir).unwrap().finish()
    }

    #[test]
    fn a_tree_changes_with_content_names_and_link_targets() {
        let dir = scratch("tree");
        fs::create_dir_all(dir.join("a")).unwrap();
        fs::write(dir.join("a/file"), "one").unwrap();
        std::os::unix::fs::symlink("file", dir.join("a/link")).unwrap();
        let before = tree_digest(&dir);
        assert_eq!(tree_digest(&dir), before, "stable");

        fs::write(dir.join("a/file"), "two").unwrap();
        let content = tree_digest(&dir);
        assert_ne!(content, before);

        fs::rename(dir.join("a/file"), dir.join("a/renamed")).unwrap();
        let renamed = tree_digest(&dir);
        assert_ne!(renamed, content);

        fs::remove_file(dir.join("a/link")).unwrap();
        std::os::unix::fs::symlink("renamed", dir.join("a/link")).unwrap();
        assert_ne!(tree_digest(&dir), renamed);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_stamp_is_current_only_with_the_same_inputs_and_its_outputs() {
        let dir = scratch("stamp");
        let paths = Paths::new().unwrap();
        let output = dir.join("out");
        let stamp = |input: &str| {
            let mut stamp = Stamp::new(
                &paths,
                "test",
                Fingerprint::new().unwrap().text(input),
                &[&output],
            );
            stamp.path = dir.join("stamp");
            stamp
        };
        fs::write(&output, "built").unwrap();
        assert!(!stamp("a").is_current(), "never written");
        stamp("a").write().unwrap();
        assert!(stamp("a").is_current());
        assert!(!stamp("b").is_current(), "other inputs");
        fs::remove_file(&output).unwrap();
        assert!(!stamp("a").is_current(), "output gone");
        fs::write(&output, "built").unwrap();
        stamp("a").clear().unwrap();
        assert!(!stamp("a").is_current(), "cleared");
        fs::remove_dir_all(&dir).unwrap();
    }
}
