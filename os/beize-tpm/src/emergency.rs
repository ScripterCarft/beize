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
//!
//! The same gate is the recovery (ADR 0018): the UKI's recovery profile
//! starts rescue.target with `beize.recovery` on the command line. Its PCR 11
//! has no signed TPM policy, so the initrd already asked for the recovery
//! key; the gate takes that key from systemd-cryptsetup's cache in the kernel
//! keyring – checked against the LUKS2 header like a typed one – instead of
//! asking a second time.
//!
//! The key may be typed with or without its dashes – or with whatever the
//! dash key gives on another keyboard: only its letters count (ADR 0017).

use std::io::{self, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

use crate::{keyring, luks};

/// The root partition's LUKS2 volume, linked by udev (90-image-dissect.rules).
const ROOT_LUKS: &str = "/dev/gpt-auto-root-luks";
/// Shown first; the smoke test waits for it (tools/bz/src/vm/smoke.rs).
pub const HEADLINE: &str = "Beize could not start.";
/// Shown first in the recovery instead.
pub const RECOVERY_HEADLINE: &str = "Beize recovery.";
/// On the command line of the UKI's recovery profile (bz, image.rs).
const RECOVERY_PARAMETER: &str = "beize.recovery";
/// systemd-ask-password's question for the key.
const KEY_PROMPT: &str = "Recovery key of this computer:";
/// Measured into PCR 11 before the shell starts.
const PCR11_WORD: &str = "beize-emergency-shell";
/// After a wrong key: slows down guessing at the console.
const WRONG_KEY_DELAY: Duration = Duration::from_secs(3);

pub fn run() -> Result<(), String> {
    println!();
    if recovery() {
        println!("{RECOVERY_HEADLINE}");
    } else {
        println!("{HEADLINE}");
        show_failed_units();
    }
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

/// Whether this is the recovery start (the UKI's recovery profile).
fn recovery() -> bool {
    std::fs::read_to_string("/proc/cmdline")
        .is_ok_and(|line| line.split_whitespace().any(|p| p == RECOVERY_PARAMETER))
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
    // A key the initrd took this start (the recovery, a Secure Boot update)
    // proves as much as a typed one.
    for cached in keyring::cached_passphrases("cryptsetup") {
        let cached = normalized(&cached);
        if luks::opens(device, &cached)? {
            return open_shell();
        }
    }
    let key = ask_key()?;
    if !luks::opens(device, &normalized(&key))? {
        thread::sleep(WRONG_KEY_DELAY);
        return Err("That is not the recovery key of this computer.".into());
    }
    open_shell()
}

/// Locks the TPM, then starts the shell; restarts the computer after it.
fn open_shell() -> Result<(), String> {
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

/// The recovery key as systemd-cryptenroll formatted it – 8 groups of 8
/// modhex letters, joined by dashes – if `typed` holds exactly its 64 letters,
/// whatever else is between them; otherwise `typed` as it is.
fn normalized(typed: &str) -> String {
    const MODHEX: &str = "cbdefghijklnrtuv";
    let letters: Vec<char> = typed
        .chars()
        .filter(char::is_ascii_alphabetic)
        .map(|c| c.to_ascii_lowercase())
        .collect();
    if letters.len() != 64 || !letters.iter().all(|c| MODHEX.contains(*c)) {
        return typed.trim().to_owned();
    }
    letters
        .chunks(8)
        .map(|group| group.iter().collect::<String>())
        .collect::<Vec<_>>()
        .join("-")
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

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &str = "iefgcelh-biduvkjv-cjvuncnk-vlfchdid-jhtuhhde-urkllkeg-ilkjgbrt-hjkbgktj";

    #[test]
    fn only_the_letters_of_a_recovery_key_count() {
        for typed in [
            KEY,
            "iefgcelhbiduvkjvcjvuncnkvlfchdidjhtuhhdeurkllkegilkjgbrthjkbgktj",
            "IEFGCELH-BIDUVKJV-CJVUNCNK-VLFCHDID-JHTUHHDE-URKLLKEG-ILKJGBRT-HJKBGKTJ",
            // The dash key of a German (/) and a French keyboard (6) with the US keymap.
            "iefgcelh/biduvkjv/cjvuncnk/vlfchdid/jhtuhhde/urkllkeg/ilkjgbrt/hjkbgktj",
            "iefgcelh6biduvkjv6cjvuncnk6vlfchdid6jhtuhhde6urkllkeg6ilkjgbrt6hjkbgktj",
            " iefgcelh biduvkjv cjvuncnk vlfchdid jhtuhhde urkllkeg ilkjgbrt hjkbgktj\n",
        ] {
            assert_eq!(normalized(typed), KEY, "{typed}");
        }
    }

    #[test]
    fn anything_else_stays_as_typed() {
        // One letter short, one letter outside modhex.
        let short = &KEY[..KEY.len() - 1];
        assert_eq!(normalized(short), short);
        let foreign = KEY.replacen('i', "a", 1);
        assert_eq!(normalized(&foreign), foreign);
        assert_eq!(normalized(" a passphrase \n"), "a passphrase");
    }
}
