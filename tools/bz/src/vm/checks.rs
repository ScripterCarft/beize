//! Checks of a single boot and of the disk it leaves behind.

use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail, ensure};

use super::console::{
    Answers, REPORT_TIMEOUT, SMOKE_KEYMAP, SMOKE_TIME_ZONE, TAMPER_TIMEOUT, strip_ansi, watch_boot,
};
use super::machine::{Machine, SLIRP_DNS, SLIRP_GATEWAY};
use super::report::{BootReport, REPORT_MARKER, Units};
use crate::layout::{self, MIB};
use crate::say;
use crate::{boottimes, image, util};

/// A healthy boot: its report and what appeared on the console.
pub(super) struct HealthyBoot {
    pub(super) report: BootReport,
    pub(super) console: Vec<String>,
    /// Whether the root partition had to be unlocked with a passphrase.
    pub(super) prompted: bool,
    /// How often the boot asked for it.
    pub(super) prompts: u32,
    /// The journal of the boot, for `bz boot-times`.
    pub(super) journal: PathBuf,
}

/// Boots once and requires a fully healthy system: Secure Boot on, state
/// `running`, no failed units, clean power-off, and the expected boot count.
/// `answers` are typed at the console's questions (see [`Answers`]).
pub(super) fn boot_healthy(
    machine: &Machine,
    expected_boot: u32,
    answers: &Answers,
) -> Result<HealthyBoot> {
    let run = watch_boot(machine, REPORT_TIMEOUT, answers, BootReport::parse)?;
    ensure!(
        run.answer.is_some() || !run.prompted,
        "the boot asked for a passphrase: the TPM did not unlock the root partition"
    );
    let Some((report, elapsed)) = run.answer else {
        bail!(
            "the system did not report within {}s",
            REPORT_TIMEOUT.as_secs()
        );
    };
    ensure!(
        report.usr == "verity",
        "/usr is not protected by dm-verity (usr={})",
        report.usr
    );
    ensure!(
        report.root == "encrypted",
        "the root partition is not encrypted (root={})",
        report.root
    );
    ensure!(
        report.secure_boot == "on",
        "the system booted without Secure Boot (secureboot={})",
        report.secure_boot
    );
    // "notbound" (this TPM holds no key for the volume) says nothing about
    // the initrd-only policy; it is expected only where the TPM did not
    // unlock in the initrd either, i.e. the boot needed the recovery key.
    ensure!(
        report.system_unlock == "refused" || (run.prompted && report.system_unlock == "notbound"),
        "the TPM must unlock the root partition only in the initrd, but for the \
         running system it reported systemunlock={}",
        report.system_unlock
    );
    ensure!(
        report.state == "running" && report.failed.is_empty(),
        "boot finished in state {:?}; failed units: {}",
        report.state,
        report.failed.join(", ")
    );
    ensure!(
        run.failures.is_empty(),
        "units failed during boot (initrd or system):\n  {}",
        run.failures.join("\n  ")
    );
    // The console misses what fails while it is muted (recovery key, setup).
    ensure!(
        report.boot_failed.is_empty(),
        "units failed during boot according to the journal: {}",
        report.boot_failed.join(", ")
    );
    ensure!(
        report.network == "routable",
        "the wired network did not come up via DHCP (network={})",
        report.network
    );
    ensure!(
        report.dns == [SLIRP_DNS],
        "the DNS servers are not the network's: expected {SLIRP_DNS}, got {:?}",
        report.dns
    );
    ensure!(
        report.lookup.iter().any(|address| address == SLIRP_GATEWAY),
        "name lookup through NSS failed: _gateway resolved to {:?}",
        report.lookup
    );
    ensure!(
        report.time_zone == SMOKE_TIME_ZONE,
        "the time zone chosen in the setup is not set: expected {SMOKE_TIME_ZONE}, got {}",
        report.time_zone
    );
    // The key between T and U gives z only with the German keymap loaded.
    ensure!(
        report.keymap == SMOKE_KEYMAP && report.yz_key == "z",
        "the keyboard chosen in the setup is not in use: expected keymap {SMOKE_KEYMAP} \
         (y/z key: z), got {} (y/z key: {})",
        report.keymap,
        report.yz_key
    );
    ensure!(
        report.efi_keymap == SMOKE_KEYMAP,
        "the console keymap is not kept for the initrd: the EFI variable holds {}",
        report.efi_keymap
    );
    ensure!(
        run.powered_off,
        "the system did not power off cleanly after reporting"
    );
    ensure!(
        report.boot == expected_boot,
        "expected boot #{expected_boot}, the system reports boot #{}",
        report.boot
    );
    // A missing measurement must not pass a time budget as "0 s".
    ensure!(
        report.kernel > 0 && report.initrd > 0 && report.userspace > 0,
        "systemd did not measure the boot phases (kernel={} initrd={} userspace={} µs)",
        report.kernel,
        report.initrd,
        report.userspace
    );
    ensure!(
        report.chain > 0,
        "beize-boot-chain wrote no critical chain into the journal"
    );
    // Where the CPU has shadow stacks, systemd must run with one (ADR 0014).
    ensure!(
        matches!(report.shstk.as_str(), "on" | "nohw"),
        "the CPU has shadow stacks, but PID 1 runs without one (shstk={})",
        report.shstk
    );
    // ADR 0015: lockdown, enforced module signatures, a driver for every device.
    ensure!(
        report.lockdown == "integrity",
        "the kernel runs without lockdown \"integrity\" (lockdown={})",
        report.lockdown
    );
    ensure!(
        report.unsigned == "refused",
        "the kernel did not refuse an unsigned module (unsigned={})",
        report.unsigned
    );
    ensure!(
        report.signed == "loaded",
        "the signed module from the image did not load (signed={})",
        report.signed
    );
    ensure!(
        report.no_driver.is_empty(),
        "PCI devices without a driver (vendor:device): {}",
        report.no_driver.join(", ")
    );
    say!(
        "  ok: boot #{} running under Secure Boot after {} guest uptime \
         ({:.1}s including firmware; kernel {} + initrd {} + system {}), network via DHCP, \
         no failed units, clean power-off (NTS key exchange with {} time servers; \
         shadow stack {}; lockdown, only signed modules, every device driven)",
        report.boot,
        report.uptime,
        elapsed.as_secs_f64(),
        seconds(report.kernel),
        seconds(report.initrd),
        seconds(report.userspace),
        report.nts,
        if report.shstk == "on" {
            "on"
        } else {
            "not in this CPU"
        }
    );
    let journal = run
        .journal
        .context("the boot report wrote no journal to its disk")?;
    Ok(HealthyBoot {
        report,
        console: run.console,
        prompted: run.prompted,
        prompts: run.prompts,
        journal,
    })
}

