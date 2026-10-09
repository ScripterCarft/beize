//! Checks of a single boot and of the disk it leaves behind.

use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, bail, ensure};

use super::console::{
    Answers, REPORT_TIMEOUT, SMOKE_TIME_ZONE, TAMPER_TIMEOUT, strip_ansi, watch_boot,
};
use super::machine::{Machine, SLIRP_DNS, SLIRP_GATEWAY};
use super::report::{BootReport, REPORT_MARKER};
use crate::layout::{self, MIB};
use crate::{image, util};

/// A healthy boot: its report and what appeared on the console.
pub(super) struct HealthyBoot {
    pub(super) report: BootReport,
    pub(super) console: Vec<String>,
    /// Whether the root partition had to be unlocked with a passphrase.
    pub(super) prompted: bool,
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
    ensure!(
        report.system_unlock == "refused",
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
    ensure!(
        run.powered_off,
        "the system did not power off cleanly after reporting"
    );
    ensure!(
        report.boot == expected_boot,
        "expected boot #{expected_boot}, the system reports boot #{}",
        report.boot
    );
    eprintln!(
        "  ok: boot #{} running under Secure Boot after {} guest uptime \
         ({:.1}s including firmware), network via DHCP, no failed units, clean power-off \
         (NTS key exchange with {} time servers)",
        report.boot,
        report.uptime,
        elapsed.as_secs_f64(),
        report.nts
    );
    Ok(HealthyBoot {
        report,
        console: run.console,
        prompted: run.prompted,
    })
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
    eprintln!("  ok: refused to re-bind ({reason}); the recovery key is still needed");
    Ok(())
}

/// After a change the TPM refuses (PCR 7 or the TPM itself), boot `boot`
/// must ask for the recovery key and re-bind the TPM slot; boot `boot + 1`
/// must unlock by itself again (ADR 0010).
pub(super) fn expect_rebound(
    machine: &Machine,
    boot: u32,
    answers: &Answers,
    change: &str,
) -> Result<()> {
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
    eprintln!("  ok: after {change} the recovery key was needed once, then the TPM unlocked again");
    Ok(())
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
    eprintln!("  ok: with {condition} the TPM did not unlock the root partition");
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
    eprintln!("  ok: first boot created /usr slot B and the root partition");
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
    eprintln!(
        "  ok: the TPM unlocks the root partition only for Secure Boot as enrolled (PCR 7), \
         nothing added to the boot (PCR 12) and a signed Beize UKI in its initrd \
         (PCR 11, policy reference \"initrd\")"
    );
    Ok(())
}

/// systemd's TPM2 token from the LUKS2 header of the root partition: what the
/// TPM checks before it releases the key.
fn root_tpm2_token(disk: &Path, scratch: &Path) -> Result<serde_json::Value> {
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
    let copy = scratch.join("root-luks-header");
    fs::write(&copy, &header)?;
    let dump = util::output(
        Command::new("cryptsetup")
            .args(["luksDump", "--dump-json-metadata"])
            .arg(&copy),
    );
    fs::remove_file(&copy)?;
    let metadata: serde_json::Value = serde_json::from_str(&dump?)?;
    metadata["tokens"]
        .as_object()
        .and_then(|tokens| tokens.values().find(|t| t["type"] == "systemd-tpm2"))
        .cloned()
        .context("the root partition has no systemd-tpm2 token")
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
    let usr = layout::usr_label();
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

/// systemd-bless-boot must have dropped the boot counter from the UKI name.
pub(super) fn check_blessed(disk: &Path) -> Result<()> {
    let listing = util::output(
        Command::new("mdir")
            .arg("-b")
            .arg("-i")
            .arg(format!("{}@@{}", disk.display(), image::ESP_OFFSET))
            .arg("::/EFI/Linux"),
    )?;
    let blessed = format!("/{}", layout::blessed_uki_name());
    ensure!(
        listing.lines().any(|line| line.ends_with(&blessed)),
        "the UKI was not marked as good after a successful boot; ESP holds:\n{listing}"
    );
    eprintln!(
        "  ok: boot counting marked {} as good",
        layout::blessed_uki_name()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_missing_partitions() {
        let usr = layout::usr_label();
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
