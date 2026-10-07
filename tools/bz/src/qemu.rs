//! Booting the disk image in QEMU: interactively (`bz run`) and as the
//! automated end-to-end test (`bz smoke`).

use std::fs;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail, ensure};

use crate::layout::{self, MIB};
use crate::paths::Paths;
use crate::{image, secureboot, util};

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
    /// `verity` if /usr is mounted from a dm-verity device.
    usr: String,
    /// `encrypted` if / is mounted from a LUKS2 device.
    root: String,
    /// How often this installation has booted, counted on the root partition.
    boot: u32,
    machine_id: String,
    uptime: String,
    failed: Vec<String>,
}

impl BootReport {
    /// Parses `BEIZE-BOOT-REPORT state=running secureboot=on usr=verity root=encrypted boot=1 machineid=… uptime=2.95s failed=none`.
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
            usr: field("usr")?,
            root: field("root")?,
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

/// A virtual machine: a disk, its own firmware variable store and its own
/// TPM (the state directory of an swtpm instance).
#[derive(Clone)]
struct Machine {
    disk: PathBuf,
    vars: PathBuf,
    tpm: PathBuf,
}

impl Machine {
    fn named(paths: &Paths, name: &str) -> Self {
        Self {
            disk: paths.work.join(format!("{name}.img")),
            vars: paths.work.join(format!("{name}-vars.fd")),
            tpm: paths.work.join(format!("{name}-tpm")),
        }
    }

    /// Installs the current image onto a fresh test disk (`name.img`), with
    /// a factory-new TPM.
    fn install(paths: &Paths, name: &str) -> Result<Self> {
        let machine = Self::named(paths, name);
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
        new_tpm(&machine.tpm)?;
        Ok(machine)
    }

    fn remove(self) -> Result<()> {
        fs::remove_file(&self.disk)?;
        fs::remove_file(&self.vars)?;
        fs::remove_dir_all(&self.tpm)?;
        Ok(())
    }
}

/// Creates an empty TPM state directory; swtpm initialises a factory-new TPM in it.
fn new_tpm(dir: &Path) -> Result<()> {
    if dir.exists() {
        fs::remove_dir_all(dir)?;
    }
    fs::create_dir_all(dir)?;
    Ok(())
}

/// A running software TPM, stopped when dropped. It also ends by itself as
/// soon as QEMU closes the control connection.
struct Swtpm {
    child: Child,
    socket: PathBuf,
}

impl Swtpm {
    fn start(state: &Path) -> Result<Self> {
        let socket = state.join("swtpm.sock");
        let _ = fs::remove_file(&socket);
        let child = Command::new("swtpm")
            .args(["socket", "--tpm2", "--tpmstate"])
            .arg(format!("dir={}", state.display()))
            .arg("--ctrl")
            .arg(format!("type=unixio,path={},terminate", socket.display()))
            .arg("--log")
            .arg(format!("file={}", state.join("swtpm.log").display()))
            .stdin(Stdio::null())
            .spawn()
            .context("starting swtpm; install the `swtpm` package")?;
        let mut tpm = Self { child, socket };
        let deadline = Instant::now() + Duration::from_secs(5);
        while !tpm.socket.exists() {
            if let Some(status) = tpm.child.try_wait()? {
                bail!(
                    "swtpm exited with {status}; see {}/swtpm.log",
                    state.display()
                );
            }
            ensure!(
                Instant::now() < deadline,
                "swtpm did not open {}",
                tpm.socket.display()
            );
            thread::sleep(Duration::from_millis(20));
        }
        Ok(tpm)
    }