/// The most each phase of an ordinary boot (not the first, nothing typed)
/// may take in the VM, in µs: about 1.5 times what it takes on the slower
/// machine, the 2-CPU CI runner (run 38574057805, 2026-10-14: the 17 second
/// and later boots of a full smoke run – kernel 0.46–0.58 s, initrd
/// 2.85–3.35 s, system 2.29–2.64 s; the owner's PC: kernel 0.36 s, initrd
/// 1.72 s, system 1.72 s). The software TPM dominates the initrd phase.
/// A boot that gets slower than this has a cause to find before it reaches
/// real hardware; a runner's bad day must not fail it.
const BOOT_BUDGET: [(&str, u64); 3] = [
    ("kernel", 900_000),
    ("initrd", 5_000_000),
    ("system", 4_000_000),
];

/// Holds an ordinary boot to [`BOOT_BUDGET`].
pub(super) fn check_boot_budget(report: &BootReport) -> Result<()> {
    let measured = [report.kernel, report.initrd, report.userspace];
    let over: Vec<String> = BOOT_BUDGET
        .iter()
        .zip(measured)
        .filter(|((_, budget), took)| took > budget)
        .map(|((phase, budget), took)| {
            format!("{phase} {} (budget {})", seconds(took), seconds(*budget))
        })
        .collect();
    ensure!(
        over.is_empty(),
        "the boot took longer than its time budget: {}",
        over.join(", ")
    );
    let budget = BOOT_BUDGET
        .iter()
        .map(|(phase, budget)| format!("{phase} {}", seconds(*budget)))
        .collect::<Vec<_>>()
        .join(", ");
    say!("  ok: within the time budget ({budget})");
    Ok(())
}

