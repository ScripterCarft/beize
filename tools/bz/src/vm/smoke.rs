//! `bz smoke`: the end-to-end test of the image.

use std::fs;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail, ensure};

use super::checks::{
    boot_healthy, check_blessed, check_partitions, check_root_binding, expect_rebound,
    expect_refused, expect_root_locked,
};
use super::console::{
    Answers, LAST_CONSOLE, RECOVERY_KEY_SAVED_PROMPT, REPORT_TIMEOUT, SETUP_ANSWERS, SMOKE_USER,
    TAMPER_TIMEOUT, find_recovery_key, watch_boot,
};
use super::machine::{Machine, boot_logs, edit_firmware, new_tpm};
use super::report::{BootReport, REPORT_MARKER};
use crate::paths::Paths;
use crate::{image, secureboot, util};

/// Verifies the image end to end on a fresh test disk:
/// 1. first boot under enforced Secure Boot creates `/usr` slot B and the
///    root partition, reaches `running` without failed units and marks the
///    UKI as good (boot counting);
/// 2. the second boot finds the same installation (state persisted);
/// 3. the image with an unsigned kernel image is refused by the firmware;
/// 4. the owner's workflow (`new-keys`, `sign`) yields an image that boots.
///
/// The firmware is set up like a PC as shipped, with Beize's db certificate
/// appended (ADR 0010).
///
/// Each boot's console goes to `build/work/logs/boot/`; a failure shows the
/// end of the last one. The summary lists every step with its duration.
pub fn smoke(paths: &Paths) -> Result<()> {
    let logs = boot_logs(paths);
    if logs.exists() {
        fs::remove_dir_all(&logs)?;
    }
    let mut steps = Steps::new();
    let result = smoke_steps(paths, &mut steps);
    if result.is_err()
        && let Some(log) = LAST_CONSOLE.with_borrow(Clone::clone)
    {
        util::print_tail(&log);
    }
    steps.summary(result.is_ok());
    result
}

/// The steps of a test run with their durations, for the summary at the end.
struct Steps {
    started: Instant,
    current: Option<(String, Instant)>,
    done: Vec<(String, Duration)>,
}

impl Steps {
    fn new() -> Self {
        Self {
            started: Instant::now(),
            current: None,
            done: Vec::new(),
        }
    }

    fn begin(&mut self, title: &str) {
        self.finish();
        util::step(title);
        self.current = Some((title.to_owned(), Instant::now()));
    }

    fn finish(&mut self) {
        if let Some((title, started)) = self.current.take() {
            self.done.push((title, started.elapsed()));
        }
    }

    /// Lists the steps that passed and, unless `passed`, the one that failed.
    fn summary(mut self, passed: bool) {
        let failed = if passed {
            self.finish();
            None
        } else {
            self.current.take()
        };
        util::step("Summary");
        for (title, took) in &self.done {
            eprintln!("  ok     {:>4.0}s  {title}", took.as_secs_f64());
        }
        if let Some((title, started)) = failed {
            eprintln!(
                "  FAILED {:>4.0}s  {title}",
                started.elapsed().as_secs_f64()
            );
        }
        eprintln!(
            "  {} after {:.0}s",
            if passed { "passed" } else { "FAILED" },
            self.started.elapsed().as_secs_f64()
        );
    }
}

