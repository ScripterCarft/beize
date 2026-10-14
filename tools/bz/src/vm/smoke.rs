//! `bz smoke`: the end-to-end test of the image.
//!
//! The image is installed like on a PC whose Secure Boot keys were cleared
//! (firmware in setup mode, factory-new TPM) and booted twice. Each scenario
//! that changes something – firmware variables, the TPM, the boot – then
//! starts from its own copy of that installation, so they are independent
//! and run side by side, as many as the machine has room for, together with
//! the scenarios that install an image of their own. `--quick` stops after
//! the two boots.
//!
//! Each boot's console goes to `build/work/logs/boot/`. A scenario's output
//! is shown in one piece when it is done; when it fails, with the end of its
//! last console. A failed scenario leaves its machine in `build/work` for
//! `bz debug --machine <name>`.

use std::collections::VecDeque;
use std::fs;
use std::io::{Read, Seek, SeekFrom, Write};
use std::panic::{self, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Condvar, Mutex, MutexGuard, OnceLock, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail, ensure};

use super::checks;
use super::checks::{
    boot_healthy, check_blessed, check_boot_budget, check_partitions, check_root_binding,
    check_units, damage_root_file_system, expect_rebound, expect_refused, expect_root_locked,
    show_slowest_units,
};
use super::console::{
    Answers, ConsoleEvent, EMERGENCY_HEADLINE, EMERGENCY_SHELL_STARTS, EMERGENCY_WRONG_KEY,
    LAST_CONSOLE, OWNER_CONSOLE_CREDENTIALS, PASSWORD_PAUSE, RECOVERY_HEADLINE,
    RECOVERY_KEY_SAVED_PROMPT, REPORT_TIMEOUT, SETUP_ANSWERS, SETUP_STARTS_OVER, SMOKE_CREDENTIALS,
    SMOKE_PASSWORD, SMOKE_USER, Session, TAMPER_TIMEOUT, find_recovery_key, strip_ansi, watch_boot,
};
use super::debug::Shell;
use super::machine::{Machine, boot_once, clear_boot_logs, edit_firmware, new_tpm, parallel_vms};
use super::report::{BootReport, REPORT_MARKER};
use super::run::DEBUG_CONSOLE_CREDENTIALS;
use super::serve::Server;
use crate::paths::Paths;
use crate::say;
use crate::{image, layout, release, secureboot, usr, util};

/// A part of the smoke test, run on its own machine beside the others.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Scenario {
    Install,
    ChangedBoot,
    SecureBootUpdate,
    ForeignDb,
    OtherTpm,
    SecureBootOff,
    UnsignedUki,
    CorruptedUsr,
    OwnerKeys,
    EmergencyGate,
    Privileges,
    Recovery,
    Update,
    BrokenUpdate,
}

use Scenario::*;

/// What starts right away; the longest first, so they finish together.
const INDEPENDENT: [Scenario; 3] = [CorruptedUsr, OwnerKeys, UnsignedUki];
/// What starts from a copy of the installation once [`Install`] passed.
const ON_THE_INSTALLATION: [Scenario; 10] = [
    BrokenUpdate,
    Update,
    OtherTpm,
    EmergencyGate,
    Recovery,
    Privileges,
    ForeignDb,
    SecureBootUpdate,
    SecureBootOff,
    ChangedBoot,
];

impl Scenario {
    fn title(self) -> &'static str {
        match self {
            Install => "Install, first boot (setup, recovery key) and second boot",
            ChangedBoot => {
                "A boot with something added must stop before asking for the recovery key"
            }
            SecureBootUpdate => {
                "A Secure Boot update needs the recovery key once, then the TPM unlocks again"
            }
            ForeignDb => {
                "A foreign certificate in db must not be approved, even with the recovery key"
            }
            OtherTpm => "Another TPM (new mainboard) is adopted only when the owner confirms it",
            SecureBootOff => "The root partition must stay locked with Secure Boot disabled",
            UnsignedUki => "Secure Boot must refuse an unsigned kernel image",
            CorruptedUsr => {
                "dm-verity must detect a modified /usr; with no other version the start waits"
            }
            OwnerKeys => "The owner's signing workflow must yield an image that boots",
            EmergencyGate => {
                "A failed start gives a shell only for the recovery key, and the TPM is locked then"
            }
            Recovery => {
                "The recovery in the boot menu opens a root shell with the recovery key only"
            }
            Privileges => {
                "The owner gets root with run0 and their password only; no file grants privileges"
            }
            Update => {
                "An update signed with the update key starts with the next boot; others are refused"
            }
            BrokenUpdate => {
                "An update that does not start falls back to the previous version after its last try"
            }
        }
    }

    fn run(self, smoke: &Smoke) -> Result<()> {
        let paths = smoke.paths;
        match self {
            Install => {
                let installed = install(paths)?;
                let _ = smoke.installed.set(installed);
                Ok(())
            }
            ChangedBoot => changed_boot(paths, smoke.installed()?),
            SecureBootUpdate => secure_boot_update(paths, smoke.installed()?),
            ForeignDb => foreign_db(paths, smoke.installed()?),
            OtherTpm => other_tpm(paths, smoke.installed()?),
            SecureBootOff => secure_boot_off(paths, smoke.installed()?),
            UnsignedUki => unsigned_uki(paths),
            CorruptedUsr => corrupted_usr(paths),
            OwnerKeys => owner_workflow(paths),
            EmergencyGate => emergency_gate(paths, smoke.installed()?),
            Privileges => privileges(paths, smoke.installed()?),
            Recovery => recovery(paths, smoke.installed()?),
            Update => update(paths, &smoke.test_keys, smoke.installed()?),
            BrokenUpdate => broken_update(paths, &smoke.test_keys, smoke.installed()?),
        }
    }
}

/// Runs the smoke test (`quick`: only [`Install`]).
pub fn smoke(paths: &Paths, quick: bool) -> Result<()> {
    clear_boot_logs(paths)?;
    let started = Instant::now();
    let mut waiting = VecDeque::from([Install]);
    if !quick {
        waiting.extend(INDEPENDENT);
    }
    let smoke = Smoke {
        paths,
        quick,
        installed: OnceLock::new(),
        test_keys: secureboot::Keys::test(paths, secureboot::Profile::WithWindows)?,
        state: Mutex::new(State {
            waiting,
            running: 0,
            done: Vec::new(),
        }),
        changed: Condvar::new(),
    };
    let workers = parallel_vms();
    say!("  {workers} machines side by side");
    thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| smoke.work());
        }
    });
    let failed = smoke.summary(started.elapsed());
    ensure!(
        failed.is_empty(),
        "the smoke test failed: {}",
        failed.join("; ")
    );
    if let Some(installed) = smoke.installed.into_inner() {
        installed.machine.remove()?;
    }
    Ok(())
}

/// The installation after its first two boots: what the scenarios that
/// change something start from, each on its own copy.
struct Installed {
    machine: Machine,
    recovery_key: String,
}

/// The shared state of a smoke test run.
struct Smoke<'a> {
    paths: &'a Paths,
    quick: bool,
    installed: OnceLock<Installed>,
    /// The test keys, loaded once: loading them writes their enrollment
    /// files anew, which scenarios building releases read meanwhile.
    test_keys: secureboot::Keys,
    state: Mutex<State>,
    /// Signalled whenever a scenario ends (new ones may be waiting).
    changed: Condvar,
}

struct State {
    waiting: VecDeque<Scenario>,
    running: usize,
    done: Vec<(Scenario, Duration, bool)>,
}