/// Shows where the initrd and the system spent their time (the slowest
/// units): next to the phases, so a CI run's log explains its numbers.
pub(super) fn show_slowest_units(boot: &HealthyBoot) -> Result<()> {
    let [initrd, system] = boottimes::slowest_units(&boot.journal, 5)?;
    say!(
        "  slowest units – initrd: {}; system: {} (all: bz boot-times {})",
        initrd.join(", "),
        system.join(", "),
        boot.journal.display()
    );
    Ok(())
}

/// What runs in an ordinary boot of the image in the VM (ADR 0013).
const EXPECTED_UNITS: &str = include_str!("../../../../images/running-units.txt");

/// Requires exactly the units of [`EXPECTED_UNITS`] to be active, and no unit
/// links in /etc: what runs is decided in the image, never by accident (a
/// new systemd release, a package that enables itself).
pub(super) fn check_units(console: &[String]) -> Result<()> {
    let units = Units::find(console).context("the boot did not report its units")?;
    ensure!(
        units.etc.is_empty(),
        "units are enabled in /etc, not in the image: {}",
        units.etc.join(", ")
    );
    let expected = expected_units(EXPECTED_UNITS);
    let unexpected: Vec<&str> = units
        .active
        .iter()
        .map(String::as_str)
        .filter(|unit| !expected.contains(unit))
        .collect();
    let missing: Vec<&str> = expected
        .iter()
        .copied()
        .filter(|unit| !units.active.iter().any(|active| active == unit))
        .collect();
    ensure!(
        unexpected.is_empty() && missing.is_empty(),
        "the units that run are not the expected ones (images/running-units.txt; \
         enable or disable in images/base.preset, with the reason)\n  \
         unexpected: {}\n  missing: {}",
        if unexpected.is_empty() {
            "none".to_owned()
        } else {
            unexpected.join(", ")
        },
        if missing.is_empty() {
            "none".to_owned()
        } else {
            missing.join(", ")
        }
    );
    say!(
        "  ok: exactly the {} expected units run, none enabled in /etc",
        expected.len()
    );
    Ok(())
}

/// The unit names of `images/running-units.txt`, without comments.
fn expected_units(list: &str) -> Vec<&str> {
    list.lines()
        .map(|line| line.split('#').next().unwrap_or_default())
        .flat_map(str::split_whitespace)
        .collect()
}

/// µs as seconds, e.g. `0.61s`.
fn seconds(us: u64) -> String {
    format!("{:.2}s", us as f64 / 1e6)
}

/// After a change that must not be approved, boot `boot` opens with the
/// recovery key but keeps the TPM binding (beize-tpm says `reason`); boot
/// `boot + 1` must still need the recovery key.
pub(super) fn expect_refused(
    machine: &Machine,
    boot: u32,
    answers: &Answers,
    reason: &str,
) -> Result<()> {
    let first = boot_healthy(machine, boot, answers)?;
    ensure!(first.prompted, "the TPM unlocked although it should refuse");
    ensure!(
        first.console.iter().any(|line| line.contains(reason)),
        "the recovery-key boot did not refuse with \"{reason}\""
    );
    ensure!(
        !first
            .console
            .iter()
            .any(|line| line.contains("binding the data to the current state")),
        "the TPM was re-bound although \"{reason}\""
    );
    let second = boot_healthy(machine, boot + 1, answers)?;
    ensure!(
        second.prompted,
        "the TPM unlocked after a refused re-binding"
    );
    say!("  ok: refused to re-bind ({reason}); the recovery key is still needed");
    Ok(())
}