fn smoke_steps(paths: &Paths, steps: &mut Steps) -> Result<()> {
    steps.begin("Installing the image on a fresh test disk");
    let machine = Machine::install(paths, "smoke")?;

    steps.begin("First boot");
    let first = boot_healthy(&machine, 1, &Answers::none())?;
    check_partitions(&machine.disk)?;
    check_blessed(&machine.disk)?;
    check_root_binding(&machine.disk, &paths.work)?;
    let recovery_key = find_recovery_key(&first.console)
        .context("first boot did not show a recovery key for the root partition")?;
    // On real hardware the login prompt clears the screen: the boot must wait
    // until the owner confirms the key is saved.
    ensure!(
        first
            .console
            .iter()
            .any(|line| line.contains(RECOVERY_KEY_SAVED_PROMPT)),
        "first boot did not wait for the recovery key to be confirmed as saved"
    );
    eprintln!("  ok: first boot showed a recovery key and waited until it was saved");
    let created = format!("Account \"{SMOKE_USER}\" created");
    ensure!(
        first.console.iter().any(|line| line.contains(&created)),
        "first boot did not create the owner's account"
    );
    eprintln!("  ok: first boot created the owner's account \"{SMOKE_USER}\"");
    // The setup mutes the console: no boot messages in the middle of its
    // questions (found on the owner's PC, where they garbled the prompts).
    let setup: Vec<&String> = first
        .console
        .iter()
        .skip_while(|line| !line.contains("Welcome to Beize. Create your account"))
        .take_while(|line| !line.contains(&created))
        .collect();
    let noise: Vec<&&String> = setup
        .iter()
        .filter(|line| {
            line.contains("[  OK  ]") || line.contains("Starting ") || line.contains("Finished ")
        })
        .collect();
    ensure!(
        !setup.is_empty() && noise.is_empty(),
        "boot messages appeared in the middle of the account setup:\n  {}",
        noise
            .iter()
            .map(|l| l.as_str())
            .collect::<Vec<_>>()
            .join("\n  ")
    );
    eprintln!("  ok: the account setup had the console to itself");

    steps.begin("Second boot");
    let second = boot_healthy(&machine, 2, &Answers::none())?;
    ensure!(
        second.report.machine_id == first.report.machine_id,
        "machine ID changed between boots ({} -> {}): the root partition did not persist",
        first.report.machine_id,
        second.report.machine_id
    );
    eprintln!(
        "  ok: same installation after reboot (machine ID {})",
        second.report.machine_id
    );
    ensure!(
        find_recovery_key(&second.console).is_none(),
        "the recovery key was shown again on the second boot"
    );
    eprintln!("  ok: the recovery key was shown only once");
    ensure!(
        !second
            .console
            .iter()
            .any(|line| line.contains(SETUP_ANSWERS[0].0)),
        "the second boot asked for the owner's account again"
    );
    eprintln!("  ok: the owner's account was set up only once");

    steps.begin("A boot with something added must stop before asking for the recovery key");
    // Like a firmware whose SMBIOS OEM strings were rewritten from another OS:
    // systemd-stub appends these kernel parameters even under Secure Boot.
    let added = Machine {
        smbios: vec!["io.systemd.stub.kernel-cmdline-extra=beize.added=1".to_owned()],
        ..machine.clone()
    };
    let run = watch_boot(
        &added,
        TAMPER_TIMEOUT,
        &Answers::recovery_key(&recovery_key),
        |line| line.contains("This start was changed").then_some(()),
    )?;
    ensure!(
        run.answer.is_some(),
        "a boot with added kernel parameters was not stopped"
    );
    ensure!(
        !run.prompted,
        "a boot with added kernel parameters asked for the recovery key"
    );
    eprintln!(
        "  ok: the changed boot stopped before unlocking, without asking for the recovery key"
    );

    steps.begin("A Secure Boot update needs the recovery key once, then the TPM unlocks again");
    // Like Windows installing a revocation: a new dbx entry changes PCR 7.
    edit_firmware(
        &machine,
        &["--add-dbx-hash", secureboot::OWNER_GUID, SMOKE_REVOKED_HASH],
    )?;
    let with_key = Answers::recovery_key(&recovery_key);
    expect_rebound(&machine, 3, &with_key, "the Secure Boot update")?;

    steps.begin("A foreign certificate in db must not be approved, even with the recovery key");
    // Like someone appending their own certificate in the firmware setup.
    let approved = paths.work.join("smoke-vars-approved.fd");
    fs::copy(&machine.vars, &approved)?;
    let foreign = foreign_certificate(paths)?;
    edit_firmware(
        &machine,
        &[
            "--add-db",
            secureboot::OWNER_GUID,
            foreign.to_str().context("certificate path")?,
        ],
    )?;
    expect_refused(
        &machine,
        5,
        &with_key,
        "the signature database (db) holds a certificate outside the profile",
    )?;
    // Back to the approved state: the TPM unlocks by itself again.
    fs::rename(&approved, &machine.vars)?;
    boot_healthy(&machine, 7, &Answers::none())?;
    eprintln!("  ok: back in the approved state the TPM unlocked by itself");

    steps.begin("Another TPM (new mainboard) is adopted only when the owner confirms it");
    let other_tpm = Machine {
        tpm: paths.work.join("smoke-other-tpm"),
        ..machine.clone()
    };
    new_tpm(&other_tpm.tpm)?;
    expect_refused(
        &other_tpm,
        8,
        &with_key.clone().adopt_tpm("no"),
        "The new security chip was not adopted",
    )?;
    expect_rebound(
        &other_tpm,
        10,
        &with_key.clone().adopt_tpm("yes"),
        "a confirmed new TPM",
    )?;
    fs::remove_dir_all(&other_tpm.tpm)?;
    // From here on the volume is bound to the other TPM: the machine's own
    // TPM must not unlock it any more (checked below, with Secure Boot off).

    steps.begin("The root partition must stay locked with Secure Boot disabled");
    let insecure = Machine {
        vars: paths.work.join("smoke-insecure-vars.fd"),
        ..machine.clone()
    };
    // Keys stay enrolled, Secure Boot is switched off – as in the firmware setup.
    fs::copy(&machine.vars, &insecure.vars)?;
    edit_firmware(&insecure, &["--set-false", "SecureBootEnable"])?;
    expect_root_locked(&insecure, "Secure Boot disabled")?;
    // The recovery key opens the data, but must not approve this state.
    let run = watch_boot(&insecure, REPORT_TIMEOUT, &with_key, BootReport::parse)?;
    ensure!(
        run.prompted && run.answer.is_some(),
        "with Secure Boot disabled the recovery key did not open the root partition"
    );
    ensure!(
        run.console
            .iter()
            .any(|line| line.contains("Secure Boot is off: the TPM stays bound")),
        "with Secure Boot disabled the boot did not refuse to re-bind the TPM"
    );
    expect_root_locked(&insecure, "Secure Boot disabled after a recovery-key boot")?;
    eprintln!(
        "  ok: with Secure Boot disabled the recovery key opened the data without re-binding the TPM"
    );
    fs::remove_file(&insecure.vars)?;
    machine.remove()?;

    steps.begin("Secure Boot must refuse an unsigned kernel image");
    let machine = Machine::install(paths, "tampered")?;
    util::run(
        Command::new("mcopy")
            .arg("-o")
            .arg("-i")
            .arg(format!("{}@@{}", machine.disk.display(), image::ESP_OFFSET))
            .arg(image::unsigned_uki(paths))
            .arg(image::uki_path_on_esp()),
    )?;
    let run = watch_boot(&machine, TAMPER_TIMEOUT, &Answers::none(), |line| {
        if line.contains(REPORT_MARKER) || line.contains("Welcome to") {
            Some(false)
        } else if line.contains("Access Denied") || line.contains("Access denied") {
            Some(true)
        } else {
            None
        }
    });
    machine.remove()?;
    match run?.answer {
        Some((true, _)) => eprintln!("  ok: the unsigned kernel image was refused"),
        Some((false, _)) => bail!("an unsigned kernel image booted under Secure Boot"),
        None => bail!(
            "no verdict on the unsigned kernel image within {}s",
            TAMPER_TIMEOUT.as_secs()
        ),
    }

    steps.begin("dm-verity must detect a modified /usr");
    let machine = Machine::install(paths, "tampered")?;
    // Flip one byte of the EROFS superblock (at 1 KiB) in /usr slot A.
    {
        let mut disk = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&machine.disk)?;
        let offset = image::USR_OFFSET + 1024;
        let mut byte = [0u8; 1];
        disk.seek(SeekFrom::Start(offset))?;
        disk.read_exact(&mut byte)?;
        disk.seek(SeekFrom::Start(offset))?;
        disk.write_all(&[byte[0] ^ 0xff])?;
    }
    let run = watch_boot(&machine, TAMPER_TIMEOUT, &Answers::none(), |line| {
        if line.contains(REPORT_MARKER) || line.contains("Welcome to") {
            Some(false)
        } else if line.contains("verity") && line.contains("is corrupted") {
            Some(true)
        } else {
            None
        }
    });
    machine.remove()?;
    match run?.answer {
        Some((true, _)) => eprintln!("  ok: the modified /usr was detected and not used"),
        Some((false, _)) => bail!("the system booted from a modified /usr"),
        None => bail!(
            "no verdict on the modified /usr within {}s",
            TAMPER_TIMEOUT.as_secs()
        ),
    }

    steps.begin("The owner's signing workflow must yield an image that boots");
    owner_workflow(paths, steps)?;
    Ok(())
}

