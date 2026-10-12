//! A virtual machine like the owner's PC: Secure Boot firmware with its own
//! variable store, a software TPM, a virtio disk and a network card.

use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail, ensure};

use super::console::{Answers, TAMPER_TIMEOUT, watch_boot};
use crate::layout::{self, MIB};
use crate::paths::Paths;
use crate::say;
use crate::{secureboot, util};

/// OVMF build that enforces Secure Boot (requires SMM).
const OVMF_CODE: &str = "/usr/share/OVMF/OVMF_CODE_4M.secboot.fd";

/// Size of the virtual disk the image is installed on for tests: the shipped
/// image is minimal, first boot grows into the remaining space. Sparse: only
/// what is written takes space on the host.
const TEST_DISK_SIZE: u64 = 16384 * MIB;
const _: () = assert!(TEST_DISK_SIZE >= layout::MIN_DISK_SIZE);

/// QEMU user networking ("slirp"): its DHCP server hands out this gateway and
/// this DNS server – the guest must learn both from the network (ADR 0011).
pub(super) const SLIRP_GATEWAY: &str = "10.0.2.2";

pub(super) const SLIRP_DNS: &str = "10.0.2.3";

/// A virtual machine: a disk, its own firmware variable store and its own
/// TPM (the state directory of an swtpm instance).
#[derive(Clone)]
pub(super) struct Machine {
    pub(super) disk: PathBuf,
    pub(super) vars: PathBuf,
    pub(super) tpm: PathBuf,
    /// Extra SMBIOS type 11 OEM strings, as a manipulated firmware could set.
    pub(super) smbios: Vec<String>,
    /// The hardware clock's time at power-on (QEMU `-rtc base=`); `None`:
    /// the host's time, like a working clock.
    pub(super) rtc: Option<&'static str>,
    /// Where the console of each boot is logged.
    pub(super) logs: PathBuf,
    /// An unsigned kernel module the boot report tries to load (ADR 0015),
    /// passed in through QEMU's fw_cfg.
    pub(super) unsigned_module: PathBuf,
    /// A USB stick plugged in (a raw disk image), as the owner brings an
    /// update to a computer without another one to serve it (ADR 0020).
    pub(super) usb_stick: Option<PathBuf>,
}

impl Machine {
    pub(super) fn named(paths: &Paths, name: &str) -> Self {
        Self {
            disk: paths.work.join(format!("{name}.img")),
            vars: paths.work.join(format!("{name}-vars.fd")),
            tpm: paths.work.join(format!("{name}-tpm")),
            smbios: Vec::new(),
            rtc: None,
            logs: boot_logs(paths),
            unsigned_module: paths.out.join("unsigned-module.ko"),
            usb_stick: None,
        }
    }

    /// QEMU's control socket (QMP) while the machine runs: the tests type on
    /// its keyboard through it.
    pub(super) fn qmp(&self) -> PathBuf {
        self.vars.with_extension("qmp")
    }

    /// The machine's name, as in its file names.
    pub(super) fn name(&self) -> String {
        self.disk
            .file_stem()
            .map_or_else(String::new, |stem| stem.to_string_lossy().into_owned())
    }

    /// Installs the current image onto a fresh test disk (`name.img`); see
    /// [`Self::install_from`].
    pub(super) fn install(paths: &Paths, name: &str) -> Result<Self> {
        Self::install_from(paths, name, &paths.out.join("disk.img"))
    }