/// After a change the TPM refuses (PCR 7 or the TPM itself), boot `boot`
/// must ask for the recovery key and re-bind the TPM slot; boot `boot + 1`
/// must unlock by itself again (ADR 0010). Returns boot `boot`.
pub(super) fn expect_rebound(
    machine: &Machine,
    boot: u32,
    answers: &Answers,
    change: &str,
) -> Result<HealthyBoot> {
    let first = boot_healthy(machine, boot, answers)?;
    ensure!(
        first.prompted,
        "after {change} the TPM still unlocked the root partition"
    );
    ensure!(
        first
            .console
            .iter()
            .any(|line| line.contains("binding the data to the current state")),
        "after {change} the recovery-key boot did not re-bind the TPM"
    );
    let second = boot_healthy(machine, boot + 1, &Answers::none())?;
    ensure!(
        !second.prompted,
        "after re-binding the TPM still did not unlock"
    );
    say!("  ok: after {change} the recovery key was needed once, then the TPM unlocked again");
    Ok(first)
}

/// Boots an installed disk under a changed condition (another TPM, other
/// firmware state) and requires that the TPM refuses to unlock the root
/// partition: the boot asks for the recovery key (or, without one, fails to
/// set up the volume) and nothing boots.
pub(super) fn expect_root_locked(machine: &Machine, condition: &str) -> Result<()> {
    let run = watch_boot(machine, TAMPER_TIMEOUT, &Answers::none(), |line| {
        let line = strip_ansi(line);
        if line.contains(REPORT_MARKER) {
            Some(false)
        } else if line.contains("Failed to start Cryptography Setup for root") {
            Some(true)
        } else {
            None
        }
    })?;
    match run.answer {
        Some((true, _)) => {}
        None if run.prompted => {}
        Some((false, _)) => bail!("with {condition} the root partition was unlocked"),
        None => bail!(
            "no verdict on unlocking the root partition with {condition} within {}s",
            TAMPER_TIMEOUT.as_secs()
        ),
    }
    say!("  ok: with {condition} the TPM did not unlock the root partition");
    Ok(())
}

/// After first boot the disk must hold both `/usr` slots and the root partition.
pub(super) fn check_partitions(disk: &Path) -> Result<()> {
    let dump = util::output(Command::new("sfdisk").arg("--dump").arg(disk))?;
    let missing = missing_partitions(&dump);
    ensure!(
        missing.is_empty(),
        "first boot did not create the expected partitions; missing: {}\n{dump}",
        missing.join(", ")
    );
    say!("  ok: first boot created /usr slot B and the root partition");
    Ok(())
}

/// The root partition's TPM2 slot must be bound to the Secure Boot state
/// (PCR 7), to nothing added to the boot (PCR 12) and to the signed PCR 11
/// policy for the initrd phase only (ADR 0010).
pub(super) fn check_root_binding(disk: &Path, scratch: &Path) -> Result<()> {
    let token = root_tpm2_token(disk, scratch)?;
    let pcrs = |key: &str| token[key].as_array().cloned().unwrap_or_default();
    ensure!(
        pcrs("tpm2-pcrs") == [7, 12] && pcrs("tpm2_pubkey_pcrs") == [11],
        "the root partition is bound to PCRs {} and signed policies over PCRs {}, \
         expected 7+12 and 11",
        token["tpm2-pcrs"],
        token["tpm2_pubkey_pcrs"]
    );
    ensure!(
        token["tpm2_pubkey_ref"] == "initrd",
        "the signed policy of the root partition is not limited to the initrd \
         (policy reference {})",
        token["tpm2_pubkey_ref"]
    );
    say!(
        "  ok: the TPM unlocks the root partition only for Secure Boot as enrolled (PCR 7), \
         nothing added to the boot (PCR 12) and a signed Beize UKI in its initrd \
         (PCR 11, policy reference \"initrd\")"
    );
    Ok(())
}

