//! `bz smoke`: the end-to-end test of the image.
//!
//! The image is installed like on a PC whose Secure Boot keys were cleared
//! (firmware in setup mode, factory-new TPM) and booted twice. Each scenario
//! that changes something – firmware variables, the TPM, the boot – then
//! starts from its own copy of that installation, so they are independent
//! and run side by side, as many as the machine has room for, together with
//! the scenarios that install an image of their own. The installation runs
//! alone, as its second boot is measured. `--quick` stops after the two
//! boots.
//!
//! Each boot's console goes to `build/work/logs/boot/`. A scenario's output
//! is shown in one piece when it is done; when it fails, with the end of its
//! last console. A failed scenario leaves its machine in `build/work` for
//! `bz debug --machine <name>`.
//!
//! Here: the order of the scenarios and the installation they start from.
//! The scenarios themselves, by theme: `trust` (Secure Boot and the TPM),
//! `access` (emergency gate, recovery, privileges), `updates`, `integrity`
//! (a damaged /usr, starts that fail), `desktop` (the login that starts it).

use std::collections::VecDeque;
use std::panic::{self, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::{Condvar, Mutex, MutexGuard, OnceLock, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, ensure};

use super::checks::{
    boot_healthy, check_blessed, check_boot_budget, check_partitions, check_root_binding,
    check_units, show_slowest_units,
};
use super::console::{
    Answers, LAST_CONSOLE, RECOVERY_KEY_SAVED_PROMPT, SETUP_ANSWERS, SETUP_STARTS_OVER, SMOKE_USER,
    find_recovery_key,
};
use super::machine::{Machine, clear_boot_logs, parallel_vms};
use crate::paths::Paths;
use crate::say;
use crate::{layout, secureboot, tree, util};

mod access;
mod desktop;
mod integrity;
mod trust;
mod updates;

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
    WithoutAdditions,
    Update,
    BrokenUpdate,
    Desktop,
}

use Scenario::*;

/// What runs once [`Install`] is done – it runs alone, as its second boot
/// is the one held to the time budget, and work beside it (building an
/// image takes every CPU) would measure the host. Then the longest first,
/// so they finish together; the update first of all, since it builds the
/// release the broken update waits for. Those that start from a copy of
/// the installation only if it passed, and only those that
/// [`Scenario::applies`] to the variant.
const AFTER_INSTALL: [Scenario; 15] = [
    Update,
    OwnerKeys,
    BrokenUpdate,
    Desktop,
    OtherTpm,
    ForeignDb,
    Privileges,
    SecureBootUpdate,
    SecureBootOff,
    Recovery,
    WithoutAdditions,
    CorruptedUsr,
    UnsignedUki,
    ChangedBoot,
    EmergencyGate,
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
            WithoutAdditions => {
                "The start without the desktop runs exactly the base, with the recovery key"
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
            Desktop => {
                "The login on tty1 starts the desktop with a client's window; signing out stops it"
            }
        }
    }

    /// Whether the scenario tests something the variant has.
    fn applies(self, paths: &Paths) -> bool {
        match self {
            WithoutAdditions => paths.variant.extended().is_some(),
            Desktop => paths
                .variant
                .packages()
                .iter()
                .any(|p| p == "beize-session"),
            _ => true,
        }
    }

    /// Whether it starts from a copy of the installation; the others
    /// install an image of their own.
    fn needs_installation(self) -> bool {
        !matches!(self, OwnerKeys | CorruptedUsr | UnsignedUki)
    }

    fn run(self, smoke: &Smoke) -> Result<()> {
        let paths = smoke.paths;
        match self {
            Install => {
                let installed = install(paths)?;
                let _ = smoke.installed.set(installed);
                Ok(())
            }
            ChangedBoot => trust::changed_boot(paths, smoke.installed()?),
            SecureBootUpdate => trust::secure_boot_update(paths, smoke.installed()?),
            ForeignDb => trust::foreign_db(paths, smoke.installed()?),
            OtherTpm => trust::other_tpm(paths, smoke.installed()?),
            SecureBootOff => trust::secure_boot_off(paths, smoke.installed()?),
            UnsignedUki => trust::unsigned_uki(paths),
            CorruptedUsr => integrity::corrupted_usr(paths),
            OwnerKeys => trust::owner_workflow(paths),
            EmergencyGate => access::emergency_gate(paths, smoke.installed()?),
            Privileges => access::privileges(paths, smoke.installed()?),
            Recovery => access::recovery(paths, smoke.installed()?),
            WithoutAdditions => access::without_additions(paths, smoke.installed()?),
            Update => updates::update(
                paths,
                &smoke.test_keys,
                smoke.installed()?,
                smoke.next_release()?,
            ),
            Desktop => desktop::desktop(paths, smoke.installed()?),
            BrokenUpdate => updates::broken_update(
                paths,
                &smoke.test_keys,
                smoke.installed()?,
                smoke.next_release()?,
            ),
        }
    }
}

/// Runs the smoke test (`quick`: only [`Install`]).
pub fn smoke(paths: &Paths, quick: bool) -> Result<()> {
    clear_boot_logs(paths)?;
    let started = Instant::now();
    let waiting = VecDeque::from([Install]);
    let smoke = Smoke {
        paths,
        quick,
        installed: OnceLock::new(),
        next_release: OnceLock::new(),
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
    tree::remove(&next_release_dir(paths))
}

/// Where the release of the next version that the updates share lies.
fn next_release_dir(paths: &Paths) -> PathBuf {
    paths.variant_work.join("smoke-next-release")
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
    /// The release of the next version, signed with the test keys, which
    /// the updates install: built once, by the first scenario that needs it
    /// (the other waits for it), as building one is most of their time.
    next_release: OnceLock<Result<PathBuf, String>>,
    /// The test keys, loaded once: loading them may sign their enrollment
    /// files, which scenarios building releases read meanwhile.
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

    /// The release of the next version, which only reading scenarios use
    /// as it is; one that changes it changes a copy.
    fn next_release(&self) -> Result<&Path> {
        self.next_release
            .get_or_init(|| {
                let dir = next_release_dir(self.paths);
                updates::test_release(self.paths, &self.test_keys, &updates::next_version(), &dir)
                    .map(|()| dir)
                    .map_err(|error| format!("{error:#}"))
            })
            .as_deref()
            .map_err(|error| anyhow!("the release of the next version: {error}"))
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
        if scenario == Install && !self.quick {
            let next = AFTER_INSTALL
                .into_iter()
                .filter(|s| s.applies(self.paths) && (passed || !s.needs_installation()));
            state.waiting.extend(next);
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
            for scenario in AFTER_INSTALL
                .into_iter()
                .filter(|s| s.applies(self.paths) && s.needs_installation())
            {
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
    check_units(&second.console, &paths.variant.running_units()?)?;
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
