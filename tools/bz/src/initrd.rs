//! The initrd embedded in the UKI.
//!
//! It contains just enough of Beize to find and mount `/usr` and the root
//! partition. On first boot systemd-repart, running here, creates the second
//! `/usr` slot and the root partition before the root is mounted (ADR 0007).

use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::Result;

use crate::install;
use crate::layout;
use crate::packages;
use crate::paths::Paths;
use crate::say;
use crate::tree;
use crate::usr;
use crate::util;

/// Parts of the initrd packages that finding and mounting the root never needs.
const SKIPPED: [&str; 5] = [
    "usr/lib/gconv",
    "usr/share/locale",
    "usr/share/i18n",
    "usr/share/factory",
    // EFI binaries (boot loader, stub) live on the ESP, not in the initrd.
    "usr/lib/systemd/boot",
];

/// Builds `out/initrd.cpio.zst` from the `initrd` packages of `images/base.toml`.
pub fn build(paths: &Paths) -> Result<PathBuf> {
    let definition = tree::image_definition(paths)?;
    let recipes = packages::build(paths, &definition.initrd)?;

    util::step("Assembling initrd");
    let root = tree::recreate(&paths.work.join("initrd-tree"))?;
    install::prepare_layout(&root)?;
    for dir in ["dev", "proc", "sys", "run", "tmp", "etc"] {
        fs::create_dir_all(root.join(dir))?;
    }
    tree::populate(paths, &recipes, &root, |file| {
        !tree::is_build_only(file) && !SKIPPED.iter().any(|dir| file.starts_with(dir))
    })?;

    // Beize's own files for the initrd (units, drop-ins, scripts).
    util::run(
        Command::new("cp")
            .arg("--recursive")
            .arg("--no-dereference")
            .arg(format!("{}/.", paths.root.join("system/initrd").display()))
            .arg(&root),
    )?;
    // /etc/initrd-release tells systemd it runs in the initrd (initrd.target).
    fs::write(root.join("etc/initrd-release"), layout::os_release())?;
    symlink("usr/lib/systemd/systemd", root.join("init"))?;
    usr::write_repart_definitions(&root.join("usr"))?;
    // sysusers does not run in the initrd, yet sockets and udev rules refer to
    // system groups (systemd-journal, disk, tty, ...). Generate the user and
    // group databases now; password hashes are not needed in the initrd.
    util::run(Command::new("systemd-sysusers").arg(format!("--root={}", root.display())))?;
    for secret in ["etc/shadow", "etc/gshadow"] {
        let _ = fs::remove_file(root.join(secret));
    }
    tree::normalize_mtimes(&root)?;

    util::step("Packing initrd");
    let image = paths.out.join("initrd.cpio.zst");
    pack(&root, &image)?;
    say!(
        "  initrd -> {} ({:.1} MiB)",
        image.display(),
        fs::metadata(&image)?.len() as f64 / layout::MIB as f64
    );
    Ok(image)
}

/// Writes a reproducible, zstd-compressed newc cpio archive of `root`.
pub fn pack(root: &Path, image: &Path) -> Result<()> {
    let pipeline = format!(
        "set -euo pipefail; find . -print0 | LC_ALL=C sort -z \
         | cpio --null --create --format=newc --owner=0:0 --reproducible --quiet \
         | zstd -19 -T0 --quiet --force -o '{}'",
        image.display()
    );
    util::run(
        Command::new("bash")
            .arg("-c")
            .arg(pipeline)
            .current_dir(root),
    )
}
