//! The kernel (ADR 0015): its configuration, the bzImage and the modules.
//!
//! The configuration starts from every option's upstream default
//! (`alldefconfig`), adds upstream's hardening fragments and then Beize's
//! fragments in `kernel/config`. The resolved `.config` is kept in the
//! repository as [`RESOLVED`], so every option a new kernel version adds or
//! changes shows up in review.
//!
//! Modules leave the kernel build unsigned, the same on every build machine;
//! each key set signs them ([`signed_modules`]), and its kernel has the key
//! set's certificate built in ([`image_for`]).

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail, ensure};

use crate::jobserver;
use crate::packages::{self, TARGET};
use crate::paths::Paths;
use crate::recipe::Recipe;
use crate::say;
use crate::secureboot;
use crate::stamp::{Fingerprint, Stamp};
use crate::{tree, util};

/// Fixed build metadata so the same sources produce the same kernel.
const BUILD_USER: &str = "beize";
const BUILD_HOST: &str = "beize";
const BUILD_TIMESTAMP: &str = "Thu Jan  1 00:00:00 UTC 1970";
/// Upstream's hardening fragments, applied before Beize's own.
const UPSTREAM_FRAGMENTS: [&str; 2] = [
    "kernel/configs/hardening.config",
    "arch/x86/configs/hardening.config",
];
/// The resolved configuration, kept in the repository.
pub const RESOLVED: &str = "kernel/beize.config";
/// Where modules live in the image (and below `out/modules`).
pub const MODULE_DIR: &str = "usr/lib/modules";
/// The certificate the kernel trusts for modules, in the object tree
/// (`CONFIG_SYSTEM_TRUSTED_KEYS`).
const TRUSTED_CERT: &str = "certs/beize-modules.pem";
/// What kbuild would generate and sign with if the configuration asked it
/// to; Beize's modules are signed by bz alone (ADR 0015).
const GENERATED_KEY: &str = "certs/signing_key.pem";
/// The certificate `out/vmlinuz` has built in: the test keys'.
const LINKED_CERT: &str = "kernel-cert.pem";
/// The hash modules are signed with (`CONFIG_MODULE_SIG_HASH`).
const SIGNATURE_HASH: &str = "sha512";
/// What `sign-file` appends to a signed module.
const SIGNATURE_MAGIC: &[u8] = b"~Module signature appended~\n";
/// A module as the compiler wrote it, unsigned: `bz smoke` checks that the
/// kernel refuses it. nls_utf8: never loaded in the VMs, and the smallest
/// module – a large file in QEMU's fw_cfg delays the firmware (237 KiB: 45 s).
const UNSIGNED_MODULE: &str = "fs/nls/nls_utf8.ko";

/// What the kernel step produces in `out/`.
pub struct Kernel {
    pub image: PathBuf,
    /// The release, e.g. `7.2.9-beize`.
    pub release: String,
    /// `<dir>/usr/lib/modules/<release>`, unsigned and compressed, without
    /// depmod's index (made where the tree is assembled, with Beize's kmod).
    pub modules: PathBuf,
    /// An unsigned copy of a module, for `bz smoke`.
    pub unsigned_module: PathBuf,
}

