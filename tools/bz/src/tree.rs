//! Assembles file trees (the `/usr` image, the initrd) from installed packages.

use std::fs;
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use crate::install;
use crate::packages::{self, BUILD_EPOCH};
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
/// of every package (including the toolchain that strips them), what each
/// recipe's `[files]` lets in, and those files.
pub fn assembly_inputs(
    paths: &Paths,
    definition: &str,
    recipes: &[Recipe],
    own: &str,
) -> Result<Fingerprint> {
    let mut inputs = Fingerprint::new()?.file(&paths.root.join(definition))?;
    for recipe in recipes {
        inputs = inputs
            .text(&format!(
                "{} {}",
                recipe.name,
                packages::built_fingerprint(paths, &recipe.name)?
            ))
            .text(&recipe.metadata_text);
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
/// Static libraries, objects, libtool archives and C headers, also outside
/// usr/include (glib's glibconfig.h, dbus's dbus-arch-deps.h, bash's
/// loadables.h live under usr/lib).
const BUILD_ONLY_EXTENSIONS: [&str; 4] = ["a", "o", "la", "h"];

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

/// Copies `files` (relative to the root) from the sysroot into `dest`,
/// keeping hard links. Programs and libraries come as their packages left
/// them: stripped, with MiniDebugInfo (ADR 0023). Returns the file count.
pub fn copy_files(paths: &Paths, files: &[PathBuf], dest: &Path) -> Result<usize> {
    let mut copier = install::Copier::default();
    for file in files {
        copier.copy(&paths.sysroot.join(file), &dest.join(file))?;
    }
    say!("  {} files", files.len());
    Ok(files.len())
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

/// Removes `path` – a directory with all below it, a file or a symlink – if
/// it is there.
pub fn remove(path: &Path) -> Result<()> {
    let removed = match path.symlink_metadata() {
        Ok(meta) if meta.is_dir() => fs::remove_dir_all(path),
        Ok(_) => fs::remove_file(path),
        Err(_) => return Ok(()),
    };
    removed.with_context(|| format!("removing {}", path.display()))
}

/// An empty directory `dir`, whatever was there before.
pub fn recreate(dir: &Path) -> Result<PathBuf> {
    remove(dir)?;
    fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    Ok(dir.to_owned())
}

/// Every regular file and symlink below `root`, relative to it and sorted;
/// anything else (a device, a socket) is an error.
pub fn walk(root: &Path) -> Result<Vec<PathBuf>> {
    let mut found = Vec::new();
    let mut pending = vec![PathBuf::new()];
    while let Some(rel) = pending.pop() {
        let dir = root.join(&rel);
        for entry in fs::read_dir(&dir).with_context(|| format!("reading {}", dir.display()))? {
            let entry = entry?;
            let child = rel.join(entry.file_name());
            let kind = entry.file_type()?;
            if kind.is_dir() {
                pending.push(child);
            } else if kind.is_file() || kind.is_symlink() {
                found.push(child);
            } else {
                bail!("{} is not a file, symlink or directory", child.display());
            }
        }
    }
    found.sort();
    Ok(found)
}

/// A path relative to the root, `.` and `..` resolved by name (not through
/// symlinks); a leading `/` is dropped.
pub fn lexical(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                out.pop();
            }
            Component::Normal(part) => out.push(part),
            Component::RootDir | Component::CurDir | Component::Prefix(_) => {}
        }
    }
    out
}

pub fn is_elf(path: &Path) -> Result<bool> {
    if !path.symlink_metadata()?.is_file() {
        return Ok(false);
    }
    let mut magic = [0; 4];
    let read = fs::File::open(path)?.read(&mut magic)?;
    Ok(read == 4 && magic == *b"\x7fELF")
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
        assert!(build_only("usr/lib/glib-2.0/include/glibconfig.h"));
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
    fn walk_lists_files_and_symlinks() {
        let dir = std::env::temp_dir().join(format!("bz-walk-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("usr/bin")).unwrap();
        fs::write(dir.join("usr/bin/bash"), "").unwrap();
        std::os::unix::fs::symlink("bash", dir.join("usr/bin/sh")).unwrap();
        fs::create_dir_all(dir.join("empty")).unwrap();

        let files = walk(&dir).unwrap();
        fs::remove_dir_all(&dir).unwrap();
        assert_eq!(
            files,
            [PathBuf::from("usr/bin/bash"), PathBuf::from("usr/bin/sh")]
        );
    }

    #[test]
    fn resolves_paths_by_name() {
        assert_eq!(
            lexical(Path::new("usr/lib/../bin/./x")),
            Path::new("usr/bin/x")
        );
        assert_eq!(
            lexical(Path::new("/usr/lib/systemd/../../lib")),
            Path::new("usr/lib")
        );
    }

    #[test]
    fn repository_image_definition_parses() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../images/base.toml");
        let def: ImageDefinition = read_toml(&path).unwrap();
        assert!(def.packages.contains(&"systemd".to_owned()));
    }
}