impl Smoke<'_> {
    fn installed(&self) -> Result<&Installed> {
        self.installed
            .get()
            .context("the installation the scenario starts from is missing")
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// A worker: runs scenarios until none is waiting and none is running
    /// that could add more.
    fn work(&self) {
        while let Some(scenario) = self.next() {
            let started = Instant::now();
            LAST_CONSOLE.set(None);
            let (passed, said) = util::capture(|| {
                let result = panic::catch_unwind(AssertUnwindSafe(|| scenario.run(self)))
                    .unwrap_or_else(|_| Err(anyhow!("the test itself panicked")));
                if let Err(error) = &result {
                    if let Some(log) = LAST_CONSOLE.with_borrow(Clone::clone) {
                        util::print_tail(&log);
                    }
                    say!("  FAILED: {error:#}");
                }
                result.is_ok()
            });
            self.done(scenario, started.elapsed(), passed, &said);
        }
    }

    fn next(&self) -> Option<Scenario> {
        let mut state = self.lock();
        loop {
            if let Some(scenario) = state.waiting.pop_front() {
                state.running += 1;
                return Some(scenario);
            }
            if state.running == 0 {
                return None;
            }
            state = self
                .changed
                .wait(state)
                .unwrap_or_else(PoisonError::into_inner);
        }
    }

    fn done(&self, scenario: Scenario, took: Duration, passed: bool, said: &[String]) {
        let mut state = self.lock();
        // In one piece: other workers print only while holding the lock, too.
        util::step(scenario.title());
        for line in said {
            eprintln!("{line}");
        }
        if passed && scenario == Install && !self.quick {
            state.waiting.extend(ON_THE_INSTALLATION);
        }
        state.running -= 1;
        state.done.push((scenario, took, passed));
        self.changed.notify_all();
    }

    /// Lists every scenario with its duration; returns the failed ones.
    fn summary(&self, took: Duration) -> Vec<&'static str> {
        let mut state = self.lock();
        state.done.sort();
        util::step("Summary");
        let mut failed = Vec::new();
        for &(scenario, took, passed) in &state.done {
            say!(
                "  {:<6} {:>4.0}s  {}",
                if passed { "ok" } else { "FAILED" },
                took.as_secs_f64(),
                scenario.title()
            );
            if !passed {
                failed.push(scenario.title());
            }
        }
        if !self.quick && self.installed.get().is_none() {
            for scenario in ON_THE_INSTALLATION {
                say!("  {:<6}        {}", "-", scenario.title());
            }
        }
        say!(
            "  {} after {:.0}s",
            if failed.is_empty() {
                "passed"
            } else {
                "FAILED"
            },
            took.as_secs_f64()
        );
        failed
    }
}

/// Installs the image, boots it twice and checks everything a first and a
/// later start must do.
fn install(paths: &Paths) -> Result<Installed> {
    let machine = Machine::install(paths, "smoke")?;

    let first = boot_healthy(&machine, 1, &Answers::none())?;
    check_partitions(&machine.disk)?;
    check_blessed(&machine.disk, layout::OS_VERSION)?;
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
    say!("  ok: first boot showed a recovery key and waited until it was saved");
    let created = format!("Account \"{SMOKE_USER}\" created");
    ensure!(
        first.console.iter().any(|line| line.contains(&created)),
        "first boot did not create the owner's account"
    );
    say!("  ok: first boot created the owner's account \"{SMOKE_USER}\"");
    // The answers press Ctrl+C at the first password: the setup must start
    // over, not end – it did, as the account exists.
    ensure!(
        first
            .console
            .iter()
            .any(|line| line.contains(SETUP_STARTS_OVER)),
        "the setup did not start over after Ctrl+C"
    );
    say!("  ok: Ctrl+C in the account setup started it over");
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
            line.contains("[  OK  ]")
                || (line.contains("Starting ") && !line.contains(SETUP_STARTS_OVER))
                || line.contains("Finished ")
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
    say!("  ok: the account setup had the console to itself");

    let second = boot_healthy(&machine, 2, &Answers::none())?;
    // The first boot creates partitions and asks questions: the second is
    // the one every later boot is like.
    show_slowest_units(&second)?;
    check_boot_budget(&second.report)?;
    check_units(&second.console)?;
    ensure!(
        second.report.machine_id == first.report.machine_id,
        "machine ID changed between boots ({} -> {}): the root partition did not persist",
        first.report.machine_id,
        second.report.machine_id
    );
    say!(
        "  ok: same installation after reboot (machine ID {})",
        second.report.machine_id
    );
    ensure!(
        find_recovery_key(&second.console).is_none(),
        "the recovery key was shown again on the second boot"
    );
    say!("  ok: the recovery key was shown only once");
    ensure!(
        !second
            .console
            .iter()
            .any(|line| line.contains(SETUP_ANSWERS[0].0)),
        "the second boot asked for the owner's account again"
    );
    say!("  ok: the owner's account was set up only once");
    Ok(Installed {
        machine,
        recovery_key,
    })
}

