//! The installable disk image: ESP with the signed boot chain, plus `/usr`
//! slot A (data and dm-verity hash tree). Everything else is created on first
//! boot (ADR 0007, ADR 0008).

use std::fs::{self, File, OpenOptions};
use std::io::{self, Seek, SeekFrom, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail, ensure};

use crate::layout::{
    self, ESP_CLUSTER_SIZE, ESP_SIZE, ESP_TYPE, MIB, USR_SLOT_SIZE, USR_TYPE, USR_VERITY_SLOT_SIZE,
    USR_VERITY_TYPE,
};
use crate::paths::Paths;
use crate::say;
use crate::stamp::{Fingerprint, Stamp};
use crate::{initrd, secureboot, util};

const SECTOR: u64 = 512;
/// The ESP starts at 1 MiB; 1 MiB at the end holds the backup GPT.
const ESP_START: u64 = MIB / SECTOR;
const USR_START: u64 = ESP_START + ESP_SIZE / SECTOR;
/// Byte offset of /usr slot A (its EROFS data) inside the disk image.
pub const USR_OFFSET: u64 = USR_START * SECTOR;
const USR_VERITY_START: u64 = USR_START + USR_SLOT_SIZE / SECTOR;
/// Byte offset of the ESP inside the disk image (for mtools `-i disk@@offset`).
pub const ESP_OFFSET: u64 = ESP_START * SECTOR;

/// Fixed identifiers keep the image reproducible.
pub const DISK_GUID: &str = "6265697A-6500-4000-8000-000000000001";
const ESP_GUID: &str = "6265697A-6500-4000-8000-000000000002";
const ESP_VOLUME_ID: &str = "42454953";

/// Boot loader configuration: boot the newest entry immediately; the menu
/// appears while a key is held. No command line editing under Secure Boot.
/// If the firmware is in setup mode (no keys, e.g. after "clear Secure Boot
/// keys" in the firmware setup), enroll Beize's keys from `/loader/keys/auto`
/// – after a 15 s warning that a key press aborts, at once in a VM – and
/// reboot (ADR 0010).
const LOADER_CONF: &str = "timeout 0\neditor no\nconsole-mode keep\nsecure-boot-enroll force\n";

/// The trust profile every image is built for; selectable once there is a
/// second one.
const PROFILE: secureboot::Profile = secureboot::Profile::WithWindows;

/// Where an image boots. The images differ only in their command line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    /// QEMU in `bz smoke`/`bz run`: the serial port is the console the tests
    /// read, and test hooks arrive as credentials (SMBIOS type 11).
    Vm,
    /// Real hardware: the screen is the console – where systemd asks for the
    /// recovery key – and no credentials are imported: SMBIOS OEM strings can
    /// be rewritten from another operating system on some boards.
    Hardware,
}

/// The kernel command line, sealed into the signed UKI. `usrhash=` pins
/// `/usr` to exactly the slot whose content matches the root hash (systemd
/// finds data and hash partition by the UUIDs derived from it) and makes the
/// kernel verify every block read. The root partition is discovered on the
/// boot disk (and created there on first boot). The last `console=` becomes
/// /dev/console; kernel messages go to both.
fn kernel_cmdline(root_hash: &str, target: Target) -> String {
    let common = format!(
        "usrhash={root_hash} mount.usr=/dev/mapper/usr mount.usrfstype=erofs mount.usrflags=ro \
         rw loglevel=4"
    );
    match target {
        Target::Vm => format!("{common} console=tty0 console=ttyS0,115200"),
        Target::Hardware => {
            format!("{common} console=ttyS0,115200 console=tty0 systemd.import_credentials=no")
        }
    }
}

/// The UKI on the ESP, as an mtools path.
pub fn uki_path_on_esp() -> String {
    format!("::/EFI/Linux/{}", layout::uki_name())
}

/// The UKI before signing; kept for the Secure Boot rejection test.
pub fn unsigned_uki(paths: &Paths) -> PathBuf {
    paths.out.join(UNSIGNED_UKI)
}

const UNSIGNED_UKI: &str = "beize.unsigned.efi";