/// Configures and builds the kernel, with the test keys' module certificate,
/// and its unsigned modules.
pub fn build(paths: &Paths) -> Result<Kernel> {
    let recipe = Recipe::load(paths, "linux")?;
    let fragments = own_fragments(paths)?;
    let cert = secureboot::test_module_cert(paths)?;

    let image = paths.out.join("vmlinuz");
    let linked_cert = paths.out.join(LINKED_CERT);
    let release_file = paths.out.join("kernel.release");
    let modules_root = paths.out.join("modules");
    let unsigned_module = paths.out.join("unsigned-module.ko");
    let obj = paths.work.join("linux-obj");
    // The recipe pins the source (and with it upstream's fragments); Beize's
    // fragments are the configuration; the toolchain compiles it.
    let mut inputs = Fingerprint::new()?.text(&recipe.text);
    for tool in TOOLCHAIN {
        inputs = inputs.text(&format!(
            "{tool} {}",
            packages::built_fingerprint(paths, tool)?
        ));
    }
    for fragment in &fragments {
        inputs = inputs.file(fragment)?;
    }
    inputs = inputs.file(&cert)?;
    let stamp = Stamp::new(
        paths,
        "kernel",
        inputs,
        &[
            &image,
            &linked_cert,
            &release_file,
            &modules_root,
            &unsigned_module,
        ],
    );
    if stamp.is_current() {
        check_resolved(paths, &obj.join(".config"))?;
        let release = fs::read_to_string(&release_file)?.trim().to_owned();
        say!("  linux {} is up to date", recipe.version);
        return Ok(Kernel {
            modules: modules_root.join(MODULE_DIR).join(&release),
            image,
            release,
            unsigned_module,
        });
    }
    stamp.clear()?;

    let src = recipe.unpack(paths)?;
    fs::create_dir_all(&obj)?;
    util::step(&format!("Configuring linux {}", recipe.version));
    let all: Vec<PathBuf> = UPSTREAM_FRAGMENTS
        .iter()
        .map(|fragment| src.join(fragment))
        .chain(fragments)
        .collect();
    // Without -m, merge_config.sh resolves the merged fragments with
    // `make alldefconfig`: every option not named takes its upstream default.
    util::run(
        Command::new(src.join("scripts/kconfig/merge_config.sh"))
            .current_dir(&src)
            .env("CROSS_COMPILE", cross_compile(paths))
            .arg("-O")
            .arg(&obj)
            .args(&all),
    )?;
    let config = obj.join(".config");
    verify_config(&all, &config)?;
    check_resolved(paths, &config)?;
    fs::copy(&cert, obj.join(TRUSTED_CERT))?;

    util::step(&format!("Building linux {}", recipe.version));
    // Its jobs come from the pool the package builds share (bz build runs
    // both side by side).
    let jobs = jobserver::shared(paths)?;
    let _token = jobs.acquire()?;
    util::run_logged(
        make(paths, &src, &obj)
            .env("MAKEFLAGS", jobs.makeflags())
            .args(["bzImage", "modules"]),
        "linux",
        &paths.work.join("logs/linux.log"),
    )?;
    let release = util::output(make(paths, &src, &obj).args(["-s", "kernelrelease"]))?;

    ensure!(
        !obj.join(GENERATED_KEY).exists(),
        "kbuild generated a module signing key: the configuration must leave \
         signing to bz (ADR 0015)"
    );

    util::step("Installing modules");
    if modules_root.exists() {
        fs::remove_dir_all(&modules_root)?;
    }
    util::run(
        make(paths, &src, &obj)
            .arg("modules_install")
            // kbuild installs into <prefix>/lib/modules: prefix /usr.
            .arg(format!(
                "INSTALL_MOD_PATH={}",
                modules_root.join("usr").display()
            ))
            // depmod runs on the assembled trees, with Beize's kmod.
            .arg("DEPMOD=true"),
    )?;
    let modules = modules_root.join(MODULE_DIR).join(&release);
    // Links back into the build tree; nothing in the image may point there.
    for link in ["build", "source"] {
        let _ = fs::remove_file(modules.join(link));
    }
    fs::copy(obj.join(UNSIGNED_MODULE), &unsigned_module)?;

    fs::copy(obj.join("arch/x86/boot/bzImage"), &image)?;
    fs::copy(&cert, &linked_cert)?;
    fs::write(&release_file, &release)?;
    stamp.write()?;
    say!("  kernel {release} -> {}", image.display());
    Ok(Kernel {
        image,
        release,
        modules,
        unsigned_module,
    })
}

