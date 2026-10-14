//! Scenarios of a damaged /usr and of starts that fail (ADR 0008, ADR 0016).

use std::fs;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{Result, bail, ensure};

use crate::image;
use crate::paths::Paths;
use crate::say;
use crate::vm::console::{Answers, ConsoleEvent, SMOKE_CREDENTIALS, Session, strip_ansi};
use crate::vm::machine::Machine;
use crate::vm::report::REPORT_MARKER;

/// A modified /usr is detected and not used. The fresh installation is on
/// trial too (its UKI counts its starts), but there is no other version to
/// fall back to: the start waits at the emergency gate.
pub(super) fn corrupted_usr(paths: &Paths) -> Result<()> {
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
pub(super) fn damage_verity(file: &Path, offset: u64) -> Result<()> {
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
pub(super) struct FailedStart {
    /// The gate's line if it restarted the computer by itself; `None`: it
    /// waits for the owner.
    pub(super) restarted: Option<String>,
    /// dm-verity reported a modified /usr.
    pub(super) verity_corrupted: bool,
    /// systemd's lines about failed units and devices that timed out.
    pub(super) failed: Vec<String>,
}

impl FailedStart {
    /// dm-verity could not be set up for /usr.
    pub(super) fn verity_setup_failed(&self) -> bool {
        self.failed.iter().any(|line| line.contains(VERITY_SETUP))
    }
}

/// Starts `machine` once, expecting the start to fail: it either restarts
/// by itself (QEMU ends, `-no-reboot`) or waits at the emergency gate.
pub(super) fn failed_start(machine: &Machine) -> Result<FailedStart> {
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