/// Where `bz sign` puts the boot chain and disk image for real hardware.
pub fn production_dir(paths: &Paths) -> PathBuf {
    paths.out.join("production")
}

/// ukify from Beize's own systemd build, using systemd-measure from the same
/// build: both must match the stub and the systemd inside the image (e.g. the
/// signed PCR policies systemd expects). systemd-measure is a target binary;
/// it runs on the build host through the sysroot's dynamic loader.
fn ukify(paths: &Paths) -> Result<Command> {
    let ukify = paths.sysroot.join("usr/lib/beize-build/ukify");
    ensure!(
        ukify.exists(),
        "{} is missing; run `bz package systemd`",
        ukify.display()
    );
    let tools = paths.work.join("uki-tools");
    fs::create_dir_all(&tools)?;
    let measure = tools.join("systemd-measure");
    let sysroot = paths.sysroot.display();
    fs::write(
        &measure,
        format!(
            "#!/bin/sh\n\
             exec '{sysroot}/usr/lib/ld-linux-x86-64.so.2' \
             --library-path '{sysroot}/usr/lib:{sysroot}/usr/lib/systemd' \
             '{sysroot}/usr/lib/systemd/systemd-measure' \"$@\"\n"
        ),
    )?;
    fs::set_permissions(&measure, fs::Permissions::from_mode(0o755))?;
    let mut cmd = Command::new("python3");
    cmd.arg(ukify).arg("--tools").arg(&tools);
    Ok(cmd)
}

/// Builds the boot chain, signs it with the test keys and writes `out/disk.img`.
pub fn build(paths: &Paths) -> Result<PathBuf> {
    let disk = paths.out.join("disk.img");
    let mut inputs = Fingerprint::new()?;
    for built in [
        "vmlinuz",
        "kernel.release",
        "initrd.cpio.zst",
        "usr.erofs",
        "usr.verity",
        "usr.roothash",
    ] {
        inputs = inputs.file(&paths.out.join(built))?;
    }
    inputs = inputs
        .tree(&paths.sysroot.join("usr/lib/systemd/boot/efi"))?
        .file(&paths.sysroot.join("usr/lib/beize-build/ukify"))?
        .tree(&secureboot::microsoft_dir(paths))?;
    // The test keys, not the enrollment files made from them: those carry
    // the time of signing and differ on every build.
    let test_keys = secureboot::test_keys_dir(paths);
    if test_keys.is_dir() {
        for entry in fs::read_dir(&test_keys)? {
            let path = entry?.path();
            if path.is_file() {
                inputs = inputs.file(&path)?;
            }
        }
    }
    let stamp = Stamp::new(
        paths,
        "image",
        inputs,
        &[&disk, &unsigned_uki(paths), &paths.out.join("beize.efi")],
    );
    if stamp.is_current() {
        say!("  disk image is up to date");
        return Ok(disk);
    }
    stamp.clear()?;
    let keys = secureboot::Keys::test(paths, PROFILE)?;
    let disk = assemble(paths, &keys, &paths.out, Target::Vm)?;
    stamp.write()?;
    Ok(disk)
}

/// Builds the boot chain and disk image for real hardware into
/// `out/production/`, signed with the owner's keys from `store`.
pub fn sign(paths: &Paths, store: &secureboot::Store) -> Result<PathBuf> {
    let keys = secureboot::Keys::production(store, &secureboot::production_public_dir(paths))?;
    let out = production_dir(paths);
    fs::create_dir_all(&out)?;
    assemble(paths, &keys, &out, Target::Hardware)
}