/// systemd's TPM2 token from the LUKS2 header of the root partition: what the
/// TPM checks before it releases the key.
fn root_tpm2_token(disk: &Path, scratch: &Path) -> Result<serde_json::Value> {
    let (_, metadata) = root_luks(disk, scratch)?;
    metadata["tokens"]
        .as_object()
        .and_then(|tokens| tokens.values().find(|t| t["type"] == "systemd-tpm2"))
        .cloned()
        .context("the root partition has no systemd-tpm2 token")
}

/// Damages the file system inside the encrypted root partition without the
/// key, as anyone with the disk could: one flipped byte of the ciphertext
/// garbles 16 bytes of the first ext4 group descriptor. The LUKS2 header
/// stays intact, so the TPM still unlocks, and blkid still sees ext4 (the
/// superblock is untouched) – mounting the root then fails its checksum.
pub(super) fn damage_root_file_system(disk: &Path, scratch: &Path) -> Result<()> {
    /// The group descriptors follow the superblock's 4 KiB block.
    const EXT4_GROUP_DESCRIPTORS: u64 = 4096;
    let (start, metadata) = root_luks(disk, scratch)?;
    let data: u64 = metadata["segments"]["0"]["offset"]
        .as_str()
        .context("no data segment in the root partition's LUKS2 header")?
        .parse()?;
    let offset = start + data + EXT4_GROUP_DESCRIPTORS;
    let mut file = fs::OpenOptions::new().read(true).write(true).open(disk)?;
    let mut byte = [0u8; 1];
    file.seek(SeekFrom::Start(offset))?;
    file.read_exact(&mut byte)?;
    file.seek(SeekFrom::Start(offset))?;
    std::io::Write::write_all(&mut file, &[byte[0] ^ 0xff])?;
    Ok(())
}

/// The byte offset of the root partition on `disk` and its LUKS2 metadata.
fn root_luks(disk: &Path, scratch: &Path) -> Result<(u64, serde_json::Value)> {
    let table: serde_json::Value = serde_json::from_str(&util::output(
        Command::new("sfdisk").arg("--json").arg(disk),
    )?)?;
    let root = table["partitiontable"]["partitions"]
        .as_array()
        .and_then(|parts| {
            parts.iter().find(|part| {
                part["type"]
                    .as_str()
                    .is_some_and(|ty| ty.eq_ignore_ascii_case(layout::ROOT_TYPE))
            })
        })
        .context("no root partition")?;
    let start = root["start"].as_u64().context("root partition start")? * 512;
    // The LUKS2 header (with both metadata copies) fits into the first 16 MiB.
    let mut header = vec![0u8; 16 * MIB as usize];
    let mut file = fs::File::open(disk)?;
    file.seek(SeekFrom::Start(start))?;
    file.read_exact(&mut header)?;
    // One copy per disk: scenarios run side by side.
    let copy = scratch.join(format!(
        "{}.luks-header",
        disk.file_name().context("a disk without a name")?.display()
    ));
    fs::write(&copy, &header)?;
    let dump = util::output(
        Command::new("cryptsetup")
            .args(["luksDump", "--dump-json-metadata"])
            .arg(&copy),
    );
    fs::remove_file(&copy)?;
    Ok((start, serde_json::from_str(&dump?)?))
}

/// The partitions expected after first boot, as (type, label), that the
/// `sfdisk --dump` output lacks.
fn missing_partitions(sfdisk_dump: &str) -> Vec<String> {
    let present: Vec<(String, String)> = sfdisk_dump
        .lines()
        .filter_map(|line| {
            let field = |key: &str| {
                let start = line.find(key)? + key.len();
                let rest = &line[start..];
                let end = rest.find([',', '"']).unwrap_or(rest.len());
                Some(rest[..end].trim().to_uppercase())
            };
            let name = line.split("name=\"").nth(1)?.split('"').next()?.to_owned();
            Some((field("type=")?, name))
        })
        .collect();
    let usr = layout::usr_label(layout::OS_VERSION);
    [
        ("esp", layout::ESP_TYPE, "esp"),
        ("/usr A", layout::USR_TYPE, usr.as_str()),
        ("/usr A verity", layout::USR_VERITY_TYPE, usr.as_str()),
        ("/usr B", layout::USR_TYPE, "_empty"),
        ("/usr B verity", layout::USR_VERITY_TYPE, "_empty"),
        ("root", layout::ROOT_TYPE, "root"),
    ]
    .into_iter()
    .filter(|(_, ty, name)| !present.contains(&(ty.to_string(), name.to_string())))
    .map(|(what, _, _)| what.to_owned())
    .collect()
}

