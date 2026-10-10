//! `bz hw-check`: which PCI devices of a real machine the built image has a
//! driver for (ADR 0015), read from that machine's journal before installing.
//!
//! The kernel logs every PCI device it enumerates (`pci 0000:00:17.0:
//! [8086:7a62] type 00 class 0x010601 ...`). Each is matched against the
//! module aliases of the assembled `/usr` tree, as udev would match its
//! modalias – except for the subsystem IDs, which that line lacks: an alias
//! that names specific ones never matches here, so a device is reported
//! without a driver rather than with a wrong one.

use std::fs;
use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result};

use crate::kernel::{self, MODULE_DIR};
use crate::paths::Paths;
use crate::util;

/// A PCI device as the kernel logged it.
#[derive(Debug, PartialEq)]
struct Device {
    address: String,
    vendor: String,
    device: String,
    /// Class, subclass and programming interface, e.g. `010601`.
    class: String,
}

impl Device {
    /// Parses `pci 0000:00:17.0: [8086:7a62] type 00 class 0x010601 ...`.
    fn parse(line: &str) -> Option<Self> {
        let rest = line.strip_prefix("pci ")?;
        let (address, rest) = rest.split_once(": [")?;
        let (ids, rest) = rest.split_once("] type ")?;
        let (vendor, device) = ids.split_once(':')?;
        let class = rest.split_once(" class 0x")?.1.get(..6)?;
        let hex = |s: &str, len| s.len() == len && s.chars().all(|c| c.is_ascii_hexdigit());
        (hex(vendor, 4) && hex(device, 4) && hex(class, 6)).then(|| Self {
            address: address.trim_start_matches("0000:").to_owned(),
            vendor: vendor.to_ascii_uppercase(),
            device: device.to_ascii_uppercase(),
            class: class.to_ascii_uppercase(),
        })
    }

    /// The modalias udev would see, with unknown subsystem IDs.
    fn modalias(&self) -> String {
        let (class, rest) = self.class.split_at(2);
        let (subclass, interface) = rest.split_at(2);
        format!(
            "pci:v0000{}d0000{}svXXXXXXXXsdXXXXXXXXbc{class}sc{subclass}i{interface}",
            self.vendor, self.device
        )
    }

    /// Bridges (class 06xx) are handled by the PCI core, not by a driver
    /// found through an alias (root ports: the built-in port driver).
    fn is_bridge(&self) -> bool {
        self.class.starts_with("06")
    }
}

/// Prints every PCI device of boot `boot` in the journal at `journal` with
/// the driver the built image would load for it.
pub fn show(paths: &Paths, journal: &Path, boot: &str) -> Result<()> {
    let kernel = kernel::built(paths)?;
    let modules = paths
        .work
        .join("usr-tree")
        .join(MODULE_DIR)
        .join(&kernel.release);
    let aliases =
        read_aliases(&modules).context("reading the image's module aliases; run `bz usr`")?;

    let source = if journal.is_dir() {
        "--directory"
    } else {
        "--file"
    };
    let log = util::output(Command::new("journalctl").arg(source).arg(journal).args([
        "--dmesg",
        "--boot",
        boot,
        "--output=cat",
        "--no-pager",
        "--quiet",
    ]))?;
    let mut devices: Vec<Device> = log.lines().filter_map(Device::parse).collect();
    devices.sort_by(|a, b| a.address.cmp(&b.address));
    devices.dedup_by(|a, b| a.address == b.address);

    println!(
        "PCI devices of boot {boot} in {}, against the drivers of Beize {}:",
        journal.display(),
        kernel.release
    );
    let mut missing = Vec::new();
    for device in &devices {
        let driver = if device.is_bridge() {
            "(bridge: PCI core)".to_owned()
        } else {
            let alias = device.modalias();
            let mut drivers: Vec<&str> = aliases
                .iter()
                .filter(|(pattern, _)| glob_match(pattern, &alias))
                .map(|(_, module)| module.as_str())
                .collect();
            drivers.dedup();
            if drivers.is_empty() {
                missing.push(device);
                "NONE".to_owned()
            } else {
                drivers.join(", ")
            }
        };
        println!(
            "  {}  {}:{}  {}  {:<22} {driver}",
            device.address,
            device.vendor.to_ascii_lowercase(),
            device.device.to_ascii_lowercase(),
            &device.class[..4],
            class_name(&device.class),
        );
    }
    if devices.is_empty() {
        println!("  (the kernel log of this boot lists no PCI devices)");
    } else if missing.is_empty() {
        println!("Every device has a driver.");
    } else {
        println!("{} devices without a driver in this image.", missing.len());
    }
    Ok(())
}