/// Builds the UKI for `target`, signs it and systemd-boot with `keys` and
/// writes the disk image, all into `out`.
pub fn assemble(
    paths: &Paths,
    keys: &secureboot::Keys,
    out: &Path,
    target: Target,
) -> Result<PathBuf> {
    let kernel = paths.out.join("vmlinuz");
    let initrd = paths.out.join("initrd.cpio.zst");
    let usr_image = paths.out.join("usr.erofs");
    let usr_verity = paths.out.join("usr.verity");
    for input in [&kernel, &initrd, &usr_image, &usr_verity] {
        if !input.exists() {
            bail!("{} is missing; run `bz build`", input.display());
        }
    }
    let release = fs::read_to_string(paths.out.join("kernel.release"))
        .context("reading kernel.release; run `bz kernel`")?;
    let root_hash = fs::read_to_string(paths.out.join("usr.roothash"))
        .context("reading usr.roothash; run `bz usr`")?;
    let root_hash = root_hash.trim();
    // Boot loader and UKI stub come from Beize's own systemd build.
    let efi_dir = paths.sysroot.join("usr/lib/systemd/boot/efi");

    util::step("Building Unified Kernel Image");
    let os_release = paths.work.join("os-release");
    fs::write(&os_release, layout::os_release())?;
    let unsigned_uki = out.join(UNSIGNED_UKI);
    let (pcr_private, pcr_public) = keys.pcr_key();
    let profile = profile_initrd(keys, out)?;
    util::run(
        ukify(paths)?
            .arg("build")
            .arg("--linux")
            .arg(&kernel)
            .arg("--initrd")
            .arg(&initrd)
            // The Secure Boot profile of these keys, signed with the UKI.
            .arg("--initrd")
            .arg(&profile)
            .arg("--stub")
            .arg(efi_dir.join("linuxx64.efi.stub"))
            .arg(format!("--cmdline={}", kernel_cmdline(root_hash, target)))
            .arg(format!("--os-release=@{}", os_release.display()))
            .arg(format!("--uname={}", release.trim()))
            // Signed PCR 11 policies (.pcrsig) and their public key (.pcrpkey).
            // The extra initrd-only policies authorise systemd to set up the
            // TPM's NvPCRs (systemd-tpm2-setup-early).
            .arg("--pcr-private-key")
            .arg(&pcr_private)
            .arg("--pcr-public-key")
            .arg(&pcr_public)
            .arg("--sign-initrd-pcrs")
            .arg("--output")
            .arg(&unsigned_uki),
    )?;

    util::step("Signing boot chain");
    let uki = out.join("beize.efi");
    keys.sign(&unsigned_uki, &uki)?;
    let boot_loader = out.join("systemd-bootx64.efi");
    keys.sign(&efi_dir.join("systemd-bootx64.efi"), &boot_loader)?;

    util::step("Writing disk image");
    let disk_size = USR_VERITY_START * SECTOR + USR_VERITY_SLOT_SIZE + MIB;
    let disk = out.join("disk.img");
    let file = File::create(&disk)?;
    file.set_len(disk_size)?;
    drop(file);

    partition(&disk, root_hash)?;
    util::run(
        Command::new("mkfs.vfat")
            .args(["-F", "32", "-n", "BEIZE-ESP", "-i", ESP_VOLUME_ID])
            .args(["-S", &SECTOR.to_string()])
            .args(["-s", &(ESP_CLUSTER_SIZE / SECTOR).to_string()])
            .arg(format!("--offset={ESP_START}"))
            .arg(&disk)
            .arg((ESP_SIZE / 1024).to_string()),
    )?;
    let esp = format!("{}@@{ESP_OFFSET}", disk.display());
    util::run(Command::new("mmd").args(["-i", &esp]).args([
        "::/EFI",
        "::/EFI/BOOT",
        "::/EFI/systemd",
        "::/EFI/Linux",
        "::/loader",
        "::/loader/keys",
        "::/loader/keys/auto",
    ]))?;
    let loader_conf = paths.work.join("loader.conf");
    fs::write(&loader_conf, LOADER_CONF)?;
    for (file, dest) in [
        // Removable-media path: what firmware boots without NVRAM entries.
        (&boot_loader, "::/EFI/BOOT/BOOTX64.EFI".to_owned()),
        (
            &boot_loader,
            "::/EFI/systemd/systemd-bootx64.efi".to_owned(),
        ),
        // Type #2 entry with boot counter, discovered by systemd-boot.
        (&uki, uki_path_on_esp()),
        (&loader_conf, "::/loader/loader.conf".to_owned()),
    ] {
        util::run(Command::new("mcopy").args(["-i", &esp]).arg(file).arg(dest))?;
    }
    for file in secureboot::ENROLL_FILES {
        util::run(
            Command::new("mcopy")
                .args(["-i", &esp])
                .arg(keys.enroll_dir().join(file))
                .arg(format!("::/loader/keys/auto/{file}")),
        )?;
    }
    write_at(&disk, USR_START * SECTOR, &usr_image)?;
    write_at(&disk, USR_VERITY_START * SECTOR, &usr_verity)?;

    say!(
        "  disk image -> {} ({} MiB, sparse)",
        disk.display(),
        disk_size / MIB
    );
    Ok(disk)
}

