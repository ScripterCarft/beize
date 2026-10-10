//! `beize-tpm emergency`: what the console offers when a start fails, in the
//! initrd and in the system, instead of sulogin (ADR 0016).
//!
//! It shows what failed and offers to restart, to turn off or – with the
//! recovery key – an emergency shell. The key is checked against the LUKS2
//! header of the root partition, also when the TPM has unlocked it already:
//! the TPM unlocks without anyone present, so a failure provoked after that
//! must not hand out the data. Before the shell starts, PCR 11 is extended:
//! no signed TPM policy matches any more, the TPM unlocks nothing from that
//! shell. A wrong key, Ctrl+C or a crash never lead to a shell – systemd
//! starts this again (`Restart=always` in the units' drop-ins).

use std::io::{self, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

use crate::luks;

/// The root partition's LUKS2 volume, linked by udev (90-image-dissect.rules).
const ROOT_LUKS: &str = "/dev/gpt-auto-root-luks";
/// Shown first; the smoke test waits for it (tools/bz/src/vm/smoke.rs).
pub const HEADLINE: &str = "Beize could not start.";
/// systemd-ask-password's question for the key.
const KEY_PROMPT: &str = "Recovery key of this computer:";
/// Measured into PCR 11 before the shell starts.
const PCR11_WORD: &str = "beize-emergency-shell";
/// After a wrong key: slows down guessing at the console.
const WRONG_KEY_DELAY: Duration = Duration::from_secs(3);

pub fn run() -> Result<(), String> {
    println!();
    println!("{HEADLINE}");
    show_failed_units();
    loop {
        println!();
        println!("  r  restart");
        println!("  o  turn off");
        println!("  s  emergency shell (needs the recovery key)");
        print!("Choose: ");
        let _ = io::stdout().flush();
        let mut line = String::new();
        if io::stdin()
            .read_line(&mut line)
            .map_err(|e| format!("reading the console: {e}"))?
            == 0
        {
            return Err("the console closed".into());
        }
        match line.trim() {
            "r" => return systemctl("reboot"),
            "o" => return systemctl("poweroff"),
            "s" => {
                if let Err(refused) = shell() {
                    println!("{refused}");
                }
            }
            _ => {}
        }
    }
}

/// What failed, as systemd sees it.
fn show_failed_units() {
    let failed = Command::new("systemctl")
        .args(["--failed", "--plain", "--no-legend", "--no-pager"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default();
    let units: Vec<&str> = failed
        .lines()
        .filter_map(|line| line.split_whitespace().next())
        .collect();
    if units.is_empty() {
        return;
    }
    println!("What failed:");
    for unit in units.iter().take(3) {
        let _ = Command::new("systemctl")
            .args(["status", "--no-pager", "--lines=6", "--output=cat", unit])
            .stdout(Stdio::inherit())
            .stderr(Stdio::null())
            .status();
    }
}

/// Starts the shell once the recovery key checked out; returns why not otherwise.
fn shell() -> Result<(), String> {
    let device = Path::new(ROOT_LUKS);
    if !device.exists() {
        return Err(
            "There is no encrypted root partition, so no recovery key opens a shell. \
                    Reinstalling Beize is the way out."
                .into(),
        );
    }
    let key = ask_key()?;
    if !luks::opens(device, key.trim())? {
        thread::sleep(WRONG_KEY_DELAY);
        return Err("That is not the recovery key of this computer.".into());
    }
    // Lock the TPM first; without a TPM (--graceful) there is nothing it
    // could unlock, and the key opens the data anyway.
    let locked = Command::new("/usr/lib/systemd/systemd-pcrextend")
        .args(["--graceful", "--pcr=11", PCR11_WORD])
        .status()
        .map_err(|e| format!("starting systemd-pcrextend: {e}"))?;
    if !locked.success() {
        return Err("The security chip (TPM) could not be locked: no emergency shell.".into());
    }
    println!("The security chip (TPM) is locked until the next start.");
    println!("Leaving the shell restarts the computer.");
    let _ = Command::new("/usr/bin/bash").arg("--login").status();
    systemctl("reboot")
}

/// Asks for the recovery key on the console: with the terminal as its stdin,
/// systemd-ask-password queries it directly, without echo and without
/// caching the answer (there is no password agent in emergency mode).
fn ask_key() -> Result<String, String> {
    let output = Command::new("systemd-ask-password")
        .args(["--timeout=0", "--id=beize-emergency", KEY_PROMPT])
        .stdin(Stdio::inherit())
        .stderr(Stdio::inherit())
        .output()
        .map_err(|e| format!("starting systemd-ask-password: {e}"))?;
    if !output.status.success() {
        return Err("No key entered.".into());
    }
    String::from_utf8(output.stdout).map_err(|_| "The key is not text.".into())
}

fn systemctl(verb: &str) -> Result<(), String> {
    let status = Command::new("systemctl")
        .arg(verb)
        .status()
        .map_err(|e| format!("starting systemctl {verb}: {e}"))?;
    if status.success() {
        // systemd is going down; wait for it instead of being restarted.
        thread::sleep(Duration::from_secs(3600));
        Ok(())
    } else {
        Err(format!("systemctl {verb} failed with {status}"))
    }
}
