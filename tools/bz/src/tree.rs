//! Assembles file trees (the `/usr` image, the initrd) from installed packages.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, ensure};
use serde::Deserialize;

use crate::install;
use crate::packages::{BUILD_EPOCH, TARGET};
use crate::paths::Paths;
use crate::recipe::{Recipe, Stage};
use crate::util;

/// `images/base.toml`: which packages go into the image and the initrd.
/// Target dependencies (glibc, ...) are included automatically.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImageDefinition {
    pub packages: Vec<String>,
    pub initrd: Vec<String>,
}

pub fn image_definition(paths: &Paths) -> Result<ImageDefinition> {
    let path = paths.root.join("images/base.toml");
    let text = fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))
}

/// Build-time files that are never part of a running system.
const BUILD_ONLY_DIRS: [&str; 6] = [
    "usr/include",
    "usr/lib/pkgconfig",
    "usr/share/pkgconfig",
    "usr/share/man",
    "usr/share/info",
    "usr/share/doc",
];
const BUILD_ONLY_EXTENSIONS: [&str; 3] = ["a", "o", "la"];

pub fn is_build_only(path: &Path) -> bool {
    BUILD_ONLY_DIRS.iter().any(|dir| path.starts_with(dir))
        || path
            .extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| BUILD_ONLY_EXTENSIONS.contains(&ext))
}

/// The target packages among `recipes` (toolchain recipes ship nothing).
pub fn target_packages(recipes: &[Recipe]) -> impl Iterator<Item = &Recipe> {
    recipes.iter().filter(|r| r.stage == Some(Stage::Target))
}

/// Copies every file of the target packages that `keep` accepts from the
/// sysroot into `dest` and strips debug information. Returns the file count.
pub fn populate(
    paths: &Paths,
    recipes: &[Recipe],
    dest: &Path,
    keep: impl Fn(&Path) -> bool,
) -> Result<usize> {
    let mut elf_files = Vec::new();
    let mut count = 0;
    for recipe in target_packages(recipes) {
        for file in install::manifest(paths, &recipe.name)? {
            if !keep(&file) {
                continue;
            }
            let target = dest.join(&file);
            install::copy_entry(&paths.sysroot.join(&file), &target)?;
            if is_elf(&target)? {
                elf_files.push(target);
            }
            count += 1;
        }
    }
    strip_debug_info(paths, &elf_files)?;
    eprintln!(
        "  {count} files, debug info stripped from {} ELF files",
        elf_files.len()
    );
    Ok(count)
}

/// Sets every timestamp in `dir` to the build epoch, for reproducible archives.
pub fn normalize_mtimes(dir: &Path) -> Result<()> {
    util::run(
        Command::new("find")
            .arg(dir)
            .args(["-exec", "touch", "--no-dereference", "--date"])
            .arg(format!("@{BUILD_EPOCH}"))
            .args(["{}", "+"]),
    )
}

pub fn recreate(dir: &Path) -> Result<PathBuf> {
    if dir.exists() {
        fs::remove_dir_all(dir).with_context(|| format!("removing {}", dir.display()))?;
    }
    fs::create_dir_all(dir)?;
    Ok(dir.to_owned())
}

fn is_elf(path: &Path) -> Result<bool> {
    if !path.symlink_metadata()?.is_file() {
        return Ok(false);
    }
    let mut magic = [0; 4];
    let read = fs::File::open(path)?.read(&mut magic)?;
    Ok(read == 4 && magic == *b"\x7fELF")
}

/// Removes debug information; symbols needed for dynamic linking stay.
fn strip_debug_info(paths: &Paths, files: &[PathBuf]) -> Result<()> {
    let strip = paths.tools.join("bin").join(format!("{TARGET}-strip"));
    for chunk in files.chunks(256) {
        let status = Command::new(&strip)
            .arg("--strip-debug")
            .args(chunk)
            .status()
            .with_context(|| format!("starting {}", strip.display()))?;
        ensure!(status.success(), "{} failed with {status}", strip.display());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognises_build_only_files() {
        assert!(is_build_only(Path::new("usr/include/stdio.h")));
        assert!(is_build_only(Path::new("usr/lib/libc.a")));
        assert!(is_build_only(Path::new("usr/lib/crt1.o")));
        assert!(is_build_only(Path::new("usr/share/man/man1/ls.1")));
        assert!(!is_build_only(Path::new("usr/lib/libc.so.6")));
        assert!(!is_build_only(Path::new("usr/bin/bash")));
        assert!(!is_build_only(Path::new("usr/share/factory/etc/issue")));
    }

    #[test]
    fn repository_image_definition_parses() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../images/base.toml");
        let def: ImageDefinition = toml::from_str(&fs::read_to_string(path).unwrap()).unwrap();
        assert!(def.packages.contains(&"systemd".to_owned()));
        assert!(def.initrd.contains(&"systemd".to_owned()));
    }
}
