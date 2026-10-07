//! The installable disk image: ESP with the signed boot chain, plus `/usr`
//! slot A. Everything else is created on first boot (ADR 0007).

use std::fs::{self, File, OpenOptions};
use std::io::{self, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};

use crate::layout::{self, ESP_CLUSTER_SIZE, ESP_SIZE, ESP_TYPE, MIB, USR_SLOT_SIZE, USR_TYPE};
use crate::paths::Paths;
use crate::{secureboot, util};

const SECTOR: u64 = 512;
/// The ESP starts at 1 MiB; 1 MiB at the end holds the backup GPT.
const ESP_START: u64 = MIB / SECTOR;
const USR_START: u64 = ESP_START + ESP_SIZE / SECTOR;
/// Byte offset of the ESP inside the disk image (for mtools `-i disk@@offset`).
pub const ESP_OFFSET: u64 = ESP_START * SECTOR;

/// Fixed identifiers keep the image reproducible.
const DISK_GUID: &str = "6265697A-6500-4000-8000-000000000001";
const ESP_GUID: &str = "6265697A-6500-4000-8000-000000000002";
const USR_A_GUID: &str = "6265697A-6500-4000-8000-000000000003";
const ESP_VOLUME_ID: &str = "42454953";

/// Boot loader configuration: boot the newest entry immediately; the menu
/// appears while a key is held. No command line editing under Secure Boot.
const LOADER_CONF: &str = "timeout 0\neditor no\nconsole-mode keep\n";

/// The kernel command line, sealed into the signed UKI. `/usr` is pinned to
/// this version's slot; the root partition is discovered on the boot disk
/// (and created there on first boot).
fn kernel_cmdline() -> String {
    format!(
        "mount.usr=PARTLABEL={} mount.usrfstype=erofs mount.usrflags=ro rw \
         console=tty0 console=ttyS0,115200 loglevel=4",
        layout::usr_label()
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

/// Builds and signs the boot chain and writes `out/disk.img`.
pub fn build(paths: &Paths) -> Result<PathBuf> {
    let kernel = paths.out.join("vmlinuz");
    let initrd = paths.out.join("initrd.cpio.zst");
    let usr_image = paths.out.join("usr.erofs");
    for input in [&kernel, &initrd, &usr_image] {
        if !input.exists() {
            bail!("{} is missing; run `bz build`", input.display());
        }
    }
    let release = fs::read_to_string(paths.out.join("kernel.release"))
        .context("reading kernel.release; run `bz kernel`")?;
    // Boot loader and UKI stub come from Beize's own systemd build.
    let efi_dir = paths.sysroot.join("usr/lib/systemd/boot/efi");
    let keys = secureboot::Keys::test(paths)?;

    util::step("Building Unified Kernel Image");
    let os_release = paths.work.join("os-release");
    fs::write(&os_release, layout::os_release())?;
    let unsigned_uki = unsigned_uki(paths);
    util::run(
        Command::new("ukify")
            .arg("build")
            .arg("--linux")
            .arg(&kernel)
            .arg("--initrd")
            .arg(&initrd)
            .arg("--stub")
            .arg(efi_dir.join("linuxx64.efi.stub"))
            .arg(format!("--cmdline={}", kernel_cmdline()))
            .arg(format!("--os-release=@{}", os_release.display()))
            .arg(format!("--uname={}", release.trim()))
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
    let disk_size = USR_START * SECTOR + USR_SLOT_SIZE + MIB;
    let disk = paths.out.join("disk.img");
    let file = File::create(&disk)?;
    file.set_len(disk_size)?;
    drop(file);

    partition(&disk)?;
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

    eprintln!(
        "  disk image -> {} ({} MiB, sparse)",
        disk.display(),
        disk_size / MIB
    );
    Ok(disk)
}

fn partition(disk: &Path) -> Result<()> {
    let layout = format!(
        "label: gpt\nlabel-id: {DISK_GUID}\nunit: sectors\n\
         start={ESP_START}, size={}, type={ESP_TYPE}, uuid={ESP_GUID}, name=\"esp\"\n\
         start={USR_START}, size={}, type={USR_TYPE}, uuid={USR_A_GUID}, name=\"{}\"\n",
        ESP_SIZE / SECTOR,
        USR_SLOT_SIZE / SECTOR,
        layout::usr_label()
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
