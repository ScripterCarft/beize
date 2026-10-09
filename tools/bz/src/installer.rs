//! `bz install`: writes the image for real hardware onto a disk – the one disk
//! the owner names by its GPT disk GUID, and only if it holds nothing.
//!
//! Disks are easy to confuse (on the owner's PC the Windows disk and the
//! target have exactly the same size), so the target must prove its identity
//! and emptiness before anything is written:
//! - a whole disk, nothing on it mounted;
//! - its GPT disk GUID equals the one the owner gives (Windows shows it with
//!   `Get-Disk`, Linux reads it from the disk itself);
//! - no partitions, or only an empty "Microsoft reserved" one (what Windows
//!   creates on an initialised but unused disk);
//! - the image is the one signed with the owner's keys (`bz sign`);
//! - the owner confirms in a terminal.
//!
//! Beize grows into the rest of the disk on first boot (ADR 0007).

use std::fs;
use std::io::{self, BufRead, IsTerminal, Write};
use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, bail, ensure};
use serde_json::Value;

use crate::image;
use crate::paths::Paths;
use crate::say;
use crate::util;

/// "Microsoft reserved": Windows puts it on every GPT disk it initialises.
const MICROSOFT_RESERVED: &str = "E3C9E316-0B5C-4DB8-817D-F92DF00215AE";

/// What `sfdisk --json` says about a disk.
#[derive(Debug)]
struct Table {
    guid: String,
    partition_types: Vec<String>,
}

fn parse_table(sfdisk_json: &str) -> Result<Option<Table>> {
    if sfdisk_json.trim().is_empty() {
        return Ok(None); // no partition table at all
    }
    let json: Value = serde_json::from_str(sfdisk_json)?;
    let table = &json["partitiontable"];
    ensure!(
        table["label"] == "gpt",
        "the disk has a {} partition table, not GPT",
        table["label"]
    );
    Ok(Some(Table {
        guid: table["id"].as_str().context("disk GUID")?.to_uppercase(),
        partition_types: table["partitions"]
            .as_array()
            .map(|parts| {
                parts
                    .iter()
                    .filter_map(|p| p["type"].as_str().map(str::to_uppercase))
                    .collect()
            })
            .unwrap_or_default(),
    }))
}