/// The PCI aliases of loadable (`modules.alias`) and built-in
/// (`modules.builtin.modinfo`) drivers: (pattern, module).
fn read_aliases(modules: &Path) -> Result<Vec<(String, String)>> {
    let mut aliases = Vec::new();
    for line in fs::read_to_string(modules.join("modules.alias"))?.lines() {
        if let Some(rest) = line.strip_prefix("alias pci:")
            && let Some((pattern, module)) = rest.split_once(' ')
        {
            aliases.push((format!("pci:{pattern}"), module.to_owned()));
        }
    }
    let builtin = fs::read(modules.join("modules.builtin.modinfo"))?;
    for entry in builtin.split(|byte| *byte == 0) {
        let entry = String::from_utf8_lossy(entry);
        if let Some((module, pattern)) = entry.split_once(".alias=")
            && pattern.starts_with("pci:")
        {
            aliases.push((pattern.to_owned(), format!("{module} (built in)")));
        }
    }
    Ok(aliases)
}

/// Shell-style matching with `*` and `?`, as kmod matches aliases.
fn glob_match(pattern: &str, text: &str) -> bool {
    let (pattern, text) = (pattern.as_bytes(), text.as_bytes());
    let (mut p, mut t) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while t < text.len() {
        if p < pattern.len() && (pattern[p] == b'?' || pattern[p] == text[t]) {
            p += 1;
            t += 1;
        } else if p < pattern.len() && pattern[p] == b'*' {
            star = Some((p, t));
            p += 1;
        } else if let Some((star_p, star_t)) = star {
            p = star_p + 1;
            t = star_t + 1;
            star = Some((star_p, star_t + 1));
        } else {
            return false;
        }
    }
    pattern[p..].iter().all(|&c| c == b'*')
}

/// A short name for the PCI class codes Beize meets.
fn class_name(class: &str) -> &'static str {
    match &class[..4] {
        "0100" => "SCSI storage",
        "0104" => "RAID (Intel VMD)",
        "0106" => "SATA",
        "0108" => "NVMe",
        "0200" => "Ethernet",
        "0280" => "network (WLAN)",
        "0300" => "VGA",
        "0302" => "3D controller",
        "0380" => "display",
        "0401" => "audio",
        "0403" => "audio (HDA)",
        "0500" => "RAM",
        "0700" => "serial port",
        "0780" => "communication (MEI)",
        "0880" => "system peripheral",
        "0C03" => "USB",
        "0C05" => "SMBus",
        "0C80" => "serial bus (I2C, SPI)",
        "1180" => "signal processing",
        c if c.starts_with("06") => "bridge",
        _ => "other",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_kernels_enumeration_line() {
        let device = Device::parse(
            "pci 0000:00:17.0: [8086:7a62] type 00 class 0x010601 conventional PCI endpoint",
        )
        .unwrap();
        assert_eq!(device.address, "00:17.0");
        assert_eq!(
            device.modalias(),
            "pci:v00008086d00007A62svXXXXXXXXsdXXXXXXXXbc01sc06i01"
        );
        assert!(!device.is_bridge());
        assert!(
            Device::parse("pci 0000:00:1c.0: [8086:7a38] type 01 class 0x060400 PCIe Root Port")
                .unwrap()
                .is_bridge()
        );
        assert_eq!(Device::parse("pci 0000:00:17.0: BAR 5 [mem 0x1-0x2]"), None);
    }

    #[test]
    fn matches_aliases_like_kmod_and_never_on_subsystem_ids_it_cannot_know() {
        let ahci = "pci:v00008086d00007A62svXXXXXXXXsdXXXXXXXXbc01sc06i01";
        assert!(glob_match("pci:v*d*sv*sd*bc01sc06i01*", ahci));
        assert!(glob_match("pci:v00008086d00007A62sv*sd*bc*sc*i*", ahci));
        assert!(!glob_match("pci:v00008086d00007A63sv*sd*bc*sc*i*", ahci));
        assert!(!glob_match(
            "pci:v00008086d00007A62sv00001043sd*bc*sc*i*",
            ahci
        ));
        assert!(glob_match("a?c", "abc") && !glob_match("a?c", "ac"));
    }
}
