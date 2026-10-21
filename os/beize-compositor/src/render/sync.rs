//! Synchronisation without waiting on the CPU: a frame's end is a
//! sync_file, which KMS waits for (IN_FENCE_FD) and which goes into the
//! dmabufs a frame read or wrote. A client's dmabuf hands out its pending
//! writes the same way, and the GPU waits for them (as wlroots does).

use std::os::fd::{AsFd, BorrowedFd, FromRawFd, OwnedFd};

use rustix::event::{PollFd, PollFlags, Timespec, poll};
use rustix::ioctl::{Setter, Updater, ioctl, opcode};
use smithay::backend::renderer::sync::{Fence, Interrupted};

/// `struct dma_buf_export_sync_file` and `..._import_...` (linux/dma-buf.h).
#[repr(C)]
#[derive(Debug)]
struct SyncFileArg {
    flags: u32,
    fd: i32,
}

const DMA_BUF_SYNC_READ: u32 = 1;
const DMA_BUF_SYNC_WRITE: u32 = 2;
const EXPORT_SYNC_FILE: rustix::ioctl::Opcode = opcode::read_write::<SyncFileArg>(b'b', 2);
const IMPORT_SYNC_FILE: rustix::ioctl::Opcode = opcode::write::<SyncFileArg>(b'b', 3);

/// What a reader of `dmabuf` must wait for: its pending writes.
pub fn pending_writes(dmabuf: BorrowedFd<'_>) -> rustix::io::Result<OwnedFd> {
    let mut arg = SyncFileArg {
        flags: DMA_BUF_SYNC_READ,
        fd: -1,
    };
    // SAFETY: the argument has the kernel's layout and lives during the
    // call; on success the kernel returns a new file descriptor.
    unsafe {
        ioctl(
            dmabuf,
            Updater::<EXPORT_SYNC_FILE, SyncFileArg>::new(&mut arg),
        )?;
        Ok(OwnedFd::from_raw_fd(arg.fd))
    }
}

/// Adds `fence` to `dmabuf`: as a read (others' writes wait for it) or as
/// a write (everyone waits for it).
pub fn attach(
    dmabuf: BorrowedFd<'_>,
    fence: BorrowedFd<'_>,
    write: bool,
) -> rustix::io::Result<()> {
    let arg = SyncFileArg {
        flags: if write {
            DMA_BUF_SYNC_WRITE
        } else {
            DMA_BUF_SYNC_READ
        },
        fd: std::os::fd::AsRawFd::as_raw_fd(&fence),
    };
    // SAFETY: the argument has the kernel's layout; the kernel only reads
    // the fence, which stays open during the call.
    unsafe { ioctl(dmabuf, Setter::<IMPORT_SYNC_FILE, SyncFileArg>::new(arg)) }
}

/// The end of a frame, as Smithay's `SyncPoint` carries it.
#[derive(Debug)]
pub struct SyncFile(pub OwnedFd);

impl SyncFile {
    fn ready(&self, timeout: Option<&Timespec>) -> bool {
        let mut fds = [PollFd::new(&self.0, PollFlags::IN)];
        matches!(poll(&mut fds, timeout), Ok(n) if n > 0)
    }
}

impl Fence for SyncFile {
    fn is_signaled(&self) -> bool {
        self.ready(Some(&Timespec {
            tv_sec: 0,
            tv_nsec: 0,
        }))
    }

    fn wait(&self) -> Result<(), Interrupted> {
        loop {
            let mut fds = [PollFd::new(&self.0, PollFlags::IN)];
            match poll(&mut fds, None) {
                Ok(_) => return Ok(()),
                Err(rustix::io::Errno::INTR) => return Err(Interrupted),
                Err(rustix::io::Errno::AGAIN) => continue,
                Err(_) => return Ok(()),
            }
        }
    }

    fn is_exportable(&self) -> bool {
        true
    }

    fn export(&self) -> Option<OwnedFd> {
        self.0.as_fd().try_clone_to_owned().ok()
    }
}
