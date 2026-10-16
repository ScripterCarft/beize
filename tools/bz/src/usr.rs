//! The `/usr` image: everything Beize ships, as a read-only EROFS filesystem.
//!
//! Beize ships nothing outside `/usr` (ADR 0007). `/etc`, `/var` and `/home`
//! live on the root partition, which is created and populated on first boot.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail, ensure};

use crate::elf;
use crate::files;
use crate::install;
use crate::kernel::{self, Kernel};
use crate::layout;
use crate::packages::{self, BUILD_EPOCH};
use crate::paths::Paths;
use crate::privileges;
use crate::say;
use crate::secureboot::Keys;
use crate::security;
use crate::stamp::Stamp;
use crate::tree;
use crate::util;

/// Filesystem UUID of the `/usr` image; fixed for reproducibility.
const USR_FS_UUID: &str = "6265697a-6500-4000-8000-000000000010";
/// UUID in the dm-verity superblock; fixed for reproducibility.
const USR_VERITY_UUID: &str = "6265697a-6500-4000-8000-000000000011";
/// Which units the image enables (ADR 0013): system units, and user units
/// for every user (`systemctl --global`).
const UNIT_PRESETS: [&str; 2] = ["images/base.preset", "images/user.preset"];
/// The only preset file the image ships, system and user: `disable *`.
const SHIPPED_PRESET: &str = "99-beize.preset";
/// The units every boot runs (ADR 0013); in `/usr` for beize-boot-health,
/// which decides by them whether a boot succeeded (ADR 0020).
const RUNNING_UNITS: &str = "images/running-units.txt";

/// Where [`build`] assembles `/usr`, as a tree with `usr/` in it.
pub fn tree_dir(paths: &Paths) -> PathBuf {
    paths.work.join("usr-tree")
}

/// A packed `/usr`: the EROFS image, its dm-verity hash tree and root hash.
pub struct Packed {
    pub image: PathBuf,
    pub verity: PathBuf,
    pub root_hash: String,
}

