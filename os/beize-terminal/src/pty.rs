//! The PTY and the shell on it (ADR 0027 §9): the user's login shell, as
//! macOS's Terminal starts it, in a session of its own with the PTY as its
//! controlling terminal. The terminal waits for it in the window's poll:
//! the PTY's master for output, a pidfd for its end.

use std::ffi::OsString;
use std::io;
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::os::unix::process::CommandExt as _;
use std::path::Path;
use std::process::{Child, Command, Stdio};

use rustix::fs::{Mode, OFlags};
use rustix::process::{Pid, PidfdFlags};
use rustix::pty::OpenptFlags;
use rustix::termios::{self, InputModes, OptionalActions, Winsize};

/// What the shell's programs learn about the terminal: xterm's sequences
/// with 256 colours (as VTE, GNOME's terminal, says of itself), and 24-bit
/// colour.
const ENVIRONMENT: [(&str, &str); 2] = [("TERM", "xterm-256color"), ("COLORTERM", "truecolor")];

pub struct Pty {
    master: OwnedFd,
    child: Child,
    /// Readable once the shell has ended.
    pidfd: OwnedFd,
}

impl Pty {
    /// Starts the user's login shell on a new PTY of `size`.
    pub fn spawn(size: Winsize) -> io::Result<Pty> {
        let master =
            rustix::pty::openpt(OpenptFlags::RDWR | OpenptFlags::NOCTTY | OpenptFlags::CLOEXEC)?;
        rustix::pty::grantpt(&master)?;
        rustix::pty::unlockpt(&master)?;
        let name = rustix::pty::ptsname(&master, Vec::new())?;
        let slave = rustix::fs::open(
            name.as_c_str(),
            OFlags::RDWR | OFlags::NOCTTY | OFlags::CLOEXEC,
            Mode::empty(),
        )?;
        termios::tcsetwinsize(&master, size)?;
        // Backspace removes a whole UTF-8 character in the line editor of
        // programs that leave editing to the kernel (cat, read).
        let mut modes = termios::tcgetattr(&slave)?;
        modes.input_modes |= InputModes::IUTF8;
        termios::tcsetattr(&slave, OptionalActions::Now, &modes)?;
        rustix::fs::fcntl_setfl(&master, OFlags::NONBLOCK)?;

        // The service manager gives the app the user's SHELL and HOME (from
        // the user record).
        let shell = std::env::var_os("SHELL")
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "/bin/sh".into());
        let home = std::env::var_os("HOME").unwrap_or_else(|| "/".into());
        let mut command = Command::new(&shell);
        // A login shell: its name with a dash in front (login(1)).
        let mut name = OsString::from("-");
        name.push(Path::new(&shell).file_name().unwrap_or(shell.as_os_str()));
        command
            .arg0(name)
            .current_dir(home)
            .envs(ENVIRONMENT)
            .stdin(Stdio::from(slave.try_clone()?))
            .stdout(Stdio::from(slave.try_clone()?))
            .stderr(Stdio::from(slave));
        // SAFETY: between fork and exec only system calls, no allocation
        // or locks: a session of its own, whose controlling terminal is the
        // PTY on stdin.
        unsafe {
            command.pre_exec(|| {
                rustix::process::setsid()?;
                // SAFETY: fd 0 is the PTY's slave, set up by Command.
                let stdin = BorrowedFd::borrow_raw(0);
                rustix::process::ioctl_tiocsctty(stdin)?;
                Ok(())
            })
        };
        let child = command.spawn()?;
        let pid = Pid::from_raw(child.id() as i32).ok_or(io::ErrorKind::InvalidData)?;
        let pidfd = rustix::process::pidfd_open(pid, PidfdFlags::empty())?;
        Ok(Pty {
            master,
            child,
            pidfd,
        })
    }

    /// Readable when the shell wrote something (or the PTY closed).
    pub fn output(&self) -> BorrowedFd<'_> {
        self.master.as_fd()
    }

    /// Readable when the shell has ended.
    pub fn ended(&self) -> BorrowedFd<'_> {
        self.pidfd.as_fd()
    }

    /// What the shell wrote so far, appended to `buffer`, at most `limit`
    /// bytes. False when nothing more can come (the PTY closed).
    pub fn read(&self, buffer: &mut Vec<u8>, limit: usize) -> io::Result<bool> {
        let mut chunk = [0; 65536];
        while buffer.len() < limit {
            match rustix::io::read(&self.master, &mut chunk) {
                Ok(0) => return Ok(false),
                Ok(n) => buffer.extend_from_slice(&chunk[..n]),
                Err(rustix::io::Errno::AGAIN) => break,
                Err(rustix::io::Errno::INTR) => continue,
                // Linux: every slave closed.
                Err(rustix::io::Errno::IO) => return Ok(false),
                Err(e) => return Err(e.into()),
            }
        }
        Ok(true)
    }

    /// Writes as much of `bytes` as the PTY takes now; returns how much.
    pub fn write(&self, bytes: &[u8]) -> io::Result<usize> {
        let mut written = 0;
        while written < bytes.len() {
            match rustix::io::write(&self.master, &bytes[written..]) {
                Ok(n) => written += n,
                Err(rustix::io::Errno::AGAIN) => break,
                Err(rustix::io::Errno::INTR) => continue,
                Err(e) => return Err(e.into()),
            }
        }
        Ok(written)
    }

    /// Tells the shell's programs the new size (SIGWINCH).
    pub fn resize(&self, size: Winsize) -> io::Result<()> {
        termios::tcsetwinsize(&self.master, size)?;
        Ok(())
    }

    /// Collects the ended shell.
    pub fn wait(&mut self) -> io::Result<std::process::ExitStatus> {
        self.child.wait()
    }
}
