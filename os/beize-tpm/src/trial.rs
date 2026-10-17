//! Whether a failed start restarts instead of waiting at the emergency gate
//! (ADR 0020): a new version is on trial until a start succeeded. When such
//! a start fails and systemd-boot has another version of Beize to fall back
//! to, the computer restarts by itself; after the last try systemd-boot
//! starts the previous version. Everything else waits for the owner at the
//! gate: a version that succeeded once (no boot counter), the recovery,
//! a computer with no other version, and a version systemd-boot chose
//! although it had no tries left – then every version failed, and restarting
//! would only loop.
//!
//! systemd-boot tells the OS what it did in EFI variables (Boot Loader
//! Interface): `LoaderEntries`, `LoaderEntrySelected` and, when it counted
//! the start, `LoaderBootCountPath` – the UKI's new name with tries left and
//! done, e.g. `\EFI\Linux\beize_0.2+2-1.efi`. It counts on even without
//! tries left (`+0-3` → `+0-4`).

use std::fs;
use std::process::Command;

/// Tries a new version gets; `BOOT_TRIES` in tools/bz/src/layout.rs, which
/// a test there keeps equal.
pub const BOOT_TRIES: u32 = 3;
/// The vendor GUID of systemd-boot's variables.
const LOADER_GUID: &str = "4a67b082-0a4c-41cf-b6c7-440b29bb8c4f";
/// How Beize names its UKIs (tools/bz/src/layout.rs) and so its entries.
const ENTRY_PREFIX: &str = "beize_";
/// Shown before restarting; the smoke test waits for it.
pub const RESTARTING: &str = "This version of Beize is new and did not start. Restarting";

/// A counted start: tries left and done after this start.
#[derive(Debug, PartialEq, Eq)]
struct Count {
    left: u32,
    done: u32,
}

/// Whether this failed start should restart: what systemd-boot reported,
/// and whether the start was already marked good. Returns the try (1-based)
/// that failed.
pub fn restart() -> Option<u32> {
    let entries = variable("LoaderEntries")?;
    let selected = variable("LoaderEntrySelected")?;
    let count_path = variable("LoaderBootCountPath")?;
    // After boot-complete.target systemd-bless-boot has marked this start
    // good; the variable stays.
    let blessed = Command::new("systemctl")
        .args(["is-active", "--quiet", "systemd-bless-boot.service"])
        .status()
        .is_ok_and(|s| s.success());
    if blessed {
        return None;
    }
    decide(&entries, &selected, &count_path)
}

fn decide(entries: &[String], selected: &[String], count_path: &[String]) -> Option<u32> {
    let selected = selected.first()?;
    // Another profile of the UKI: the recovery or the start without the
    // desktop, which the owner chose and which needs the recovery key.
    if selected.contains('@') || !selected.starts_with(ENTRY_PREFIX) {
        return None;
    }
    let count = count(count_path.first()?, selected)?;
    let fallback = entries
        .iter()
        .any(|e| e.starts_with(ENTRY_PREFIX) && !e.contains('@') && e != selected);
    // done > BOOT_TRIES: chosen with no tries left, so nothing better is left.
    (fallback && (count.left > 0 || count.done <= BOOT_TRIES)).then_some(count.done)
}

/// The count in `path` if it is the file of entry `selected`
/// (`beize_0.2.efi` for `\EFI\Linux\beize_0.2+2-1.efi`).
fn count(path: &str, selected: &str) -> Option<Count> {
    let file = path.rsplit('\\').next()?;
    let stem = file.strip_suffix(".efi")?;
    let (name, counter) = stem.rsplit_once('+')?;
    if format!("{name}.efi") != selected {
        return None;
    }
    let (left, done) = counter.split_once('-').unwrap_or((counter, "0"));
    Some(Count {
        left: left.parse().ok()?,
        done: done.parse().ok()?,
    })
}

/// The strings of systemd-boot's variable `name` (UTF-16LE after 4 attribute
/// bytes, separated and ended by NUL); `None` if it is not set.
fn variable(name: &str) -> Option<Vec<String>> {
    let raw = fs::read(format!("/sys/firmware/efi/efivars/{name}-{LOADER_GUID}")).ok()?;
    let units: Vec<u16> = raw
        .get(4..)?
        .as_chunks::<2>()
        .0
        .iter()
        .map(|&pair| u16::from_le_bytes(pair))
        .collect();
    let text = String::from_utf16(&units).ok()?;
    Some(
        text.split('\0')
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_owned()).collect()
    }

    const BOTH: [&str; 5] = [
        "beize_0.2.efi",
        "beize_0.2.efi@recovery",
        "beize_0.1.efi",
        "beize_0.1.efi@recovery",
        "auto-reboot-to-firmware-setup",
    ];

    fn restarts(entries: &[&str], selected: &str, path: &str) -> Option<u32> {
        decide(&strings(entries), &strings(&[selected]), &strings(&[path]))
    }

    #[test]
    fn a_new_version_restarts_until_its_last_try() {
        for (path, attempt) in [
            (r"\EFI\Linux\beize_0.2+2-1.efi", 1),
            (r"\EFI\Linux\beize_0.2+1-2.efi", 2),
            (r"\EFI\Linux\beize_0.2+0-3.efi", 3),
        ] {
            assert_eq!(
                restarts(&BOTH, "beize_0.2.efi", path),
                Some(attempt),
                "{path}"
            );
        }
    }

    #[test]
    fn chosen_without_tries_left_it_waits() {
        // Every version failed: systemd-boot fell back to a bad entry.
        assert_eq!(
            restarts(&BOTH, "beize_0.2.efi", r"\EFI\Linux\beize_0.2+0-4.efi"),
            None
        );
    }

    #[test]
    fn without_another_version_it_waits() {
        // A fresh installation counts its first starts too.
        let one = ["beize_0.1.efi", "beize_0.1.efi@recovery"];
        assert_eq!(
            restarts(&one, "beize_0.1.efi", r"\EFI\Linux\beize_0.1+2-1.efi"),
            None
        );
    }

    #[test]
    fn the_recovery_waits() {
        assert_eq!(
            restarts(
                &BOTH,
                "beize_0.2.efi@recovery",
                r"\EFI\Linux\beize_0.2+2-1.efi"
            ),
            None
        );
    }

    #[test]
    fn a_count_of_another_file_is_not_this_start() {
        assert_eq!(
            restarts(&BOTH, "beize_0.1.efi", r"\EFI\Linux\beize_0.2+2-1.efi"),
            None
        );
        assert_eq!(restarts(&BOTH, "beize_0.1.efi", "garbage"), None);
    }

    #[test]
    fn a_counter_without_tries_done_counts_none() {
        assert_eq!(
            count(r"\EFI\Linux\beize_0.2+3.efi", "beize_0.2.efi"),
            Some(Count { left: 3, done: 0 })
        );
    }
}