    /// QEMU arguments attaching this TPM as a CRB device (like Intel PTT).
    fn qemu_args(&self) -> Vec<String> {
        vec![
            "-chardev".into(),
            format!("socket,id=chrtpm,path={}", self.socket.display()),
            "-tpmdev".into(),
            "emulator,id=tpm0,chardev=chrtpm".into(),
            "-device".into(),
            "tpm-crb,tpmdev=tpm0".into(),
        ]
    }
}

impl Drop for Swtpm {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Boots the image with the serial console attached to this terminal.
/// The VM keeps its disk between runs until a new image is built.
/// Quit QEMU with Ctrl-A X.
pub fn run(paths: &Paths) -> Result<()> {
    let image = paths.out.join("disk.img");
    let machine = Machine::named(paths, "run");
    let outdated = |p: &Path| -> Result<bool> {
        Ok(!p.exists() || fs::metadata(p)?.modified()? < fs::metadata(&image)?.modified()?)
    };
    if outdated(&machine.disk)? || outdated(&machine.vars)? || !machine.tpm.exists() {
        util::step("Installing the current image onto a new VM disk");
        Machine::install(paths, "run")?;
    }
    util::step("Booting in QEMU (quit with Ctrl-A X)");
    let tpm = Swtpm::start(&machine.tpm)?;
    util::run(qemu(&machine, &tpm)?.args(["-nographic", "-serial", "mon:stdio"]))
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
    let first = boot_healthy(&machine, 1, None)?;
    check_partitions(&machine.disk)?;
    check_blessed(&machine.disk)?;
    let recovery_key = find_recovery_key(&first.console)
        .context("first boot did not show a recovery key for the root partition")?;
    eprintln!("  ok: first boot created and showed a recovery key");

    util::step("Second boot");
    let second = boot_healthy(&machine, 2, None)?;
    ensure!(
        second.report.machine_id == first.report.machine_id,
        "machine ID changed between boots ({} -> {}): the root partition did not persist",
        first.report.machine_id,
        second.report.machine_id
    );
    eprintln!(
        "  ok: same installation after reboot (machine ID {})",
        second.report.machine_id
    );
    ensure!(
        find_recovery_key(&second.console).is_none(),
        "the recovery key was shown again on the second boot"
    );
    eprintln!("  ok: the recovery key was shown only once");

    util::step("Another TPM must not unlock the root partition, the recovery key must");
    let other_tpm = Machine {
        tpm: paths.work.join("smoke-other-tpm"),
        ..machine.clone()
    };
    new_tpm(&other_tpm.tpm)?;
    let third = boot_healthy(&other_tpm, 3, Some(recovery_key))?;
    ensure!(
        third.prompted,
        "the root partition was unlocked without the recovery key on a different TPM"
    );
    eprintln!("  ok: a different TPM did not unlock the root partition, the recovery key did");
    fs::remove_dir_all(&other_tpm.tpm)?;

    util::step("The root partition must stay locked with Secure Boot disabled");
    let insecure = Machine {
        vars: paths.work.join("smoke-insecure-vars.fd"),
        ..machine.clone()
    };
    fs::copy(secureboot::OVMF_VARS, &insecure.vars)
        .with_context(|| format!("copying {}", secureboot::OVMF_VARS))?;
    expect_root_locked(&insecure, "Secure Boot disabled")?;
    fs::remove_file(&insecure.vars)?;
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
    let run = watch_boot(&machine, TAMPER_TIMEOUT, None, |line| {
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

    util::step("dm-verity must detect a modified /usr");
    let machine = Machine::install(paths, "tampered")?;
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
    let run = watch_boot(&machine, TAMPER_TIMEOUT, None, |line| {
        if line.contains(REPORT_MARKER) || line.contains("Welcome to") {
            Some(false)
        } else if line.contains("verity") && line.contains("is corrupted") {
            Some(true)
        } else {
            None
        }
    });
    machine.remove()?;
    match run?.answer {
        Some((true, _)) => eprintln!("\n  ok: the modified /usr was detected and not used"),
        Some((false, _)) => bail!("the system booted from a modified /usr"),
        None => bail!(
            "no verdict on the modified /usr within {}s",
            TAMPER_TIMEOUT.as_secs()
        ),
    }
    Ok(())
}

/// A healthy boot: its report and what appeared on the console.
struct HealthyBoot {
    report: BootReport,
    console: Vec<String>,
    /// Whether the root partition had to be unlocked with a passphrase.
    prompted: bool,
}

/// Boots once and requires a fully healthy system: Secure Boot on, state
/// `running`, no failed units, clean power-off, and the expected boot count.
/// `passphrase` is typed in if the boot asks for one.
fn boot_healthy(
    machine: &Machine,
    expected_boot: u32,
    passphrase: Option<String>,
) -> Result<HealthyBoot> {
    let run = watch_boot(machine, SMOKE_TIMEOUT, passphrase, BootReport::parse)?;
    ensure!(
        run.answer.is_some() || !run.prompted,
        "the boot asked for a passphrase: the TPM did not unlock the root partition"
    );
    let Some((report, elapsed)) = run.answer else {
        bail!(
            "the system did not report within {}s",
            SMOKE_TIMEOUT.as_secs()
        );
    };
    ensure!(
        report.usr == "verity",
        "/usr is not protected by dm-verity (usr={})",
        report.usr
    );
    ensure!(
        report.root == "encrypted",
        "the root partition is not encrypted (root={})",
        report.root
    );
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
    Ok(HealthyBoot {
        report,
        console: run.console,
        prompted: run.prompted,
    })
}

/// Boots an installed disk under a changed condition (another TPM, other
/// firmware state) and requires that the TPM refuses to unlock the root
/// partition: the boot asks for the recovery key (or, without one, fails to
/// set up the volume) and nothing boots.
fn expect_root_locked(machine: &Machine, condition: &str) -> Result<()> {
    let run = watch_boot(machine, TAMPER_TIMEOUT, None, |line| {
        let line = strip_ansi(line);
        if line.contains(REPORT_MARKER) {
            Some(false)
        } else if line.contains("Failed to start Cryptography Setup for root") {
            Some(true)
        } else {
            None
        }
    })?;
    match run.answer {
        Some((true, _)) => {}
        None if run.prompted => {}
        Some((false, _)) => bail!("with {condition} the root partition was unlocked"),
        None => bail!(
            "no verdict on unlocking the root partition with {condition} within {}s",
            TAMPER_TIMEOUT.as_secs()
        ),
    }
    eprintln!("\n  ok: with {condition} the TPM did not unlock the root partition");
    Ok(())
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

/// The partitions expected after first boot, as (type, label), that the
/// `sfdisk --dump` output lacks.
fn missing_partitions(sfdisk_dump: &str) -> Vec<String> {
    let present: Vec<(String, String)> = sfdisk_dump
        .lines()
        .filter_map(|line| {
            let field = |key: &str| {
                let start = line.find(key)? + key.len();
                let rest = &line[start..];
                let end = rest.find([',', '"']).unwrap_or(rest.len());
                Some(rest[..end].trim().to_uppercase())
            };
            let name = line.split("name=\"").nth(1)?.split('"').next()?.to_owned();
            Some((field("type=")?, name))
        })
        .collect();
    let usr = layout::usr_label();
    [
        ("esp", layout::ESP_TYPE, "esp"),
        ("/usr A", layout::USR_TYPE, usr.as_str()),
        ("/usr A verity", layout::USR_VERITY_TYPE, usr.as_str()),
        ("/usr B", layout::USR_TYPE, "_empty"),
        ("/usr B verity", layout::USR_VERITY_TYPE, "_empty"),
        ("root", layout::ROOT_TYPE, "root"),
    ]
    .into_iter()
    .filter(|(_, ty, name)| !present.contains(&(ty.to_string(), name.to_string())))
    .map(|(what, _, _)| what.to_owned())
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
    /// Whether the boot asked for a passphrase (the TPM did not unlock).
    prompted: bool,
    /// Every console line, without terminal control sequences.
    console: Vec<String>,
}

/// systemd's prompt for a disk passphrase or recovery key ("Please enter
/// recovery key for disk root: ..."). It does not end with a newline, so it
/// is looked for in incomplete lines, too.
fn is_passphrase_prompt(text: &str) -> bool {
    text.contains("Please enter") && text.contains(" for disk ")
}

enum ConsoleEvent<T> {
    Verdict(T),
    Prompted,
}

/// Boots headless and feeds every console line to `verdict` until it
/// answers or `timeout` passes. If the boot asks for a passphrase, `passphrase`
/// is typed in; without one, the prompt ends the boot. QEMU is always stopped
/// before returning.
fn watch_boot<T: Send + 'static>(
    machine: &Machine,
    timeout: Duration,
    passphrase: Option<String>,
    verdict: impl Fn(&str) -> Option<T> + Send + 'static,
) -> Result<BootRun<T>> {
    let tpm = Swtpm::start(&machine.tpm)?;
    let started = Instant::now();
    let mut child = qemu(machine, &tpm)?
        .args(["-display", "none", "-serial", "stdio", "-monitor", "none"])
        .args(["-smbios", SMOKE_CREDENTIAL])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .context("starting qemu")?;

    let mut stdout = child.stdout.take().context("qemu stdout")?;
    let mut stdin = child.stdin.take().context("qemu stdin")?;
    let stop_at_prompt = passphrase.is_none();
    let (tx, rx) = mpsc::channel();
    let reader = thread::spawn(move || {
        let mut failures = Vec::new();
        let mut console = Vec::new();
        let mut answered = false;
        let mut prompted = false;
        let mut pending = Vec::new();
        let mut chunk = [0u8; 4096];
        while let Ok(n @ 1..) = stdout.read(&mut chunk) {
            pending.extend_from_slice(&chunk[..n]);
            while let Some(end) = pending.iter().position(|&b| b == b'\n') {
                let raw: Vec<u8> = pending.drain(..=end).collect();
                let line = String::from_utf8_lossy(&raw);
                let line = line.trim_end_matches(['\r', '\n']);
                eprintln!("  | {line}");
                if is_unit_failure(line) {
                    failures.push(strip_ansi(line));
                }
                if !answered && let Some(answer) = verdict(line) {
                    answered = true;
                    let _ = tx.send(ConsoleEvent::Verdict(answer));
                }
                console.push(strip_ansi(line));
            }
            if !prompted && is_passphrase_prompt(&strip_ansi(&String::from_utf8_lossy(&pending))) {
                prompted = true;
                if let Some(passphrase) = &passphrase {
                    eprintln!("  | (typing the recovery key)");
                    let _ = write!(stdin, "{passphrase}\r");
                    let _ = stdin.flush();
                }
                let _ = tx.send(ConsoleEvent::Prompted);
            }
        }
        (failures, console, prompted)
    });

    let deadline = started + timeout;
    let mut answer = None;
    loop {
        match rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            Ok(ConsoleEvent::Verdict(verdict)) => {
                answer = Some((verdict, started.elapsed()));
                break;
            }
            Ok(ConsoleEvent::Prompted) if stop_at_prompt => break,
            Ok(ConsoleEvent::Prompted) => {}
            Err(_) => break,
        }
    }
    // A healthy guest powers off right after reporting; give it a moment.
    let grace = if answer.is_some() { 10 } else { 0 };
    let deadline = Instant::now() + Duration::from_secs(grace);
    while child.try_wait()?.is_none() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(100));
    }
    let powered_off = child.try_wait()?.is_some();
    if !powered_off {
        child.kill()?;
        child.wait()?;
    }
    let (failures, console, prompted) = reader.join().unwrap_or_default();
    Ok(BootRun {
        answer,
        powered_off,
        failures,
        prompted,
        console,
    })
}

/// A systemd recovery key: 64 characters from the modhex alphabet in eight
/// groups separated by dashes.
fn find_recovery_key(console: &[String]) -> Option<String> {
    const MODHEX: &str = "cbdefghijklnrtuv";
    console
        .iter()
        .flat_map(|line| line.split_whitespace())
        .find(|word| {
            let groups: Vec<&str> = word.split('-').collect();
            groups.len() == 8
                && groups
                    .iter()
                    .all(|g| g.len() == 8 && g.chars().all(|c| MODHEX.contains(c)))
        })
        .map(str::to_owned)
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

fn qemu(machine: &Machine, tpm: &Swtpm) -> Result<Command> {
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
        ))
        .args(tpm.qemu_args());
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