/// systemd-bless-boot must have dropped the boot counter from the name of
/// the UKI of `version`.
pub(super) fn check_blessed(disk: &Path, version: &str) -> Result<()> {
    let listing = uki_listing(disk)?;
    let blessed = layout::blessed_uki_name(version);
    ensure!(
        listing.contains(&blessed),
        "the UKI was not marked as good after a successful boot; ESP holds: {listing:?}"
    );
    say!("  ok: boot counting marked {blessed} as good");
    Ok(())
}

/// The file names in the ESP's /EFI/Linux of `disk`.
pub(super) fn uki_listing(disk: &Path) -> Result<Vec<String>> {
    let listing = util::output(
        Command::new("mdir")
            .arg("-b")
            .arg("-i")
            .arg(format!("{}@@{}", disk.display(), image::ESP_OFFSET))
            .arg("::/EFI/Linux"),
    )?;
    Ok(listing
        .lines()
        .filter_map(|line| line.rsplit('/').next())
        .filter(|name| !name.is_empty())
        .map(str::to_owned)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn boot_budget_names_every_phase_over_it() {
        let line = |kernel: u64, initrd: u64, userspace: u64| {
            format!(
                "BEIZE-BOOT-REPORT state=running secureboot=on usr=verity root=encrypted boot=2 \
                 machineid=x uptime=5.3s systemunlock=refused network=routable dns=none \
                 lookup=none tz=none keymap=none yzkey=y efikeymap=none nts=0 clock=0 kernel={kernel} initrd={initrd} \
                 userspace={userspace} chain=12 shstk=on lockdown=integrity \
                 unsigned=refused signed=loaded nodriver=none failed=none bootfailed=none"
            )
        };
        let [(_, kernel), (_, initrd), (_, system)] = BOOT_BUDGET;
        let within = BootReport::parse(&line(kernel, initrd, system)).unwrap();
        assert!(check_boot_budget(&within).is_ok());
        let over = BootReport::parse(&line(kernel + 1, initrd, system + 1)).unwrap();
        let error = check_boot_budget(&over).unwrap_err().to_string();
        assert!(
            error.contains("kernel") && error.contains("system"),
            "{error}"
        );
        assert!(!error.contains("initrd"), "{error}");
    }

    #[test]
    fn detects_missing_partitions() {
        let usr = layout::usr_label(layout::OS_VERSION);
        let line = |n: u32, ty: &str, name: &str| {
            format!("disk.img{n} : start=1, size=1, type={ty}, uuid=X, name=\"{name}\"\n")
        };
        let after_first_boot = [
            "label: gpt\n".to_owned(),
            line(1, layout::ESP_TYPE, "esp"),
            line(2, layout::USR_TYPE, &usr),
            line(3, layout::USR_VERITY_TYPE, &usr),
            line(4, layout::USR_TYPE, "_empty"),
            line(5, layout::USR_VERITY_TYPE, "_empty"),
            line(6, &layout::ROOT_TYPE.to_lowercase(), "root"),
        ]
        .concat();
        assert!(missing_partitions(&after_first_boot).is_empty());

        // The shipped image, before first boot.
        let shipped: String = after_first_boot
            .lines()
            .take(4)
            .map(|l| format!("{l}\n"))
            .collect();
        assert_eq!(
            missing_partitions(&shipped),
            ["/usr B", "/usr B verity", "root"]
        );

        // A data partition does not stand in for a missing verity partition.
        let no_b_verity = after_first_boot.replace(
            &line(5, layout::USR_VERITY_TYPE, "_empty"),
            &line(5, layout::USR_TYPE, "_empty"),
        );
        assert_eq!(missing_partitions(&no_b_verity), ["/usr B verity"]);
    }
}
