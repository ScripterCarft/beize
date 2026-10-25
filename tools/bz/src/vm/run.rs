//! `bz run`: the image in QEMU with the serial console on this terminal.

use std::fs;
use std::path::Path;

use anyhow::Result;

use super::machine::{Machine, Swtpm, clear_boot_logs, qemu};
use crate::paths::Paths;
use crate::util;

/// Credentials for `bz run` and `bz debug`: root logged in on the serial
/// console without a password, read by util-linux's agetty and login (as
/// mkosi does). Only the VMs pass them; the image itself has no autologin,
/// and images for real hardware import no credentials at all.
pub(super) const DEBUG_CONSOLE_CREDENTIALS: [&str; 2] = [
    "type=11,value=io.systemd.credential:tty.serial.ttyS0.agetty.autologin=root",
    "type=11,value=io.systemd.credential:tty.serial.ttyS0.login.noauth=yes",
];

/// Boots the image with the serial console attached to this terminal.
/// Quit QEMU with Ctrl-A X.
pub fn run(paths: &Paths) -> Result<()> {
    let machine = machine(paths)?;
    util::step("Booting in QEMU (quit with Ctrl-A X)");
    let tpm = Swtpm::start(&machine.tpm)?;
    util::run_attached(
        qemu(&machine, &tpm)?
            .args(["-nographic", "-serial", "mon:stdio"])
            .args(
                DEBUG_CONSOLE_CREDENTIALS
                    .iter()
                    .flat_map(|c| ["-smbios", c]),
            ),
    )
}

/// The VM of `bz run` and `bz debug`: it keeps its disk between runs until a
/// new image is built, then it is installed afresh.
pub(super) fn machine(paths: &Paths) -> Result<Machine> {
    clear_boot_logs(paths)?;
    let image = paths.image.join("disk.img");
    let mut machine = Machine::named(paths, "run");
    // By hand the desktop runs too.
    machine.memory_mib = super::machine::DESKTOP_MEMORY_MIB;
    let outdated = |p: &Path| -> Result<bool> {
        Ok(!p.exists() || fs::metadata(p)?.modified()? < fs::metadata(&image)?.modified()?)
    };
    if outdated(&machine.disk)? || outdated(&machine.vars)? || !machine.tpm.exists() {
        util::step("Installing the current image onto a new VM disk");
        Machine::install(paths, "run")?;
    }
    Ok(machine)
}
