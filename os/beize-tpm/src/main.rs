//! `beize-tpm` – Beize's TPM helper for the initrd (ADR 0010).
//!
//! `beize-tpm reseal` runs once the root partition is unlocked and mounted at
//! /sysroot. After an ordinary boot (the TPM unlocked) it only keeps the
//! record of which TPM the data is bound to. After the owner unlocked with
//! the recovery key, because the TPM refused, it binds the TPM slot to the
//! current state – but only if that state may be approved:
//! - the measured Secure Boot state matches the profile (see `policy`);
//! - on a different TPM (new mainboard, or a swapped chip) only after the
//!   owner explicitly confirms it.
//!
//! Everything it prints goes to the console; the recovery key never does.

mod efi;
mod eventlog;
mod keyring;
mod policy;

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};

const EVENT_LOG: &str = "/sys/kernel/security/tpm0/binary_bios_measurements";
const PCR7: &str = "/sys/class/tpm/tpm0/pcr-sha256/7";
const PROFILE: &str = "/usr/lib/beize/secureboot";
/// The TPM's storage root key, as systemd-tpm2-setup-early found it this boot.
const CURRENT_SRK: &str = "/run/systemd/tpm2-srk-public-key.pem";
/// The SRK of the TPM the data is bound to. systemd keeps its own copy in
/// /var/lib/systemd, but overwrites it when the TPM changes.
const BOUND_SRK: &str = "/sysroot/var/lib/beize/tpm-srk.pem";

fn main() -> ExitCode {
    let command = std::env::args().nth(1);
    let result = match command.as_deref() {
        Some("reseal") => reseal(),
        _ => Err("usage: beize-tpm reseal".into()),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("beize-tpm: {error}");
            ExitCode::FAILURE
        }
    }
}

fn reseal() -> Result<(), String> {
    let current_srk = read(CURRENT_SRK)?;
    let bound_srk = fs::read(BOUND_SRK).ok();

    let Some(recovery_key) = keyring::cached_passphrase("cryptsetup") else {
        // The TPM unlocked: it is the one the data is bound to.
        if bound_srk.as_deref() != Some(current_srk.as_slice()) {
            record_srk(&current_srk)?;
        }
        return Ok(());
    };

    // Unlocked with the recovery key: the TPM refused this boot's state.
    if !secure_boot_on() {
        println!("Secure Boot is off: the TPM stays bound to the previous state.");
        return Ok(());
    }
    let events = eventlog::parse(&read(EVENT_LOG)?)?;
    let profile = policy::Profile::load(Path::new(PROFILE))?;
    if let Err(reason) = policy::check(&events, read_pcr(PCR7)?, &profile) {
        println!("The data stays bound to the previous state: {reason}.");
        println!("The recovery key will be needed again on the next start.");
        return Ok(());
    }

    if bound_srk.as_deref() != Some(current_srk.as_slice()) && !confirm_new_tpm()? {
        println!("The new security chip was not adopted; the recovery key will be needed again.");
        return Ok(());
    }

    let device = root_luks_device()?;
    println!("The security chip did not recognise this computer's state (firmware or");
    println!("Secure Boot changed); binding the data to the current state.");
    // Wipe first, then enroll: on a new TPM the PCR values match the old slot
    // and systemd-cryptenroll would take it for current and do nothing.
    // Should enrolling fail, the recovery key still opens the data.
    cryptenroll(&recovery_key, &["--wipe-slot=tpm2"], &device)?;
    // PCR 7, and the signed PCR 11 policy (public key from the UKI) with
    // policy reference "initrd" – as systemd-repart enrolls on first boot.
    cryptenroll(
        &recovery_key,
        &[
            "--tpm2-device=auto",
            "--tpm2-pcrs=7",
            "--tpm2-public-key-policyref=initrd",
        ],
        &device,
    )?;
    record_srk(&current_srk)
}

fn read(path: &str) -> Result<Vec<u8>, String> {
    fs::read(path).map_err(|e| format!("reading {path}: {e}"))
}

/// The SHA-256 bank value of a PCR, from the kernel's sysfs (hex text).
fn read_pcr(path: &str) -> Result<[u8; 32], String> {
    let text = String::from_utf8(read(path)?).map_err(|_| format!("{path} is not text"))?;
    let hex = text.trim();
    if hex.len() != 64 || !hex.is_ascii() {
        return Err(format!("{path} does not hold a SHA-256 value"));
    }
    let mut value = [0u8; 32];
    for (i, byte) in value.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&hex[2 * i..2 * i + 2], 16)
            .map_err(|_| format!("{path} does not hold a SHA-256 value"))?;
    }
    Ok(value)
}

/// The SecureBoot EFI variable: 4 attribute bytes, then 1 for enabled.
fn secure_boot_on() -> bool {
    fs::read("/sys/firmware/efi/efivars/SecureBoot-8be4df61-93ca-11d2-aa0d-00e098032b8c")
        .is_ok_and(|v| v.get(4) == Some(&1))
}

fn record_srk(srk: &[u8]) -> Result<(), String> {
    let path = Path::new(BOUND_SRK);
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(|e| format!("creating {}: {e}", dir.display()))?;
    }
    fs::write(path, srk).map_err(|e| format!("writing {BOUND_SRK}: {e}"))
}

/// Asks the owner on the console whether to adopt a different TPM.
fn confirm_new_tpm() -> Result<bool, String> {
    println!("This computer has a different security chip (TPM) than the one the data");
    println!("is bound to - expected after replacing the mainboard, but not otherwise.");
    let output = Command::new("systemd-ask-password")
        .args(["--echo=yes", "--timeout=300", "--id=beize-tpm-adopt"])
        .arg("New security chip found. Type yes to bind the data to it:")
        .stderr(Stdio::inherit())
        .output()
        .map_err(|e| format!("starting systemd-ask-password: {e}"))?;
    Ok(output.status.success() && String::from_utf8_lossy(&output.stdout).trim() == "yes")
}

/// The LUKS volume below the device-mapper device "root".
fn root_luks_device() -> Result<PathBuf, String> {
    let blocks = fs::read_dir("/sys/block").map_err(|e| format!("reading /sys/block: {e}"))?;
    for block in blocks.flatten() {
        let dm = block.path().join("dm");
        if fs::read_to_string(dm.join("name")).is_ok_and(|n| n.trim() == "root")
            && let Some(slave) = fs::read_dir(block.path().join("slaves"))
                .ok()
                .and_then(|mut s| s.next())
                .and_then(Result::ok)
        {
            return Ok(Path::new("/dev").join(slave.file_name()));
        }
    }
    Err("no LUKS volume below /dev/mapper/root".into())
}

/// Runs systemd-cryptenroll, unlocking with the recovery key passed on stdin
/// (never as an argument, never in a file).
fn cryptenroll(recovery_key: &str, args: &[&str], device: &Path) -> Result<(), String> {
    let mut child = Command::new("systemd-cryptenroll")
        .arg("--unlock-key-file=/dev/stdin")
        .args(args)
        .arg(device)
        .stdin(Stdio::piped())
        .spawn()
        .map_err(|e| format!("starting systemd-cryptenroll: {e}"))?;
    child
        .stdin
        .take()
        .ok_or("no stdin for systemd-cryptenroll")?
        .write_all(recovery_key.as_bytes())
        .map_err(|e| format!("passing the key to systemd-cryptenroll: {e}"))?;
    let status = child
        .wait()
        .map_err(|e| format!("waiting for systemd-cryptenroll: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!(
            "systemd-cryptenroll {} failed with {status}",
            args.join(" ")
        ))
    }
}
