//! Writing EFI variables through efivarfs, for the console keymap
//! (`keymap`) and the request to clear memory (`memory`).

use std::ffi::c_int;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::fd::AsRawFd;
use std::path::Path;

/// Non-volatile, readable at boot and from the running system.
pub const ATTRIBUTES: u32 = 0x1 | 0x2 | 0x4;

/// Writes `value` into the variable at `path`, creating it: efivarfs takes
/// attributes and value in a single write.
pub fn write(path: &str, value: &[u8]) -> Result<(), String> {
    if Path::new(path).exists() {
        make_mutable(path)?;
    }
    let mut data = ATTRIBUTES.to_le_bytes().to_vec();
    data.extend_from_slice(value);
    OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .and_then(|mut file| file.write_all(&data))
        .map_err(|e| format!("writing {path}: {e}"))
}

/// efivarfs marks existing variables immutable, so that `rm -rf` cannot
/// delete firmware settings by accident; changing one takes clearing that
/// flag first (as systemd and efivar do).
pub fn make_mutable(path: &str) -> Result<(), String> {
    const FS_IOC_GETFLAGS: u64 = 0x8008_6601;
    const FS_IOC_SETFLAGS: u64 = 0x4008_6602;
    const FS_IMMUTABLE_FL: c_int = 0x10;
    unsafe extern "C" {
        fn ioctl(fd: c_int, request: u64, ...) -> c_int;
    }
    let file = File::open(path).map_err(|e| format!("opening {path}: {e}"))?;
    let mut flags: c_int = 0;
    // SAFETY: both requests read or write one int through a valid pointer.
    unsafe {
        if ioctl(file.as_raw_fd(), FS_IOC_GETFLAGS, &raw mut flags) != 0 {
            return Err(format!(
                "reading the flags of {path}: {}",
                std::io::Error::last_os_error()
            ));
        }
        if flags & FS_IMMUTABLE_FL != 0 {
            flags &= !FS_IMMUTABLE_FL;
            if ioctl(file.as_raw_fd(), FS_IOC_SETFLAGS, &raw const flags) != 0 {
                return Err(format!(
                    "making {path} writable: {}",
                    std::io::Error::last_os_error()
                ));
            }
        }
    }
    Ok(())
}
