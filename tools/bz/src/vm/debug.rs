//! `bz debug`: boots a VM headless, runs commands on its root shell and
//! prints what they output – for looking inside the guest (journal, PCRs,
//! ...) without typing at `bz run`.

use std::time::{Duration, Instant};

use anyhow::{Result, bail, ensure};

use super::console::{Answers, ConsoleEvent, REPORT_TIMEOUT, Session, strip_ansi};
use super::machine::{Machine, clear_boot_logs};
use super::run::{self, DEBUG_CONSOLE_CREDENTIALS};
use crate::paths::Paths;
use crate::say;
use crate::util;

/// systemd's last line of a boot; the root shell (serial getty) starts after
/// the first-boot setup, which runs before this target.
const BOOTED: &str = "Reached target Graphical Interface";
/// How long the root shell may take to answer after the boot.
const SHELL_TIMEOUT: Duration = Duration::from_secs(30);
/// How long one command may run.
const COMMAND_TIMEOUT: Duration = Duration::from_secs(120);
const POWER_OFF_TIMEOUT: Duration = Duration::from_secs(60);

/// The shell prints this plus a tag after each command. It is typed as
/// `printf 'BZ-DEBUG-%s\n' TAG`, so the echo of the typed line never matches.
const MARKER: &str = "BZ-DEBUG-";

/// Boots `machine` (`run`, or a machine `bz smoke` left in build/work),
/// optionally with another TPM state directory, types `recovery_key` if the
/// boot asks for it and runs `commands` on the root shell (`@wait N` pauses
/// N seconds), then powers off.
pub fn debug(
    paths: &Paths,
    machine: &str,
    recovery_key: Option<&str>,
    tpm: Option<&str>,
    commands: &[String],
) -> Result<()> {
    let mut machine = if machine == "run" {
        run::machine(paths)?
    } else {
        let found = Machine::named(paths, machine);
        ensure!(
            found.disk.exists() && found.vars.exists(),
            "no machine \"{machine}\" in {}: use `run` or a disk left there by `bz smoke`",
            paths.work.display()
        );
        clear_boot_logs(paths)?;
        found
    };
    if let Some(tpm) = tpm {
        machine.tpm = paths.work.join(tpm);
        ensure!(
            machine.tpm.is_dir(),
            "no TPM state in {}",
            machine.tpm.display()
        );
    }
    let answers = recovery_key.map_or_else(Answers::none, Answers::recovery_key);

    util::step(&format!("Booting {}", machine.name()));
    let shell = Shell::boot(
        &machine,
        &answers,
        &DEBUG_CONSOLE_CREDENTIALS,
        recovery_key.is_some(),
    )?;

    util::step("Running the commands");
    for (number, command) in commands.iter().enumerate() {
        if let Some(seconds) = command.strip_prefix("@wait ") {
            let seconds: f64 = seconds.trim().parse()?;
            shell.wait_for(Duration::from_secs_f64(seconds), true, |_| false)?;
            continue;
        }
        let tag = number.to_string();
        shell
            .session
            .type_line(&format!("{command}; {}", marker_command(&tag)));
        let done = format!("{MARKER}{tag}");
        if !shell.wait_for(COMMAND_TIMEOUT, true, |line| line == done)? {
            say!(
                "  warning: `{command}` did not finish within {}s",
                COMMAND_TIMEOUT.as_secs()
            );
        }
    }

    // `exec`: no interactive shell is left on the console while systemd
    // stops its getty (it ignores SIGTERM). Once a power-off hung there for
    // over a minute; not reproduced, the cause is unproven.
    shell.session.type_line("exec systemctl poweroff");
    if !shell.session.power_off_within(POWER_OFF_TIMEOUT)? {
        say!("  warning: the machine did not power off, it was stopped");
    }
    Ok(())
}

/// The command that makes the shell print the marker with `tag`.
fn marker_command(tag: &str) -> String {
    format!("printf '{MARKER}%s\\n' {tag}")
}

/// The shell on the serial console of a booting machine: root's for `bz
/// debug`, the owner's in the smoke test (by the credentials it boots with).
pub(super) struct Shell {
    session: Session,
    key_given: bool,
}

