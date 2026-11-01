//! Scenarios of a damaged /usr and of starts that fail (ADR 0008, ADR 0016).

use std::fs;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail, ensure};

use crate::image;
use crate::paths::Paths;
use crate::say;
use crate::vm::checks::{self, boot_healthy};
use crate::vm::console::{
    Answers, ConsoleEvent, SMOKE_CREDENTIALS, Session, find_recovery_key, strip_ansi,
};
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

/// Power goes while the recovery key is on screen: nobody saw it for sure. The
/// next start shows a new one, and the first opens nothing any more.
pub(super) fn key_not_confirmed(paths: &Paths) -> Result<()> {
    let machine = Machine::install(paths, "smoke-key-not-confirmed")?;
    let session = Session::start(&machine, &Answers::key_unconfirmed(), &SMOKE_CREDENTIALS)?;
    let deadline = Instant::now() + FAILED_START_TIMEOUT;
    let mut console = Vec::new();
    let first = loop {
        if let Some(key) = find_recovery_key(&console) {
            break key;
        }
        let Some(event) = session.next(deadline) else {
            bail!("the first start showed no recovery key");
        };
        if let ConsoleEvent::Line(line) = event {
            console.push(strip_ansi(&line));
        }
    };
    session.power_off_within(Duration::ZERO)?;
    ensure!(
        checks::root_recovery_keys(&machine.disk, &paths.work)? == (1, true),
        "the key on screen should be marked as not confirmed"
    );

    let next = boot_healthy(&machine, 1, &Answers::none())?;
    let second = find_recovery_key(&next.console)
        .context("the start after the power cut showed no recovery key")?;
    ensure!(first != second, "the same recovery key was shown twice");
    ensure!(
        checks::root_recovery_keys(&machine.disk, &paths.work)? == (1, false),
        "after the confirmation there should be one recovery key, no mark"
    );
    say!("  ok: cut short at the recovery key, the next start showed a new one and replaced it");
    machine.remove()
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
    let mut split = String::new();
    while let Some(event) = session.next(deadline) {
        let ConsoleEvent::Line(line) = event else {
            continue;
        };
        let Some(line) = whole_line(&mut split, strip_ansi(&line)) else {
            continue;
        };
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
        ended,
        "the failed start neither restarted nor reached the emergency gate within {}s",
        FAILED_START_TIMEOUT.as_secs()
    );
    ensure!(
        start.restarted.is_some(),
        "the failed start ended the VM without saying it restarts"
    );
    Ok(start)
}

/// A console line in one piece. The kernel writes to the serial port itself,
/// also into the middle of a line user space is writing: `[FAILED] Failed to
/// [    0.98] Buffer I/O error…`, and the rest of the line comes after. Such a
/// line waits in `split` for its rest: split, a match for a question or a
/// verdict is missed.
fn whole_line(split: &mut String, line: String) -> Option<String> {
    if let Some(at) = kernel_stamp(&line).filter(|at| *at > 0) {
        split.push_str(&line[..at]);
        return None;
    }
    if split.is_empty() {
        return Some(line);
    }
    let whole = std::mem::take(split) + &line;
    Some(whole)
}

/// Where a kernel message's stamp `[    0.980958]` starts in `line`.
fn kernel_stamp(line: &str) -> Option<usize> {
    line.match_indices('[').map(|(at, _)| at).find(|at| {
        line[*at + 1..].split_once(']').is_some_and(|(stamp, _)| {
            let stamp = stamp.trim_start();
            let (seconds, micros) = stamp.split_once('.').unwrap_or(("", ""));
            !seconds.is_empty()
                && micros.len() == 6
                && seconds.bytes().all(|b| b.is_ascii_digit())
                && micros.bytes().all(|b| b.is_ascii_digit())
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_kernel_message_inside_a_line_leaves_it_whole() {
        let mut split = String::new();
        let first = "Restar[    0.980958] Buffer I/O error on dev vda".to_owned();
        assert_eq!(whole_line(&mut split, first), None);
        assert_eq!(
            whole_line(&mut split, "ting now".to_owned()),
            Some("Restarting now".to_owned())
        );
        let plain = "[    0.980958] a kernel line".to_owned();
        assert_eq!(whole_line(&mut split, plain.clone()), Some(plain));
    }
}
