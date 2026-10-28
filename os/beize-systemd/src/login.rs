//! logind for the Beize menu (ADR 0028 §3): sleep, restart, shut down and
//! log out, as GNOME asks logind over the system bus
//! (`org.freedesktop.login1`). polkit decides: logind's own rules let the
//! active local session do it without asking (`allow_active`); the shell
//! runs beside the session in the user's service manager, and polkit takes
//! the user's display session for it (`sd_uid_get_display`). logind elects
//! that display when a session starts, not when beize-session makes tty1 a
//! Wayland session (SetType): with an earlier login elsewhere it stays
//! there, and polkit asks for a password – a gap in logind, patch for
//! upstream prepared (CLAUDE.md §11).

use std::ffi::{CStr, c_char, c_int, c_void};
use std::ptr;

use super::{
    BusError, Message, c, errno, failure, sd_bus_call, sd_bus_flush_close_unref,
    sd_bus_message_append_basic, sd_bus_message_new_method_call, sd_bus_message_read_basic, take,
};

const LOGIN: &CStr = c"org.freedesktop.login1";
const LOGIN_PATH: &CStr = c"/org/freedesktop/login1";
const LOGIN_MANAGER: &CStr = c"org.freedesktop.login1.Manager";

#[link(name = "systemd")]
unsafe extern "C" {
    fn sd_bus_open_system(ret: *mut *mut c_void) -> c_int;
    fn sd_uid_get_display(uid: u32, session: *mut *mut c_char) -> c_int;
}

unsafe extern "C" {
    fn getuid() -> u32;
}

/// What the Beize menu asks of the computer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Power {
    Sleep,
    Restart,
    ShutDown,
}

impl Power {
    /// logind's method, and the one that asks whether it may.
    fn methods(self) -> (&'static CStr, &'static CStr) {
        match self {
            Power::Sleep => (c"Suspend", c"CanSuspend"),
            Power::Restart => (c"Reboot", c"CanReboot"),
            Power::ShutDown => (c"PowerOff", c"CanPowerOff"),
        }
    }
}

/// An argument of a call.
enum Arg<'a> {
    Bool(bool),
    Str(&'a str),
}

/// Calls `member` of logind's manager on the system bus with `args`;
/// `read` takes the reply.
fn call<T>(
    member: &CStr,
    args: &[Arg<'_>],
    read: impl FnOnce(*mut c_void) -> Result<T, String>,
) -> Result<T, String> {
    struct Bus(*mut c_void);
    impl Drop for Bus {
        fn drop(&mut self) {
            // SAFETY: a bus of ours, or null.
            unsafe { sd_bus_flush_close_unref(self.0) };
        }
    }
    let mut bus = Bus(ptr::null_mut());
    // SAFETY: valid out pointer; the bus is ours (`Bus`).
    let r = unsafe { sd_bus_open_system(&mut bus.0) };
    if r < 0 {
        return Err(format!("the system bus: {}", errno(r)));
    }
    let mut m = ptr::null_mut();
    // SAFETY: an open bus, NUL-terminated names, valid out pointer.
    let r = unsafe {
        sd_bus_message_new_method_call(
            bus.0,
            &mut m,
            LOGIN.as_ptr(),
            LOGIN_PATH.as_ptr(),
            LOGIN_MANAGER.as_ptr(),
            member.as_ptr(),
        )
    };
    if r < 0 {
        return Err(errno(r));
    }
    let message = Message(m);
    for arg in args {
        let r = match arg {
            Arg::Bool(b) => {
                let value = c_int::from(*b);
                // SAFETY: a message being built; for 'b' the value is an int.
                unsafe { sd_bus_message_append_basic(m, b'b' as c_char, (&raw const value).cast()) }
            }
            Arg::Str(s) => {
                let s = c(s)?;
                // SAFETY: a message being built; for 's' the value is the
                // string, copied by sd-bus.
                unsafe { sd_bus_message_append_basic(m, b's' as c_char, s.as_ptr().cast()) }
            }
        };
        if r < 0 {
            return Err(errno(r));
        }
    }
    let mut error = BusError {
        name: ptr::null(),
        message: ptr::null(),
        need_free: 0,
    };
    let mut reply = ptr::null_mut();
    // SAFETY: a complete message; the reply is ours (`Message`).
    let r = unsafe { sd_bus_call(bus.0, message.0, 0, &mut error, &mut reply) };
    if r < 0 {
        return Err(failure(r, &mut error));
    }
    let reply = Message(reply);
    read(reply.0)
}

/// Sleeps, restarts or shuts down the computer, without asking for a
/// password (`interactive` false: polkit's rules decide, as for GNOME).
pub fn power(what: Power) -> Result<(), String> {
    call(what.methods().0, &[Arg::Bool(false)], |_| Ok(()))
}

/// Whether `what` may be done now: logind's answer – "yes", "challenge"
/// (polkit would ask for a password), "no", "na" (the machine cannot).
pub fn can(what: Power) -> Result<String, String> {
    call(what.methods().1, &[], |reply| {
        let mut answer: *const c_char = ptr::null();
        // SAFETY: the reply of a Can* call, one string, alive as long as
        // the reply, so copied right away.
        unsafe {
            let r = sd_bus_message_read_basic(reply, b's' as c_char, (&raw mut answer).cast());
            if r <= 0 || answer.is_null() {
                return Err("no answer".into());
            }
            Ok(CStr::from_ptr(answer).to_string_lossy().into_owned())
        }
    })
}

/// Ends the desktop's session, as macOS' "Log Out": logind stops its
/// processes, the desktop with them. It is the one beize-session put into
/// the desktop's environment (`XDG_SESSION_ID`) – not logind's choice of the
/// user's display, which may be an earlier login elsewhere (a serial
/// console) – else, without it, that display.
pub fn log_out() -> Result<(), String> {
    let session = match std::env::var("XDG_SESSION_ID") {
        Ok(id) if !id.is_empty() => id,
        _ => {
            let mut session = ptr::null_mut();
            // SAFETY: valid out pointer; the result is ours (`take`).
            let r = unsafe { sd_uid_get_display(getuid(), &mut session) };
            if r < 0 {
                return Err(format!("the desktop's session: {}", errno(r)));
            }
            // SAFETY: allocated by sd_uid_get_display.
            unsafe { take(session) }.ok_or("no desktop session")?
        }
    };
    call(c"TerminateSession", &[Arg::Str(&session)], |_| Ok(()))
}