/// The kernel for the key set whose module certificate is `cert`: the one
/// the kernel step built if it has that certificate (the test keys), else
/// that kernel linked again with `cert` into `out/vmlinuz`. Only the
/// certificate's object and the link are redone.
pub fn image_for(paths: &Paths, cert: &Path, out: &Path) -> Result<PathBuf> {
    let built = built(paths)?;
    if fs::read(cert)? == fs::read(paths.out.join(LINKED_CERT))? {
        return Ok(built.image);
    }
    // One object tree: one link at a time.
    static LINKING: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _linking = LINKING
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let recipe = Recipe::load(paths, "linux")?;
    let src = recipe.unpack(paths)?;
    let obj = paths.work.join("linux-obj");
    util::step("Linking the kernel with the module certificate of these keys");
    fs::copy(cert, obj.join(TRUSTED_CERT))?;
    let jobs = jobserver::shared(paths)?;
    let token = jobs.acquire()?;
    let linked = util::run_logged(
        make(paths, &src, &obj)
            .env("MAKEFLAGS", jobs.makeflags())
            .arg("bzImage"),
        "linux",
        &paths.work.join("logs/linux-link.log"),
    );
    drop(token);
    let image = out.join("vmlinuz");
    let copied = linked.and_then(|()| {
        fs::copy(obj.join("arch/x86/boot/bzImage"), &image)
            .map(drop)
            .map_err(Into::into)
    });
    // The object tree belongs to the kernel step: back to its certificate,
    // so that its next build there links what its stamp says.
    fs::copy(paths.out.join(LINKED_CERT), obj.join(TRUSTED_CERT))?;
    copied?;
    Ok(image)
}

/// The kernel's modules signed with a module key and its certificate, in
/// `out/signed-modules`, laid out as [`Kernel::modules`]. ML-DSA signs with
/// fresh randomness, so a signature is kept like anything built: made once
/// per kernel build and key set, and an image assembled again comes out the
/// same. What is unsigned is the same on every build machine (#5).
pub fn signed_modules(
    paths: &Paths,
    (key, cert): (PathBuf, PathBuf),
    out: &Path,
) -> Result<PathBuf> {
    let kernel = built(paths)?;
    let signed = out.join("signed-modules");
    // A stamp per place, so the signatures of the test and the owner's keys
    // are both kept.
    let place = out
        .strip_prefix(&paths.root)
        .unwrap_or(out)
        .to_string_lossy()
        .replace('/', "-");
    let stamp = Stamp::new(
        paths,
        &format!("signed-modules-{place}"),
        Fingerprint::new()?.tree(&kernel.modules)?.file(&cert)?,
        &[&signed],
    );
    if stamp.is_current() {
        return Ok(signed);
    }
    stamp.clear()?;
    tree::recreate(&signed)?;
    for module in module_files(&kernel.modules)? {
        let target = signed.join(module.strip_prefix(&kernel.modules)?);
        fs::create_dir_all(target.parent().context("a module without a directory")?)?;
        fs::copy(&module, &target)?;
    }
    sign(paths, &signed, &key, &cert)?;
    stamp.write()?;
    Ok(signed)
}

/// Replaces every module below `modules` – `usr/lib/modules/<release>` of an
/// assembled tree – with its copy from [`signed_modules`].
pub fn use_signed(signed: &Path, modules: &Path) -> Result<()> {
    for module in module_files(modules)? {
        let source = signed.join(module.strip_prefix(modules)?);
        fs::copy(&source, &module)
            .with_context(|| format!("copying the signed {}", source.display()))?;
    }
    Ok(())
}

/// Signs every module below `dir` in place with `key` and its certificate,
/// as kbuild would: uncompressed, with kbuild's `sign-file` and the
/// configured hash, then compressed again as kbuild compresses.
fn sign(paths: &Paths, dir: &Path, key: &Path, cert: &Path) -> Result<()> {
    let count = module_files(dir)?.len();
    let sign_file = paths.work.join("linux-obj/scripts/sign-file");
    for compressed in module_files(dir)? {
        let module = compressed.with_extension("");
        util::run(
            Command::new("zstd")
                .args(["-d", "--rm", "-f", "-q"])
                .arg(&compressed),
        )?;
        ensure!(
            !fs::read(&module)?.ends_with(SIGNATURE_MAGIC),
            "{} is signed already",
            module.display()
        );
        util::run(
            Command::new(&sign_file)
                .arg(SIGNATURE_HASH)
                .arg(key)
                .arg(cert)
                .arg(&module),
        )?;
        ensure!(
            fs::read(&module)?.ends_with(SIGNATURE_MAGIC),
            "sign-file left {} unsigned",
            module.display()
        );
        util::run(Command::new("zstd").args(["--rm", "-f", "-q"]).arg(&module))?;
    }
    say!("  signed {count} modules -> {}", dir.display());
    Ok(())
}

