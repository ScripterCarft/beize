//! Assembles the read-only root filesystem from installed packages.

use std::fs;
use std::io::Read;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::install;
use crate::packages::{self, BUILD_EPOCH, TARGET};
use crate::paths::Paths;
use crate::recipe::Stage;
use crate::util;

/// Filesystem UUID of the root image; fixed for reproducibility.
const ROOT_FS_UUID: &str = "6265697a-6500-4000-8000-000000000010";

pub const OS_RELEASE: &str = "\
NAME=\"Beize\"
ID=beize
PRETTY_NAME=\"Beize (Entwicklungsversion)\"
VERSION_ID=0.1
";

/// Empty directories every root needs (mount points and runtime state).
const ROOT_DIRS: [&str; 12] = [
    "dev", "proc", "sys", "run", "tmp", "var", "root", "home", "mnt", "etc", "srv", "opt",
];

/// Build-time files that are not part of the running system.
const EXCLUDED_DIRS: [&str; 6] = [
    "usr/include",
    "usr/lib/pkgconfig",
    "usr/share/pkgconfig",
    "usr/share/man",
    "usr/share/info",
    "usr/share/doc",
];
const EXCLUDED_EXTENSIONS: [&str; 3] = ["a", "o", "la"];

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ImageDefinition {
    packages: Vec<String>,
}

/// Builds the packages of `images/base.toml` and writes `out/root.erofs`.
pub fn build(paths: &Paths) -> Result<PathBuf> {
    let definition_path = paths.root.join("images/base.toml");
    let definition: ImageDefinition = toml::from_str(
        &fs::read_to_string(&definition_path)
            .with_context(|| format!("reading {}", definition_path.display()))?,
    )?;
    let recipes = packages::build(paths, &definition.packages)?;

    util::step("Assembling root filesystem");
    let root = paths.work.join("rootfs");
    if root.exists() {
        fs::remove_dir_all(&root)?;
    }
    install::prepare_layout(&root)?;
    for dir in ROOT_DIRS {
        fs::create_dir_all(root.join(dir))?;
    }

    let mut elf_files = Vec::new();
    let mut count = 0;
    let targets: Vec<_> = recipes
        .iter()
        .filter(|r| r.stage == Some(Stage::Target))
        .collect();
    for recipe in &targets {
        for file in install::manifest(paths, &recipe.name)? {
            if is_excluded(&file) {
                continue;
            }
            let dest = root.join(&file);
            install::copy_entry(&paths.sysroot.join(&file), &dest)?;
            if is_elf(&dest)? {
                elf_files.push(dest);
            }
            count += 1;
        }
    }
    eprintln!("  {count} files from {} packages", targets.len());
    strip_debug_info(paths, &elf_files)?;

    let overlay = paths.root.join("system/rootfs");
    util::run(
        Command::new("cp")
            .arg("--recursive")
            .arg("--no-dereference")
            .arg(format!("{}/.", overlay.display()))
            .arg(&root),
    )?;
    fs::write(root.join("usr/lib/os-release"), OS_RELEASE)?;
    symlink("../usr/lib/os-release", root.join("etc/os-release"))?;

    // Host systemd tools operate offline on the image: create system users
    // from sysusers.d and enable units according to the presets.
    util::run(Command::new("systemd-sysusers").arg(format!("--root={}", root.display())))?;
    util::run(
        Command::new("systemctl")
            .arg(format!("--root={}", root.display()))
            .arg("preset-all"),
    )?;

    // The root is read-only at runtime, so everything boot would otherwise
    // generate is generated here: the dynamic linker cache, and the marker
    // that tells ConditionNeedsUpdate=/etc units there is nothing to update.
    util::run(
        Command::new("unshare")
            .arg("--map-root-user")
            .arg("chroot")
            .arg(&root)
            .arg("/usr/bin/ldconfig"),
    )?;
    fs::write(root.join("etc/.updated"), "")?;

    util::step("Writing root image");
    let image = paths.out.join("root.erofs");
    let _ = fs::remove_file(&image);
    // Some files are deliberately unreadable (e.g. /etc/shadow, mode 0000).
    // Run as root inside an unprivileged user namespace so they can be packed
    // with their exact permissions, without real root privileges.
    util::run(
        Command::new("unshare")
            .arg("--map-root-user")
            .arg("mkfs.erofs")
            .arg("-zlz4hc")
            .arg("--all-root")
            .arg(format!("-T{BUILD_EPOCH}"))
            .arg(format!("-U{ROOT_FS_UUID}"))
            .arg("-Lbeize-root")
            .arg(&image)
            .arg(&root),
    )?;
    let size = fs::metadata(&image)?.len();
    eprintln!(
        "  root image -> {} ({:.1} MiB)",
        image.display(),
        size as f64 / 1024.0 / 1024.0
    );
    Ok(image)
}

fn is_excluded(path: &Path) -> bool {
    EXCLUDED_DIRS.iter().any(|dir| path.starts_with(dir))
        || path
            .extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| EXCLUDED_EXTENSIONS.contains(&ext))
}

fn is_elf(path: &Path) -> Result<bool> {
    let meta = path.symlink_metadata()?;
    if !meta.is_file() {
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
        anyhow::ensure!(status.success(), "{} failed with {status}", strip.display());
    }
    eprintln!("  stripped debug info from {} ELF files", files.len());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn excludes_development_files() {
        assert!(is_excluded(Path::new("usr/include/stdio.h")));
        assert!(is_excluded(Path::new("usr/lib/libc.a")));
        assert!(is_excluded(Path::new("usr/lib/crt1.o")));
        assert!(is_excluded(Path::new("usr/share/man/man1/ls.1")));
        assert!(!is_excluded(Path::new("usr/lib/libc.so.6")));
        assert!(!is_excluded(Path::new("usr/bin/bash")));
        assert!(!is_excluded(Path::new("usr/share/factory/etc/issue")));
    }
}