impl Shell {
    /// Boots `machine` with `credentials` that log a user in on the serial
    /// console, and waits until the shell answers.
    pub(super) fn boot(
        machine: &Machine,
        answers: &Answers,
        credentials: &[&str],
        key_given: bool,
    ) -> Result<Self> {
        let shell = Self {
            session: Session::start(machine, answers, credentials)?,
            key_given,
        };
        ensure!(
            shell.wait_for(REPORT_TIMEOUT, false, |line| line.contains(BOOTED))?,
            "the system did not finish booting within {}s",
            REPORT_TIMEOUT.as_secs()
        );
        shell.wait_until_ready()?;
        // The console is a terminal, so journalctl, systemctl and others
        // would wait in less for a key; bz reads their output instead.
        let (status, lines) = shell.capture("export SYSTEMD_PAGER=cat PAGER=cat")?;
        ensure!(
            status == 0,
            "could not turn off the pager (exit status {status}): {lines:?}"
        );
        Ok(shell)
    }

    /// Runs `command` and returns its exit status and the console lines it
    /// printed (without the echo of the command line itself).
    pub(super) fn capture(&self, command: &str) -> Result<(i32, Vec<String>)> {
        self.session
            .type_line(&format!("{command}; {}", marker_command("rc$?")));
        let done = format!("{MARKER}rc");
        let echo = format!("; printf '{MARKER}");
        let deadline = Instant::now() + COMMAND_TIMEOUT;
        let mut lines = Vec::new();
        while let Some(event) = self.session.next(deadline) {
            let ConsoleEvent::Line(line) = event else {
                continue;
            };
            let line = strip_ansi(line.rsplit('\r').next().unwrap_or_default());
            if let Some(status) = line.strip_prefix(&done) {
                return Ok((status.parse()?, lines));
            }
            if !line.contains(&echo) {
                lines.push(line);
            }
        }
        bail!(
            "`{command}` did not finish within {}s",
            COMMAND_TIMEOUT.as_secs()
        )
    }

    /// Stops the machine (the test needs no clean power-off).
    pub(super) fn stop(self) -> Result<()> {
        self.session.power_off_within(Duration::ZERO)?;
        Ok(())
    }

    /// Powers the machine off cleanly, so everything written reaches the disk.
    pub(super) fn power_off(self) -> Result<()> {
        // `exec`: see `debug`.
        self.session.type_line("exec systemctl poweroff");
        ensure!(
            self.session.power_off_within(POWER_OFF_TIMEOUT)?,
            "the machine did not power off within {}s",
            POWER_OFF_TIMEOUT.as_secs()
        );
        Ok(())
    }
}

impl Shell {
    /// Reads console lines until one matches `wanted` (true) or `timeout`
    /// passes (false), printing them to stdout if `print`.
    fn wait_for(
        &self,
        timeout: Duration,
        print: bool,
        wanted: impl Fn(&str) -> bool,
    ) -> Result<bool> {
        let deadline = Instant::now() + timeout;
        while let Some(event) = self.session.next(deadline) {
            match event {
                ConsoleEvent::Line(line) => {
                    // The shell redraws its prompt after a carriage return:
                    // keep what a terminal would show.
                    let line = strip_ansi(line.rsplit('\r').next().unwrap_or_default());
                    if wanted(&line) {
                        return Ok(true);
                    }
                    if print && !line.starts_with(MARKER) {
                        // The echo of a typed command, without the marker.
                        let shown = line
                            .find(&format!("; printf '{MARKER}"))
                            .map_or(line.as_str(), |end| &line[..end]);
                        println!("{shown}");
                    }
                }
                ConsoleEvent::Prompted if !self.key_given => {
                    bail!("the boot asks for the recovery key; pass --recovery-key")
                }
                ConsoleEvent::Prompted => {}
            }
        }
        Ok(false)
    }

    /// Types the marker until the shell answers: getty starts a moment after
    /// the boot, and what is typed before is lost.
    fn wait_until_ready(&self) -> Result<()> {
        let deadline = Instant::now() + SHELL_TIMEOUT;
        let ready = format!("{MARKER}ready");
        while Instant::now() < deadline {
            self.session.type_line(&marker_command("ready"));
            if self.wait_for(Duration::from_secs(2), false, |line| line == ready)? {
                return Ok(());
            }
        }
        bail!(
            "the root shell on the serial console did not answer within {}s",
            SHELL_TIMEOUT.as_secs()
        )
    }
}