/// Like a firmware whose SMBIOS OEM strings were rewritten from another OS:
/// systemd-stub appends these kernel parameters even under Secure Boot.
fn changed_boot(paths: &Paths, installed: &Installed) -> Result<()> {
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
fn secure_boot_update(paths: &Paths, installed: &Installed) -> Result<()> {
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
fn foreign_db(paths: &Paths, installed: &Installed) -> Result<()> {
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
fn other_tpm(paths: &Paths, installed: &Installed) -> Result<()> {
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
fn secure_boot_off(paths: &Paths, installed: &Installed) -> Result<()> {
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

fn unsigned_uki(paths: &Paths) -> Result<()> {
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

/// Bounds a boot into the emergency gate, a wrong key (3 s penalty), a
/// restart after Ctrl+C and the shell.
const GATE_TIMEOUT: Duration = Duration::from_secs(90);

/// beize-account's questions (os/beize-accounts/src/bin/beize-account.rs),
/// for the owner "smoke" and the account "guest" the owner creates. None is
/// part of another ("New password:" – "New password again:" – "New password
/// of guest:"; "Password of guest:" starts with a capital).
const CURRENT_PASSWORD_PROMPT: &str = "Current password of smoke:";
const NEW_PASSWORD_PROMPT: &str = "New password:";
const NEW_PASSWORD_AGAIN_PROMPT: &str = "New password again:";
const OWN_PASSWORD_PROMPT: &str = "Your password (smoke):";
const RESET_OWNER_PROMPT: &str = "New password of smoke:";
const RESET_OWNER_AGAIN_PROMPT: &str = "New password of smoke again:";
const GUEST_NAME_PROMPT: &str = "Full name of guest";
const GUEST_PASSWORD_PROMPT: &str = "Password of guest:";
const GUEST_PASSWORD_AGAIN_PROMPT: &str = "Password of guest again:";
const RESET_GUEST_PROMPT: &str = "New password of guest:";
const RESET_GUEST_AGAIN_PROMPT: &str = "New password of guest again:";
/// The owner's password after changing it, and after resetting it.
const CHANGED_PASSWORD: &str = "smoke-changed-password";
const RESET_PASSWORD: &str = "smoke-reset-password";
const WRONG_PASSWORD: &str = "wrong-password";
/// The guest's password, as created and after the owner reset it.
const GUEST_PASSWORD: &str = "guest-first-password";
const GUEST_RESET_PASSWORD: &str = "guest-reset-password";

/// Like a thief who damages the disk so that the start fails after the TPM
/// unlocked the data (ADR 0016): the console must not hand out a shell – not
/// for a wrong key, not after Ctrl+C. With the recovery key it does, and from
/// that shell the TPM unlocks nothing.
fn emergency_gate(paths: &Paths, installed: &Installed) -> Result<()> {
    let machine = installed.machine.copy_as(paths, "smoke-emergency")?;
    damage_root_file_system(&machine.disk, &paths.work)?;
    // The key as a German keyboard types it with the US keymap: "/" for each
    // dash. Only its letters count (ADR 0017).
    let as_typed = installed.recovery_key.replace('-', "/");
    let session = Session::start(
        &machine,
        &Answers::emergency_gate(&as_typed),
        &SMOKE_CREDENTIALS,
    )?;
    let deadline = Instant::now() + GATE_TIMEOUT;
    let (mut headlines, mut refused, mut detached) = (0, false, false);
    let mut tpm_refused = None;
    while let Some(event) = session.next(deadline) {
        let ConsoleEvent::Line(line) = event else {
            continue;
        };
        let line = strip_ansi(&line);
        let line = line.trim();
        if line.contains(EMERGENCY_HEADLINE) {
            headlines += 1;
        } else if line.contains(EMERGENCY_WRONG_KEY) {
            refused = true;
        } else if line.contains(EMERGENCY_SHELL_STARTS) {
            ensure!(
                refused && headlines >= 2,
                "the emergency gate opened a shell before it refused a wrong key and Ctrl+C"
            );
            // The TPM unlocked the root partition before its mount failed:
            // close it, then ask the TPM for the key once more.
            session.type_line("systemd-cryptsetup detach root && echo BEIZE-DETACHED");
            session.type_line(
                "systemd-cryptsetup attach probe /dev/gpt-auto-root-luks - \
                 tpm2-device=auto,headless=yes >/dev/null 2>&1 \
                 && echo BEIZE-TPM-UNLOCKED || echo BEIZE-TPM-REFUSED",
            );
        } else if line == "BEIZE-DETACHED" {
            detached = true;
        } else if line == "BEIZE-TPM-UNLOCKED" || line == "BEIZE-TPM-REFUSED" {
            tpm_refused = Some(line == "BEIZE-TPM-REFUSED");
            break;
        }
    }
    session.power_off_within(Duration::ZERO)?;
    match tpm_refused {
        None => bail!(
            "no emergency shell with the recovery key within {}s",
            GATE_TIMEOUT.as_secs()
        ),
        Some(_) if !detached => bail!("the emergency shell could not close the root partition"),
        Some(false) => bail!("from the emergency shell the TPM still unlocked the root partition"),
        Some(true) => say!(
            "  ok: no shell for a wrong key or after Ctrl+C; with the recovery key a shell \
             from which the TPM unlocks nothing"
        ),
    }
    machine.remove()
}

/// What `beize-update` says when it refuses a release, by the reason.
const UNTRUSTED_UPDATE: &str = "The update is not from Beize or was changed";
const NO_NEWER_UPDATE: &str = "There is no newer version.";

/// An update as the owner installs it (ADR 0020): a release of the next
/// version, fetched over HTTP from the host. Releases that are not what the
/// update key signed – a changed list, another key's signature, a changed
/// file – and an older version are refused, and nothing is written. The
/// right one goes into slot B; the next boot starts it, the TPM unlocks as
/// before, and boot counting marks it good – but not a boot in which a unit
/// of `images/running-units.txt` failed. A successful boot replaces an older
/// boot loader with the signed one from `/usr`.
fn update(paths: &Paths, keys: &secureboot::Keys, installed: &Installed) -> Result<()> {
    let mut machine = installed.machine.copy_as(paths, "smoke-update")?;
    let version = format!("{}.1", layout::OS_VERSION);
    let releases = paths.work.join("smoke-releases");
    if releases.exists() {
        fs::remove_dir_all(&releases)?;
    }
    let good = releases.join(&version);
    test_release(paths, keys, &version, &good)?;
    let refused = refused_releases(&releases, &good, keys)?;
    let older_loader = paths.work.join("smoke-update-loader.efi");
    older_boot_loader(paths, keys, &older_loader)?;
    for path in BOOT_LOADER_ON_ESP {
        put_on_esp(&machine.disk, &older_loader, path)?;
    }

    let shell = Shell::boot(
        &machine,
        &Answers::none(),
        &DEBUG_CONSOLE_CREDENTIALS,
        false,
    )?;
    // The boot loader is updated at the end of the boot (beize-boot-update):
    // the signed one from /usr over both copies of the older one.
    let (_, lines) = shell.capture(
        "systemctl is-system-running --wait; \
         journalctl -b -o cat -u beize-boot-update.service",
    )?;
    ensure!(
        lines.iter().any(|l| l.trim() == "running"),
        "the boot did not finish as running: {lines:?}"
    );
    let copied = format!("Copied \"/usr/{}\" to ", usr::SIGNED_BOOT_LOADER_PATH);
    for path in BOOT_LOADER_ON_ESP {
        ensure!(
            lines
                .iter()
                .any(|l| l.contains(&copied) && l.trim_end().ends_with(&format!("{path}\"."))),
            "bootctl did not install the signed boot loader from /usr as {path}: {lines:?}"
        );
    }
    for (what, dir, says) in &refused {
        let server = Server::start(dir)?;
        let (status, lines) = shell.capture(&format!("beize-update install {}", server.url()))?;
        ensure!(
            status != 0 && lines.iter().any(|l| l.contains(says)),
            "beize-update did not refuse {what} (exit status {status}): {lines:?}"
        );
    }
    let (_, lines) = shell.capture("ls /boot/EFI/Linux")?;
    ensure!(
        !lines.iter().any(|l| l.contains(&version)),
        "a refused release was written: {lines:?}"
    );
    say!(
        "  ok: refused {}, nothing written",
        refused
            .iter()
            .map(|(what, _, _)| *what)
            .collect::<Vec<_>>()
            .join(", ")
    );

    let server = Server::start(&good)?;
    let started = Instant::now();
    let (status, lines) = shell.capture(&format!("beize-update install {}", server.url()))?;
    let install_s = started.elapsed().as_secs_f64();
    ensure!(
        status == 0
            && lines
                .iter()
                .any(|l| l.contains(&format!("Beize {version} is installed"))),
        "beize-update did not install {version} (exit status {status}): {lines:?}"
    );
    drop(server);
    shell.power_off()?;
    let listing = checks::uki_listing(&machine.disk)?;
    ensure!(
        // sysupdate writes the tries done as well: `+3-0`, the same count.
        listing
            .iter()
            .any(|name| name.starts_with(layout::uki_name(&version).trim_end_matches(".efi"))),
        "the update's UKI with boot counter is not on the ESP: {listing:?}"
    );
    say!("  ok: beize-update installed {version} into slot B ({install_s:.1}s)");
    let signed_loader = fs::read(paths.out.join("systemd-bootx64.efi"))?;
    for path in BOOT_LOADER_ON_ESP {
        ensure!(
            read_from_esp(&machine.disk, path, &paths.work)? == signed_loader,
            "the boot did not replace the older boot loader {path} with the signed one from /usr"
        );
    }
    fs::remove_file(&older_loader)?;
    say!("  ok: the boot replaced an older boot loader with the signed one from /usr");

    // A unit every boot runs fails: the boot does not count as successful,
    // and nothing of it is kept.
    let credentials = [&DEBUG_CONSOLE_CREDENTIALS[..], &[FAILING_NTP_CREDENTIAL]].concat();
    let shell = Shell::boot(&machine, &Answers::none(), &credentials, false)?;
    let (_, lines) = shell.capture(
        "echo \"state=$(systemctl is-system-running --wait) \
         ntp=$(systemctl is-failed ntpd-rs.service) \
         health=$(systemctl is-active beize-boot-health.service) \
         loader=$(systemctl is-active beize-boot-update.service)\"",
    )?;
    let expected = "state=degraded ntp=failed health=failed loader=inactive";
    ensure!(
        lines.iter().any(|l| l.trim() == expected),
        "a boot with a failed unit was not judged as failed (expected {expected}): {lines:?}"
    );
    shell.power_off()?;
    let listing = checks::uki_listing(&machine.disk)?;
    let tried_once = format!(
        "{}+{}-1.efi",
        layout::blessed_uki_name(&version).trim_end_matches(".efi"),
        layout::BOOT_TRIES - 1
    );
    ensure!(
        listing.contains(&tried_once),
        "after a failed boot the update's UKI should be {tried_once}: {listing:?}"
    );
    say!("  ok: a boot with a failed unit was not marked good ({tried_once})");

    // Boot #3: the starts with the root shell have no boot report.
    boot_healthy(&machine, 3, &Answers::none())?;
    check_blessed(&machine.disk, &version)?;
    say!("  ok: {version} started, the TPM unlocked the data as before");

    // The next update replaces the oldest version, never the running one.
    // It comes from a USB stick, as on the owner's PC: Beize and Windows
    // share the computer, there is no other one to serve it.
    let next = format!("{}.2", layout::OS_VERSION);
    test_release(paths, keys, &next, &releases.join(&next))?;
    let stick = paths.work.join("smoke-update-stick.img");
    usb_stick(&releases.join(&next), &format!("beize-{next}"), &stick)?;
    machine.usb_stick = Some(stick.clone());
    let shell = Shell::boot(
        &machine,
        &Answers::none(),
        &DEBUG_CONSOLE_CREDENTIALS,
        false,
    )?;
    let (status, lines) = shell.capture(&format!(
        "mkdir -p /run/stick && mount -o ro /dev/disk/by-label/{STICK_LABEL} /run/stick \
         && beize-update install /run/stick/beize-{next}"
    ))?;
    ensure!(
        status == 0
            && lines
                .iter()
                .any(|l| l.contains(&format!("Beize {next} is installed"))),
        "beize-update did not install {next} from the USB stick (exit status {status}): {lines:?}"
    );
    shell.power_off()?;
    machine.usb_stick = None;
    fs::remove_file(&stick)?;
    let labels = usr_labels(&machine.disk)?;
    let expected = [&next, &next, &version, &version].map(|v| layout::usr_label(v));
    ensure!(
        labels == expected,
        "the second update did not replace slot A ({}): /usr slots are {labels:?}",
        layout::usr_label(layout::OS_VERSION)
    );
    let listing = checks::uki_listing(&machine.disk)?;
    ensure!(
        !listing.contains(&layout::blessed_uki_name(layout::OS_VERSION)),
        "the UKI of the replaced version is still on the ESP: {listing:?}"
    );
    boot_healthy(&machine, 4, &Answers::none())?;
    check_blessed(&machine.disk, &next)?;
    say!(
        "  ok: the second update {next}, from a USB stick, replaced {} in slot A",
        layout::OS_VERSION
    );
    machine.remove()?;
    fs::remove_dir_all(&releases)?;
    Ok(())
}

/// An update that is signed but does not start: the header of its /usr
/// hash tree is damaged, as from a broken build. Each of its tries fails in
/// the initrd and restarts by itself; then the previous version starts
/// again. That version started once: when it fails too, the start waits at
/// the emergency gate instead of restarting.
///
/// A damaged hash tree header fails when dm-verity is set up; damaged data
/// when /usr is mounted – `corrupted_usr` shows that. Both fail at once.
fn broken_update(paths: &Paths, keys: &secureboot::Keys, installed: &Installed) -> Result<()> {
    let machine = installed.machine.copy_as(paths, "smoke-broken-update")?;
    let version = format!("{}.1", layout::OS_VERSION);
    let releases = paths.work.join("smoke-broken-releases");
    if releases.exists() {
        fs::remove_dir_all(&releases)?;
    }
    let broken = releases.join(&version);
    test_release(paths, keys, &version, &broken)?;
    // Damage the hash tree, then sign the list of the damaged files.
    let list = fs::read_to_string(broken.join(release::LIST))?;
    let mut signed = String::new();
    for line in list.lines() {
        let (_, name) = line.split_once("  ").context("release list line")?;
        if name.ends_with(".usr.verity") {
            damage_verity(&broken.join(name), 0)?;
        }
        signed += &format!("{}  {name}\n", util::sha256_file(&broken.join(name))?);
    }
    fs::write(broken.join(release::LIST), signed)?;
    keys.sign_update(
        &broken.join(release::LIST),
        &broken.join(release::SIGNATURE),
    )?;
    install_update(&machine, &broken, &version)?;

    for attempt in 1..=layout::BOOT_TRIES {
        let start = failed_start(&machine)?;
        let try_n = format!("(try {attempt} of {})", layout::BOOT_TRIES);
        ensure!(
            start.verity_setup_failed()
                && start
                    .restarted
                    .as_ref()
                    .is_some_and(|line| line.contains(&try_n)),
            "the broken update did not restart after try {attempt}: {start:?}"
        );
        // It fails at once, not after the device timeout (see
        // system/initrd/.../dev-mapper-usr.device.d).
        ensure!(
            !start.failed.iter().any(|line| line.contains("Timed out")),
            "try {attempt} waited for /dev/mapper/usr until the device timeout: {start:?}"
        );
    }
    say!(
        "  ok: the broken update {version} restarted by itself after each of its {} tries",
        layout::BOOT_TRIES
    );

    // Boot #3: the starts with the root shell and the broken ones have no
    // boot report.
    boot_healthy(&machine, 3, &Answers::none())?;
    let listing = checks::uki_listing(&machine.disk)?;
    let used_up = format!(
        "{}+0-{}.efi",
        layout::blessed_uki_name(&version).trim_end_matches(".efi"),
        layout::BOOT_TRIES
    );
    ensure!(
        listing.contains(&layout::blessed_uki_name(layout::OS_VERSION))
            && listing.contains(&used_up),
        "expected {} and {used_up} on the ESP: {listing:?}",
        layout::blessed_uki_name(layout::OS_VERSION)
    );
    say!(
        "  ok: then the previous version {} started again; {used_up} has no tries left",
        layout::OS_VERSION
    );

    damage_verity(&machine.disk, image::USR_VERITY_OFFSET)?;
    let start = failed_start(&machine)?;
    ensure!(
        start.verity_setup_failed() && start.restarted.is_none(),
        "the previous version, which started before, did not wait at the emergency gate \
         when it failed: {start:?}"
    );
    say!(
        "  ok: when the previous version fails too, the start waits at the emergency gate \
         (it started once, so it is not on trial)"
    );
    machine.remove()?;
    fs::remove_dir_all(&releases)?;
    Ok(())
}

/// The volume label of the test's USB stick.
const STICK_LABEL: &str = "BEIZE";

/// A USB stick image as it comes from a shop: an MBR with one FAT32
/// partition, holding the release in `dir` as directory `name`.
fn usb_stick(dir: &Path, name: &str, image: &Path) -> Result<()> {
    const SIZE: u64 = 512 * layout::MIB;
    const START: u64 = 2048; // sectors of 512 bytes
    let file = fs::File::create(image)?;
    file.set_len(SIZE)?;
    drop(file);
    let mut sfdisk = Command::new("sfdisk")
        .arg("--quiet")
        .arg(image)
        .stdin(std::process::Stdio::piped())
        .spawn()?;
    sfdisk
        .stdin
        .take()
        .context("sfdisk stdin")?
        .write_all(format!("label: dos\nstart={START}, type=c\n").as_bytes())?;
    ensure!(
        sfdisk.wait()?.success(),
        "sfdisk failed on {}",
        image.display()
    );
    util::output(
        Command::new("mkfs.vfat")
            .args(["-F", "32", "-n", STICK_LABEL])
            .arg(format!("--offset={START}"))
            .arg(image)
            .arg(((SIZE - START * 512) / 1024).to_string()),
    )?;
    util::output(
        Command::new("mcopy")
            .arg("-s")
            .arg("-i")
            .arg(format!("{}@@{}", image.display(), START * 512))
            .arg(dir)
            .arg(format!("::/{name}")),
    )?;
    Ok(())
}

/// Writes the release of `version`, signed with the test keys `keys`, into
/// `dir`; its output only if it fails.
fn test_release(paths: &Paths, keys: &secureboot::Keys, version: &str, dir: &Path) -> Result<()> {
    let (built, said) =
        util::capture(|| release::write(paths, keys, version, image::Target::Vm, dir));
    if built.is_err() {
        said.into_iter().for_each(util::say_line);
    }
    built
}

/// Installs the release in `dir` as the owner would, from a root shell over
/// HTTP, and powers off.
fn install_update(machine: &Machine, dir: &Path, version: &str) -> Result<()> {
    let shell = Shell::boot(machine, &Answers::none(), &DEBUG_CONSOLE_CREDENTIALS, false)?;
    let server = Server::start(dir)?;
    let (status, lines) = shell.capture(&format!("beize-update install {}", server.url()))?;
    ensure!(
        status == 0
            && lines
                .iter()
                .any(|l| l.contains(&format!("Beize {version} is installed"))),
        "beize-update did not install {version} (exit status {status}): {lines:?}"
    );
    drop(server);
    shell.power_off()
}

/// The GPT labels of the /usr partitions of `disk`, in partition order:
/// slot A data and verity, then slot B.
fn usr_labels(disk: &Path) -> Result<Vec<String>> {
    let dump = util::output(Command::new("sfdisk").arg("--dump").arg(disk))?;
    Ok(dump
        .lines()
        .filter(|line| {
            let line = line.to_uppercase();
            line.contains(&format!("TYPE={}", layout::USR_TYPE))
                || line.contains(&format!("TYPE={}", layout::USR_VERITY_TYPE))
        })
        .filter_map(|line| line.split("name=\"").nth(1))
        .filter_map(|rest| rest.split('"').next())
        .map(str::to_owned)
        .collect())
}

/// Where the boot loader lies on the ESP: systemd-boot's own path and the
/// removable-media path the firmware starts.
const BOOT_LOADER_ON_ESP: [&str; 2] = ["/EFI/systemd/systemd-bootx64.efi", "/EFI/BOOT/BOOTX64.EFI"];

/// Makes ntpd-rs, a unit every boot runs, fail: a drop-in as a credential
/// (systemd-debug-generator), `[Service] ExecStartPre=/usr/bin/false`.
const FAILING_NTP_CREDENTIAL: &str = "type=11,value=io.systemd.credential.binary:\
     systemd.unit-dropin.ntpd-rs.service=W1NlcnZpY2VdCkV4ZWNTdGFydFByZT0vdXNyL2Jpbi9mYWxzZQo=";

/// The image's boot loader as an older version, signed with `keys`, into
/// `out`: its version (`#### LoaderInfo: systemd-boot 262 ####`, which
/// bootctl compares) begins with 0 instead.
fn older_boot_loader(paths: &Paths, keys: &secureboot::Keys, out: &Path) -> Result<()> {
    const INFO: &[u8] = b"#### LoaderInfo: systemd-boot ";
    let mut loader = fs::read(
        paths
            .sysroot
            .join("usr/lib/systemd/boot/efi/systemd-bootx64.efi"),
    )?;
    let mut patched = 0;
    let mut at = 0;
    while let Some(found) = loader[at..].windows(INFO.len()).position(|w| w == INFO) {
        let version = at + found + INFO.len();
        ensure!(
            matches!(loader[version], b'1'..=b'9'),
            "unexpected boot loader version"
        );
        loader[version] = b'0';
        patched += 1;
        at = version;
    }
    ensure!(patched > 0, "the boot loader carries no version");
    let unsigned = out.with_extension("unsigned");
    fs::write(&unsigned, loader)?;
    keys.sign(paths, &unsigned, out)?;
    fs::remove_file(&unsigned)?;
    Ok(())
}

/// Writes `file` to `path` on the ESP of `disk`.
fn put_on_esp(disk: &Path, file: &Path, path: &str) -> Result<()> {
    util::run(
        Command::new("mcopy")
            .arg("-o")
            .arg("-i")
            .arg(format!("{}@@{}", disk.display(), image::ESP_OFFSET))
            .arg(file)
            .arg(format!("::{path}")),
    )
}

/// The content of `path` on the ESP of `disk`, read through a file in `work`.
fn read_from_esp(disk: &Path, path: &str, work: &Path) -> Result<Vec<u8>> {
    let copy = work.join(format!(
        "esp-{}",
        path.rsplit('/').next().unwrap_or_default()
    ));
    util::run(
        Command::new("mcopy")
            .arg("-o")
            .arg("-i")
            .arg(format!("{}@@{}", disk.display(), image::ESP_OFFSET))
            .arg(format!("::{path}"))
            .arg(&copy),
    )?;
    let content = fs::read(&copy)?;
    fs::remove_file(&copy)?;
    Ok(content)
}

/// Releases next to `good` that beize-update must refuse: what each is, its
/// directory and what beize-update says.
fn refused_releases(
    releases: &Path,
    good: &Path,
    keys: &secureboot::Keys,
) -> Result<Vec<(&'static str, PathBuf, &'static str)>> {
    let copy = |name: &str, all: bool| -> Result<PathBuf> {
        let dir = releases.join(name);
        fs::create_dir_all(&dir)?;
        for entry in fs::read_dir(good)? {
            let path = entry?.path();
            let file = path.file_name().context("file name")?;
            if all || [release::LIST, release::SIGNATURE].contains(&file.to_str().unwrap_or("")) {
                fs::copy(&path, dir.join(file))?;
            }
        }
        Ok(dir)
    };
    let list = fs::read_to_string(good.join(release::LIST))?;

    // The list changed after signing: another sum for the first file.
    let changed_list = copy("changed-list", false)?;
    let flipped = if list.starts_with('0') { "1" } else { "0" };
    fs::write(
        changed_list.join(release::LIST),
        format!("{flipped}{}", &list[1..]),
    )?;

    // The list signed with another ML-DSA-65 key.
    let other_key = copy("other-key", false)?;
    util::output(
        Command::new("openssl")
            .args(["genpkey", "-algorithm", "ML-DSA-65", "-out"])
            .arg(other_key.join("key")),
    )?;
    util::output(
        Command::new("openssl")
            .args(["pkeyutl", "-sign", "-rawin", "-inkey"])
            .arg(other_key.join("key"))
            .arg("-in")
            .arg(other_key.join(release::LIST))
            .arg("-out")
            .arg(other_key.join(release::SIGNATURE)),
    )?;
    fs::remove_file(other_key.join("key"))?;

    // A file changed after signing: one byte of the UKI.
    let changed_file = copy("changed-file", true)?;
    let uki = list
        .lines()
        .filter_map(|line| line.split_once("  ").map(|(_, name)| name))
        .find(|name| name.ends_with(".efi"))
        .context("the release has no UKI")?
        .to_owned();
    let mut bytes = fs::read(changed_file.join(&uki))?;
    bytes[4096] ^= 1;
    fs::write(changed_file.join(&uki), bytes)?;

    // Correctly signed, but older than the running version.
    let older = releases.join("older");
    fs::create_dir_all(&older)?;
    fs::write(
        older.join(release::LIST),
        list.lines()
            .map(|line| {
                let (sum, name) = line.split_once("  ").unwrap_or_default();
                let version = name
                    .strip_prefix("beize_")
                    .and_then(|rest| rest.split(['_']).next())
                    .map(|v| v.trim_end_matches(".efi"))
                    .unwrap_or_default();
                format!("{sum}  {}\n", name.replacen(version, "0.0.9", 1))
            })
            .collect::<String>(),
    )?;
    keys.sign_update(&older.join(release::LIST), &older.join(release::SIGNATURE))?;

    Ok(vec![
        ("a changed list", changed_list, UNTRUSTED_UPDATE),
        ("another key's signature", other_key, UNTRUSTED_UPDATE),
        ("a changed file", changed_file, UNTRUSTED_UPDATE),
        ("an older version", older, NO_NEWER_UPDATE),
    ])
}

/// The recovery (ADR 0018), chosen in the boot menu: the TPM refuses it, so
/// the initrd asks for the recovery key; the gate takes that key instead of
/// asking again and opens a root shell with the data unlocked. The next
/// normal start unlocks by the TPM again: the recovery bound nothing to it.
fn recovery(paths: &Paths, installed: &Installed) -> Result<()> {
    let machine = installed.machine.copy_as(paths, "smoke-recovery")?;
    boot_once(
        &machine,
        &format!(
            "{}{}",
            layout::blessed_uki_name(layout::OS_VERSION),
            image::RECOVERY_ENTRY
        ),
    )?;
    // A forgotten password (ADR 0019): root in the recovery sets a new one
    // through the account service, and PAM's own check takes it.
    let answers = Answers::recovery(&installed.recovery_key)
        .answering(RESET_OWNER_PROMPT, &[RESET_PASSWORD])
        .answering(RESET_OWNER_AGAIN_PROMPT, &[RESET_PASSWORD]);
    let session = Session::start(&machine, &answers, &SMOKE_CREDENTIALS)?;
    let deadline = Instant::now() + GATE_TIMEOUT;
    let (mut prompted, mut headline, mut shell, mut reset) = (false, false, None, None);
    let mut failed = Vec::new();
    while let Some(event) = session.next(deadline) {
        let ConsoleEvent::Line(line) = event else {
            prompted = true;
            continue;
        };
        let line = strip_ansi(&line);
        if line.contains("[FAILED]") {
            failed.push(line);
        } else if line.contains(RECOVERY_HEADLINE) {
            headline = true;
        } else if line.contains(EMERGENCY_SHELL_STARTS) {
            session.type_line(
                "echo BEIZE-RECOVERY-$(id -u)-$(systemctl show -P ConditionResult \
                 systemd-tpm2-setup-early)-$(ls /home)",
            );
        } else if let Some(found) = line.strip_prefix("BEIZE-RECOVERY-") {
            shell = Some(found.to_owned());
            session.type_line(&format!(
                "beize-account reset {SMOKE_USER} && printf '%s\\0' {RESET_PASSWORD} \
                 | unix_chkpwd {SMOKE_USER} nullok; echo BEIZE-RESET-$?"
            ));
        } else if let Some(status) = line.strip_prefix("BEIZE-RESET-") {
            reset = Some(status.to_owned());
            break;
        }
    }
    session.power_off_within(Duration::ZERO)?;
    ensure!(
        prompted,
        "the recovery start did not ask for the recovery key"
    );
    ensure!(
        headline,
        "the recovery start did not reach the recovery gate"
    );
    let Some(shell) = shell else {
        bail!(
            "no root shell within {}s after the recovery key (did the gate ask for it again?)",
            GATE_TIMEOUT.as_secs()
        );
    };
    ensure!(
        shell.starts_with("0-") && shell.ends_with(&format!("-{SMOKE_USER}")),
        "the recovery shell is not root with the data unlocked: {shell}"
    );
    // The NvPCR setup needs the signed initrd policy: skipped, not failed
    // (on real TPMs it fails rather than giving up quietly).
    ensure!(
        shell.starts_with("0-no-"),
        "the recovery started the TPM's NvPCR setup in the system: {shell}"
    );
    say!("  ok: the recovery asks for the recovery key once and opens a root shell with the data");
    ensure!(
        reset.as_deref() == Some("0"),
        "beize-account reset in the recovery did not give the owner a new password \
         (status {reset:?})"
    );
    say!("  ok: in the recovery root sets a new password for a forgotten one");
    boot_healthy(&machine, 3, &Answers::none())?;
    say!("  ok: after the recovery the TPM unlocks by itself again");
    machine.remove()
}

/// Privileges (ADR 0018), as the owner at a terminal: run0 refuses a wrong
/// password and gives root for the right one; a file made setuid or given a
/// capability grants nothing, because / and /usr are mounted nosuid.
fn privileges(paths: &Paths, installed: &Installed) -> Result<()> {
    let machine = installed.machine.copy_as(paths, "smoke-privileges")?;
    // polkit's questions, in order: the serial console is no active session,
    // so polkit asks every time. A wrong password comes last: polkit's agent
    // may ask again after one – however often, from there on only wrong
    // passwords are typed.
    let mut passwords = vec![
        SMOKE_PASSWORD,
        SMOKE_PASSWORD,
        SMOKE_PASSWORD,
        CHANGED_PASSWORD,
        CHANGED_PASSWORD,
    ];
    passwords.extend([WRONG_PASSWORD; 10]);
    let answers = Answers::passwords(&passwords)
        // The owner creates "guest", first with a wrong password of their
        // own, then resets the guest's password.
        .answering(
            OWN_PASSWORD_PROMPT,
            &[WRONG_PASSWORD, CHANGED_PASSWORD, CHANGED_PASSWORD],
        )
        .answering(GUEST_NAME_PROMPT, &["Guest"; 2])
        .answering(GUEST_PASSWORD_PROMPT, &[GUEST_PASSWORD; 2])
        .answering(GUEST_PASSWORD_AGAIN_PROMPT, &[GUEST_PASSWORD; 2])
        .answering(RESET_GUEST_PROMPT, &[GUEST_RESET_PASSWORD])
        .answering(RESET_GUEST_AGAIN_PROMPT, &[GUEST_RESET_PASSWORD])
        // beize-account passwd twice: with a wrong current password, then
        // with the right one.
        .answering(CURRENT_PASSWORD_PROMPT, &[WRONG_PASSWORD, SMOKE_PASSWORD])
        .answering(NEW_PASSWORD_PROMPT, &[CHANGED_PASSWORD; 2])
        .answering(NEW_PASSWORD_AGAIN_PROMPT, &[CHANGED_PASSWORD; 2]);
    let shell = Shell::boot(&machine, &answers, &OWNER_CONSOLE_CREDENTIALS, false)?;
    // How long the login took, on the guest's clock: from the start of the
    // serial getty to the start of the owner's shell (field 22 of its stat,
    // in clock ticks of 10 ms on x86).
    let (_, lines) = shell.capture(
        "read -r -a s < /proc/$$/stat; echo BEIZE-LOGIN \
         $(systemctl show -P ExecMainStartTimestampMonotonic serial-getty@ttyS0.service) ${s[21]}",
    )?;
    let login_ms = lines
        .iter()
        .find_map(|l| {
            let mut fields = l.strip_prefix("BEIZE-LOGIN ")?.split(' ');
            let getty_us: u64 = fields.next()?.parse().ok()?;
            let shell_ticks: u64 = fields.next()?.parse().ok()?;
            (shell_ticks * 10).checked_sub(getty_us / 1000)
        })
        .with_context(|| format!("could not measure the login: {lines:?}"))?;

    let started = Instant::now();
    let (status, lines) = shell.capture("run0 bash -c 'echo BEIZE-UID-$(id -u)'")?;
    let run0_ms = started.elapsed().saturating_sub(PASSWORD_PAUSE).as_millis();
    ensure!(
        status == 0 && lines.iter().any(|l| l.contains("BEIZE-UID-0")),
        "run0 with the owner's password did not run as root (exit status {status}): {lines:?}"
    );
    ensure!(
        lines
            .iter()
            .any(|l| l.contains(&format!("Authenticating as: Smoke Test ({SMOKE_USER})"))),
        "polkit did not ask for the owner's own password: {lines:?}"
    );
    ensure!(
        !lines.iter().any(|l| l.contains("Unknown line")),
        "polkit's agent got the helper's error messages: {lines:?}"
    );
    say!("  ok: run0 runs as root with the owner's password");
    say!(
        "  measured: login {login_ms} ms (getty to shell), run0 with the password \
         {run0_ms} ms (without typing)"
    );
    // Nobody logs in as root: no password matches its entry.
    let (_, lines) = shell.capture("run0 getent shadow root")?;
    ensure!(
        lines.iter().any(|l| l.starts_with("root:!*:")),
        "root's password is not locked: {lines:?}"
    );
    say!("  ok: root has no password, only run0 gives root");

    // Mount options of / and /usr (field 6 of mountinfo), without grep.
    let (_, lines) = shell.capture(
        "while read -r _ _ _ _ m o _; do case $m in /|/usr) echo \"BEIZE-MOUNT $m $o\";; esac; \
         done < /proc/self/mountinfo",
    )?;
    for mount in ["/", "/usr"] {
        let options = lines
            .iter()
            .find_map(|l| l.strip_prefix(&format!("BEIZE-MOUNT {mount} ")))
            .with_context(|| format!("{mount} is not mounted: {lines:?}"))?;
        let options: Vec<&str> = options.split(',').collect();
        ensure!(
            options.contains(&"nosuid") && options.contains(&"nodev"),
            "{mount} is mounted without nosuid,nodev: {options:?}"
        );
    }
    // The counter-check: as root, a setuid copy of id and a copy of cat with
    // the capability to read any file, both in /var.
    let (status, lines) = shell.capture(
        "run0 bash -c 'cp /usr/bin/id /usr/bin/cat /var/tmp/ && chmod 4755 /var/tmp/id \
         && setcap cap_dac_read_search=ep /var/tmp/cat && echo BEIZE-PREPARED'",
    )?;
    ensure!(
        status == 0 && lines.iter().any(|l| l.contains("BEIZE-PREPARED")),
        "could not prepare the setuid and capability files (exit status {status}): {lines:?}"
    );
    // The same as the owner's own, whatever ID the account service gave.
    let (_, lines) = shell.capture("echo BEIZE-SETUID-$(/var/tmp/id -u)-$(id -u)")?;
    ensure!(
        lines.iter().any(|l| {
            l.strip_prefix("BEIZE-SETUID-")
                .and_then(|ids| ids.split_once('-'))
                .is_some_and(|(setuid, own)| setuid == own && own != "0")
        }),
        "a setuid-root file in /var ran with other rights than the owner's: {lines:?}"
    );
    let (status, _) = shell.capture("/var/tmp/cat /etc/shadow > /dev/null")?;
    ensure!(
        status != 0,
        "a file with a capability in /var read /etc/shadow for the owner"
    );
    say!(
        "  ok: / and /usr are mounted nosuid,nodev; a setuid file and a file capability \
         in /var grant nothing"
    );
    // Units' system call filters are in force (systemd with libseccomp):
    // mode 2 is "filter" in /proc/<pid>/status.
    for unit in ["systemd-journald", "ntpd-rs"] {
        let (_, lines) = shell.capture(&format!(
            "while read -r k v; do [ \"$k\" = Seccomp: ] && echo BEIZE-SECCOMP-$v; \
             done < /proc/$(systemctl show -P MainPID {unit})/status"
        ))?;
        ensure!(
            lines.iter().any(|l| l == "BEIZE-SECCOMP-2"),
            "{unit} runs without its system call filter: {lines:?}"
        );
    }
    say!("  ok: units run with their system call filters (seccomp)");
    // ping and tracepath as the owner, with neither setuid nor capabilities:
    // ICMP datagram sockets (net.ipv4.ping_group_range).
    for command in [
        "ping -c 1 -W 2 127.0.0.1",
        "ping -c 1 -W 2 ::1",
        "tracepath -n -m 2 127.0.0.1",
    ] {
        let (status, lines) = shell.capture(&format!("{command} > /dev/null"))?;
        ensure!(
            status == 0,
            "`{command}` failed for the owner (exit status {status}): {lines:?}"
        );
    }
    say!("  ok: the owner pings (IPv4, IPv6) and traces a path without privileges");
    // lsblk as the owner shows what udev knows (recipes/lsblk.toml).
    let (_, lines) = shell.capture("lsblk -n -o PARTLABEL,FSTYPE")?;
    ensure!(
        lines
            .iter()
            .any(|l| l.split_whitespace().collect::<Vec<_>>() == ["root", "crypto_LUKS"]),
        "lsblk shows the owner no partition names and file systems: {lines:?}"
    );
    say!("  ok: lsblk shows the owner partition names and file systems");

    // Accounts (ADR 0019), through Beize's account service: the owner
    // changes their own password – a wrong current one changes nothing.
    let (status, lines) = shell.capture("beize-account passwd")?;
    ensure!(
        status != 0
            && lines
                .iter()
                .any(|l| l.contains("That password is not right.")),
        "beize-account passwd changed the password with a wrong current one \
         (exit status {status}): {lines:?}"
    );
    let started = Instant::now();
    let (status, lines) = shell.capture("beize-account passwd")?;
    let passwd_ms = started.elapsed().as_millis();
    ensure!(
        status == 0 && lines.iter().any(|l| l.contains("Password changed.")),
        "beize-account passwd did not change the owner's password (exit status {status}): \
         {lines:?}"
    );
    let (status, lines) = shell.capture("run0 bash -c 'echo BEIZE-NEW-$(id -u)'")?;
    ensure!(
        status == 0 && lines.iter().any(|l| l.contains("BEIZE-NEW-0")),
        "run0 did not take the owner's new password (exit status {status}): {lines:?}"
    );
    say!("  ok: the owner changed their password with beize-account; run0 takes the new one");
    // An administrator creates an account – not with a wrong password of
    // their own – and resets its password, as on macOS.
    let (status, lines) = shell.capture("beize-account add guest")?;
    ensure!(
        status != 0
            && lines
                .iter()
                .any(|l| l.contains("That password is not right.")),
        "an account was created without the administrator's password \
         (exit status {status}): {lines:?}"
    );
    let (status, lines) = shell.capture("beize-account add guest")?;
    ensure!(
        status == 0 && lines.iter().any(|l| l.contains("Account guest created.")),
        "the owner could not create an account (exit status {status}): {lines:?}"
    );
    let (status, lines) = shell.capture("beize-account reset guest")?;
    ensure!(
        status == 0
            && lines
                .iter()
                .any(|l| l.contains("New password set for guest.")),
        "the owner could not reset the guest's password (exit status {status}): {lines:?}"
    );
    // PAM's own check (as root, which may read the hash) takes the new one.
    let (_, lines) = shell.capture(&format!(
        "run0 bash -c \"printf '%s\\0' {GUEST_RESET_PASSWORD} | unix_chkpwd guest nullok \
         && echo BEIZE-GUEST-OK\""
    ))?;
    ensure!(
        lines.iter().any(|l| l == "BEIZE-GUEST-OK"),
        "the guest's reset password does not unlock the account: {lines:?}"
    );
    say!(
        "  ok: an administrator creates an account and resets its password, only with their \
         own password"
    );
    // Last (see the answers above): a wrong password gives nothing.
    let (status, lines) = shell.capture("run0 id -u")?;
    ensure!(
        status != 0 && lines.iter().any(|l| l.contains("AUTHENTICATION FAILED")),
        "run0 did not refuse a wrong password (exit status {status}): {lines:?}"
    );
    ensure!(
        !lines.iter().any(|l| l.contains(WRONG_PASSWORD)),
        "the password was shown while it was typed: {lines:?}"
    );
    say!("  ok: run0 refuses a wrong password");
    say!("  measured: password change {passwd_ms} ms (with typing)");
    shell.stop()?;
    machine.remove()
}

/// A modified /usr is detected and not used. The fresh installation is on
/// trial too (its UKI counts its starts), but there is no other version to
/// fall back to: the start waits at the emergency gate.
fn corrupted_usr(paths: &Paths) -> Result<()> {
    let machine = Machine::install(paths, "corrupted")?;
    damage_usr(&machine.disk, image::USR_OFFSET)?;
    let start = failed_start(&machine)?;
    ensure!(
        start.verity_corrupted,
        "dm-verity did not report the modified /usr"
    );
    ensure!(
        start.restarted.is_none(),
        "with no other version the start restarted instead of waiting at the emergency gate: {:?}",
        start.restarted
    );
    say!(
        "  ok: the modified /usr was detected and not used; with no other version the start \
         waits at the emergency gate"
    );
    machine.remove()
}

/// Flips one byte of the EROFS superblock (at 1 KiB) of the /usr image at
/// `offset` – in a disk or in a release file.
fn damage_usr(file: &Path, offset: u64) -> Result<()> {
    flip_byte(file, offset + 1024)
}

/// Flips the first byte of the magic of the dm-verity superblock at
/// `offset`: the hash tree's header – in a disk or in a release file.
fn damage_verity(file: &Path, offset: u64) -> Result<()> {
    flip_byte(file, offset)
}

fn flip_byte(file: &Path, offset: u64) -> Result<()> {
    let mut file = fs::OpenOptions::new().read(true).write(true).open(file)?;
    let mut byte = [0u8; 1];
    file.seek(SeekFrom::Start(offset))?;
    file.read_exact(&mut byte)?;
    file.seek(SeekFrom::Start(offset))?;
    file.write_all(&[byte[0] ^ 0xff])?;
    Ok(())
}

/// Shown by the emergency gate (os/beize-tpm/src/emergency.rs) as one of its
/// choices: it waits for the owner.
const GATE_MENU: &str = "emergency shell (needs the recovery key)";
/// Shown by the emergency gate before a new version restarts by itself
/// (os/beize-tpm/src/trial.rs).
const TRIAL_RESTARTING: &str = "This version of Beize is new and did not start. Restarting";
/// How long a failed start may take to restart or reach the gate. Below
/// systemd's 90 s device timeout on purpose: a damaged /usr must fail the
/// start at once, not after waiting for a device that never becomes ready
/// (system/initrd: dev-mapper-usr.device.d, 99-zz-beize-verity.rules).
const FAILED_START_TIMEOUT: Duration = Duration::from_secs(60);
/// systemd's description of systemd-veritysetup@usr.service.
const VERITY_SETUP: &str = "Integrity Protection Setup for usr";

/// How a start that must fail ended.
#[derive(Debug)]
struct FailedStart {
    /// The gate's line if it restarted the computer by itself; `None`: it
    /// waits for the owner.
    restarted: Option<String>,
    /// dm-verity reported a modified /usr.
    verity_corrupted: bool,
    /// systemd's lines about failed units and devices that timed out.
    failed: Vec<String>,
}

impl FailedStart {
    /// dm-verity could not be set up for /usr.
    fn verity_setup_failed(&self) -> bool {
        self.failed.iter().any(|line| line.contains(VERITY_SETUP))
    }
}

/// Starts `machine` once, expecting the start to fail: it either restarts
/// by itself (QEMU ends, `-no-reboot`) or waits at the emergency gate.
fn failed_start(machine: &Machine) -> Result<FailedStart> {
    let session = Session::start(machine, &Answers::none(), &SMOKE_CREDENTIALS)?;
    let deadline = Instant::now() + FAILED_START_TIMEOUT;
    let mut start = FailedStart {
        restarted: None,
        verity_corrupted: false,
        failed: Vec::new(),
    };
    while let Some(event) = session.next(deadline) {
        let ConsoleEvent::Line(line) = event else {
            continue;
        };
        let line = strip_ansi(&line);
        if line.contains(REPORT_MARKER) || line.contains("Welcome to") {
            bail!("the system started from a modified /usr");
        } else if line.contains("verity") && line.contains("is corrupted") {
            start.verity_corrupted = true;
        } else if line.contains(TRIAL_RESTARTING) {
            start.restarted = Some(line.trim().to_owned());
        } else if line.contains("[FAILED]") || line.contains("[ TIME ]") {
            start.failed.push(line.trim().to_owned());
        } else if line.contains(GATE_MENU) {
            ensure!(
                start.restarted.is_none(),
                "the gate both restarted and waited"
            );
            session.power_off_within(Duration::ZERO)?;
            return Ok(start);
        }
    }
    // The guest ended QEMU by restarting, or the deadline passed. The console
    // closes a moment before QEMU has exited.
    let ended = session.power_off_within(Duration::from_secs(5))?;
    ensure!(
        ended && start.restarted.is_some(),
        "the failed start neither restarted nor reached the emergency gate within {}s",
        FAILED_START_TIMEOUT.as_secs()
    );
    Ok(start)
}

/// Runs the owner's workflow end to end – `bz secureboot new-keys`, then
/// `bz sign` – with a throwaway directory instead of 1Password, and boots the
/// result: enrollment from setup mode, then a healthy first boot, then one
/// with an empty BIOS battery.
fn owner_workflow(paths: &Paths) -> Result<()> {
    let dir = paths.work.join("owner-workflow");
    if dir.exists() {
        fs::remove_dir_all(&dir)?;
    }
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
