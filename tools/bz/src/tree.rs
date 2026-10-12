//! Assembles file trees (the `/usr` image, the initrd) from installed packages.

use std::fs;
use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, ensure};
use serde::Deserialize;

use crate::install;
use crate::packages::{self, BUILD_EPOCH, TARGET};
use crate::paths::Paths;
use crate::recipe::{Recipe, Stage};
use crate::say;
use crate::stamp::Fingerprint;
use crate::util;

/// The image definition of `/usr`.
pub const IMAGE_DEFINITION: &str = "images/base.toml";

/// `images/base.toml`: which packages go into the `/usr` image. Target
/// dependencies (glibc, ...) are included automatically.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImageDefinition {
    pub packages: Vec<String>,
}

pub fn image_definition(paths: &Paths) -> Result<ImageDefinition> {
    read_toml(&paths.root.join(IMAGE_DEFINITION))
}

pub fn read_toml<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T> {
    let text = fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))
}

/// What a tree assembled from `recipes` and the repository's `system/<own>`
/// files depends on: its definition (relative to the repository), the build
/// of every package (including the toolchain that strips them) and those files.
pub fn assembly_inputs(
    paths: &Paths,
    definition: &str,
    recipes: &[Recipe],
    own: &str,
) -> Result<Fingerprint> {
    let mut inputs = Fingerprint::new()?.file(&paths.root.join(definition))?;
    for recipe in recipes {
        inputs = inputs.text(&format!(
            "{} {}",
            recipe.name,
            packages::built_fingerprint(paths, &recipe.name)?
        ));
    }
    inputs.tree(&paths.root.join("system").join(own))
}

/// Build-time files that are never part of a running system.
const BUILD_ONLY_DIRS: [&str; 10] = [
    // Tools that bz runs on the build host from the sysroot (e.g. ukify).
    "usr/lib/beize-build",
    "usr/include",
    "usr/lib/pkgconfig",
    "usr/share/pkgconfig",
    "usr/lib/cmake",
    // Autoconf macros and gettext's extraction rules.
    "usr/share/aclocal",
    "usr/share/gettext",
    "usr/share/man",
    "usr/share/info",
    "usr/share/doc",
];
const BUILD_ONLY_EXTENSIONS: [&str; 3] = ["a", "o", "la"];

/// Whether `path` (relative to `sysroot`) is only for building: by place and
/// extension, and the linker names of libraries – `usr/lib/libfoo.so` as a
/// link to the versioned library or as a linker script (libc.so), what
/// Fedora's -devel packages hold. Programs load libraries by their SONAME
/// (`libfoo.so.1`); a library whose SONAME has no version is a real file
/// and stays (glibc's libmemusage.so).
pub fn is_build_only(sysroot: &Path, path: &Path) -> Result<bool> {
    if is_build_only_by_name(path) {
        return Ok(true);
    }
    let linker_name = path.parent() == Some(Path::new("usr/lib"))
        && path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("lib") && name.ends_with(".so"));
    Ok(linker_name && !is_elf(&sysroot.join(path))?)
}

fn is_build_only_by_name(path: &Path) -> bool {
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
    keep: impl Fn(&Path) -> Result<bool>,
) -> Result<usize> {
    let mut files = Vec::new();
    for recipe in target_packages(recipes) {
        for file in install::manifest(paths, &recipe.name)? {
            if keep(&file)? {
                files.push(file);
            }
        }
    }
    copy_files(paths, &files, dest)
}

/// Copies `files` (relative to the root) from the sysroot into `dest` and
/// strips debug information. Returns the file count.
pub fn copy_files(paths: &Paths, files: &[PathBuf], dest: &Path) -> Result<usize> {
    let mut elf_files = Vec::new();
    for file in files {
        let target = dest.join(file);
        install::copy_entry(&paths.sysroot.join(file), &target)?;
        if is_elf(&target)? {
            elf_files.push(target);
        }
    }
    let count = files.len();
    strip_debug_info(paths, &elf_files)?;
    say!(
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

pub fn is_elf(path: &Path) -> Result<bool> {
    if !path.symlink_metadata()?.is_file() {
        return Ok(false);
    }
    let mut magic = [0; 4];
    let read = fs::File::open(path)?.read(&mut magic)?;
    Ok(read == 4 && magic == *b"\x7fELF")
}

/// Removes debug information; symbols needed for dynamic linking stay.
/// Some packages install read-only binaries; they are made writable for the
/// strip and get their original mode back afterwards.
fn strip_debug_info(paths: &Paths, files: &[PathBuf]) -> Result<()> {
    let strip = paths.tools.join("bin").join(format!("{TARGET}-strip"));
    let mut read_only = Vec::new();
    for file in files {
        let mode = fs::metadata(file)?.permissions().mode();
        if mode & 0o200 == 0 {
            fs::set_permissions(file, fs::Permissions::from_mode(mode | 0o200))?;
            read_only.push((file, mode));
        }
    }
    for chunk in files.chunks(256) {
        let status = Command::new(&strip)
            .arg("--strip-debug")
            .args(chunk)
            .status()
            .with_context(|| format!("starting {}", strip.display()))?;
        ensure!(status.success(), "{} failed with {status}", strip.display());
    }
    for (file, mode) in read_only {
        fs::set_permissions(file, fs::Permissions::from_mode(mode))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognises_build_only_files() {
        let sysroot = std::env::temp_dir().join(format!("bz-build-only-{}", std::process::id()));
        let lib = sysroot.join("usr/lib");
        fs::create_dir_all(lib.join("security")).unwrap();
        let elf = b"\x7fELF and the rest";
        for name in ["libfoo.so.1", "libmemusage.so", "security/pam_unix.so"] {
            fs::write(lib.join(name), elf).unwrap();
        }
        std::os::unix::fs::symlink("libfoo.so.1", lib.join("libfoo.so")).unwrap();
        fs::write(lib.join("libc.so"), "GROUP ( libc.so.6 )\n").unwrap();
        let build_only = |path: &str| is_build_only(&sysroot, Path::new(path)).unwrap();

        assert!(build_only("usr/include/stdio.h"));
        assert!(build_only("usr/lib/libc.a"));
        assert!(build_only("usr/lib/crt1.o"));
        assert!(build_only("usr/lib/beize-build/ukify"));
        assert!(build_only("usr/share/man/man1/ls.1"));
        assert!(build_only("usr/share/aclocal/glib-2.0.m4"));
        // Linker names: a link and a linker script.
        assert!(build_only("usr/lib/libfoo.so"));
        assert!(build_only("usr/lib/libc.so"));
        // What programs load: by SONAME, also one without a version, and
        // modules that end in .so.
        assert!(!build_only("usr/lib/libfoo.so.1"));
        assert!(!build_only("usr/lib/libmemusage.so"));
        assert!(!build_only("usr/lib/security/pam_unix.so"));
        assert!(!build_only("usr/bin/bash"));
        assert!(!build_only("usr/share/factory/etc/issue"));
        fs::remove_dir_all(&sysroot).unwrap();
    }

    #[test]
    fn repository_image_definition_parses() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../images/base.toml");
        let def: ImageDefinition = read_toml(&path).unwrap();
        assert!(def.packages.contains(&"systemd".to_owned()));
    }
}
