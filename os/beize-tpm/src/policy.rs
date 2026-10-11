//! Decides whether the measured Secure Boot state may be approved, i.e. the
//! root partition bound to it (ADR 0010).
//!
//! PCR 7 holds the Secure Boot configuration (SecureBoot, PK, KEK, db, dbx)
//! and the db entries that authorised each boot binary. It may be approved
//! only if, replayed from the event log, it is exactly what the TPM holds and
//! - Secure Boot is on,
//! - PK is Beize's and KEK and db hold nothing outside the profile,
//! - every authority came from db, one of them Beize's own certificate,
//! - nothing else was measured (e.g. a firmware debug mode).
//!
//! dbx may change freely: a longer revocation list only removes trust.

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use sha2::{Digest, Sha256};

use crate::efi::{self, Entry};
use crate::eventlog::{
    self, EV_EFI_VARIABLE_AUTHORITY, EV_EFI_VARIABLE_DRIVER_CONFIG, EV_NO_ACTION, EV_SEPARATOR,
    Event,
};

const PCR_SECURE_BOOT: u32 = 7;
/// What systemd-pcrosseparator.service measures in the initrd, into PCR 7
/// among others, to mark where the firmware ends and the OS begins. It is in
/// systemd's own log, not the firmware's.
const OS_SEPARATOR: &[u8] = b"os-separator";

/// What the firmware may hold: the enrolled PK, KEK and db, and Beize's own
/// db certificate. Built into the signed UKI (`/usr/lib/beize/secureboot/`).
pub struct Profile {
    pk: BTreeSet<Entry>,
    kek: BTreeSet<Entry>,
    db: BTreeSet<Entry>,
    beize: BTreeSet<Entry>,
}

impl Profile {
    pub fn load(dir: &Path) -> Result<Self, String> {
        let entries = |file: &str| -> Result<BTreeSet<Entry>, String> {
            let path = dir.join(file);
            let bytes = fs::read(&path).map_err(|e| format!("reading {}: {e}", path.display()))?;
            Ok(efi::signature_entries(&bytes)
                .map_err(|e| format!("{}: {e}", path.display()))?
                .into_iter()
                .collect())
        };
        Ok(Self {
            pk: entries("PK.esl")?,
            kek: entries("KEK.esl")?,
            db: entries("db.esl")?,
            beize: entries("beize-db.esl")?,
        })
    }
}

/// What PCR 12 says about this boot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Boot {
    /// Beize as signed: nothing added.
    Unchanged,
    /// The UKI's recovery profile (ADR 0018), nothing added. Its PCR 11 has
    /// no signed policy: only the recovery key opens the data.
    Recovery,
    /// Something was added (kernel parameters, credentials, add-ons).
    Changed,
}

/// The UKI profile of the recovery (image.rs builds it as profile 1).
const RECOVERY_PROFILE: &str = "1";

/// Reads PCR 12: what systemd-boot and systemd-stub add to the UKI's own
/// measurements (kernel parameters from SMBIOS or the boot loader,
/// credentials and add-ons from the ESP – for Beize nothing), then systemd's
/// OS separator. Starting a profile other than the first, systemd-boot
/// passes and measures "@<number>" as load options, and systemd-stub then
/// measures the number.
pub fn boot(pcr12: [u8; 32]) -> Boot {
    let separator: [u8; 32] = Sha256::digest(OS_SEPARATOR).into();
    let unchanged = [0u8; 32];
    let options = format!("@{RECOVERY_PROFILE}");
    let recovery = eventlog::extend(
        eventlog::extend(unchanged, Sha256::digest(utf16z(&options)).into()),
        Sha256::digest(utf16z(RECOVERY_PROFILE)).into(),
    );
    if pcr12 == unchanged || pcr12 == eventlog::extend(unchanged, separator) {
        Boot::Unchanged
    } else if pcr12 == recovery || pcr12 == eventlog::extend(recovery, separator) {
        Boot::Recovery
    } else {
        Boot::Changed
    }
}

/// UTF-16LE with the terminating NUL: how systemd-boot and systemd-stub
/// measure strings (`strsize16`).
fn utf16z(text: &str) -> Vec<u8> {
    text.encode_utf16()
        .chain([0])
        .flat_map(u16::to_le_bytes)
        .collect()
}