/// Runs the owner's workflow end to end – `bz secureboot new-keys`, then
/// `bz sign` – with a throwaway directory instead of 1Password, and boots the
/// result: enrollment from setup mode, then a healthy first boot.
fn owner_workflow(paths: &Paths, steps: &mut Steps) -> Result<()> {
    let dir = paths.work.join("owner-workflow");
    if dir.exists() {
        fs::remove_dir_all(&dir)?;
    }
    let store = secureboot::Store::Directory(dir.join("store"));
    let public = dir.join("public");
    let microsoft = secureboot::microsoft_dir(paths);
    let profile = secureboot::Profile::WithWindows;
    secureboot::new_keys(&store, &public, &microsoft, profile)?;
    ensure!(
        secureboot::new_keys(&store, &public, &microsoft, profile).is_err(),
        "new-keys replaced existing production keys"
    );
    let keys = secureboot::Keys::production(&store, &public)?;
    let out = dir.join("out");
    fs::create_dir_all(&out)?;
    let disk = image::assemble(paths, &keys, &out, image::Target::Vm)?;
    drop(keys);

    let machine = Machine::install_from(paths, "owner", &disk)?;
    boot_healthy(&machine, 1, &Answers::none())?;
    eprintln!("  ok: keys from the key store signed an image that boots under Secure Boot");
    empty_battery(&machine, steps)?;
    machine.remove()?;
    fs::remove_dir_all(&dir)?;
    Ok(())
}

