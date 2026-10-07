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
use crate::{secureboot, util};

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
const DISK_GUID: &str = "6265697A-6500-4000-8000-000000000001";
const ESP_GUID: &str = "6265697A-6500-4000-8000-000000000002";
const ESP_VOLUME_ID: &str = "42454953";

/// Boot loader configuration: boot the newest entry immediately; the menu
/// appears while a key is held. No command line editing under Secure Boot.
const LOADER_CONF: &str = "timeout 0\neditor no\nconsole-mode keep\n";

/// The kernel command line, sealed into the signed UKI. `usrhash=` pins
/// `/usr` to exactly the slot whose content matches the root hash (systemd
/// finds data and hash partition by the UUIDs derived from it) and makes the
/// kernel verify every block read. The root partition is discovered on the
/// boot disk (and created there on first boot).
fn kernel_cmdline(root_hash: &str) -> String {
    format!(
        "usrhash={root_hash} mount.usr=/dev/mapper/usr mount.usrfstype=erofs mount.usrflags=ro \
         rw console=tty0 console=ttyS0,115200 loglevel=4"
    )
}

/// The UKI on the ESP, as an mtools path.
pub fn uki_path_on_esp() -> String {
    format!("::/EFI/Linux/{}", layout::uki_name())
}

/// The UKI before signing; kept for the Secure Boot rejection test.
pub fn unsigned_uki(paths: &Paths) -> PathBuf {
    paths.work.join("beize.unsigned.efi")
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

/// Builds and signs the boot chain and writes `out/disk.img`.
pub fn build(paths: &Paths) -> Result<PathBuf> {
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
    let keys = secureboot::Keys::test(paths)?;

    util::step("Building Unified Kernel Image");
    let os_release = paths.work.join("os-release");
    fs::write(&os_release, layout::os_release())?;
    let unsigned_uki = unsigned_uki(paths);
    let (pcr_private, pcr_public) = keys.pcr_key();
    util::run(
        ukify(paths)?
            .arg("build")
            .arg("--linux")
            .arg(&kernel)
            .arg("--initrd")
            .arg(&initrd)
            .arg("--stub")
            .arg(efi_dir.join("linuxx64.efi.stub"))
            .arg(format!("--cmdline={}", kernel_cmdline(root_hash)))
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

    util::step("Signing boot chain (test keys)");
    let uki = paths.out.join("beize.efi");
    keys.sign(&unsigned_uki, &uki)?;
    let boot_loader = paths.out.join("systemd-bootx64.efi");
    keys.sign(&efi_dir.join("systemd-bootx64.efi"), &boot_loader)?;
    keys.firmware_vars(&paths.out.join("ovmf-vars-secureboot.fd"))?;

    util::step("Writing disk image");
    let disk_size = USR_VERITY_START * SECTOR + USR_VERITY_SLOT_SIZE + MIB;
    let disk = paths.out.join("disk.img");
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
    write_at(&disk, USR_START * SECTOR, &usr_image)?;
    write_at(&disk, USR_VERITY_START * SECTOR, &usr_verity)?;

    eprintln!(
        "  disk image -> {} ({} MiB, sparse)",
        disk.display(),
        disk_size / MIB
    );
    Ok(disk)
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
    eprintln!("  $ sfdisk {}", disk.display());
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
