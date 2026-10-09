//! The `/usr` image: everything Beize ships, as a read-only EROFS filesystem.
//!
//! Beize ships nothing outside `/usr` (ADR 0007). `/etc`, `/var` and `/home`
//! live on the root partition, which is created and populated on first boot.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail, ensure};

use crate::install;
use crate::layout;
use crate::packages::{self, BUILD_EPOCH};
use crate::paths::Paths;
use crate::say;
use crate::tree;
use crate::util;

/// Filesystem UUID of the `/usr` image; fixed for reproducibility.
const USR_FS_UUID: &str = "6265697a-6500-4000-8000-000000000010";
/// UUID in the dm-verity superblock; fixed for reproducibility.
const USR_VERITY_UUID: &str = "6265697a-6500-4000-8000-000000000011";

/// Builds the packages of `images/base.toml` and writes `out/usr.erofs`.
pub fn build(paths: &Paths) -> Result<PathBuf> {
    let definition = tree::image_definition(paths)?;
    let recipes = packages::build(paths, &definition.packages)?;

    util::step("Assembling /usr");
    for recipe in tree::target_packages(&recipes) {
        if let Some(outside) = install::manifest(paths, &recipe.name)?
            .into_iter()
            .find(|file| !file.starts_with("usr"))
        {
            bail!(
                "{} installs /{}: Beize images ship only /usr (ADR 0007); \
                 move it to /usr/share/factory or drop it in the recipe",
                recipe.name,
                outside.display()
            );
        }
    }
    let root = tree::recreate(&paths.work.join("usr-tree"))?;
    // usr/sbin -> bin and usr/lib64 -> lib; the root-level links (/bin, /lib64,
    // ...) are created by systemd on first boot.
    install::prepare_layout(&root)?;
    tree::populate(paths, &recipes, &root, |file| !tree::is_build_only(file))?;

    let usr = root.join("usr");
    util::run(
        Command::new("cp")
            .arg("--recursive")
            .arg("--no-dereference")
            .arg(format!("{}/.", paths.root.join("system/usr").display()))
            .arg(&usr),
    )?;
    fs::write(usr.join("lib/os-release"), layout::os_release())?;
    write_repart_definitions(&usr)?;

    util::step("Writing /usr image");
    let image = paths.out.join("usr.erofs");
    let _ = fs::remove_file(&image);
    util::run(
        Command::new("mkfs.erofs")
            .arg("-zlz4hc")
            .arg("--all-root")
            .arg(format!("-T{BUILD_EPOCH}"))
            .arg(format!("-U{USR_FS_UUID}"))
            .arg(format!("-L{}", layout::usr_label()))
            .arg(&image)
            .arg(&usr),
    )?;
    let size = fs::metadata(&image)?.len();
    if size > layout::USR_SLOT_SIZE {
        bail!("the /usr image ({size} bytes) does not fit into a /usr slot");
    }
    ensure!(
        size % layout::VERITY_BLOCK_SIZE == 0,
        "the /usr image is not a multiple of the verity block size"
    );
    say!(
        "  /usr image -> {} ({:.1} MiB)",
        image.display(),
        size as f64 / layout::MIB as f64
    );

    let root_hash = build_verity(paths, &image)?;
    say!("  dm-verity root hash {root_hash}");
    Ok(image)
}

/// Writes the dm-verity hash tree of the `/usr` image to `out/usr.verity` and
/// its root hash to `out/usr.roothash`. The salt is derived from the image
/// itself, so the same `/usr` always yields the same root hash.
fn build_verity(paths: &Paths, image: &Path) -> Result<String> {
    util::step("Computing dm-verity hash tree");
    let hash_tree = paths.out.join("usr.verity");
    let _ = fs::remove_file(&hash_tree);
    let block_size = layout::VERITY_BLOCK_SIZE.to_string();
    let report = util::output(
        Command::new("veritysetup")
            .arg("format")
            .args(["--data-block-size", &block_size])
            .args(["--hash-block-size", &block_size])
            .arg(format!("--salt={}", util::sha256_file(image)?))
            .arg(format!("--uuid={USR_VERITY_UUID}"))
            .arg(image)
            .arg(&hash_tree),
    )?;
    let root_hash = report
        .lines()
        .find_map(|line| line.strip_prefix("Root hash:"))
        .map(|hash| hash.trim().to_owned())
        .context("veritysetup did not report a root hash")?;
    layout::verity_partition_uuids(&root_hash)?; // validates the format
    ensure!(
        fs::metadata(&hash_tree)?.len() <= layout::USR_VERITY_SLOT_SIZE,
        "the dm-verity hash tree does not fit into a verity slot"
    );
    fs::write(paths.out.join("usr.roothash"), &root_hash)?;
    Ok(root_hash)
}

/// Installs the systemd-repart definitions into a `/usr` tree.
pub fn write_repart_definitions(usr: &Path) -> Result<()> {
    let dir = usr.join("lib/repart.d");
    fs::create_dir_all(&dir)?;
    for (name, content) in layout::repart_definitions() {
        fs::write(dir.join(name), content)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    /// Scripts lose their executable bit easily (e.g. when edited through a
    /// Windows path); systemd then fails with "Permission denied".
    #[test]
    fn scripts_in_system_are_executable() {
        let system = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../system");
        let mut pending = vec![system.join("usr"), system.join("initrd")];
        let mut scripts = 0;
        while let Some(dir) = pending.pop() {
            for entry in fs::read_dir(&dir).unwrap() {
                let path = entry.unwrap().path();
                let meta = fs::symlink_metadata(&path).unwrap();
                if meta.is_dir() {
                    pending.push(path);
                } else if meta.is_file() && fs::read(&path).unwrap().starts_with(b"#!") {
                    assert!(
                        meta.permissions().mode() & 0o111 != 0,
                        "{} is a script but not executable",
                        path.display()
                    );
                    scripts += 1;
                }
            }
        }
        assert!(scripts > 0);
    }
}
