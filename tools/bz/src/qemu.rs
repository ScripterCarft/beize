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
/// Credential for `bz run`: a root shell on the serial console (VM images only).
const DEBUG_CONSOLE_CREDENTIAL: &str = "type=11,value=io.systemd.credential:beize.debug-console=1";

/// QEMU user networking ("slirp"): its DHCP server hands out this gateway and
/// this DNS server – the guest must learn both from the network (ADR 0011).
const SLIRP_GATEWAY: &str = "10.0.2.2";
const SLIRP_DNS: &str = "10.0.2.3";

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
    /// `refused` if the TPM did not unlock the root partition for the running
    /// system (it may only in the initrd).
    system_unlock: String,
    /// networkd's operational state; `routable` = address + default route.
    network: String,
    /// DNS servers resolved learned from the network.
    dns: Vec<String>,
    /// The address `_gateway` resolved to through NSS, or `failed`.
    lookup: String,
    /// The zone /etc/localtime points at, or `none`.
    time_zone: String,
    /// How many time servers completed the NTS key exchange. Needs the
    /// internet, so reported, not required.
    nts: String,
    /// The system clock when reporting, in seconds since 1970.
    clock: u64,
    failed: Vec<String>,
}

impl BootReport {
    /// Parses `BEIZE-BOOT-REPORT state=running secureboot=on usr=verity root=encrypted boot=1 machineid=… uptime=2.95s systemunlock=refused network=routable dns=10.0.2.3 lookup=10.0.2.2 tz=Europe/Berlin nts=4 clock=1791500000 failed=none`.
    fn parse(line: &str) -> Option<Self> {
        let fields = &line[line.find(REPORT_MARKER)? + REPORT_MARKER.len()..];
        let field = |key: &str| {
            fields
                .split_whitespace()
                .find_map(|f| f.strip_prefix(key)?.strip_prefix('='))
                .map(str::to_owned)
        };
        let list = |key: &str| {
            Some(match field(key)?.as_str() {
                "none" => Vec::new(),
                list => list.split(',').map(str::to_owned).collect(),
            })
        };
        Some(Self {
            state: field("state")?,
            secure_boot: field("secureboot")?,
            usr: field("usr")?,
            root: field("root")?,
            boot: field("boot")?.parse().ok()?,
            machine_id: field("machineid")?,
            uptime: field("uptime")?,
            system_unlock: field("systemunlock")?,
            network: field("network")?,
            dns: list("dns")?,
            lookup: field("lookup")?,
            time_zone: field("tz")?,
            nts: field("nts")?,
            clock: field("clock")?.parse().ok()?,
            failed: list("failed")?,
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
    /// Extra SMBIOS type 11 OEM strings, as a manipulated firmware could set.
    smbios: Vec<String>,
    /// The hardware clock's time at power-on (QEMU `-rtc base=`); `None`:
    /// the host's time, like a working clock.
    rtc: Option<&'static str>,
}

impl Machine {
    fn named(paths: &Paths, name: &str) -> Self {
        Self {
            disk: paths.work.join(format!("{name}.img")),
            vars: paths.work.join(format!("{name}-vars.fd")),
            tpm: paths.work.join(format!("{name}-tpm")),
            smbios: Vec::new(),
            rtc: None,
        }
    }

    /// Installs the current image onto a fresh test disk (`name.img`); see
    /// [`Self::install_from`].
    fn install(paths: &Paths, name: &str) -> Result<Self> {
        Self::install_from(paths, name, &paths.out.join("disk.img"))
    }

    /// Installs `image` onto a fresh test disk, with a factory-new TPM and
    /// firmware in setup mode, and powers it on once: systemd-boot enrolls
    /// the image's keys and reboots – the same first start as on real
    /// hardware after clearing its Secure Boot keys (ADR 0010).
    fn install_from(paths: &Paths, name: &str, image: &Path) -> Result<Self> {
        let machine = Self::named(paths, name);
        fs::copy(image, &machine.disk)
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
        fs::copy(secureboot::OVMF_VARS, &machine.vars)
            .with_context(|| format!("copying {}; install ovmf", secureboot::OVMF_VARS))?;
        new_tpm(&machine.tpm)?;
        enroll_keys(&machine)?;
        Ok(machine)
    }

    fn remove(self) -> Result<()> {
        fs::remove_file(&self.disk)?;
        fs::remove_file(&self.vars)?;
        fs::remove_dir_all(&self.tpm)?;
        Ok(())
    }
}

/// Certificates the firmware must hold after enrollment (profile "with
/// Windows"): Beize's own, Microsoft's KEK (Windows' Secure Boot updates)
/// and db (Windows boot manager, option ROMs).
const ENROLLED: [(&str, &str); 6] = [
    ("PK", "Beize"),
    ("KEK", "Beize"),
    ("KEK", "Microsoft Corporation KEK 2K CA 2023"),
    ("db", "Beize"),
    ("db", "Windows UEFI CA 2023"),
    ("db", "Microsoft Option ROM UEFI CA 2023"),
];

/// Powers a machine in setup mode on once and requires systemd-boot to
/// enroll the keys from the ESP and reboot, leaving PK, KEK, db and dbx set.
fn enroll_keys(machine: &Machine) -> Result<()> {
    eprintln!("  first power-on: enrolling the Secure Boot keys");
    // Wait for the end of the enrollment, not its start: writing the
    // variables takes seconds on a slow CI runner, and only the reboot
    // after it gets the short grace period of `watch_boot`.
    let run = watch_boot(machine, TAMPER_TIMEOUT, &Answers::none(), |line| {
        line.contains("Secure Boot keys successfully enrolled")
            .then_some(())
    })?;
    ensure!(
        run.answer.is_some(),
        "systemd-boot did not enroll the Secure Boot keys"
    );
    ensure!(
        run.powered_off,
        "the machine did not reboot after enrolling the Secure Boot keys"
    );
    let firmware = util::output(
        Command::new("virt-fw-vars")
            .arg("--input")
            .arg(&machine.vars)
            .args(["--print", "--verbose"]),
    )?;
    let enrolled = enrolled_certificates(&firmware);
    for (var, name) in ENROLLED {
        ensure!(
            enrolled
                .iter()
                .any(|(v, subject)| v == var && subject.contains(name)),
            "after enrollment {var} holds no certificate \"{name}\"; it holds: {enrolled:?}"
        );
    }
    ensure!(
        firmware.lines().any(|line| line.starts_with("name=dbx")),
        "after enrollment the firmware has no revocation list (dbx)"
    );
    Ok(())
}

/// (variable, certificate subject) pairs from `virt-fw-vars --print --verbose`.
fn enrolled_certificates(firmware: &str) -> Vec<(String, String)> {
    let mut variable = String::new();
    let mut found = Vec::new();
    for line in firmware.lines() {
        if let Some(rest) = line.strip_prefix("name=") {
            variable = rest.split_whitespace().next().unwrap_or("").to_owned();
        } else if let Some(subject) = line.trim().strip_prefix("subject ") {
            found.push((variable.clone(), subject.to_owned()));
        }
    }
    found
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
    util::run(
        qemu(&machine, &tpm)?
            .args(["-nographic", "-serial", "mon:stdio"])
            .args(["-smbios", DEBUG_CONSOLE_CREDENTIAL]),
    )
}

/// Verifies the image end to end on a fresh test disk:
/// 1. first boot under enforced Secure Boot creates `/usr` slot B and the
///    root partition, reaches `running` without failed units and marks the
///    UKI as good (boot counting);
/// 2. the second boot finds the same installation (state persisted);
/// 3. the image with an unsigned kernel image is refused by the firmware;
/// 4. the owner's workflow (`new-keys`, `sign`) yields an image that boots.
///
/// The firmware is set up like a PC as shipped, with Beize's db certificate
/// appended (ADR 0010).
pub fn smoke(paths: &Paths) -> Result<()> {
    let machine = Machine::install(paths, "smoke")?;

    util::step("First boot");
    let first = boot_healthy(&machine, 1, &Answers::none())?;
    check_partitions(&machine.disk)?;
    check_blessed(&machine.disk)?;
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
    eprintln!("  ok: first boot showed a recovery key and waited until it was saved");
    let created = format!("Account \"{SMOKE_USER}\" created");
    ensure!(
        first.console.iter().any(|line| line.contains(&created)),
        "first boot did not create the owner's account"
    );
    eprintln!("  ok: first boot created the owner's account \"{SMOKE_USER}\"");
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
            line.contains("[  OK  ]") || line.contains("Starting ") || line.contains("Finished ")
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
    eprintln!("  ok: the account setup had the console to itself");

    util::step("Second boot");
    let second = boot_healthy(&machine, 2, &Answers::none())?;
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
    ensure!(
        !second
            .console
            .iter()
            .any(|line| line.contains(SETUP_ANSWERS[0].0)),
        "the second boot asked for the owner's account again"
    );
    eprintln!("  ok: the owner's account was set up only once");

    util::step("A boot with something added must stop before asking for the recovery key");
    // Like a firmware whose SMBIOS OEM strings were rewritten from another OS:
    // systemd-stub appends these kernel parameters even under Secure Boot.
    let added = Machine {
        smbios: vec!["io.systemd.stub.kernel-cmdline-extra=beize.added=1".to_owned()],
        ..machine.clone()
    };
    let run = watch_boot(
        &added,
        TAMPER_TIMEOUT,
        &Answers::recovery_key(&recovery_key),
        |line| line.contains("This start was changed").then_some(()),
    )?;
    ensure!(
        run.answer.is_some(),
        "a boot with added kernel parameters was not stopped"
    );
    ensure!(
        !run.prompted,
        "a boot with added kernel parameters asked for the recovery key"
    );
    eprintln!(
        "\n  ok: the changed boot stopped before unlocking, without asking for the recovery key"
    );

    util::step("A Secure Boot update needs the recovery key once, then the TPM unlocks again");
    // Like Windows installing a revocation: a new dbx entry changes PCR 7.
    edit_firmware(
        &machine,
        &["--add-dbx-hash", secureboot::OWNER_GUID, SMOKE_REVOKED_HASH],
    )?;
    let with_key = Answers::recovery_key(&recovery_key);
    expect_rebound(&machine, 3, &with_key, "the Secure Boot update")?;

    util::step("A foreign certificate in db must not be approved, even with the recovery key");
    // Like someone appending their own certificate in the firmware setup.
    let approved = paths.work.join("smoke-vars-approved.fd");
    fs::copy(&machine.vars, &approved)?;
    let foreign = foreign_certificate(paths)?;
    edit_firmware(
        &machine,
        &[
            "--add-db",
            secureboot::OWNER_GUID,
            foreign.to_str().context("certificate path")?,
        ],
    )?;
    expect_refused(
        &machine,
        5,
        &with_key,
        "the signature database (db) holds a certificate outside the profile",
    )?;
    // Back to the approved state: the TPM unlocks by itself again.
    fs::rename(&approved, &machine.vars)?;
    boot_healthy(&machine, 7, &Answers::none())?;
    eprintln!("  ok: back in the approved state the TPM unlocked by itself");

    util::step("Another TPM (new mainboard) is adopted only when the owner confirms it");
    let other_tpm = Machine {
        tpm: paths.work.join("smoke-other-tpm"),
        ..machine.clone()
    };
    new_tpm(&other_tpm.tpm)?;
    expect_refused(
        &other_tpm,
        8,
        &with_key.clone().adopt_tpm("no"),
        "The new security chip was not adopted",
    )?;
    expect_rebound(
        &other_tpm,
        10,
        &with_key.clone().adopt_tpm("yes"),
        "a confirmed new TPM",
    )?;
    fs::remove_dir_all(&other_tpm.tpm)?;
    // From here on the volume is bound to the other TPM: the machine's own
    // TPM must not unlock it any more (checked below, with Secure Boot off).

    util::step("The root partition must stay locked with Secure Boot disabled");
    let insecure = Machine {
        vars: paths.work.join("smoke-insecure-vars.fd"),
        ..machine.clone()
    };
    // Keys stay enrolled, Secure Boot is switched off – as in the firmware setup.
    fs::copy(&machine.vars, &insecure.vars)?;
    edit_firmware(&insecure, &["--set-false", "SecureBootEnable"])?;
    expect_root_locked(&insecure, "Secure Boot disabled")?;
    // The recovery key opens the data, but must not approve this state.
    let run = watch_boot(&insecure, SMOKE_TIMEOUT, &with_key, BootReport::parse)?;
    ensure!(
        run.prompted && run.answer.is_some(),
        "with Secure Boot disabled the recovery key did not open the root partition"
    );
    ensure!(
        run.console
            .iter()
            .any(|line| line.contains("Secure Boot is off: the TPM stays bound")),
        "with Secure Boot disabled the boot did not refuse to re-bind the TPM"
    );
    expect_root_locked(&insecure, "Secure Boot disabled after a recovery-key boot")?;
    eprintln!(
        "  ok: with Secure Boot disabled the recovery key opened the data without re-binding the TPM"
    );
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
    let run = watch_boot(&machine, TAMPER_TIMEOUT, &Answers::none(), |line| {
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
    let run = watch_boot(&machine, TAMPER_TIMEOUT, &Answers::none(), |line| {
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

    util::step("The owner's signing workflow must yield an image that boots");
    owner_workflow(paths)?;
    Ok(())
}

/// Runs the owner's workflow end to end – `bz secureboot new-keys`, then
/// `bz sign` – with a throwaway directory instead of 1Password, and boots the
/// result: enrollment from setup mode, then a healthy first boot.
fn owner_workflow(paths: &Paths) -> Result<()> {
    let dir = paths.work.join("owner-workflow");
    if dir.exists() {
        fs::remove_dir_all(&dir)?;
    }
    let store = secureboot::Store::Directory(dir.join("store"));
    let public = dir.join("public");
    let microsoft = secureboot::microsoft_dir(paths);
    let profile = secureboot::Profile::WithWindows;
    secureboot::new_keys(&store, &public, &microsoft, profile)?;
    ensure!(
        secureboot::new_keys(&store, &public, &microsoft, profile).is_err(),
        "new-keys replaced existing production keys"
    );
    let keys = secureboot::Keys::production(&store, &public)?;
    let out = dir.join("out");
    fs::create_dir_all(&out)?;
    let disk = image::assemble(paths, &keys, &out, image::Target::Vm)?;
    drop(keys);

    let machine = Machine::install_from(paths, "owner", &disk)?;
    boot_healthy(&machine, 1, &Answers::none())?;
    empty_battery(&machine)?;
    machine.remove()?;
    fs::remove_dir_all(&dir)?;
    eprintln!("  ok: keys from the key store signed an image that boots under Secure Boot");
    Ok(())
}

/// A hardware clock that lost its time, as with an empty BIOS battery.
const EMPTY_BATTERY_RTC: &str = "2000-01-01T00:00:00";

/// Boots with the hardware clock reset to 2000: systemd must move the clock
/// to at least Beize's build time (or the last verified time saved by
/// beize-save-clock), so certificates and NTS can work (ADR 0012).
fn empty_battery(machine: &Machine) -> Result<()> {
    util::step("A hardware clock reset to 2000 (empty BIOS battery) must not set the time back");
    let reset = Machine {
        rtc: Some(EMPTY_BATTERY_RTC),
        ..machine.clone()
    };
    let boot = boot_healthy(&reset, 2, &Answers::none())?;
    let host_now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_secs();
    ensure!(
        boot.report.clock >= crate::packages::BUILD_EPOCH,
        "the clock started from the reset hardware clock: {} is before Beize's build time {}",
        boot.report.clock,
        crate::packages::BUILD_EPOCH
    );
    // Otherwise the test did not reset anything (e.g. the guest read the
    // host's time instead of its hardware clock).
    ensure!(
        boot.report.clock + 3600 < host_now,
        "the guest's clock is the host's time ({}), the hardware clock reset had no effect",
        boot.report.clock
    );
    eprintln!(
        "  ok: the system clock started at or after Beize's build time ({} time servers verified)",
        boot.report.nts
    );
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
/// `answers` are typed at the console's questions (see [`Answers`]).
fn boot_healthy(machine: &Machine, expected_boot: u32, answers: &Answers) -> Result<HealthyBoot> {
    let run = watch_boot(machine, SMOKE_TIMEOUT, answers, BootReport::parse)?;
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
        report.system_unlock == "refused",
        "the TPM must unlock the root partition only in the initrd, but for the \
         running system it reported systemunlock={}",
        report.system_unlock
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
        report.network == "routable",
        "the wired network did not come up via DHCP (network={})",
        report.network
    );
    ensure!(
        report.dns == [SLIRP_DNS],
        "the DNS servers are not the network's: expected {SLIRP_DNS}, got {:?}",
        report.dns
    );
    ensure!(
        report.lookup == SLIRP_GATEWAY,
        "name lookup through NSS failed: _gateway resolved to {}",
        report.lookup
    );
    ensure!(
        report.time_zone == SMOKE_TIME_ZONE,
        "the time zone chosen in the setup is not set: expected {SMOKE_TIME_ZONE}, got {}",
        report.time_zone
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
         ({:.1}s including firmware), network via DHCP, no failed units, clean power-off \
         (NTS key exchange with {} time servers)",
        report.boot,
        report.uptime,
        elapsed.as_secs_f64(),
        report.nts
    );
    Ok(HealthyBoot {
        report,
        console: run.console,
        prompted: run.prompted,
    })
}

/// A dbx entry no binary has: added to simulate a Secure Boot update.
const SMOKE_REVOKED_HASH: &str = "62656967652d736d6f6b652d7265766f6b656462656967652d736d6f6b652d31";

/// Applies `virt-fw-vars` options to a machine's firmware variables in place.
fn edit_firmware(machine: &Machine, options: &[&str]) -> Result<()> {
    let edited = machine.vars.with_extension("edited");
    util::output(
        Command::new("virt-fw-vars")
            .arg("--input")
            .arg(&machine.vars)
            .arg("--output")
            .arg(&edited)
            .args(options),
    )?;
    fs::rename(&edited, &machine.vars)?;
    Ok(())
}

/// A self-signed certificate that is in no profile.
fn foreign_certificate(paths: &Paths) -> Result<PathBuf> {
    let dir = paths.work.join("smoke-foreign");
    fs::create_dir_all(&dir)?;
    let cert = dir.join("foreign.crt");
    util::output(
        Command::new("openssl")
            .args(["req", "-new", "-x509", "-newkey", "rsa:2048", "-nodes"])
            .args(["-days", "1", "-subj", "/CN=Not Beize/", "-keyout"])
            .arg(dir.join("foreign.key"))
            .arg("-out")
            .arg(&cert),
    )?;
    Ok(cert)
}

/// After a change that must not be approved, boot `boot` opens with the
/// recovery key but keeps the TPM binding (beize-tpm says `reason`); boot
/// `boot + 1` must still need the recovery key.
fn expect_refused(machine: &Machine, boot: u32, answers: &Answers, reason: &str) -> Result<()> {
    let first = boot_healthy(machine, boot, answers)?;
    ensure!(first.prompted, "the TPM unlocked although it should refuse");
    ensure!(
        first.console.iter().any(|line| line.contains(reason)),
        "the recovery-key boot did not refuse with \"{reason}\""
    );
    ensure!(
        !first
            .console
            .iter()
            .any(|line| line.contains("binding the data to the current state")),
        "the TPM was re-bound although \"{reason}\""
    );
    let second = boot_healthy(machine, boot + 1, answers)?;
    ensure!(
        second.prompted,
        "the TPM unlocked after a refused re-binding"
    );
    eprintln!("  ok: refused to re-bind ({reason}); the recovery key is still needed");
    Ok(())
}

/// After a change the TPM refuses (PCR 7 or the TPM itself), boot `boot`
/// must ask for the recovery key and re-bind the TPM slot; boot `boot + 1`
/// must unlock by itself again (ADR 0010).
fn expect_rebound(machine: &Machine, boot: u32, answers: &Answers, change: &str) -> Result<()> {
    let first = boot_healthy(machine, boot, answers)?;
    ensure!(
        first.prompted,
        "after {change} the TPM still unlocked the root partition"
    );
    ensure!(
        first
            .console
            .iter()
            .any(|line| line.contains("binding the data to the current state")),
        "after {change} the recovery-key boot did not re-bind the TPM"
    );
    let second = boot_healthy(machine, boot + 1, &Answers::none())?;
    ensure!(
        !second.prompted,
        "after re-binding the TPM still did not unlock"
    );
    eprintln!("  ok: after {change} the recovery key was needed once, then the TPM unlocked again");
    Ok(())
}

/// Boots an installed disk under a changed condition (another TPM, other
/// firmware state) and requires that the TPM refuses to unlock the root
/// partition: the boot asks for the recovery key (or, without one, fails to
/// set up the volume) and nothing boots.
fn expect_root_locked(machine: &Machine, condition: &str) -> Result<()> {
    let run = watch_boot(machine, TAMPER_TIMEOUT, &Answers::none(), |line| {
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

/// The root partition's TPM2 slot must be bound to the Secure Boot state
/// (PCR 7), to nothing added to the boot (PCR 12) and to the signed PCR 11
/// policy for the initrd phase only (ADR 0010).
fn check_root_binding(disk: &Path, scratch: &Path) -> Result<()> {
    let token = root_tpm2_token(disk, scratch)?;
    let pcrs = |key: &str| token[key].as_array().cloned().unwrap_or_default();
    ensure!(
        pcrs("tpm2-pcrs") == [7, 12] && pcrs("tpm2_pubkey_pcrs") == [11],
        "the root partition is bound to PCRs {} and signed policies over PCRs {}, \
         expected 7+12 and 11",
        token["tpm2-pcrs"],
        token["tpm2_pubkey_pcrs"]
    );
    ensure!(
        token["tpm2_pubkey_ref"] == "initrd",
        "the signed policy of the root partition is not limited to the initrd \
         (policy reference {})",
        token["tpm2_pubkey_ref"]
    );
    eprintln!(
        "  ok: the TPM unlocks the root partition only for Secure Boot as enrolled (PCR 7), \
         nothing added to the boot (PCR 12) and a signed Beize UKI in its initrd \
         (PCR 11, policy reference \"initrd\")"
    );
    Ok(())
}

/// systemd's TPM2 token from the LUKS2 header of the root partition: what the
/// TPM checks before it releases the key.
fn root_tpm2_token(disk: &Path, scratch: &Path) -> Result<serde_json::Value> {
    let table: serde_json::Value = serde_json::from_str(&util::output(
        Command::new("sfdisk").arg("--json").arg(disk),
    )?)?;
    let root = table["partitiontable"]["partitions"]
        .as_array()
        .and_then(|parts| {
            parts.iter().find(|part| {
                part["type"]
                    .as_str()
                    .is_some_and(|ty| ty.eq_ignore_ascii_case(layout::ROOT_TYPE))
            })
        })
        .context("no root partition")?;
    let start = root["start"].as_u64().context("root partition start")? * 512;
    // The LUKS2 header (with both metadata copies) fits into the first 16 MiB.
    let mut header = vec![0u8; 16 * MIB as usize];
    let mut file = fs::File::open(disk)?;
    file.seek(SeekFrom::Start(start))?;
    file.read_exact(&mut header)?;
    let copy = scratch.join("root-luks-header");
    fs::write(&copy, &header)?;
    let dump = util::output(
        Command::new("cryptsetup")
            .args(["luksDump", "--dump-json-metadata"])
            .arg(&copy),
    );
    fs::remove_file(&copy)?;
    let metadata: serde_json::Value = serde_json::from_str(&dump?)?;
    metadata["tokens"]
        .as_object()
        .and_then(|tokens| tokens.values().find(|t| t["type"] == "systemd-tpm2"))
        .cloned()
        .context("the root partition has no systemd-tpm2 token")
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

/// beize-tpm's question whether to adopt a different TPM (beize-tpm/src/main.rs).
const ADOPT_TPM_PROMPT: &str = "Type yes to bind the data to it";
/// The first boot waits for the owner to confirm the recovery key is saved
/// (system/initrd/usr/lib/beize/recovery-key); the tests answer "saved".
const RECOVERY_KEY_SAVED_PROMPT: &str = "Type saved once the recovery key is written down";
/// The owner's account the tests create on first boot (beize-setup).
const SMOKE_USER: &str = "smoke";
/// beize-setup's questions and the tests' answers. "Password again:" comes
/// first: "Password:" alone does not match it, but the order of the check
/// must not depend on that.
/// The time zone: a country with several zones, so the setup asks back.
const SETUP_ANSWERS: [(&str, &str); 6] = [
    ("Your login name", SMOKE_USER),
    ("Your full name", "Smoke Test"),
    ("Password again:", "smoke-test-password"),
    ("Password:", "smoke-test-password"),
    ("Your time zone", "Brazil"),
    ("Which one?", "Sao Paulo"),
];
/// The zone the answers above must lead to.
const SMOKE_TIME_ZONE: &str = "America/Sao_Paulo";

/// What to type at the console during a boot, as the owner would.
#[derive(Clone, Default)]
struct Answers {
    /// Typed when the boot asks for the recovery key; without one, that
    /// question ends the boot.
    recovery_key: Option<String>,
    /// Typed when beize-tpm asks whether to adopt a new TPM ("no" if unset).
    adopt_tpm: Option<&'static str>,
}

impl Answers {
    fn none() -> Self {
        Self::default()
    }

    fn recovery_key(key: &str) -> Self {
        Self {
            recovery_key: Some(key.to_owned()),
            ..Self::default()
        }
    }

    fn adopt_tpm(self, answer: &'static str) -> Self {
        Self {
            adopt_tpm: Some(answer),
            ..self
        }
    }
}

enum ConsoleEvent<T> {
    Verdict(T),
    Prompted,
}

/// Boots headless and feeds every console line to `verdict` until it
/// answers or `timeout` passes, typing `answers` at the console's questions.
/// Without a recovery key, being asked for one ends the boot. QEMU is always
/// stopped before returning.
fn watch_boot<T: Send + 'static>(
    machine: &Machine,
    timeout: Duration,
    answers: &Answers,
    verdict: impl Fn(&str) -> Option<T> + Send + 'static,
) -> Result<BootRun<T>> {
    let answers = answers.clone();
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
    let stop_at_prompt = answers.recovery_key.is_none();
    let (tx, rx) = mpsc::channel();
    let reader = thread::spawn(move || {
        let mut failures = Vec::new();
        let mut console = Vec::new();
        let mut answered = false;
        let mut prompted = false;
        let mut asked_adopt = false;
        let mut asked_saved = false;
        let mut asked_setup = [false; SETUP_ANSWERS.len()];
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
            let shown = strip_ansi(&String::from_utf8_lossy(&pending));
            if !prompted && is_passphrase_prompt(&shown) {
                prompted = true;
                if let Some(key) = &answers.recovery_key {
                    eprintln!("  | (typing the recovery key)");
                    let _ = write!(stdin, "{key}\r");
                    let _ = stdin.flush();
                }
                let _ = tx.send(ConsoleEvent::Prompted);
            }
            if !asked_adopt && shown.contains(ADOPT_TPM_PROMPT) {
                asked_adopt = true;
                let answer = answers.adopt_tpm.unwrap_or("no");
                eprintln!("  | (answering {answer})");
                let _ = write!(stdin, "{answer}\r");
                let _ = stdin.flush();
            }
            if !asked_saved && shown.contains(RECOVERY_KEY_SAVED_PROMPT) {
                asked_saved = true;
                eprintln!("  | (confirming the recovery key is saved)");
                let _ = write!(stdin, "saved\r");
                let _ = stdin.flush();
            }
            // beize-setup's questions on first boot, each answered once.
            if let Some(i) = SETUP_ANSWERS
                .iter()
                .position(|(prompt, _)| shown.contains(prompt))
                && !asked_setup[i]
            {
                asked_setup[i] = true;
                let (prompt, answer) = SETUP_ANSWERS[i];
                eprintln!("  | (answering \"{prompt}\")");
                let _ = write!(stdin, "{answer}\r");
                let _ = stdin.flush();
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
        // A guest under KVM takes the wall clock from the host (kvmclock),
        // not from the hardware clock. A machine with its own hardware clock
        // time goes without it, like a real PC.
        let cpu = if machine.rtc.is_some() {
            "host,-kvmclock"
        } else {
            "host"
        };
        cmd.args(["-machine", "q35,smm=on,accel=kvm", "-cpu", cpu]);
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
        .args(tpm.qemu_args())
        // One wired port behind QEMU's NAT with DHCP; virtio like the disk.
        .args(["-nic", "user,model=virtio-net-pci"]);
    for value in &machine.smbios {
        cmd.arg("-smbios").arg(format!("type=11,value={value}"));
    }
    if let Some(base) = machine.rtc {
        cmd.arg("-rtc").arg(format!("base={base}"));
    }
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
                         machineid=0123456789abcdef0123456789abcdef uptime=2.95s systemunlock=refused \
                         network=routable dns=10.0.2.3 lookup=10.0.2.2 tz=America/Sao_Paulo nts=4 \
                         clock=1791500000 failed=none";

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
        assert_eq!(report.system_unlock, "refused");
        assert_eq!(report.network, "routable");
        assert_eq!(report.dns, [SLIRP_DNS]);
        assert_eq!(report.lookup, SLIRP_GATEWAY);
        assert_eq!(report.time_zone, SMOKE_TIME_ZONE);
        assert_eq!(report.nts, "4");
        assert_eq!(report.clock, 1_791_500_000);
        assert!(report.failed.is_empty());
    }

    #[test]
    fn parses_degraded_report_behind_console_noise() {
        let line = "\x1b[0m[  3.1] BEIZE-BOOT-REPORT state=degraded secureboot=off usr=unprotected root=plain boot=2 \
                    machineid=x uptime=3.1s systemunlock=none \
                    network=timeout dns=none lookup=failed tz=none nts=0 \
                    clock=946684800 failed=systemd-logind.service,systemd-logind-varlink.socket";
        let report = BootReport::parse(line).unwrap();
        assert_eq!(report.state, "degraded");
        assert_eq!(report.network, "timeout");
        assert!(report.dns.is_empty());
        assert_eq!(report.lookup, "failed");
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