/// The kernel the last `bz kernel` built, for the steps that assemble trees.
pub fn built(paths: &Paths) -> Result<Kernel> {
    let release = fs::read_to_string(paths.out.join("kernel.release"))
        .context("reading kernel.release; run `bz kernel`")?
        .trim()
        .to_owned();
    let kernel = Kernel {
        image: paths.out.join("vmlinuz"),
        modules: paths.out.join("modules").join(MODULE_DIR).join(&release),
        unsigned_module: paths.out.join("unsigned-module.ko"),
        release,
    };
    for output in [&kernel.image, &kernel.modules, &kernel.unsigned_module] {
        if !output.exists() {
            bail!("{} is missing; run `bz kernel`", output.display());
        }
    }
    Ok(kernel)
}

/// Every module file below `dir`, sorted.
pub fn module_files(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    let mut pending = vec![dir.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in fs::read_dir(&dir).with_context(|| format!("reading {}", dir.display()))? {
            let path = entry?.path();
            if path.is_dir() {
                pending.push(path);
            } else if path.to_string_lossy().ends_with(".ko.zst") {
                files.push(path);
            }
        }
    }
    files.sort();
    Ok(files)
}

/// A module's name as the kernel and kmod use it: `sd_mod` for
/// `.../sd_mod.ko.zst`, with `-` and `_` treated alike (as `_`).
pub fn module_name(file: &Path) -> String {
    let name = file.file_name().unwrap_or_default().to_string_lossy();
    name.strip_suffix(".ko.zst")
        .unwrap_or(&name)
        .replace('-', "_")
}

/// Beize's fragments in `kernel/config`, in name order.
fn own_fragments(paths: &Paths) -> Result<Vec<PathBuf>> {
    let mut fragments: Vec<PathBuf> = fs::read_dir(paths.root.join("kernel/config"))?
        .map(|entry| entry.map(|e| e.path()))
        .collect::<Result<_, _>>()?;
    fragments.retain(|path| path.extension().is_some_and(|ext| ext == "config"));
    fragments.sort();
    Ok(fragments)
}

/// What compiles the kernel: Beize's toolchain, the same as every package's
/// (as Yocto, Buildroot and Fedora do), so out-of-tree modules can be built
/// with the kernel's compiler. Only kbuild's helper programs, which run
/// during the build, use the build host's compiler (HOSTCC).
pub const TOOLCHAIN: [&str; 2] = ["binutils", "gcc"];

/// The toolchain's prefix for kbuild. Kconfig asks the compiler what it
/// supports, so configuring needs it too.
fn cross_compile(paths: &Paths) -> String {
    format!("{}/{TARGET}-", paths.tools.join("bin").display())
}

fn make(paths: &Paths, src: &Path, obj: &Path) -> Command {
    let mut cmd = Command::new("make");
    cmd.current_dir(src)
        .arg(format!("O={}", obj.display()))
        .env("CROSS_COMPILE", cross_compile(paths))
        .env("KBUILD_BUILD_USER", BUILD_USER)
        .env("KBUILD_BUILD_HOST", BUILD_HOST)
        .env("KBUILD_BUILD_TIMESTAMP", BUILD_TIMESTAMP);
    cmd
}

/// Compares the resolved configuration with [`RESOLVED`] in the repository.
/// A difference is written there for review; with `--locked` (CI) it fails.
fn check_resolved(paths: &Paths, config: &Path) -> Result<()> {
    let resolved = without_tool_versions(
        &fs::read_to_string(config).with_context(|| format!("reading {}", config.display()))?,
    );
    let kept = paths.root.join(RESOLVED);
    if fs::read_to_string(&kept).is_ok_and(|text| text == resolved) {
        return Ok(());
    }
    if util::locked() {
        bail!(
            "the resolved kernel configuration differs from {RESOLVED}; \
             run `bz kernel` and commit the result together with the fragments"
        );
    }
    fs::write(&kept, resolved)?;
    say!("  {RESOLVED} changed: review `git diff {RESOLVED}` and commit it");
    Ok(())
}

