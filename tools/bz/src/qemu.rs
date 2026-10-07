//! Booting the disk image in QEMU: interactively (`bz run`) and as the
//! automated end-to-end test (`bz smoke`).

use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail, ensure};

use crate::layout::{self, MIB};
use crate::paths::Paths;
use crate::{image, util};

/// OVMF build that enforces Secure Boot (requires SMM).
const OVMF_CODE: &str = "/usr/share/OVMF/OVMF_CODE_4M.secboot.fd";

/// Size of the virtual disk the image is installed on for tests: the shipped
/// image is minimal, first boot grows into the remaining space.
const TEST_DISK_SIZE: u64 = 8192 * MIB;

/// Printed by /usr/lib/beize/boot-report once systemd has finished booting.
const REPORT_MARKER: &str = "BEIZE-BOOT-REPORT ";
const SMOKE_TIMEOUT: Duration = Duration::from_secs(120);
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
    /// How often this installation has booted, counted on the root partition.
    boot: u32,
    machine_id: String,
    uptime: String,
    failed: Vec<String>,
}

impl BootReport {
    /// Parses `BEIZE-BOOT-REPORT state=running secureboot=on boot=1 machineid=… uptime=2.95s failed=none`.
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
            boot: field("boot")?.parse().ok()?,
            machine_id: field("machineid")?,
            uptime: field("uptime")?,
            failed: match failed.as_str() {
                "none" => Vec::new(),
                list => list.split(',').map(str::to_owned).collect(),
            },
        })
    }
}

/// A virtual machine: a disk and its own firmware variable store.
struct Machine {
    disk: PathBuf,
    vars: PathBuf,
}

impl Machine {
    /// Installs the current image onto a fresh test disk (`name.img`).
    fn install(paths: &Paths, name: &str) -> Result<Self> {
        let machine = Self {
            disk: paths.work.join(format!("{name}.img")),
            vars: paths.work.join(format!("{name}-vars.fd")),
        };
        let image = paths.out.join("disk.img");
        let template = paths.out.join("ovmf-vars-secureboot.fd");
        fs::copy(&image, &machine.disk)
            .with_context(|| format!("copying {}; run `bz build`", image.display()))?;
        fs::OpenOptions::new()
            .write(true)
            .open(&machine.disk)?
            .set_len(TEST_DISK_SIZE)?;
        // Like any install onto a larger disk: the backup GPT must move to the
        // real end of the disk, or firmware rejects the partition table.
        util::output(
            Command::new("sfdisk")
                .args(["--relocate", "gpt-bak-std"])
                .arg(&machine.disk),
        )?;
        fs::copy(&template, &machine.vars)
            .with_context(|| format!("copying {}; run `bz image`", template.display()))?;
        Ok(machine)
    }

    fn remove(self) -> Result<()> {
        fs::remove_file(&self.disk)?;
        fs::remove_file(&self.vars)?;
        Ok(())
    }
}

/// Boots the image with the serial console attached to this terminal.
/// The VM keeps its disk between runs until a new image is built.
/// Quit QEMU with Ctrl-A X.
pub fn run(paths: &Paths) -> Result<()> {
    let image = paths.out.join("disk.img");
    let machine = Machine {
        disk: paths.work.join("run.img"),
        vars: paths.work.join("run-vars.fd"),
    };
    let outdated = |p: &Path| -> Result<bool> {
        Ok(!p.exists() || fs::metadata(p)?.modified()? < fs::metadata(&image)?.modified()?)
    };
    if outdated(&machine.disk)? || outdated(&machine.vars)? {
        util::step("Installing the current image onto a new VM disk");
        Machine::install(paths, "run")?;
    }
    util::step("Booting in QEMU (quit with Ctrl-A X)");
    util::run(qemu(&machine)?.args(["-nographic", "-serial", "mon:stdio"]))
}