    const CLEAN: &str = "BEIZE-BOOT-REPORT state=running secureboot=on usr=verity root=encrypted boot=1 \
                         machineid=0123456789abcdef0123456789abcdef uptime=2.95s failed=none";

    #[test]
    fn parses_clean_report() {
        let report = BootReport::parse(CLEAN).unwrap();
        assert_eq!(report.state, "running");
        assert_eq!(report.secure_boot, "on");
        assert_eq!(report.usr, "verity");
        assert_eq!(report.root, "encrypted");
        assert_eq!(report.boot, 1);
        assert_eq!(report.machine_id, "0123456789abcdef0123456789abcdef");
        assert_eq!(report.uptime, "2.95s");
        assert!(report.failed.is_empty());
    }

    #[test]
    fn parses_degraded_report_behind_console_noise() {
        let line = "\x1b[0m[  3.1] BEIZE-BOOT-REPORT state=degraded secureboot=off usr=unprotected root=plain boot=2 \
                    machineid=x uptime=3.1s failed=systemd-logind.service,systemd-logind-varlink.socket";
        let report = BootReport::parse(line).unwrap();
        assert_eq!(report.state, "degraded");
        assert_eq!(report.secure_boot, "off");
        assert_eq!(report.root, "plain");
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
        assert!(is_passphrase_prompt(
            "Please enter recovery key for disk root: (press TAB for no echo) "
        ));
        assert!(is_passphrase_prompt(
            "Please enter passphrase for disk root (root): "
        ));
        assert!(!is_passphrase_prompt(
            "Starting Cryptography Setup for root..."
        ));
    }

