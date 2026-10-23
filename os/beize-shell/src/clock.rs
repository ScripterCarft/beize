//! The menu bar's clock: the local time, and a timer at the start of each
//! minute – also when the clock is set (NTS, ADR 0012), so the bar never
//! shows a stale minute.

use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rustix::io::Errno;
use rustix::time::{
    Itimerspec, TimerfdClockId, TimerfdFlags, TimerfdTimerFlags, Timespec, timerfd_create,
    timerfd_settime,
};

/// Hours and minutes, local time.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Time {
    pub hour: u8,
    pub minute: u8,
}

impl std::fmt::Display for Time {
    /// 24 hours, as C.UTF-8 (`%H:%M`): regional formats and the date come
    /// with the settings (ADR 0017).
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:02}:{:02}", self.hour, self.minute)
    }
}

unsafe extern "C" {
    /// glibc: reads the zone again if /etc/localtime changed. localtime_r
    /// alone keeps the zone it read first.
    fn tzset();
}

/// The local time now; None if the C library cannot say.
pub fn now() -> Option<Time> {
    let seconds = SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_secs();
    let t = libc::time_t::try_from(seconds).ok()?;
    // SAFETY: tm is plain data that localtime_r fills; tzset only reads
    // the environment and /etc/localtime (again, if timedated changed it).
    let tm = unsafe {
        tzset();
        let mut tm: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&t, &mut tm).is_null() {
            return None;
        }
        tm
    };
    Some(Time {
        hour: u8::try_from(tm.tm_hour).ok()?,
        minute: u8::try_from(tm.tm_min).ok()?,
    })
}

/// Readable at the start of every minute of the wall clock, and when the
/// clock is set.
pub struct Minutes {
    timer: OwnedFd,
}

impl Minutes {
    pub fn new() -> rustix::io::Result<Minutes> {
        let timer = timerfd_create(
            TimerfdClockId::Realtime,
            TimerfdFlags::CLOEXEC | TimerfdFlags::NONBLOCK,
        )?;
        let minutes = Minutes { timer };
        minutes.arm()?;
        Ok(minutes)
    }

    pub fn fd(&self) -> BorrowedFd<'_> {
        self.timer.as_fd()
    }

    /// After it was readable: takes the expiry and, if the clock was set,
    /// aligns the timer to the new minutes.
    pub fn take(&self) -> rustix::io::Result<()> {
        let mut count = [0u8; 8];
        match rustix::io::read(&self.timer, &mut count) {
            Ok(_) | Err(Errno::AGAIN) => Ok(()),
            Err(Errno::CANCELED) => self.arm(),
            Err(e) => Err(e),
        }
    }

    /// The next full minute, then every minute; cancelled when the clock is
    /// set (`CANCEL_ON_SET`).
    fn arm(&self) -> rustix::io::Result<()> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or(Duration::ZERO)
            .as_secs();
        let next = (now / 60 + 1) * 60;
        let spec = Itimerspec {
            it_interval: Timespec {
                tv_sec: 60,
                tv_nsec: 0,
            },
            it_value: Timespec {
                tv_sec: i64::try_from(next).unwrap_or(i64::MAX),
                tv_nsec: 0,
            },
        };
        timerfd_settime(
            &self.timer,
            TimerfdTimerFlags::ABSTIME | TimerfdTimerFlags::CANCEL_ON_SET,
            &spec,
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shows_hours_and_minutes_with_two_digits() {
        let t = Time { hour: 9, minute: 5 };
        assert_eq!(t.to_string(), "09:05");
    }

    #[test]
    fn knows_the_time_and_arms_the_timer() {
        let t = now().expect("local time");
        assert!(t.hour < 24 && t.minute < 60);
        let minutes = Minutes::new().expect("timer");
        // Not yet due (unless a minute starts right now): nothing to take.
        minutes.take().expect("take");
    }
}
