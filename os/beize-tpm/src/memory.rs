//! The request to clear memory, kept true (TCG Platform Reset Attack
//! Mitigation). At every start the kernel asks the firmware to clear all
//! RAM before the next one (`CONFIG_RESET_ATTACK_MITIGATION`, the MOR
//! variable): whoever resets the running computer to read the disk's key
//! from memory with another system finds it cleared. The specification
//! leaves the other half to the system, as Windows does it: on an orderly
//! shutdown, once the secrets are out of memory, it withdraws the request,
//! and the next start is not slowed down by clearing gigabytes. The
//! kernel's own help says the option should be enabled only with that half.
//!
//! `beize-tpm wipe-keys VERB` runs last in an orderly shutdown
//! (`/usr/lib/systemd/system-shutdown/`), after systemd-shutdown unmounted
//! what it could and set the rest read-only. The root partition is still
//! open – the system runs from it –, so its key is still in the kernel:
//! every LUKS volume still active is suspended, which wipes its key
//! (libcryptsetup's `crypt_suspend`, what `cryptsetup luksSuspend` does and
//! Debian's cryptsetup-suspend does before standby). Only when every one
//! is wiped is the request withdrawn; otherwise it stays and the firmware
//! clears memory. A crash, a reset or a power cut never gets here: then
//! the firmware clears memory, which is what the request is for. Programs'
//! secrets (an unsealed TPM key) went with their memory, which the kernel
//! zeroes when it is freed (`CONFIG_INIT_ON_FREE_DEFAULT_ON`).

use std::ffi::{CString, c_char, c_int, c_void};
use std::fs;
use std::io::ErrorKind;
use std::ptr;

use crate::efivars;

/// MemoryOverwriteRequestControl, the TCG's variable; absent where the
/// firmware does not clear memory on request.
const REQUEST: &str =
    "/sys/firmware/efi/efivars/MemoryOverwriteRequestControl-e20939be-32d4-41be-a150-897f85d49829";
const DM_DEVICES: &str = "/sys/block";

#[link(name = "cryptsetup")]
unsafe extern "C" {
    fn crypt_init_by_name(cd: *mut *mut c_void, name: *const c_char) -> c_int;
    fn crypt_suspend(cd: *mut c_void, name: *const c_char) -> c_int;
    fn crypt_free(cd: *mut c_void);
}

/// `verb` is systemd-shutdown's: reboot, poweroff, halt or kexec.
pub fn run(verb: Option<&str>) -> Result<(), String> {
    match verb {
        Some("reboot" | "poweroff" | "halt") => {}
        // kexec keeps the memory for the next kernel on purpose.
        Some("kexec") => return Ok(()),
        other => {
            return Err(format!(
                "usage: beize-tpm wipe-keys reboot | poweroff | halt | kexec, not {other:?}"
            ));
        }
    }
    let volumes = luks_volumes()?;
    let mut failed = Vec::new();
    for name in &volumes {
        if let Err(e) = suspend(name) {
            failed.push(format!("{name}: {e}"));
        }
    }
    if !failed.is_empty() {
        println!(
            "Keys still in memory ({}): the firmware clears memory before the next start.",
            failed.join("; ")
        );
        return Ok(());
    }
    match fs::read(REQUEST) {
        Err(e) if e.kind() == ErrorKind::NotFound => {
            println!(
                "Wiped the keys of {} encrypted volume(s); the firmware clears no memory on \
                 request.",
                volumes.len()
            );
            return Ok(());
        }
        Err(e) => return Err(format!("reading {REQUEST}: {e}")),
        Ok(_) => {}
    }
    efivars::write(REQUEST, &[0])?;
    let now = fs::read(REQUEST).map_err(|e| format!("reading {REQUEST}: {e}"))?;
    if now.get(4) != Some(&0) {
        return Err(format!("the request to clear memory stayed set: {now:?}"));
    }
    println!(
        "Wiped the keys of {} encrypted volume(s); withdrew the request to clear memory.",
        volumes.len()
    );
    Ok(())
}

/// The device-mapper names of the LUKS volumes still active and not
/// suspended.
fn luks_volumes() -> Result<Vec<String>, String> {
    let entries = fs::read_dir(DM_DEVICES).map_err(|e| format!("reading {DM_DEVICES}: {e}"))?;
    let mut names = Vec::new();
    for entry in entries.flatten() {
        let dm = entry.path().join("dm");
        let read = |file: &str| fs::read_to_string(dm.join(file)).map(|s| s.trim().to_owned());
        let (Ok(uuid), Ok(name), Ok(suspended)) = (read("uuid"), read("name"), read("suspended"))
        else {
            continue;
        };
        if uuid.starts_with("CRYPT-LUKS") && suspended == "0" {
            names.push(name);
        }
    }
    names.sort();
    Ok(names)
}

/// Suspends the volume `name`: its I/O waits, its key is wiped from the
/// kernel.
fn suspend(name: &str) -> Result<(), String> {
    let name = CString::new(name).map_err(|_| "not a device name".to_owned())?;
    let mut cd = ptr::null_mut();
    // SAFETY: cd is an out pointer, name a NUL-terminated string that
    // outlives the calls; cd is freed once, after the last use.
    unsafe {
        let result = crypt_init_by_name(&mut cd, name.as_ptr());
        if result < 0 {
            return Err(format!("error {} opening it", -result));
        }
        let result = crypt_suspend(cd, name.as_ptr());
        crypt_free(cd);
        if result < 0 {
            return Err(format!("error {} suspending it", -result));
        }
    }
    Ok(())
}