/// Decides whether a disk may be overwritten: its GPT disk GUID must be the
/// owner's and it must hold nothing but an empty Microsoft reserved partition
/// – or, with `reinstall`, Beize (its disk GUID is Beize's own) and nothing else.
fn check_target(table: Option<&Table>, expected_guid: &str, reinstall: bool) -> Result<()> {
    let expected = expected_guid.trim_matches(['{', '}']).to_uppercase();
    let table = table.context(
        "the disk has no partition table, so its GUID cannot be checked; \
         initialise it in Windows (Disk Management, GPT) and try again",
    )?;
    ensure!(
        table.guid == expected,
        "the disk's GUID is {}, not {expected}: this is a different disk",
        table.guid
    );
    let beize = table.guid == image::DISK_GUID;
    if reinstall {
        ensure!(
            beize,
            "--reinstall only replaces Beize, and this disk does not hold Beize"
        );
        return Ok(());
    }
    ensure!(
        !beize,
        "Beize is already installed on this disk; --reinstall replaces it \
         (and erases everything Beize stored there)"
    );
    let foreign: Vec<_> = table
        .partition_types
        .iter()
        .filter(|ty| ty.as_str() != MICROSOFT_RESERVED)
        .collect();
    ensure!(
        foreign.is_empty(),
        "the disk holds partitions (types {}); Beize only installs onto an empty disk",
        foreign
            .iter()
            .map(|t| t.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    );
    Ok(())
}

/// Writes `build/out/production/disk.img` onto `disk` after all checks.
pub fn install(paths: &Paths, disk: &Path, expected_guid: &str, reinstall: bool) -> Result<()> {
    ensure!(
        io::stdin().is_terminal(),
        "bz install overwrites a disk and only runs in an interactive terminal"
    );
    let image = image::production_dir(paths).join("disk.img");
    ensure!(
        image.exists(),
        "{} is missing; build it with `bz build` and `bz sign`",
        image.display()
    );

    let name = disk
        .file_name()
        .and_then(|n| n.to_str())
        .context("disk path")?;
    let sys = Path::new("/sys/class/block").join(name);
    ensure!(sys.exists(), "{} is not a block device", disk.display());
    ensure!(
        !sys.join("partition").exists(),
        "{} is a partition; name the whole disk",
        disk.display()
    );
    let mounted = util::output(
        Command::new("lsblk")
            .args(["--noheadings", "--raw", "--output", "MOUNTPOINTS"])
            .arg(disk),
    )?;
    ensure!(
        mounted.trim().is_empty(),
        "something on {} is mounted: {}",
        disk.display(),
        mounted.trim()
    );

    let dump = Command::new("sudo")
        .args(["sfdisk", "--json"])
        .arg(disk)
        .output()
        .context("reading the partition table (sudo sfdisk)")?;
    let table = parse_table(&String::from_utf8_lossy(&dump.stdout))?;
    check_target(table.as_ref(), expected_guid, reinstall)?;

    let disk_size: u64 = fs::read_to_string(sys.join("size"))?
        .trim()
        .parse::<u64>()?
        * 512;
    let image_size = fs::metadata(&image)?.len();
    ensure!(
        disk_size >= image_size + 16 * 1024 * 1024 * 1024,
        "the disk is too small ({} GiB)",
        disk_size >> 30
    );
    let model = fs::read_to_string(sys.join("device/model")).unwrap_or_default();

    say!(
        "\n  Disk:   {} – {} – {} GB\n  GUID:   {}\n  Image:  {}\n\n  {}",
        disk.display(),
        model.trim(),
        disk_size / 1_000_000_000,
        expected_guid,
        image.display(),
        if reinstall {
            "The Beize installed here and everything it stored will be erased."
        } else {
            "Everything on this disk will be erased."
        }
    );
    eprint!("  Type yes to install Beize onto it: ");
    io::stderr().flush()?;
    let mut answer = String::new();
    io::stdin().lock().read_line(&mut answer)?;
    if answer.trim() != "yes" {
        bail!("not installed");
    }

    util::step("Installing");
    // Tell the SSD that everything is free (fast; skipped if the path to the
    // disk cannot pass it on).
    if util::run_attached(
        Command::new("sudo")
            .args(["blkdiscard", "--force"])
            .arg(disk),
    )
    .is_err()
    {
        say!("  (discard not supported here, writing over the old content)");
    }
    util::run_attached(
        Command::new("sudo")
            .arg("dd")
            .arg(format!("if={}", image.display()))
            .arg(format!("of={}", disk.display()))
            .args(["bs=4M", "conv=fsync", "oflag=direct", "status=progress"]),
    )?;
    // The image ends with its backup GPT; on the larger disk it belongs at the end.
    util::run_attached(
        Command::new("sudo")
            .args(["sfdisk", "--relocate", "gpt-bak-std"])
            .arg(disk),
    )?;
    util::run_attached(&mut Command::new("sync"))?;
    say!(
        "\n  ok: Beize is on {}. Next: keys/secureboot/README.md, step 3.",
        disk.display()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMSUNG: &str = "FEBDED67-6AF6-43E1-81B7-73F9309DF929";

    fn table(id: &str, types: &[&str]) -> String {
        let parts: Vec<String> = types
            .iter()
            .map(|ty| {
                format!(r#"{{"node": "/dev/sdx1", "start": 34, "size": 32768, "type": "{ty}"}}"#)
            })
            .collect();
        format!(
            r#"{{"partitiontable": {{"label": "gpt", "id": "{id}", "device": "/dev/sdx",
                "unit": "sectors", "partitions": [{}]}}}}"#,
            parts.join(",")
        )
    }

    fn verdict(json: &str, guid: &str) -> Result<()> {
        check_target(parse_table(json).unwrap().as_ref(), guid, false)
    }

    #[test]
    fn accepts_the_named_empty_disk() {
        assert!(
            verdict(
                &table(SAMSUNG, &[MICROSOFT_RESERVED]),
                "{febded67-6af6-43e1-81b7-73f9309df929}"
            )
            .is_ok()
        );
        assert!(verdict(&table(SAMSUNG, &[]), SAMSUNG).is_ok());
    }

    #[test]
    fn refuses_another_disk() {
        // Same size, different GUID: the Windows disk on the owner's PC.
        let windows = table(
            "6597A45F-E9C4-474C-B280-EF2F6236F2DE",
            &[
                "C12A7328-F81F-11D2-BA4B-00A0C93EC93B",
                MICROSOFT_RESERVED,
                "EBD0A0A2-B9E5-4433-87C0-68B6B72699C7",
            ],
        );
        assert!(
            verdict(&windows, SAMSUNG)
                .unwrap_err()
                .to_string()
                .contains("different disk")
        );
    }

    #[test]
    fn refuses_a_disk_with_data() {
        let with_data = table(
            SAMSUNG,
            &[MICROSOFT_RESERVED, "EBD0A0A2-B9E5-4433-87C0-68B6B72699C7"],
        );
        assert!(
            verdict(&with_data, SAMSUNG)
                .unwrap_err()
                .to_string()
                .contains("holds partitions")
        );
    }

    #[test]
    fn reinstalls_only_over_beize() {
        let beize = table(image::DISK_GUID, &["C12A7328-F81F-11D2-BA4B-00A0C93EC93B"]);
        let reinstall =
            |json: &str, guid: &str| check_target(parse_table(json).unwrap().as_ref(), guid, true);
        assert!(reinstall(&beize, "{6265697a-6500-4000-8000-000000000001}").is_ok());
        assert!(
            verdict(&beize, image::DISK_GUID)
                .unwrap_err()
                .to_string()
                .contains("--reinstall")
        );
        let empty = table(SAMSUNG, &[MICROSOFT_RESERVED]);
        assert!(
            reinstall(&empty, SAMSUNG)
                .unwrap_err()
                .to_string()
                .contains("does not hold Beize")
        );
    }

    #[test]
    fn refuses_a_disk_without_gpt() {
        assert!(check_target(None, SAMSUNG, false).is_err());
        let mbr = r#"{"partitiontable": {"label": "dos", "id": "0x1234", "partitions": []}}"#;
        assert!(parse_table(mbr).is_err());
    }
}