/// Checks the event log against the TPM's PCR 7 and the profile. The error
/// says, in plain words, why the state must not be approved.
pub fn check(events: &[Event], pcr7: [u8; 32], profile: &Profile) -> Result<(), String> {
    // The firmware's measurements, optionally followed by exactly one more:
    // systemd's OS separator. Anything else measured into PCR 7 refuses.
    let firmware = eventlog::replay(events, PCR_SECURE_BOOT);
    let separated = eventlog::extend(firmware, Sha256::digest(OS_SEPARATOR).into());
    if pcr7 != firmware && pcr7 != separated {
        return Err("the firmware's event log does not match the TPM (PCR 7)".into());
    }

    let mut secure_boot = None;
    let mut seen = BTreeSet::new();
    let mut beize_authority = false;
    for event in events.iter().filter(|e| e.pcr == PCR_SECURE_BOOT) {
        match event.event_type {
            EV_NO_ACTION | EV_SEPARATOR => continue,
            EV_EFI_VARIABLE_DRIVER_CONFIG | EV_EFI_VARIABLE_AUTHORITY => {}
            other => {
                return Err(format!(
                    "unexpected measurement of type {other:#x} in PCR 7"
                ));
            }
        }
        if <[u8; 32]>::from(Sha256::digest(&event.data)) != event.sha256 {
            return Err("a Secure Boot measurement does not match its logged data".into());
        }
        let var = efi::parse_variable(&event.data)?;

        if event.event_type == EV_EFI_VARIABLE_AUTHORITY {
            if var.name != "db" {
                return Err(format!(
                    "a boot binary was authorised by {:?}, not db",
                    var.name
                ));
            }
            let data = efi::authority_data(&var.data)?;
            let known = |set: &BTreeSet<Entry>| set.iter().any(|e| e.data == data);
            if !known(&profile.db) {
                return Err(
                    "a boot binary was authorised by a certificate outside the profile".into(),
                );
            }
            beize_authority |= known(&profile.beize);
            continue;
        }

        if !seen.insert(var.name.clone()) {
            return Err(format!("{} was measured twice", var.name));
        }
        let entries = || -> Result<BTreeSet<Entry>, String> {
            Ok(efi::signature_entries(&var.data)?.into_iter().collect())
        };
        match var.name.as_str() {
            "SecureBoot" => secure_boot = Some(var.data == [1]),
            "PK" if entries()? != profile.pk => {
                return Err("the platform key (PK) is not Beize's".into());
            }
            "KEK" if !entries()?.is_subset(&profile.kek) => {
                return Err("the key exchange keys (KEK) hold a key outside the profile".into());
            }
            "db" if !entries()?.is_subset(&profile.db) => {
                return Err(
                    "the signature database (db) holds a certificate outside the profile".into(),
                );
            }
            "PK" | "KEK" | "db" | "dbx" => {}
            // Optional databases; an empty one adds nothing.
            "dbt" | "dbr" if var.data.is_empty() => {}
            other => return Err(format!("unexpected Secure Boot variable {other:?}")),
        }
    }

    match secure_boot {
        Some(true) => {}
        Some(false) => return Err("Secure Boot is off".into()),
        None => return Err("the Secure Boot state was not measured".into()),
    }
    for required in ["PK", "KEK", "db"] {
        if !seen.contains(required) {
            return Err(format!("{required} was not measured"));
        }
    }
    if !beize_authority {
        return Err("Beize's boot chain was not authorised by Beize's certificate".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::efi::tests::{list, variable};
    use crate::eventlog::tests::log;

    fn entries(lists: &[Vec<u8>]) -> BTreeSet<Entry> {
        lists
            .iter()
            .flat_map(|l| efi::signature_entries(l).unwrap())
            .collect()
    }

    fn profile() -> Profile {
        Profile {
            pk: entries(&[list(1, b"beize-pk")]),
            kek: entries(&[list(1, b"beize-kek"), list(2, b"ms-kek")]),
            db: entries(&[list(1, b"beize-db"), list(2, b"ms-uefi-ca")]),
            beize: entries(&[list(1, b"beize-db")]),
        }
    }

    /// A PCR 7 log as firmware writes it, with `change` applied to the variables.
    fn measured(change: impl Fn(&str, Vec<u8>) -> Vec<u8>) -> Vec<Event> {
        let config = [
            ("SecureBoot", vec![1u8]),
            ("PK", list(1, b"beize-pk")),
            ("KEK", [list(1, b"beize-kek"), list(2, b"ms-kek")].concat()),
            (
                "db",
                [list(1, b"beize-db"), list(2, b"ms-uefi-ca")].concat(),
            ),
            ("dbx", list(2, b"revoked")),
        ]
        .map(|(name, data)| (name, variable(name, &change(name, data))));
        let mut authority = vec![9u8; 16];
        authority.extend(b"beize-db");
        let authority = variable("db", &authority);
        let mut records: Vec<(u32, u32, &[u8])> = config
            .iter()
            .map(|(_, v)| (7, EV_EFI_VARIABLE_DRIVER_CONFIG, v.as_slice()))
            .collect();
        records.push((7, EV_SEPARATOR, &[0, 0, 0, 0]));
        records.push((7, EV_EFI_VARIABLE_AUTHORITY, &authority));
        eventlog::parse(&log(&records)).unwrap()
    }

    fn verdict(events: &[Event]) -> Result<(), String> {
        check(events, eventlog::replay(events, 7), &profile())
    }

    #[test]
    fn approves_the_profile_and_a_grown_dbx() {
        assert_eq!(verdict(&measured(|_, d| d)), Ok(()));
        let longer_dbx = measured(|name, d| {
            if name == "dbx" {
                [d, list(2, b"also-revoked")].concat()
            } else {
                d
            }
        });
        assert_eq!(verdict(&longer_dbx), Ok(()));
    }

    #[test]
    fn refuses_foreign_keys_and_secure_boot_off() {
        let foreign_db = measured(|name, d| {
            if name == "db" {
                [d, list(1, b"attacker")].concat()
            } else {
                d
            }
        });
        assert!(verdict(&foreign_db).unwrap_err().contains("db"));
        let foreign_pk = measured(|name, d| {
            if name == "PK" {
                list(1, b"other-pk")
            } else {
                d
            }
        });
        assert!(verdict(&foreign_pk).unwrap_err().contains("PK"));
        let off = measured(|name, d| if name == "SecureBoot" { vec![0] } else { d });
        assert!(verdict(&off).unwrap_err().contains("Secure Boot is off"));
    }

    #[test]
    fn accepts_systemds_os_separator_once() {
        let events = measured(|_, d| d);
        let separator: [u8; 32] = Sha256::digest(OS_SEPARATOR).into();
        let once = eventlog::extend(eventlog::replay(&events, 7), separator);
        assert_eq!(check(&events, once, &profile()), Ok(()));
        let twice = eventlog::extend(once, separator);
        assert!(check(&events, twice, &profile()).is_err());
    }

    #[test]
    fn a_boot_is_unchanged_with_nothing_but_the_os_separator() {
        let separator: [u8; 32] = Sha256::digest(OS_SEPARATOR).into();
        let separated = eventlog::extend([0u8; 32], separator);
        assert_eq!(boot(separated), Boot::Unchanged);
        assert_eq!(boot([0u8; 32]), Boot::Unchanged);
        let added = eventlog::extend([0u8; 32], Sha256::digest(b"systemd.debug_shell").into());
        assert_eq!(boot(added), Boot::Changed);
        assert_eq!(boot(eventlog::extend(added, separator)), Boot::Changed);
    }

    #[test]
    fn the_recovery_profile_is_known_and_nothing_else() {
        let separator: [u8; 32] = Sha256::digest(OS_SEPARATOR).into();
        // systemd-boot's load options "@1", then systemd-stub's profile
        // number "1", each as UTF-16LE with NUL.
        let profile = |options: &[u8], number: &[u8]| {
            let loaded = eventlog::extend([0u8; 32], Sha256::digest(options).into());
            eventlog::extend(loaded, Sha256::digest(number).into())
        };
        let recovery = profile(&[0x40, 0, 0x31, 0, 0, 0], &[0x31, 0, 0, 0]);
        assert_eq!(boot(recovery), Boot::Recovery);
        assert_eq!(boot(eventlog::extend(recovery, separator)), Boot::Recovery);
        assert_eq!(
            boot(profile(&[0x40, 0, 0x32, 0, 0, 0], &[0x32, 0, 0, 0])),
            Boot::Changed
        );
        let without_options = eventlog::extend([0u8; 32], Sha256::digest([0x31, 0, 0, 0]).into());
        assert_eq!(boot(without_options), Boot::Changed);
        let added = eventlog::extend(recovery, Sha256::digest(b"systemd.debug_shell").into());
        assert_eq!(boot(added), Boot::Changed);
    }

    #[test]
    fn refuses_a_log_that_does_not_match_the_tpm() {
        let events = measured(|_, d| d);
        assert!(
            check(&events, [7u8; 32], &profile())
                .unwrap_err()
                .contains("PCR 7")
        );
    }

    #[test]
    fn refuses_unknown_measurements() {
        let mut events = measured(|_, d| d);
        let mut debug = events[0].clone();
        debug.event_type = 0x8000_0007; // EV_EFI_ACTION, e.g. "UEFI Debug Mode"
        events.push(debug);
        assert!(
            verdict(&events)
                .unwrap_err()
                .contains("unexpected measurement")
        );
    }
}
