//! The menu bar's clock: the local date and time, and a timer at the start
//! of each minute – also when the clock is set (NTS, ADR 0012), so the bar
//! never shows a stale minute.

use std::ffi::CStr;
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rustix::io::Errno;
use rustix::time::{
    Itimerspec, TimerfdClockId, TimerfdFlags, TimerfdTimerFlags, Timespec, timerfd_create,
    timerfd_settime,
};

/// Weekday, day, month and the time, as macOS' menu bar shows them: "Mon
/// 9 Jun 07:36", 24 hours. The C library names the day and month in the
/// shell's locale, C.UTF-8 – English – until Beize has languages (ADR
/// 0017, the owner: English for now, 25.10.); their order per language
/// (German "Mo. 9. Juni") comes with them.
const FORMAT: &CStr = c"%a %-d %b %H:%M";

unsafe extern "C" {
    /// glibc: reads the zone again if /etc/localtime changed. localtime_r
    /// alone keeps the zone it read first.
    fn tzset();
}

/// The local date and time now; None if the C library cannot say.
pub fn now() -> Option<String> {
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
    format(&tm)
}

/// `tm` in `FORMAT`.
fn format(tm: &libc::tm) -> Option<String> {
    let mut text = [0u8; 64];
    // SAFETY: strftime writes at most text's length, its end included, and
    // reads only tm and the format.
    let n = unsafe { libc::strftime(text.as_mut_ptr().cast(), text.len(), FORMAT.as_ptr(), tm) };
    // 0: it did not fit (no date is that long) or came out empty.
    (n > 0).then(|| String::from_utf8_lossy(&text[..n]).into_owned())
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
    fn shows_the_day_and_the_time_as_macos() {
        // SAFETY: tm is plain data.
        let mut tm: libc::tm = unsafe { std::mem::zeroed() };
        // Tuesday, 9 June 2026, 07:05.
        (tm.tm_year, tm.tm_mon, tm.tm_mday, tm.tm_wday) = (126, 5, 9, 2);
        (tm.tm_hour, tm.tm_min) = (7, 5);
        assert_eq!(format(&tm).as_deref(), Some("Tue 9 Jun 07:05"));
    }

    #[test]
    fn knows_the_time_and_arms_the_timer() {
        let t = now().expect("local time");
        assert!(t.ends_with(|c: char| c.is_ascii_digit()), "{t}");
        let minutes = Minutes::new().expect("timer");
        // Not yet due (unless a minute starts right now): nothing to take.
        minutes.take().expect("take");
    }
}
