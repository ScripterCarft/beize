//! Scenarios of Secure Boot and the TPM: what the root partition's key is
//! sealed to, and who may change it (ADR 0009, ADR 0010).

use std::fs;
use std::path::PathBuf;
use std::process::Command;

use anyhow::{Context, Result, bail, ensure};

use super::Installed;
use crate::paths::Paths;
use crate::say;
use crate::tree;
use crate::vm::checks::{boot_healthy, expect_rebound, expect_refused, expect_root_locked};
use crate::vm::console::{Answers, REPORT_TIMEOUT, TAMPER_TIMEOUT, watch_boot};
use crate::vm::machine::{Machine, edit_firmware, new_tpm};
use crate::vm::report::{BootReport, REPORT_MARKER};
use crate::{image, layout, secureboot, util};

/// Like a firmware whose SMBIOS OEM strings were rewritten from another OS:
/// systemd-stub appends these kernel parameters even under Secure Boot.
pub(super) fn changed_boot(paths: &Paths, installed: &Installed) -> Result<()> {
    let machine = Machine {
        smbios: vec!["io.systemd.stub.kernel-cmdline-extra=beize.added=1".to_owned()],
        ..installed.machine.copy_as(paths, "smoke-changed")?
    };
    let run = watch_boot(
        &machine,
        TAMPER_TIMEOUT,
        &Answers::recovery_key(&installed.recovery_key),
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
    say!("  ok: the changed boot stopped before unlocking, without asking for the recovery key");
    machine.remove()
}

/// A dbx entry no binary has: added to simulate a Secure Boot update.
const SMOKE_REVOKED_HASH: &str = "62656967652d736d6f6b652d7265766f6b656462656967652d736d6f6b652d31";

/// Like Windows installing a revocation: a new dbx entry changes PCR 7.
pub(super) fn secure_boot_update(paths: &Paths, installed: &Installed) -> Result<()> {
    let machine = installed.machine.copy_as(paths, "smoke-dbx")?;
    edit_firmware(
        &machine,
        &["--add-dbx-hash", secureboot::OWNER_GUID, SMOKE_REVOKED_HASH],
    )?;
    // Typed on the keyboard (ADR 0017): with the dash where a US keyboard has
    // it, the German keymap the setup chose gives "ß" and the key is refused;
    // with the dash key of a German keyboard it opens.
    let on_keyboard = Answers::recovery_key_on_german_keyboard(&installed.recovery_key);
    let first = expect_rebound(&machine, 3, &on_keyboard, "the Secure Boot update")?;
    ensure!(
        first.prompts == 2,
        "the recovery key typed on the keyboard was not refused with a US dash and \
         accepted with a German one (asked {} times): the initrd did not use the \
         owner's keymap",
        first.prompts
    );
    say!(
        "  ok: in the initrd the keyboard typed with the owner's keymap (US dash key: refused, \
         German dash key: accepted)"
    );
    machine.remove()
}

/// Like someone appending their own certificate in the firmware setup.
pub(super) fn foreign_db(paths: &Paths, installed: &Installed) -> Result<()> {
    let machine = installed.machine.copy_as(paths, "smoke-db")?;
    let approved = paths.work.join("smoke-db-vars-approved.fd");
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
        3,
        &Answers::recovery_key(&installed.recovery_key),
        "the signature database (db) holds a certificate outside the profile",
    )?;
    // Back to the approved state: the TPM unlocks by itself again.
    fs::rename(&approved, &machine.vars)?;
    boot_healthy(&machine, 5, &Answers::none())?;
    say!("  ok: back in the approved state the TPM unlocked by itself");
    machine.remove()
}

/// The same disk and firmware with a factory-new TPM, as after replacing
/// the mainboard's TPM (or moving the disk to another PC).
pub(super) fn other_tpm(paths: &Paths, installed: &Installed) -> Result<()> {
    let machine = installed.machine.copy_as(paths, "smoke-newtpm")?;
    new_tpm(&machine.tpm)?;
    let with_key = Answers::recovery_key(&installed.recovery_key);
    expect_refused(
        &machine,
        3,
        &with_key.clone().adopt_tpm("no"),
        "The new security chip was not adopted",
    )?;
    expect_rebound(
        &machine,
        5,
        &with_key.adopt_tpm("yes"),
        "a confirmed new TPM",
    )?;
    machine.remove()
}