    #[test]
    fn finds_the_recovery_key_on_the_console() {
        let key = "fjbdkvhn-nrttdbuj-ecjdhtnc-glklnkjl-crtfgehi-hdgjlbbr-ghlgucnk-lccdfuhb";
        let console = [
            "Recovery key for the data on this computer".to_owned(),
            format!("     {key}"),
        ];
        assert_eq!(find_recovery_key(&console).as_deref(), Some(key));
        // Other dash-separated words are not mistaken for a key.
        let noise = ["uuid 5024fb5b-d220-4ff2-9c32-385e3a1238fe".to_owned()];
        assert_eq!(find_recovery_key(&noise), None);
        assert_eq!(find_recovery_key(&[key.replace('f', "a")]), None);
    }

    #[test]
    fn detects_missing_partitions() {
        let usr = layout::usr_label();
        let line = |n: u32, ty: &str, name: &str| {
            format!("disk.img{n} : start=1, size=1, type={ty}, uuid=X, name=\"{name}\"\n")
        };
        let after_first_boot = [
            "label: gpt\n".to_owned(),
            line(1, layout::ESP_TYPE, "esp"),
            line(2, layout::USR_TYPE, &usr),
            line(3, layout::USR_VERITY_TYPE, &usr),
            line(4, layout::USR_TYPE, "_empty"),
            line(5, layout::USR_VERITY_TYPE, "_empty"),
            line(6, &layout::ROOT_TYPE.to_lowercase(), "root"),
        ]
        .concat();
        assert!(missing_partitions(&after_first_boot).is_empty());

        // The shipped image, before first boot.
        let shipped: String = after_first_boot
            .lines()
            .take(4)
            .map(|l| format!("{l}\n"))
            .collect();
        assert_eq!(
            missing_partitions(&shipped),
            ["/usr B", "/usr B verity", "root"]
        );

        // A data partition does not stand in for a missing verity partition.
        let no_b_verity = after_first_boot.replace(
            &line(5, layout::USR_VERITY_TYPE, "_empty"),
            &line(5, layout::USR_TYPE, "_empty"),
        );
        assert_eq!(missing_partitions(&no_b_verity), ["/usr B verity"]);
    }
}