/// Builds the packages of `images/base.toml` and assembles and checks the
/// `/usr` tree ([`tree_dir`]). What differs between images – the version
/// and the keys – comes in with [`pack`].
pub fn build(paths: &Paths) -> Result<PathBuf> {
    let definition = tree::image_definition(paths)?;
    let recipes = packages::build(paths, &definition.packages)?;
    let kernel = kernel::built(paths)?;

    let root = tree_dir(paths);
    let stamp = Stamp::new(
        paths,
        STAMP,
        tree::assembly_inputs(paths, tree::IMAGE_DEFINITION, &recipes, "usr")?
            .file(&paths.root.join(UNIT_PRESETS[0]))?
            .file(&paths.root.join(UNIT_PRESETS[1]))?
            .file(&paths.root.join(RUNNING_UNITS))?
            .file(&paths.root.join(security::DEFINITION))?
            .tree(&kernel.modules)?,
        &[&root],
    );
    if stamp.is_current() {
        say!("  /usr is up to date");
        return Ok(root);
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
    tree::recreate(&root)?;
    // usr/sbin -> bin and usr/lib64 -> lib; the root-level links (/bin, /lib64,
    // ...) are created by systemd on first boot.
    install::prepare_layout(&root)?;
    let image = files::image_files(paths, &recipes)?;
    tree::copy_files(paths, &image.into_iter().collect::<Vec<_>>(), &root)?;

    let usr = root.join("usr");
    util::run(
        Command::new("cp")
            .arg("--recursive")
            .arg("--no-dereference")
            .arg(format!("{}/.", paths.root.join("system/usr").display()))
            .arg(&usr),
    )?;
    fs::copy(
        paths.root.join(RUNNING_UNITS),
        usr.join("lib/beize/running-units.txt"),
    )?;
    enable_units(paths, &root)?;
    compile_hwdb(paths, &root)?;
    install_modules(paths, &kernel, &root, |_| true)?;
    ensure_no_firmware_needed(paths, &kernel)?;
    elf::check_tree(paths, &root, "/usr")?;
    privileges::check_tree(&root, "/usr")?;
    security::check(paths, &root)?;
    ensure_no_build_paths(paths, &root)?;
    write_repart_definitions(&usr)?;
    write_definitions(
        &usr.join("lib/sysupdate.d"),
        layout::sysupdate_definitions(),
    )?;
    stamp.write()?;
    Ok(root)
}

/// The stamp of [`build`]; later steps take it as their input instead of
/// hashing the whole tree again.
pub const STAMP: &str = "usr";

/// Where the public update key lies in `/usr` (ADR 0020).
pub const UPDATE_KEY_PATH: &str = "lib/beize/update.pub.pem";

/// Where the boot loader signed with the image's keys lies in `/usr`: bootctl
/// installs it instead of the unsigned one beside it (ADR 0020).
pub const SIGNED_BOOT_LOADER_PATH: &str = "lib/systemd/boot/efi/systemd-bootx64.efi.signed";

/// Packs the `/usr` tree as `version` for `keys` into `out`: `usr.erofs`,
/// `usr.verity` and `usr.roothash`. The version (os-release), the public
/// update key, `boot_loader` and the modules, signed with `keys`, are what
/// the images of one build differ in; the tree itself stays untouched.
pub fn pack(
    paths: &Paths,
    keys: &Keys,
    version: &str,
    boot_loader: &Path,
    out: &Path,
) -> Result<Packed> {
    let tree = tree_dir(paths);
    ensure!(tree.is_dir(), "{} is missing; run `bz usr`", tree.display());
    util::step(&format!("Writing /usr image {version}"));
    let staging = out.join("usr-staging");
    tree::remove(&staging)?;
    util::run(
        Command::new("cp")
            .args(["--archive", "--reflink=auto"])
            .arg(&tree)
            .arg(&staging),
    )?;
    let usr = staging.join("usr");
    fs::write(usr.join("lib/os-release"), layout::os_release(version))?;
    let key = usr.join(UPDATE_KEY_PATH);
    fs::create_dir_all(key.parent().context("update key path")?)?;
    fs::copy(keys.update_public(), &key)?;
    fs::copy(boot_loader, usr.join(SIGNED_BOOT_LOADER_PATH))?;
    let signed = kernel::signed_modules(paths, keys.module_key(), out)?;
    kernel::use_signed(
        &signed,
        &usr.join("lib/modules").join(&kernel::built(paths)?.release),
    )?;

    let image = out.join("usr.erofs");
    let _ = fs::remove_file(&image);
    util::run(
        Command::new("mkfs.erofs")
            // zstd level 9 in 64 KiB clusters, file tails packed together.
            // Measured in the VM (2026-10-12): 49.7 instead of 80.2 MiB with
            // lz4hc, start unchanged, all of /usr read cold 12 % faster
            // (fewer bytes); level 15 saves 3 MiB more for 26 s more packing,
            // lzma 6 MiB more but starts 0.45 s slower. Not `dedupe`: in
            // erofs-utils 1.9 it gave an impossibly small image.
            .args(["-zzstd,9", "-C65536", "-Eztailpacking,fragments"])
            .arg("--all-root")
            // No extended attributes: nothing in /usr needs one, and without
            // them no file can carry capabilities (ADR 0018).
            .arg("-x-1")
            .arg(format!("-T{BUILD_EPOCH}"))
            .arg(format!("-U{USR_FS_UUID}"))
            .arg(format!("-L{}", layout::usr_label(version)))
            .arg(&image)
            .arg(&usr),
    )?;
    fs::remove_dir_all(&staging)?;
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

    let verity = out.join("usr.verity");
    let root_hash = build_verity(&image, &verity)?;
    fs::write(out.join("usr.roothash"), &root_hash)?;
    say!("  dm-verity root hash {root_hash}");
    Ok(Packed {
        image,
        verity,
        root_hash,
    })
}

/// Writes the dm-verity hash tree of the `/usr` image to `hash_tree` and
/// returns its root hash. The salt is derived from the image itself, so the
/// same `/usr` always yields the same root hash.
fn build_verity(image: &Path, hash_tree: &Path) -> Result<String> {
    let _ = fs::remove_file(hash_tree);
    let block_size = layout::VERITY_BLOCK_SIZE.to_string();
    let report = util::output(
        Command::new("veritysetup")
            .arg("format")
            .args(["--data-block-size", &block_size])
            .args(["--hash-block-size", &block_size])
            .arg(format!("--salt={}", util::sha256_file(image)?))
            .arg(format!("--uuid={USR_VERITY_UUID}"))
            .arg(image)
            .arg(hash_tree),
    )?;
    let root_hash = report
        .lines()
        .find_map(|line| line.strip_prefix("Root hash:"))
        .map(|hash| hash.trim().to_owned())
        .context("veritysetup did not report a root hash")?;
    layout::verity_partition_uuids(&root_hash)?; // validates the format
    ensure!(
        fs::metadata(hash_tree)?.len() <= layout::USR_VERITY_SLOT_SIZE,
        "the dm-verity hash tree does not fit into a verity slot"
    );
    Ok(root_hash)
}

/// Enables the units of the image (ADR 0013): Beize's own systemctl applies
/// [`UNIT_PRESETS`], systemd's defaults and `disable *` to the tree, as
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
    for (preset, dir) in UNIT_PRESETS.iter().zip(&presets) {
        fs::copy(paths.root.join(preset), dir.join("10-beize.preset"))?;
    }
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

/// Compiles udev's hardware database into `/usr/lib/udev/hwdb.bin` with the
/// image's systemd-hwdb (ADR 0016). `/usr` is read-only, so the sources are
/// of no use at run time and stay out.
fn compile_hwdb(paths: &Paths, root: &Path) -> Result<()> {
    util::run(
        sysroot_command(paths, "usr/bin/systemd-hwdb")
            .args(["update", "--usr", "--strict"])
            .arg(format!("--root={}", root.display())),
    )?;
    let sources = root.join("usr/lib/udev/hwdb.d");
    fs::remove_dir_all(&sources).with_context(|| format!("removing {}", sources.display()))?;
    say!(
        "  hwdb.bin {:.1} MiB",
        fs::metadata(root.join("usr/lib/udev/hwdb.bin"))?.len() as f64 / layout::MIB as f64
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

/// Fails if a file names the repository on the build machine – its sysroots
/// or sources: a place that exists only there, and a path that differs
/// between builds (pkg-config variables once carried the sysroot into
/// libxkbcommon, compiler commands into bash and OpenSSL; assert() and
/// panic messages their source files, until the compilers mapped them).
fn ensure_no_build_paths(paths: &Paths, root: &Path) -> Result<()> {
    let out = Command::new("grep")
        .args(["--recursive", "--files-with-matches", "--fixed-strings"])
        .arg("-e")
        .arg(&paths.root)
        .arg(root)
        .output()
        .context("starting grep")?;
    match out.status.code() {
        Some(1) => {
            say!("  ok: no file in /usr names the build machine's repository");
            Ok(())
        }
        Some(0) => bail!(
            "these files name {}, a place only on the build machine:\n{}",
            paths.root.display(),
            String::from_utf8_lossy(&out.stdout).trim_end()
        ),
        _ => bail!(
            "grep failed: {}",
            String::from_utf8_lossy(&out.stderr).trim_end()
        ),
    }
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
    write_definitions(&usr.join("lib/repart.d"), layout::repart_definitions())
}

fn write_definitions(dir: &Path, definitions: Vec<(&str, String)>) -> Result<()> {
    fs::create_dir_all(dir)?;
    for (name, content) in definitions {
        fs::write(dir.join(name), content)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    /// Scripts lose their executable bit easily (e.g. when edited through a
    /// Windows path); systemd, ./bz and the CI then fail with "Permission
    /// denied" (the CI once did on ci/system.sh).
    #[test]
    fn scripts_are_executable() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let bz = fs::metadata(root.join("bz")).unwrap();
        assert!(
            bz.permissions().mode() & 0o111 != 0,
            "./bz is not executable"
        );
        let mut pending = vec![
            root.join("system/usr"),
            root.join("system/initrd"),
            root.join("ci"),
        ];
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