/// Keys stay enrolled, Secure Boot is switched off – as in the firmware
/// setup. The TPM is the one the data is bound to: only PCR 7 differs.
pub(super) fn secure_boot_off(paths: &Paths, installed: &Installed) -> Result<()> {
    let machine = installed.machine.copy_as(paths, "smoke-sboff")?;
    edit_firmware(&machine, &["--set-false", "SecureBootEnable"])?;
    expect_root_locked(&machine, "Secure Boot disabled")?;
    // The recovery key opens the data, but must not approve this state.
    let with_key = Answers::recovery_key(&installed.recovery_key);
    let run = watch_boot(&machine, REPORT_TIMEOUT, &with_key, BootReport::parse)?;
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
    expect_root_locked(&machine, "Secure Boot disabled after a recovery-key boot")?;
    say!(
        "  ok: with Secure Boot disabled the recovery key opened the data without re-binding the TPM"
    );
    machine.remove()
}

pub(super) fn unsigned_uki(paths: &Paths) -> Result<()> {
    let machine = Machine::install(paths, "unsigned")?;
    util::run(
        Command::new("mcopy")
            .arg("-o")
            .arg("-i")
            .arg(format!("{}@@{}", machine.disk.display(), image::ESP_OFFSET))
            .arg(image::unsigned_uki(paths))
            .arg(image::uki_path_on_esp(layout::OS_VERSION)),
    )?;
    let run = watch_boot(&machine, TAMPER_TIMEOUT, &Answers::none(), |line| {
        if line.contains(REPORT_MARKER) || line.contains("Welcome to") {
            Some(false)
        } else if line.contains("Access Denied") || line.contains("Access denied") {
            Some(true)
        } else {
            None
        }
    })?;
    match run.answer {
        Some((true, _)) => say!("  ok: the unsigned kernel image was refused"),
        Some((false, _)) => bail!("an unsigned kernel image booted under Secure Boot"),
        None => bail!(
            "no verdict on the unsigned kernel image within {}s",
            TAMPER_TIMEOUT.as_secs()
        ),
    }
    machine.remove()
}

/// Runs the owner's workflow end to end – `bz secureboot new-keys`, then
/// `bz sign` – with a throwaway directory instead of 1Password, and boots the
/// result: enrollment from setup mode, then a healthy first boot, then one
/// with an empty BIOS battery.
pub(super) fn owner_workflow(paths: &Paths) -> Result<()> {
    let dir = paths.work.join("owner-workflow");
    tree::remove(&dir)?;
    // What new-keys and sign tell the owner (files to commit, ...) is only
    // shown here when they fail.
    let (disk, said) = util::capture(|| -> Result<PathBuf> {
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
        image::assemble(paths, &keys, layout::OS_VERSION, &out, image::Target::Vm)
    });
    if disk.is_err() {
        said.into_iter().for_each(util::say_line);
    }
    let disk = disk?;

    let machine = Machine::install_from(paths, "owner", &disk)?;
    boot_healthy(&machine, 1, &Answers::none())?;
    say!("  ok: keys from the key store signed an image that boots under Secure Boot");
    empty_battery(&machine)?;
    machine.remove()?;
    fs::remove_dir_all(&dir)?;
    Ok(())
}

/// A hardware clock that lost its time, as with an empty BIOS battery.
const EMPTY_BATTERY_RTC: &str = "2000-01-01T00:00:00";

/// Boots with the hardware clock reset to 2000: systemd must move the clock
/// to at least Beize's build time (or the last verified time saved by
/// beize-save-clock), so certificates and NTS can work (ADR 0012).
fn empty_battery(machine: &Machine) -> Result<()> {
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
    say!(
        "  ok: with the hardware clock reset to 2000 the system clock started at or after \
         Beize's build time ({} time servers verified)",
        boot.report.nts
    );
    Ok(())
}

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