    /// Installs `image` onto a fresh test disk, with a factory-new TPM and
    /// firmware in setup mode, and powers it on once: systemd-boot enrolls
    /// the image's keys and reboots – the same first start as on real
    /// hardware after clearing its Secure Boot keys (ADR 0010).
    pub(super) fn install_from(paths: &Paths, name: &str, image: &Path) -> Result<Self> {
        let machine = Self::named(paths, name);
        ensure!(
            image.exists(),
            "{} is missing; run `bz build`",
            image.display()
        );
        copy_sparse(image, &machine.disk)?;
        // The image is a few GiB, of which ~100 MiB are written. A dense copy
        // filled the CI runner's disk until the runner died without a log.
        let allocated = |path: &Path| -> Result<u64> { Ok(fs::metadata(path)?.blocks() * 512) };
        ensure!(
            allocated(&machine.disk)? <= allocated(image)? + MIB,
            "the copy of {} is not sparse",
            image.display()
        );
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

    /// A copy of this machine's state – disk, firmware variables and TPM –
    /// as machine `name`, to change and boot without touching this one.
    pub(super) fn copy_as(&self, paths: &Paths, name: &str) -> Result<Self> {
        let copy = Self::named(paths, name);
        if copy.tpm.exists() {
            fs::remove_dir_all(&copy.tpm)?;
        }
        for (from, to) in [
            (&self.disk, &copy.disk),
            (&self.vars, &copy.vars),
            (&self.tpm, &copy.tpm),
        ] {
            copy_sparse(from, to)?;
        }
        Ok(copy)
    }

    pub(super) fn remove(self) -> Result<()> {
        fs::remove_file(&self.disk)?;
        fs::remove_file(&self.vars)?;
        fs::remove_dir_all(&self.tpm)?;
        Ok(())
    }
}

/// Copies `from` (a file or directory) to `to`, keeping disks sparse (mostly
/// unwritten). `fs::copy` writes the holes out: on ext4 it copies through the
/// page cache.
fn copy_sparse(from: &Path, to: &Path) -> Result<()> {
    util::run(
        Command::new("cp")
            .args(["--archive", "--sparse=always", "--reflink=auto"])
            .arg(from)
            .arg(to),
    )
}

/// How many VMs run side by side: each has 4 CPUs and 2 GiB (`qemu`), plus
/// room for QEMU and swtpm themselves.
pub(super) fn parallel_vms() -> usize {
    util::parallel(4, 3 * 1024)
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
    say!("  first power-on: enrolling the Secure Boot keys");
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
pub(super) fn new_tpm(dir: &Path) -> Result<()> {
    if dir.exists() {
        fs::remove_dir_all(dir)?;
    }
    fs::create_dir_all(dir)?;
    Ok(())
}

/// A running software TPM, stopped when dropped. It also ends by itself as
/// soon as QEMU closes the control connection.
pub(super) struct Swtpm {
    child: Child,
    socket: PathBuf,
}

impl Swtpm {
    pub(super) fn start(state: &Path) -> Result<Self> {
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
    pub(super) fn qemu_args(&self) -> Vec<String> {
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

/// Where boot consoles are logged, one file per boot.
fn boot_logs(paths: &Paths) -> PathBuf {
    paths.work.join("logs/boot")
}

/// Empties the boot logs: they always belong to the latest `bz smoke`,
/// `bz debug` or `bz run`, numbered in the order of its boots.
pub(super) fn clear_boot_logs(paths: &Paths) -> Result<()> {
    let logs = boot_logs(paths);
    if logs.exists() {
        fs::remove_dir_all(&logs)?;
    }
    Ok(())
}

/// Applies `virt-fw-vars` options to a machine's firmware variables in place.
pub(super) fn edit_firmware(machine: &Machine, options: &[&str]) -> Result<()> {
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

/// Makes systemd-boot start `entry` once on the next boot (its
/// LoaderEntryOneShot variable), as `bootctl set-oneshot` would.
pub(super) fn boot_once(machine: &Machine, entry: &str) -> Result<()> {
    // UTF-16LE with the terminating NUL; non-volatile, boot and runtime access.
    let data: String = entry
        .encode_utf16()
        .chain([0])
        .flat_map(u16::to_le_bytes)
        .map(|b| format!("{b:02x}"))
        .collect();
    let json = machine.vars.with_extension("oneshot.json");
    fs::write(
        &json,
        format!(
            r#"{{"version": 2, "variables": [{{"name": "LoaderEntryOneShot", "guid": "4a67b082-0a4c-41cf-b6c7-440b29bb8c4f", "attr": 7, "data": "{data}"}}]}}"#
        ),
    )?;
    let edited = edit_firmware(machine, &["--set-json", &json.to_string_lossy()]);
    fs::remove_file(&json)?;
    edited
}

pub(super) fn qemu(machine: &Machine, tpm: &Swtpm) -> Result<Command> {
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
        say!("  warning: /dev/kvm not accessible, falling back to slow emulation");
        cmd.args(["-machine", "q35,smm=on,accel=tcg", "-cpu", "max"]);
    }
    cmd.args(["-global", "driver=cfi.pflash01,property=secure,value=on"])
        .args(["-smp", "4", "-m", "2G", "-no-reboot"])
        // The RAM exists before the guest starts, as on a real PC. Beize's
        // kernel clears all memory at boot (INIT_ON_FREE, ADR 0015); without
        // this the host would only then provide each page, and the kernel
        // phase would measure the host's paging – on the nested CI runner
        // 0.6–4 s depending on what the job touched before.
        .args(["-object", "memory-backend-ram,id=ram,size=2G,prealloc=on"])
        .args(["-machine", "memory-backend=ram"])
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
        .arg("-qmp")
        .arg(format!(
            "unix:{},server=on,wait=off",
            machine.qmp().display()
        ))
        // One wired port behind QEMU's NAT with DHCP; virtio like the disk.
        .args(["-nic", "user,model=virtio-net-pci"]);
    for value in &machine.smbios {
        cmd.arg("-smbios").arg(format!("type=11,value={value}"));
    }
    if let Some(stick) = &machine.usb_stick {
        // On a slot of its own: the other devices keep their PCI addresses,
        // which the firmware's boot entries name (a PC has its USB
        // controller always).
        cmd.args(["-device", "qemu-xhci,id=xhci,addr=0x10"])
            .arg("-drive")
            .arg(format!(
                "if=none,id=stick,format=raw,file={}",
                stick.display()
            ))
            .args(["-device", "usb-storage,bus=xhci.0,drive=stick"]);
    }
    if let Some(base) = machine.rtc {
        cmd.arg("-rtc").arg(format!("base={base}"));
    }
    if machine.unsigned_module.exists() {
        cmd.arg("-fw_cfg").arg(format!(
            "name=opt/org.beize/unsigned-module,file={}",
            machine.unsigned_module.display()
        ));
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