/// The `.config` without the version numbers of compiler, assembler and
/// linker, which differ between build hosts with the same toolchain.
fn without_tool_versions(config: &str) -> String {
    config
        .lines()
        .filter(|line| {
            let key = line.split('=').next().unwrap_or_default();
            !(key == "CONFIG_CC_VERSION_TEXT" || key.ends_with("_VERSION"))
        })
        .map(|line| format!("{line}\n"))
        .collect()
}

/// Fails if Kconfig dropped or changed any option the fragments ask for,
/// e.g. because a dependency is missing or the option no longer exists.
/// Later fragments override earlier ones, as when they are merged.
fn verify_config(fragments: &[PathBuf], config: &Path) -> Result<()> {
    let mut wanted = BTreeMap::new();
    for fragment in fragments {
        wanted.extend(parse_config(
            &fs::read_to_string(fragment)
                .with_context(|| format!("reading {}", fragment.display()))?,
        ));
    }
    let actual = parse_config(&fs::read_to_string(config)?);

    let mismatches: Vec<String> = wanted
        .iter()
        .filter(|(key, value)| actual.get(*key).map_or("n", String::as_str) != value.as_str())
        .map(|(key, value)| {
            let got = actual.get(key).map_or("unset", String::as_str);
            format!("{key}: wanted {value}, got {got}")
        })
        .collect();

    if !mismatches.is_empty() {
        bail!(
            "the kernel configuration does not match the fragments:\n  {}",
            mismatches.join("\n  ")
        );
    }
    Ok(())
}

/// Parses `CONFIG_X=value` and `# CONFIG_X is not set` lines.
/// A disabled option is represented by the value `n`.
fn parse_config(text: &str) -> BTreeMap<String, String> {
    text.lines()
        .filter_map(|line| {
            let line = line.trim();
            if let Some(rest) = line.strip_prefix("# ") {
                let key = rest.strip_suffix(" is not set")?;
                return key
                    .starts_with("CONFIG_")
                    .then(|| (key.to_owned(), "n".to_owned()));
            }
            let (key, value) = line.split_once('=')?;
            key.starts_with("CONFIG_")
                .then(|| (key.to_owned(), value.to_owned()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_set_and_unset_options() {
        let config = parse_config(
            "# comment\nCONFIG_EFI=y\nCONFIG_LOCALVERSION=\"-beize\"\n# CONFIG_DEBUG is not set\n\n",
        );
        assert_eq!(config.len(), 3);
        assert_eq!(config["CONFIG_EFI"], "y");
        assert_eq!(config["CONFIG_LOCALVERSION"], "\"-beize\"");
        assert_eq!(config["CONFIG_DEBUG"], "n");
    }

    #[test]
    fn ignores_plain_comments() {
        assert!(parse_config("# Boot: the kernel is a UKI\n").is_empty());
    }

    #[test]
    fn later_fragments_override_and_absent_means_off() {
        let dir = std::env::temp_dir().join(format!("bz-kconfig-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let write = |name: &str, text: &str| {
            let path = dir.join(name);
            fs::write(&path, text).unwrap();
            path
        };
        let upstream = write("a.config", "CONFIG_CFI=y\nCONFIG_EFI=y\n");
        let own = write("b.config", "# CONFIG_CFI is not set\n");
        let config = write(".config", "CONFIG_EFI=y\n");
        verify_config(&[upstream.clone(), own], &config).unwrap();
        assert!(verify_config(&[upstream], &config).is_err());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn module_names_use_underscores() {
        assert_eq!(
            module_name(Path::new("kernel/drivers/scsi/sd_mod.ko.zst")),
            "sd_mod"
        );
        assert_eq!(
            module_name(Path::new("kernel/drivers/nvme/host/nvme-core.ko.zst")),
            "nvme_core"
        );
    }

    #[test]
    fn tool_versions_are_not_part_of_the_resolved_configuration() {
        let config = "CONFIG_CC_VERSION_TEXT=\"gcc 15\"\nCONFIG_GCC_VERSION=150200\n\
                      CONFIG_EFI=y\n# CONFIG_CFI is not set\n";
        assert_eq!(
            without_tool_versions(config),
            "CONFIG_EFI=y\n# CONFIG_CFI is not set\n"
        );
    }
}
