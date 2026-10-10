//! The `/usr` image: everything Beize ships, as a read-only EROFS filesystem.
//!
//! Beize ships nothing outside `/usr` (ADR 0007). `/etc`, `/var` and `/home`
//! live on the root partition, which is created and populated on first boot.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail, ensure};

use crate::elf;
use crate::install;
use crate::kernel::{self, Kernel};
use crate::layout;
use crate::packages::{self, BUILD_EPOCH};
use crate::paths::Paths;
use crate::say;
use crate::stamp::Stamp;
use crate::tree;
use crate::util;

/// Filesystem UUID of the `/usr` image; fixed for reproducibility.
const USR_FS_UUID: &str = "6265697a-6500-4000-8000-000000000010";
/// UUID in the dm-verity superblock; fixed for reproducibility.
const USR_VERITY_UUID: &str = "6265697a-6500-4000-8000-000000000011";
/// Which units the image enables (ADR 0013).
const UNIT_PRESET: &str = "images/base.preset";
/// The only preset file the image ships, system and user: `disable *`.
const SHIPPED_PRESET: &str = "99-beize.preset";

/// Builds the packages of `images/base.toml` and writes `out/usr.erofs`.
pub fn build(paths: &Paths) -> Result<PathBuf> {
    let definition = tree::image_definition(paths)?;
    let recipes = packages::build(paths, &definition.packages)?;
    let kernel = kernel::built(paths)?;

    let image = paths.out.join("usr.erofs");
    let stamp = Stamp::new(
        paths,
        "usr",
        tree::assembly_inputs(paths, &recipes, "usr")?
            .file(&paths.root.join(UNIT_PRESET))?
            .tree(&kernel.modules)?,
        &[
            &image,
            &paths.out.join("usr.verity"),
            &paths.out.join("usr.roothash"),
        ],
    );
    if stamp.is_current() {
        say!("  /usr image is up to date");
        return Ok(image);
    }
    stamp.clear()?;

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
    enable_units(paths, &root)?;
    install_modules(paths, &kernel, &root, |_| true)?;
    ensure_no_firmware_needed(paths, &kernel)?;
    elf::check_tree(paths, &root, "/usr")?;
    fs::write(usr.join("lib/os-release"), layout::os_release())?;
    write_repart_definitions(&usr)?;

    util::step("Writing /usr image");
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
    stamp.write()?;
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

/// Enables the units of the image (ADR 0013): Beize's own systemctl applies
/// `images/base.preset`, systemd's defaults and `disable *` to the tree, as
/// systemd would on first boot, and the links it creates in /etc move into
/// /usr – what runs is part of the signed image, and an update changes it
/// together with everything else. Only [`SHIPPED_PRESET`] stays in the image,
/// so systemd's own preset pass on first boot enables nothing in /etc.
fn enable_units(paths: &Paths, root: &Path) -> Result<()> {
    let systemd = root.join("usr/lib/systemd");
    let presets = ["system-preset", "user-preset"].map(|dir| systemd.join(dir));
    for dir in &presets {
        ensure!(
            dir.join(SHIPPED_PRESET).is_file(),
            "{} is missing from system/usr",
            dir.join(SHIPPED_PRESET).display()
        );
    }
    fs::copy(
        paths.root.join(UNIT_PRESET),
        presets[0].join("10-beize.preset"),
    )?;
    for scope in [None, Some("--global")] {
        util::run(
            sysroot_command(paths, "usr/bin/systemctl")
                .arg(format!("--root={}", root.display()))
                .args(scope)
                .arg("preset-all"),
        )?;
    }
    let etc = root.join("etc");
    for scope in ["system", "user"] {
        move_links(&etc.join("systemd").join(scope), &systemd.join(scope))?;
    }
    if etc.exists() {
        ensure_no_files(&etc)?;
        fs::remove_dir_all(&etc)?;
    }
    for dir in &presets {
        for entry in fs::read_dir(dir)? {
            let path = entry?.path();
            if path.file_name() != Some(SHIPPED_PRESET.as_ref()) {
                fs::remove_file(&path)?;
            }
        }
    }
    Ok(())
}

/// Copies the kernel's modules that `keep` accepts (by name, e.g. `nvme`)
/// into `root` and writes their index with Beize's own depmod.
pub fn install_modules(
    paths: &Paths,
    kernel: &Kernel,
    root: &Path,
    keep: impl Fn(&str) -> bool,
) -> Result<()> {
    let dest = root.join(kernel::MODULE_DIR).join(&kernel.release);
    for module in kernel::module_files(&kernel.modules)? {
        if !keep(&kernel::module_name(&module)) {
            continue;
        }
        let target = dest.join(module.strip_prefix(&kernel.modules)?);
        fs::create_dir_all(target.parent().context("a module without a directory")?)?;
        fs::copy(&module, &target)?;
    }
    // modules.order and modules.builtin* tell kmod what is built in.
    for entry in fs::read_dir(&kernel.modules)? {
        let path = entry?.path();
        if path.is_file() {
            fs::copy(&path, dest.join(path.file_name().context("no file name")?))?;
        }
    }
    util::run(
        sysroot_command(paths, "usr/bin/depmod")
            .arg(format!("--basedir={}", root.display()))
            .arg(&kernel.release),
    )
}

/// Beize ships firmware only where a module asks for it (ADR 0015), and no
/// module built so far does. The first one that does – WLAN, Bluetooth, a
/// GPU – must come with the firmware selection, not work half.
fn ensure_no_firmware_needed(paths: &Paths, kernel: &Kernel) -> Result<()> {
    let modules = kernel::module_files(&kernel.modules)?;
    let requested = util::output(
        sysroot_command(paths, "usr/bin/modinfo")
            .args(["--field", "firmware"])
            .args(&modules),
    )?;
    ensure!(
        requested.trim().is_empty(),
        "kernel modules request firmware, which bz does not install yet: {}",
        requested.split_whitespace().collect::<Vec<_>>().join(", ")
    );
    Ok(())
}

/// A program from the sysroot, run on the build host through the sysroot's
/// dynamic loader (same architecture), so it matches the image's version.
pub fn sysroot_command(paths: &Paths, program: &str) -> Command {
    let sysroot = &paths.sysroot;
    let mut cmd = Command::new(sysroot.join("usr/lib/ld-linux-x86-64.so.2"));
    cmd.arg("--library-path")
        .arg(format!(
            "{}:{}",
            sysroot.join("usr/lib").display(),
            sysroot.join("usr/lib/systemd").display()
        ))
        .arg(sysroot.join(program));
    cmd
}

/// Moves the unit links systemctl created under `from` into `to`. A link
/// that already exists there (systemd ships some, e.g. `ctrl-alt-del.target`)
/// must name the same unit.
fn move_links(from: &Path, to: &Path) -> Result<()> {
    if !from.exists() {
        return Ok(());
    }
    for entry in fs::read_dir(from)? {
        let path = entry?.path();
        let target = to.join(path.file_name().context("a link without a name")?);
        let meta = fs::symlink_metadata(&path)?;
        if meta.is_dir() {
            fs::create_dir_all(&target)?;
            move_links(&path, &target)?;
            fs::remove_dir(&path)?;
        } else if meta.is_symlink() {
            if fs::symlink_metadata(&target).is_ok() {
                let unit =
                    |link: &Path| fs::read_link(link).map(|t| t.file_name().map(ToOwned::to_owned));
                ensure!(
                    unit(&path)? == unit(&target)?,
                    "enabling units: {} would replace {}, which names another unit",
                    path.display(),
                    target.display()
                );
                fs::remove_file(&path)?;
            } else {
                fs::rename(&path, &target)?;
            }
        } else {
            bail!("enabling units: systemctl wrote a file, {}", path.display());
        }
    }
    Ok(())
}

/// Fails if anything but empty directories is left under `dir`.
fn ensure_no_files(dir: &Path) -> Result<()> {
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        if fs::symlink_metadata(&path)?.is_dir() {
            ensure_no_files(&path)?;
        } else {
            bail!(
                "enabling units left {} outside /usr (ADR 0007)",
                path.display()
            );
        }
    }
    Ok(())
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

    #[test]
    fn unit_links_move_into_usr_and_never_replace_another_unit() {
        use std::os::unix::fs::symlink;
        let dir = std::env::temp_dir().join(format!("bz-links-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let (etc, usr) = (dir.join("etc"), dir.join("usr"));
        fs::create_dir_all(etc.join("multi-user.target.wants")).unwrap();
        fs::create_dir_all(&usr).unwrap();
        let unit = |name: &str| format!("/usr/lib/systemd/system/{name}");
        symlink(
            unit("a.service"),
            etc.join("multi-user.target.wants/a.service"),
        )
        .unwrap();
        // Shipped by systemd already, relative: the same unit.
        symlink("reboot.target", usr.join("ctrl-alt-del.target")).unwrap();
        symlink(unit("reboot.target"), etc.join("ctrl-alt-del.target")).unwrap();
        move_links(&etc, &usr).unwrap();
        assert_eq!(
            fs::read_link(usr.join("multi-user.target.wants/a.service")).unwrap(),
            Path::new(&unit("a.service"))
        );
        assert_eq!(
            fs::read_link(usr.join("ctrl-alt-del.target")).unwrap(),
            Path::new("reboot.target")
        );
        ensure_no_files(&etc).unwrap();

        symlink(unit("poweroff.target"), etc.join("ctrl-alt-del.target")).unwrap();
        assert!(move_links(&etc, &usr).is_err());
        assert!(ensure_no_files(&etc).is_err());
        fs::remove_dir_all(&dir).unwrap();
    }
}
