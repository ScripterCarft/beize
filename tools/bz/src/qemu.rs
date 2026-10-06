use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

use crate::paths::Paths;
use crate::{image, util};

/// OVMF build that enforces Secure Boot (requires SMM).
const OVMF_CODE: &str = "/usr/share/OVMF/OVMF_CODE_4M.secboot.fd";

/// Printed by /usr/lib/beize/boot-report once systemd has finished booting.
const REPORT_MARKER: &str = "BEIZE-BOOT-REPORT ";
const SMOKE_TIMEOUT: Duration = Duration::from_secs(90);
/// The firmware refuses a bad image within seconds; this only bounds a hang.
const TAMPER_TIMEOUT: Duration = Duration::from_secs(45);
/// A systemd credential passed via SMBIOS (honoured in VMs, also under
/// Secure Boot); boot-report powers off when it is present.
const SMOKE_CREDENTIAL: &str = "type=11,value=io.systemd.credential:beize.smoke=1";

/// The boot outcome as reported by the guest.
#[derive(Debug, PartialEq)]
struct BootReport {
    state: String,
    secure_boot: String,
    uptime: String,
    failed: Vec<String>,
}

impl BootReport {
    /// Parses `BEIZE-BOOT-REPORT state=running secureboot=on uptime=2.95s failed=none`.
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
            secure_boot: field("secureboot")?,
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
    util::run(qemu(paths, &paths.out.join("disk.img"))?.args([
        "-nographic",
        "-serial",
        "mon:stdio",
    ]))
}

/// Verifies the image end to end:
/// 1. it boots under enforced Secure Boot and systemd reaches `running`
///    without a single failed unit;
/// 2. the same disk with an unsigned kernel image is refused by the firmware.
pub fn smoke(paths: &Paths) -> Result<()> {
    util::step("Boot smoke test");
    let run = watch_boot(paths, &paths.out.join("disk.img"), SMOKE_TIMEOUT, |line| {
        BootReport::parse(line)
    })?;
    let Some((report, elapsed)) = run.answer else {
        bail!(
            "the system did not report within {}s",
            SMOKE_TIMEOUT.as_secs()
        );
    };

    if report.secure_boot != "on" {
        bail!(
            "the system booted without Secure Boot (secureboot={})",
            report.secure_boot
        );
    }
    if report.state != "running" || !report.failed.is_empty() {
        bail!(
            "boot finished in state {:?}; failed units: {}",
            report.state,
            report.failed.join(", ")
        );
    }
    if !run.powered_off {
        bail!("the system did not power off cleanly after reporting");
    }
    eprintln!(
        "\n  ok: system running under Secure Boot after {} guest uptime \
         ({:.1}s including firmware), no failed units, clean power-off",
        report.uptime,
        elapsed.as_secs_f64()
    );

    util::step("Secure Boot must refuse an unsigned kernel image");
    let tampered = paths.work.join("tampered.img");
    fs::copy(paths.out.join("disk.img"), &tampered)?;
    util::run(
        Command::new("mcopy")
            .arg("-o")
            .arg("-i")
            .arg(format!("{}@@{}", tampered.display(), image::ESP_OFFSET))
            .arg(image::unsigned_uki(paths))
            .arg(image::UKI_PATH_ON_ESP),
    )?;
    let run = watch_boot(paths, &tampered, TAMPER_TIMEOUT, |line| {
        if line.contains(REPORT_MARKER) || line.contains("Welcome to") {
            Some(false)
        } else if line.contains("Access Denied") || line.contains("Access denied") {
            Some(true)
        } else {
            None
        }
    });
    fs::remove_file(&tampered)?;
    match run?.answer {
        Some((true, _)) => eprintln!("\n  ok: the unsigned kernel image was refused"),
        Some((false, _)) => bail!("an unsigned kernel image booted under Secure Boot"),
        None => bail!(
            "no verdict on the unsigned kernel image within {}s",
            TAMPER_TIMEOUT.as_secs()
        ),
    }
    Ok(())
}

