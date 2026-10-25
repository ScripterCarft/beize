//! Which client is Beize's shell (ADR 0028 §1): only it may draw panels
//! (wlr-layer-shell) – no app gets to lay itself over the desktop.
//!
//! The shell is known by its unit, as xdg-desktop-portal knows a host app
//! by its `app-<id>@` unit: the process at the other end of the socket
//! runs in the cgroup of `beize-shell.service`, which sits next to the
//! compositor's own in `session.slice`. The kernel names that process as a
//! pidfd (`SO_PEERPIDFD`) and the pidfd its cgroup (`PIDFD_GET_INFO`),
//! with no race against an exiting process whose pid comes back – unlike
//! a pid and `/proc`, which the compositor's sandbox hides anyway
//! (`ProtectProc=invisible`). A cgroup's id is the inode of its directory.
//!
//! Who may move a process into that cgroup could also trace the shell
//! itself: the user's own unsandboxed programs. Sandboxed apps cannot
//! (`ProtectControlGroups`).

use std::ffi::{c_int, c_void};
use std::os::fd::{AsFd, AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::fs::MetadataExt;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;

use rustix::ioctl::{Updater, ioctl, opcode};

use crate::error::{Context, Result, error};

/// The shell's unit, next to the compositor's (ADR 0028 §1).
const SHELL_UNIT: &str = "beize-shell.service";

/// `SOL_SOCKET`, `SO_PEERPIDFD` (asm-generic/socket.h, Linux 6.5).
const SOL_SOCKET: c_int = 1;
const SO_PEERPIDFD: c_int = 77;

#[link(name = "c")]
unsafe extern "C" {
    fn getsockopt(fd: c_int, level: c_int, name: c_int, value: *mut c_void, len: *mut u32)
    -> c_int;
}

/// `struct pidfd_info` as first published (`PIDFD_INFO_SIZE_VER0`, Linux
/// 6.13; linux/pidfd.h): the kernel fills what fits into the size the
/// opcode names.
#[repr(C)]
#[derive(Debug, Default)]
struct PidfdInfo {
    mask: u64,
    cgroupid: u64,
    pid: u32,
    tgid: u32,
    ppid: u32,
    ruid: u32,
    rgid: u32,
    euid: u32,
    egid: u32,
    suid: u32,
    sgid: u32,
    fsuid: u32,
    fsgid: u32,
    exit_code: i32,
}

const _: () = assert!(size_of::<PidfdInfo>() == 64);
const PIDFD_INFO_CGROUPID: u64 = 1 << 2;
const PIDFD_GET_INFO: rustix::ioctl::Opcode = opcode::read_write::<PidfdInfo>(0xff, 11);

/// Whether the client at the other end of `stream` is the shell. Any doubt
/// (no shell running, an answer missing) says no.
pub fn is_shell(stream: &UnixStream) -> Result<bool> {
    let peer = cgroup_of(&peer_pidfd(stream)?)?;
    let shell = std::fs::metadata(shell_cgroup()?);
    Ok(shell.is_ok_and(|shell| shell.ino() == peer))
}

/// The process that connected, as a pidfd.
fn peer_pidfd(stream: &UnixStream) -> Result<OwnedFd> {
    let mut fd: c_int = -1;
    let mut len = size_of::<c_int>() as u32;
    // SAFETY: value and len point to an int and its size, as the option
    // wants; on success the kernel put a new file descriptor there.
    let r = unsafe {
        getsockopt(
            stream.as_raw_fd(),
            SOL_SOCKET,
            SO_PEERPIDFD,
            (&raw mut fd).cast(),
            &raw mut len,
        )
    };
    if r != 0 || fd < 0 {
        return Err(error(format!(
            "the client's process (SO_PEERPIDFD): {}",
            std::io::Error::last_os_error()
        )));
    }
    // SAFETY: the kernel just opened this descriptor for us; nothing else
    // owns it.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

/// The id of the cgroup `pidfd`'s process runs in.
fn cgroup_of(pidfd: &OwnedFd) -> Result<u64> {
    let mut info = PidfdInfo {
        mask: PIDFD_INFO_CGROUPID,
        ..PidfdInfo::default()
    };
    // SAFETY: the argument has the kernel's layout (VER0) and lives during
    // the call, which only writes into it.
    unsafe {
        ioctl(
            pidfd.as_fd(),
            Updater::<PIDFD_GET_INFO, PidfdInfo>::new(&mut info),
        )
    }
    .context("the client's cgroup (PIDFD_GET_INFO)")?;
    if info.mask & PIDFD_INFO_CGROUPID == 0 {
        return Err(error("the kernel names no cgroup for the client"));
    }
    Ok(info.cgroupid)
}

/// The shell's cgroup directory: the compositor's own, with the shell's
/// unit in place of its own.
fn shell_cgroup() -> Result<PathBuf> {
    let own = std::fs::read_to_string("/proc/self/cgroup").context("/proc/self/cgroup")?;
    // cgroup v2 only: one line, "0::/path".
    let path = own
        .lines()
        .find_map(|line| line.strip_prefix("0::"))
        .context("no cgroup v2 in /proc/self/cgroup")?;
    let slice = std::path::Path::new(path)
        .parent()
        .context("the compositor's cgroup has no parent")?;
    Ok(PathBuf::from("/sys/fs/cgroup")
        .join(slice.strip_prefix("/").unwrap_or(slice))
        .join(SHELL_UNIT))
}

#[cfg(test)]
mod tests {
    use super::*;

    // Whether the id matches the shell's directory, the smoke test shows:
    // the shell's menu bar appears, a test client gets no panels. Here,
    // without a cgroup tree in every build container, only the kernel's
    // answers.
    #[test]
    fn the_peer_is_named_with_its_cgroup() {
        // Both ends are this test.
        let (a, _b) = UnixStream::pair().expect("socket pair");
        let peer = cgroup_of(&peer_pidfd(&a).expect("pidfd")).expect("cgroup");
        let own = rustix::process::pidfd_open(
            rustix::process::getpid(),
            rustix::process::PidfdFlags::empty(),
        )
        .expect("own pidfd");
        assert_eq!(peer, cgroup_of(&own).expect("own cgroup"));
        assert_ne!(peer, 0);
    }
}
