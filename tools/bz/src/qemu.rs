use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

use crate::paths::Paths;
use crate::util;

const OVMF_CODE: &str = "/usr/share/OVMF/OVMF_CODE_4M.fd";
const OVMF_VARS: &str = "/usr/share/OVMF/OVMF_VARS_4M.fd";

/// Printed by /usr/lib/beize/boot-report once systemd has finished booting.
const REPORT_MARKER: &str = "BEIZE-BOOT-REPORT ";
const SMOKE_TIMEOUT: Duration = Duration::from_secs(90);
/// systemd-stub appends this to the kernel command line (SMBIOS type 11, VMs
/// only, ignored under Secure Boot); boot-report powers off when it sees it.
const SMOKE_CMDLINE: &str = "type=11,value=io.systemd.stub.kernel-cmdline-extra=beize.smoke";

/// The boot outcome as reported by the guest.
#[derive(Debug, PartialEq)]
struct BootReport {
    state: String,
    uptime: String,
    failed: Vec<String>,
}

impl BootReport {
    /// Parses `BEIZE-BOOT-REPORT state=running uptime=2.95s failed=none`.
    fn parse(line: &str) -> Option<Self> {
        let fields = &line[line.find(REPORT_MARKER)? + REPORT_MARKER.len()..];
        let field = |key: &str| {
            fields
                .split_whitespace()
                .find_map(|f| f.strip_prefix(key)?.strip_prefix('='))
                .map(str::to_owned)
        };
        let failed = field("failed")?;
        Some(Self {
            state: field("state")?,
            uptime: field("uptime")?,
            failed: match failed.as_str() {
                "none" => Vec::new(),
                list => list.split(',').map(str::to_owned).collect(),
            },
        })
    }
}

/// Boots the image with the serial console attached to this terminal.
/// Quit QEMU with Ctrl-A X.
pub fn run(paths: &Paths) -> Result<()> {
    util::step("Booting in QEMU (quit with Ctrl-A X)");
    util::run(qemu(paths)?.args(["-nographic", "-serial", "mon:stdio"]))
}

/// Boots the image headless and succeeds if systemd reaches `running`
/// without a single failed unit.
pub fn smoke(paths: &Paths) -> Result<()> {
    util::step("Boot smoke test");
    let mut child = qemu(paths)?
        .args(["-display", "none", "-serial", "stdio", "-monitor", "none"])
        .args(["-smbios", SMOKE_CMDLINE])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .spawn()
        .context("starting qemu")?;

    let stdout = child.stdout.take().context("qemu stdout")?;
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            eprintln!("  | {line}");
            if let Some(report) = BootReport::parse(&line)
                && tx.send(report).is_err()
            {
                break;
            }
        }
    });

    let started = Instant::now();
    let report = rx.recv_timeout(SMOKE_TIMEOUT).ok();
    let elapsed = started.elapsed();
    // The guest powers off right after reporting; give QEMU a moment, then make sure it is gone.
    let deadline = Instant::now() + Duration::from_secs(10);
    while child.try_wait()?.is_none() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(100));
    }
    if child.try_wait()?.is_none() {
        child.kill()?;
        child.wait()?;
    }

    let Some(report) = report else {
        bail!(
            "the system did not report within {}s",
            SMOKE_TIMEOUT.as_secs()
        );
    };
    if report.state != "running" || !report.failed.is_empty() {
        bail!(
            "boot finished in state {:?}; failed units: {}",
            report.state,
            report.failed.join(", ")
        );
    }
    eprintln!(
        "\n  ok: system running after {} guest uptime ({:.1}s including firmware), no failed units",
        report.uptime,
        elapsed.as_secs_f64()
    );
    Ok(())
}

fn qemu(paths: &Paths) -> Result<Command> {
    let disk = paths.out.join("disk.img");
    if !disk.exists() {
        bail!("{} is missing; run `bz build`", disk.display());
    }
    for firmware in [OVMF_CODE, OVMF_VARS] {
        if !Path::new(firmware).exists() {
            bail!("{firmware} is missing; install the `ovmf` package");
        }
    }
    let vars = fresh_vars(paths)?;

    let mut cmd = Command::new("qemu-system-x86_64");
    if kvm_available() {
        cmd.args(["-machine", "q35,accel=kvm", "-cpu", "host"]);
    } else {
        eprintln!("  warning: /dev/kvm not accessible, falling back to slow emulation");
        cmd.args(["-machine", "q35,accel=tcg", "-cpu", "max"]);
    }
    cmd.args(["-smp", "4", "-m", "2G", "-no-reboot"])
        .arg("-drive")
        .arg(format!(
            "if=pflash,format=raw,unit=0,readonly=on,file={OVMF_CODE}"
        ))
        .arg("-drive")
        .arg(format!(
            "if=pflash,format=raw,unit=1,file={}",
            vars.display()
        ))
        .arg("-drive")
        .arg(format!("if=virtio,format=raw,file={}", disk.display()));
    Ok(cmd)
}

/// Every boot starts from pristine firmware variables.
fn fresh_vars(paths: &Paths) -> Result<PathBuf> {
    let vars = paths.work.join("ovmf-vars.fd");
    fs::copy(OVMF_VARS, &vars).with_context(|| format!("copying {OVMF_VARS}"))?;
    Ok(vars)
}

fn kvm_available() -> bool {
    fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/kvm")
        .is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_clean_report() {
        let report =
            BootReport::parse("BEIZE-BOOT-REPORT state=running uptime=2.95s failed=none").unwrap();
        assert_eq!(report.state, "running");
        assert_eq!(report.uptime, "2.95s");
        assert!(report.failed.is_empty());
    }

    #[test]
    fn parses_degraded_report_behind_console_noise() {
        let line = "\x1b[0m[  3.1] BEIZE-BOOT-REPORT state=degraded uptime=3.1s \
                    failed=systemd-logind.service,systemd-logind-varlink.socket";
        let report = BootReport::parse(line).unwrap();
        assert_eq!(report.state, "degraded");
        assert_eq!(
            report.failed,
            ["systemd-logind.service", "systemd-logind-varlink.socket"]
        );
    }

    #[test]
    fn ignores_other_lines_and_incomplete_reports() {
        assert_eq!(
            BootReport::parse("[  OK  ] Reached target Basic System."),
            None
        );
        assert_eq!(BootReport::parse("BEIZE-BOOT-REPORT state=running"), None);
    }
}