/// Outcome of a headless boot.
struct BootRun<T> {
    /// First answer of the verdict function and the time it took to get there.
    answer: Option<(T, Duration)>,
    /// Whether the guest shut down by itself (instead of being stopped).
    powered_off: bool,
}

/// Boots `disk` headless and feeds every console line to `verdict` until it
/// answers or `timeout` passes. QEMU is always stopped before returning.
fn watch_boot<T: Send + 'static>(
    paths: &Paths,
    disk: &Path,
    timeout: Duration,
    verdict: impl Fn(&str) -> Option<T> + Send + 'static,
) -> Result<BootRun<T>> {
    let started = Instant::now();
    let mut child = qemu(paths, disk)?
        .args(["-display", "none", "-serial", "stdio", "-monitor", "none"])
        .args(["-smbios", SMOKE_CREDENTIAL])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .spawn()
        .context("starting qemu")?;

    let stdout = child.stdout.take().context("qemu stdout")?;
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            eprintln!("  | {line}");
            if let Some(answer) = verdict(&line)
                && tx.send(answer).is_err()
            {
                break;
            }
        }
    });

    let answer = rx
        .recv_timeout(timeout)
        .ok()
        .map(|answer| (answer, started.elapsed()));
    // A healthy guest powers off right after reporting; give it a moment.
    let deadline = Instant::now() + Duration::from_secs(10);
    while child.try_wait()?.is_none() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(100));
    }
    let powered_off = child.try_wait()?.is_some();
    if !powered_off {
        child.kill()?;
        child.wait()?;
    }
    Ok(BootRun {
        answer,
        powered_off,
    })
}

fn qemu(paths: &Paths, disk: &Path) -> Result<Command> {
    if !disk.exists() {
        bail!("{} is missing; run `bz build`", disk.display());
    }
    if !Path::new(OVMF_CODE).exists() {
        bail!("{OVMF_CODE} is missing; install the `ovmf` package");
    }
    let vars = fresh_vars(paths)?;

    let mut cmd = Command::new("qemu-system-x86_64");
    // Secure Boot firmware keeps its variables in SMM-protected flash.
    if kvm_available() {
        cmd.args(["-machine", "q35,smm=on,accel=kvm", "-cpu", "host"]);
    } else {
        eprintln!("  warning: /dev/kvm not accessible, falling back to slow emulation");
        cmd.args(["-machine", "q35,smm=on,accel=tcg", "-cpu", "max"]);
    }
    cmd.args(["-global", "driver=cfi.pflash01,property=secure,value=on"])
        .args(["-smp", "4", "-m", "2G", "-no-reboot"])
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

/// Every boot starts from the variable store written by `bz image`: the test
/// keys enrolled and Secure Boot enabled.
fn fresh_vars(paths: &Paths) -> Result<PathBuf> {
    let template = paths.out.join("ovmf-vars-secureboot.fd");
    let vars = paths.work.join("ovmf-vars.fd");
    fs::copy(&template, &vars)
        .with_context(|| format!("copying {}; run `bz image`", template.display()))?;
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
        let report = BootReport::parse(
            "BEIZE-BOOT-REPORT state=running secureboot=on uptime=2.95s failed=none",
        )
        .unwrap();
        assert_eq!(report.state, "running");
        assert_eq!(report.secure_boot, "on");
        assert_eq!(report.uptime, "2.95s");
        assert!(report.failed.is_empty());
    }

    #[test]
    fn parses_degraded_report_behind_console_noise() {
        let line = "\x1b[0m[  3.1] BEIZE-BOOT-REPORT state=degraded secureboot=off uptime=3.1s \
                    failed=systemd-logind.service,systemd-logind-varlink.socket";
        let report = BootReport::parse(line).unwrap();
        assert_eq!(report.state, "degraded");
        assert_eq!(report.secure_boot, "off");
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