/// Verifies the image end to end on a fresh test disk:
/// 1. first boot under enforced Secure Boot creates `/usr` slot B and the
///    root partition, reaches `running` without failed units and marks the
///    UKI as good (boot counting);
/// 2. the second boot finds the same installation (state persisted);
/// 3. the image with an unsigned kernel image is refused by the firmware.
pub fn smoke(paths: &Paths) -> Result<()> {
    let machine = Machine::install(paths, "smoke")?;

    util::step("First boot");
    let first = boot_healthy(&machine, 1)?;
    check_partitions(&machine.disk)?;
    check_blessed(&machine.disk)?;

    util::step("Second boot");
    let second = boot_healthy(&machine, 2)?;
    ensure!(
        second.machine_id == first.machine_id,
        "machine ID changed between boots ({} -> {}): the root partition did not persist",
        first.machine_id,
        second.machine_id
    );
    eprintln!(
        "  ok: same installation after reboot (machine ID {})",
        second.machine_id
    );
    machine.remove()?;

    util::step("Secure Boot must refuse an unsigned kernel image");
    let machine = Machine::install(paths, "tampered")?;
    util::run(
        Command::new("mcopy")
            .arg("-o")
            .arg("-i")
            .arg(format!("{}@@{}", machine.disk.display(), image::ESP_OFFSET))
            .arg(image::unsigned_uki(paths))
            .arg(image::uki_path_on_esp()),
    )?;
    let run = watch_boot(&machine, TAMPER_TIMEOUT, |line| {
        if line.contains(REPORT_MARKER) || line.contains("Welcome to") {
            Some(false)
        } else if line.contains("Access Denied") || line.contains("Access denied") {
            Some(true)
        } else {
            None
        }
    });
    machine.remove()?;
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

/// Boots once and requires a fully healthy system: Secure Boot on, state
/// `running`, no failed units, clean power-off, and the expected boot count.
fn boot_healthy(machine: &Machine, expected_boot: u32) -> Result<BootReport> {
    let run = watch_boot(machine, SMOKE_TIMEOUT, BootReport::parse)?;
    let Some((report, elapsed)) = run.answer else {
        bail!(
            "the system did not report within {}s",
            SMOKE_TIMEOUT.as_secs()
        );
    };
    ensure!(
        report.secure_boot == "on",
        "the system booted without Secure Boot (secureboot={})",
        report.secure_boot
    );
    ensure!(
        report.state == "running" && report.failed.is_empty(),
        "boot finished in state {:?}; failed units: {}",
        report.state,
        report.failed.join(", ")
    );
    ensure!(
        run.failures.is_empty(),
        "units failed during boot (initrd or system):\n  {}",
        run.failures.join("\n  ")
    );
    ensure!(
        run.powered_off,
        "the system did not power off cleanly after reporting"
    );
    ensure!(
        report.boot == expected_boot,
        "expected boot #{expected_boot}, the system reports boot #{}",
        report.boot
    );
    eprintln!(
        "\n  ok: boot #{} running under Secure Boot after {} guest uptime \
         ({:.1}s including firmware), no failed units, clean power-off",
        report.boot,
        report.uptime,
        elapsed.as_secs_f64()
    );
    Ok(report)
}

/// After first boot the disk must hold both `/usr` slots and the root partition.
fn check_partitions(disk: &Path) -> Result<()> {
    let dump = util::output(Command::new("sfdisk").arg("--dump").arg(disk))?;
    let missing = missing_partitions(&dump);
    ensure!(
        missing.is_empty(),
        "first boot did not create the expected partitions; missing: {}\n{dump}",
        missing.join(", ")
    );
    eprintln!("  ok: first boot created /usr slot B and the root partition");
    Ok(())
}

fn missing_partitions(sfdisk_dump: &str) -> Vec<String> {
    [
        "esp".to_owned(),
        layout::usr_label(),
        "_empty".to_owned(),
        "root".to_owned(),
    ]
    .into_iter()
    .filter(|name| !sfdisk_dump.contains(&format!("name=\"{name}\"")))
    .collect()
}

/// systemd-bless-boot must have dropped the boot counter from the UKI name.
fn check_blessed(disk: &Path) -> Result<()> {
    let listing = util::output(
        Command::new("mdir")
            .arg("-b")
            .arg("-i")
            .arg(format!("{}@@{}", disk.display(), image::ESP_OFFSET))
            .arg("::/EFI/Linux"),
    )?;
    let blessed = format!("/{}", layout::blessed_uki_name());
    ensure!(
        listing.lines().any(|line| line.ends_with(&blessed)),
        "the UKI was not marked as good after a successful boot; ESP holds:\n{listing}"
    );
    eprintln!(
        "  ok: boot counting marked {} as good",
        layout::blessed_uki_name()
    );
    Ok(())
}

/// Outcome of a headless boot.
struct BootRun<T> {
    /// First answer of the verdict function and the time it took to get there.
    answer: Option<(T, Duration)>,
    /// Whether the guest shut down by itself (instead of being stopped).
    powered_off: bool,
    /// Console lines reporting failed units, from the initrd and the system.
    failures: Vec<String>,
}

/// Boots headless and feeds every console line to `verdict` until it
/// answers or `timeout` passes. QEMU is always stopped before returning.
fn watch_boot<T: Send + 'static>(
    machine: &Machine,
    timeout: Duration,
    verdict: impl Fn(&str) -> Option<T> + Send + 'static,
) -> Result<BootRun<T>> {
    let started = Instant::now();
    let mut child = qemu(machine)?
        .args(["-display", "none", "-serial", "stdio", "-monitor", "none"])
        .args(["-smbios", SMOKE_CREDENTIAL])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .spawn()
        .context("starting qemu")?;

    let stdout = child.stdout.take().context("qemu stdout")?;
    let (tx, rx) = mpsc::channel();
    let reader = thread::spawn(move || {
        let mut failures = Vec::new();
        let mut answered = false;
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            eprintln!("  | {line}");
            if is_unit_failure(&line) {
                failures.push(strip_ansi(&line));
            }
            if !answered && let Some(answer) = verdict(&line) {
                answered = true;
                let _ = tx.send(answer);
            }
        }
        failures
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
    let failures = reader.join().unwrap_or_default();
    Ok(BootRun {
        answer,
        powered_off,
        failures,
    })
}

/// systemd's console status lines for failed units and failed dependencies.
fn is_unit_failure(line: &str) -> bool {
    let line = strip_ansi(line);
    line.contains("[FAILED]") || line.contains("[DEPEND]")
}

fn strip_ansi(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '' && chars.peek() == Some(&'[') {
            chars.next();
            for c in chars.by_ref() {
                if c.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out.trim().to_owned()
}

fn qemu(machine: &Machine) -> Result<Command> {
    if !Path::new(OVMF_CODE).exists() {
        bail!("{OVMF_CODE} is missing; install the `ovmf` package");
    }
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
            machine.vars.display()
        ))
        .arg("-drive")
        .arg(format!(
            "if=virtio,format=raw,file={}",
            machine.disk.display()
        ));
    Ok(cmd)
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

    const CLEAN: &str = "BEIZE-BOOT-REPORT state=running secureboot=on boot=1 \
                         machineid=0123456789abcdef0123456789abcdef uptime=2.95s failed=none";

    #[test]
    fn parses_clean_report() {
        let report = BootReport::parse(CLEAN).unwrap();
        assert_eq!(report.state, "running");
        assert_eq!(report.secure_boot, "on");
        assert_eq!(report.boot, 1);
        assert_eq!(report.machine_id, "0123456789abcdef0123456789abcdef");
        assert_eq!(report.uptime, "2.95s");
        assert!(report.failed.is_empty());
    }

    #[test]
    fn parses_degraded_report_behind_console_noise() {
        let line = "\x1b[0m[  3.1] BEIZE-BOOT-REPORT state=degraded secureboot=off boot=2 \
                    machineid=x uptime=3.1s failed=systemd-logind.service,systemd-logind-varlink.socket";
        let report = BootReport::parse(line).unwrap();
        assert_eq!(report.state, "degraded");
        assert_eq!(report.secure_boot, "off");
        assert_eq!(report.boot, 2);
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
        assert_eq!(BootReport::parse(&CLEAN.replace("boot=1", "boot=x")), None);
    }

    #[test]
    fn recognises_failed_units_on_the_console() {
        assert!(is_unit_failure(
            "[0;1;31mFAILED[0m] Failed to listen on Journal Log Access Socket."
                .replace("[0;1;31mFAILED", "[[0;1;31mFAILED")
                .as_str()
        ));
        assert!(is_unit_failure(
            "[DEPEND] Dependency failed for Kernel Core Pattern."
        ));
        assert!(!is_unit_failure("[  OK  ] Reached target Basic System."));
        assert_eq!(strip_ansi("[[0;32m  OK  [0m] x"), "[  OK  ] x");
    }

    #[test]
    fn detects_missing_partitions() {
        let usr = layout::usr_label();
        let after_first_boot = format!(
            "label: gpt\n\
             disk.img1 : start=2048, size=524288, type=C12A7328-…, name=\"esp\"\n\
             disk.img2 : start=526336, size=2097152, type=8484680C-…, name=\"{usr}\"\n\
             disk.img3 : start=2623488, size=2097152, type=8484680C-…, name=\"_empty\"\n\
             disk.img4 : start=4720640, size=12056576, type=4F68BCE3-…, name=\"root\"\n"
        );
        assert!(missing_partitions(&after_first_boot).is_empty());
        let shipped = after_first_boot
            .lines()
            .filter(|l| !l.contains("_empty") && !l.contains("\"root\""))
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(missing_partitions(&shipped), ["_empty", "root"]);
    }
}
