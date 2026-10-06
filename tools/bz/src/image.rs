use std::fs::{self, File, OpenOptions};
use std::io::{self, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};

use crate::paths::Paths;
use crate::rootfs::OS_RELEASE;
use crate::{secureboot, util};

const SECTOR: u64 = 512;
const MIB: u64 = 1024 * 1024;

/// Partition layout: 1 MiB alignment gap, ESP, root, 1 MiB for the backup GPT.
const ESP_START: u64 = MIB / SECTOR;
const ESP_SIZE: u64 = 64 * MIB;
/// Byte offset of the ESP inside the disk image (for mtools `-i disk@@offset`).
pub const ESP_OFFSET: u64 = ESP_START * SECTOR;
/// The Unified Kernel Image on the ESP, as an mtools path.
pub const UKI_PATH_ON_ESP: &str = "::/EFI/Linux/beize.efi";

/// The UKI before signing; kept for the Secure Boot rejection test.
pub fn unsigned_uki(paths: &Paths) -> PathBuf {
    paths.work.join("beize.unsigned.efi")
}

/// Fixed identifiers keep the image reproducible.
const DISK_GUID: &str = "6265697A-6500-4000-8000-000000000001";
const ESP_GUID: &str = "6265697A-6500-4000-8000-000000000002";
const ROOT_GUID: &str = "6265697A-6500-4000-8000-000000000003";
const ESP_TYPE: &str = "C12A7328-F81F-11D2-BA4B-00A0C93EC93B";
/// Discoverable Partitions Specification: root partition, x86-64.
const ROOT_TYPE: &str = "4F68BCE3-E8CD-4DB1-96E7-FBCAF984B709";
const ESP_VOLUME_ID: &str = "42454953";

fn kernel_cmdline() -> String {
    format!(
        "root=PARTUUID={} rootfstype=erofs ro init=/usr/lib/systemd/systemd \
         console=tty0 console=ttyS0,115200 loglevel=4",
        ROOT_GUID.to_lowercase()
    )
}

/// Boot loader configuration: boot the newest entry immediately; the menu
/// appears while a key is held. No command line editing under Secure Boot.
const LOADER_CONF: &str = "timeout 0\neditor no\nconsole-mode keep\n";

/// Builds and signs the boot chain and writes `out/disk.img`: a GPT disk
/// whose ESP holds systemd-boot and the Unified Kernel Image, and whose
/// second partition holds the read-only root filesystem.
pub fn build(paths: &Paths) -> Result<PathBuf> {
    let kernel = paths.out.join("vmlinuz");
    let root_image = paths.out.join("root.erofs");
    for input in [&kernel, &root_image] {
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
    fs::write(&os_release, OS_RELEASE)?;
    let unsigned_uki = unsigned_uki(paths);
    util::run(
        Command::new("ukify")
            .arg("build")
            .arg("--linux")
            .arg(&kernel)
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
    let root_size = fs::metadata(&root_image)?.len().div_ceil(MIB) * MIB;
    let root_start = ESP_START + ESP_SIZE / SECTOR;
    let disk_size = root_start * SECTOR + root_size + MIB;

    let disk = paths.out.join("disk.img");
    let file = File::create(&disk)?;
    file.set_len(disk_size)?;
    drop(file);

    partition(&disk, root_start, root_size / SECTOR)?;
    util::run(
        Command::new("mkfs.vfat")
            .args(["-F", "32", "-n", "BEIZE-ESP", "-i", ESP_VOLUME_ID])
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
        (&boot_loader, "::/EFI/BOOT/BOOTX64.EFI"),
        (&boot_loader, "::/EFI/systemd/systemd-bootx64.efi"),
        // Type #2 entry, discovered by systemd-boot without a config file.
        (&uki, UKI_PATH_ON_ESP),
        (&loader_conf, "::/loader/loader.conf"),
    ] {
        util::run(Command::new("mcopy").args(["-i", &esp]).arg(file).arg(dest))?;
    }
    write_at(&disk, root_start * SECTOR, &root_image)?;

    eprintln!(
        "  disk image -> {} ({} MiB)",
        disk.display(),
        disk_size / MIB
    );
    Ok(disk)
}

fn partition(disk: &Path, root_start: u64, root_sectors: u64) -> Result<()> {
    let layout = format!(
        "label: gpt\nlabel-id: {DISK_GUID}\nunit: sectors\n\
         start={ESP_START}, size={}, type={ESP_TYPE}, uuid={ESP_GUID}, name=\"esp\"\n\
         start={root_start}, size={root_sectors}, type={ROOT_TYPE}, uuid={ROOT_GUID}, name=\"root-x86-64\"\n",
        ESP_SIZE / SECTOR
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