/// A hardware clock that lost its time, as with an empty BIOS battery.
const EMPTY_BATTERY_RTC: &str = "2000-01-01T00:00:00";

/// Boots with the hardware clock reset to 2000: systemd must move the clock
/// to at least Beize's build time (or the last verified time saved by
/// beize-save-clock), so certificates and NTS can work (ADR 0012).
fn empty_battery(machine: &Machine, steps: &mut Steps) -> Result<()> {
    steps.begin("A hardware clock reset to 2000 (empty BIOS battery) must not set the time back");
    let reset = Machine {
        rtc: Some(EMPTY_BATTERY_RTC),
        ..machine.clone()
    };
    let boot = boot_healthy(&reset, 2, &Answers::none())?;
    let host_now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_secs();
    ensure!(
        boot.report.clock >= crate::packages::BUILD_EPOCH,
        "the clock started from the reset hardware clock: {} is before Beize's build time {}",
        boot.report.clock,
        crate::packages::BUILD_EPOCH
    );
    // Otherwise the test did not reset anything (e.g. the guest read the
    // host's time instead of its hardware clock).
    ensure!(
        boot.report.clock + 3600 < host_now,
        "the guest's clock is the host's time ({}), the hardware clock reset had no effect",
        boot.report.clock
    );
    eprintln!(
        "  ok: the system clock started at or after Beize's build time ({} time servers verified)",
        boot.report.nts
    );
    Ok(())
}

/// A dbx entry no binary has: added to simulate a Secure Boot update.
const SMOKE_REVOKED_HASH: &str = "62656967652d736d6f6b652d7265766f6b656462656967652d736d6f6b652d31";

/// A self-signed certificate that is in no profile.
fn foreign_certificate(paths: &Paths) -> Result<PathBuf> {
    let dir = paths.work.join("smoke-foreign");
    fs::create_dir_all(&dir)?;
    let cert = dir.join("foreign.crt");
    util::output(
        Command::new("openssl")
            .args(["req", "-new", "-x509", "-newkey", "rsa:2048", "-nodes"])
            .args(["-days", "1", "-subj", "/CN=Not Beize/", "-keyout"])
            .arg(dir.join("foreign.key"))
            .arg("-out")
            .arg(&cert),
    )?;
    Ok(cert)
}
