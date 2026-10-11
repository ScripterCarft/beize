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
use std::path::PathBuf;
use std::process::Command;
use std::sync::{Condvar, Mutex, MutexGuard, OnceLock, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail, ensure};

use super::checks::{
    boot_healthy, check_blessed, check_boot_budget, check_partitions, check_root_binding,
    check_units, damage_root_file_system, expect_rebound, expect_refused, expect_root_locked,
};
use super::console::{
    Answers, ConsoleEvent, EMERGENCY_HEADLINE, EMERGENCY_SHELL_STARTS, EMERGENCY_WRONG_KEY,
    LAST_CONSOLE, OWNER_CONSOLE_CREDENTIALS, RECOVERY_HEADLINE, RECOVERY_KEY_SAVED_PROMPT,
    REPORT_TIMEOUT, SETUP_ANSWERS, SETUP_STARTS_OVER, SMOKE_CREDENTIALS, SMOKE_PASSWORD,
    SMOKE_USER, Session, TAMPER_TIMEOUT, find_recovery_key, strip_ansi, watch_boot,
};
use super::debug::Shell;
use super::machine::{Machine, boot_once, clear_boot_logs, edit_firmware, new_tpm, parallel_vms};
use super::report::{BootReport, REPORT_MARKER};
use crate::paths::Paths;
use crate::say;
use crate::{image, layout, secureboot, util};

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
}

use Scenario::*;

/// What starts right away; the longest first, so they finish together.
const INDEPENDENT: [Scenario; 3] = [OwnerKeys, CorruptedUsr, UnsignedUki];
/// What starts from a copy of the installation once [`Install`] passed.
const ON_THE_INSTALLATION: [Scenario; 8] = [
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
            CorruptedUsr => "dm-verity must detect a modified /usr",
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

/// The recovery (ADR 0018), chosen in the boot menu: the TPM refuses it, so
/// the initrd asks for the recovery key; the gate takes that key instead of
/// asking again and opens a root shell with the data unlocked. The next
/// normal start unlocks by the TPM again: the recovery bound nothing to it.
fn recovery(paths: &Paths, installed: &Installed) -> Result<()> {
    let machine = installed.machine.copy_as(paths, "smoke-recovery")?;
    boot_once(
        &machine,
        &format!("{}{}", layout::blessed_uki_name(), image::RECOVERY_ENTRY),
    )?;
    let session = Session::start(
        &machine,
        &Answers::recovery(&installed.recovery_key),
        &SMOKE_CREDENTIALS,
    )?;
    let deadline = Instant::now() + GATE_TIMEOUT;
    let (mut prompted, mut headline, mut shell) = (false, false, None);
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
            session.type_line("echo BEIZE-RECOVERY-$(id -u)-$(cat /etc/hostname)-$(ls /home)");
        } else if let Some(found) = line.strip_prefix("BEIZE-RECOVERY-") {
            shell = Some(found.to_owned());
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
    say!("  ok: the recovery asks for the recovery key once and opens a root shell with the data");
    boot_healthy(&machine, 3, &Answers::none())?;
    say!("  ok: after the recovery the TPM unlocks by itself again");
    machine.remove()
}

/// Privileges (ADR 0018), as the owner at a terminal: run0 refuses a wrong
/// password and gives root for the right one; a file made setuid or given a
/// capability grants nothing, because / and /usr are mounted nosuid.
fn privileges(paths: &Paths, installed: &Installed) -> Result<()> {
    let machine = installed.machine.copy_as(paths, "smoke-privileges")?;
    // The serial console is no active session: polkit asks every time.
    let answers = Answers::passwords(&[
        "wrong-password",
        SMOKE_PASSWORD,
        SMOKE_PASSWORD,
        SMOKE_PASSWORD,
    ]);
    let shell = Shell::boot(&machine, &answers, &OWNER_CONSOLE_CREDENTIALS, false)?;

    let (status, lines) = shell.capture("run0 id -u")?;
    ensure!(
        status != 0 && lines.iter().any(|l| l.contains("AUTHENTICATION FAILED")),
        "run0 did not refuse a wrong password (exit status {status}): {lines:?}"
    );
    ensure!(
        !lines.iter().any(|l| l.contains("wrong-password")),
        "the password was shown while it was typed: {lines:?}"
    );
    ensure!(
        !lines.iter().any(|l| l.contains("Unknown line")),
        "polkit's agent got the helper's error messages: {lines:?}"
    );
    let (status, lines) = shell.capture("run0 bash -c 'echo BEIZE-UID-$(id -u)'")?;
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
    say!("  ok: run0 refuses a wrong password and runs as root with the owner's");
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
    let (_, lines) = shell.capture("echo BEIZE-SETUID-$(/var/tmp/id -u)")?;
    ensure!(
        lines.iter().any(|l| l == "BEIZE-SETUID-1000"),
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
    shell.stop()?;
    machine.remove()
}

fn corrupted_usr(paths: &Paths) -> Result<()> {
    let machine = Machine::install(paths, "corrupted")?;
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
    })?;
    match run.answer {
        Some((true, _)) => say!("  ok: the modified /usr was detected and not used"),
        Some((false, _)) => bail!("the system booted from a modified /usr"),
        None => bail!(
            "no verdict on the modified /usr within {}s",
            TAMPER_TIMEOUT.as_secs()
        ),
    }
    machine.remove()
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
        image::assemble(paths, &keys, &out, image::Target::Vm)
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
