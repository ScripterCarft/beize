//! `bz run`: the image in QEMU with the serial console on this terminal.

use std::fs;
use std::path::Path;

use anyhow::Result;

use super::machine::{Machine, Swtpm, clear_boot_logs, qemu};
use crate::paths::Paths;
use crate::util;

/// Credential for `bz run` and `bz debug`: a root shell on the serial
/// console (VM images only).
pub(super) const DEBUG_CONSOLE_CREDENTIAL: &str =
    "type=11,value=io.systemd.credential:beize.debug-console=1";

/// Boots the image with the serial console attached to this terminal.
/// Quit QEMU with Ctrl-A X.
pub fn run(paths: &Paths) -> Result<()> {
    let machine = machine(paths)?;
    util::step("Booting in QEMU (quit with Ctrl-A X)");
    let tpm = Swtpm::start(&machine.tpm)?;
    util::run_attached(
        qemu(&machine, &tpm)?
            .args(["-nographic", "-serial", "mon:stdio"])
            .args(["-smbios", DEBUG_CONSOLE_CREDENTIAL]),
    )
}

/// The VM of `bz run` and `bz debug`: it keeps its disk between runs until a
/// new image is built, then it is installed afresh.
pub(super) fn machine(paths: &Paths) -> Result<Machine> {
    clear_boot_logs(paths)?;
    let image = paths.out.join("disk.img");
    let machine = Machine::named(paths, "run");
    let outdated = |p: &Path| -> Result<bool> {
        Ok(!p.exists() || fs::metadata(p)?.modified()? < fs::metadata(&image)?.modified()?)
    };
    if outdated(&machine.disk)? || outdated(&machine.vars)? || !machine.tpm.exists() {
        util::step("Installing the current image onto a new VM disk");
        Machine::install(paths, "run")?;
    }
    Ok(machine)
}