/// A second initrd archive with the Secure Boot profile of `keys` under
/// `/usr/lib/beize/secureboot/`: what beize-tpm checks the firmware against.
/// It differs between test and production keys, so it is not part of the
/// shared initrd; inside the UKI it is signed like everything else.
fn profile_initrd(keys: &secureboot::Keys, out: &Path) -> Result<PathBuf> {
    let root = out.join("profile-initrd");
    if root.exists() {
        fs::remove_dir_all(&root)?;
    }
    let dir = root.join("usr/lib/beize/secureboot");
    fs::create_dir_all(&dir)?;
    for file in secureboot::PROFILE_FILES {
        fs::copy(keys.enroll_dir().join(file), dir.join(file))?;
    }
    let archive = out.join("profile.cpio.zst");
    initrd::pack(&root, &archive)?;
    fs::remove_dir_all(&root)?;
    Ok(archive)
}

fn partition(disk: &Path, root_hash: &str) -> Result<()> {
    let (data_uuid, verity_uuid) = layout::verity_partition_uuids(root_hash)?;
    let label = layout::usr_label();
    let layout = format!(
        "label: gpt
label-id: {DISK_GUID}
unit: sectors
         start={ESP_START}, size={}, type={ESP_TYPE}, uuid={ESP_GUID}, name=\"esp\"
         start={USR_START}, size={}, type={USR_TYPE}, uuid={data_uuid}, name=\"{label}\"
         start={USR_VERITY_START}, size={}, type={USR_VERITY_TYPE}, uuid={verity_uuid},          name=\"{label}\"
",
        ESP_SIZE / SECTOR,
        USR_SLOT_SIZE / SECTOR,
        USR_VERITY_SLOT_SIZE / SECTOR,
    );
    if util::verbose() {
        say!("  $ sfdisk {}", disk.display());
    }
    let mut child = Command::new("sfdisk")
        .arg("--quiet")
        .arg(disk)
        .stdin(Stdio::piped())
        .spawn()
        .context("starting sfdisk")?;
    child
        .stdin
        .take()
        .context("sfdisk stdin")?
        .write_all(layout.as_bytes())?;
    let status = child.wait()?;
    if !status.success() {
        bail!("sfdisk failed with {status}");
    }
    Ok(())
}

fn write_at(disk: &Path, offset: u64, content: &Path) -> Result<()> {
    let mut out = OpenOptions::new().write(true).open(disk)?;
    out.seek(SeekFrom::Start(offset))?;
    io::copy(&mut File::open(content)?, &mut out)?;
    out.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The images for VM tests and real hardware must differ in nothing but
    /// the console order and the credential import: what the tests check is
    /// what runs on hardware.
    #[test]
    fn hardware_and_vm_command_lines_differ_only_where_intended() {
        let vm = kernel_cmdline("abc", Target::Vm);
        let hardware = kernel_cmdline("abc", Target::Hardware);
        let words = |line: &str| line.split(' ').map(str::to_owned).collect::<Vec<_>>();
        let strip = |mut w: Vec<String>| {
            w.retain(|w| !w.starts_with("console=") && w != "systemd.import_credentials=no");
            w
        };
        assert_eq!(strip(words(&vm)), strip(words(&hardware)));
        // The last console= is /dev/console: the serial port in the VM, the screen on hardware.
        assert!(vm.ends_with("console=ttyS0,115200"));
        assert!(hardware.contains("console=ttyS0,115200 console=tty0 "));
        assert!(hardware.ends_with("systemd.import_credentials=no"));
        assert!(!vm.contains("import_credentials"));
    }
}
