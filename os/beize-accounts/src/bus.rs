//! Method calls on the system bus through libsystemd's sd-bus – the library
//! systemd itself uses, in the image anyway. Only calls that take strings
//! and return nothing: all homed needs here.

use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::ptr;

#[repr(C)]
struct SdBusError {
    name: *const c_char,
    message: *const c_char,
    need_free: c_int,
}

#[link(name = "systemd")]
unsafe extern "C" {
    fn sd_bus_open_system(bus: *mut *mut c_void) -> c_int;
    fn sd_bus_flush_close_unref(bus: *mut c_void) -> *mut c_void;
    fn sd_bus_message_new_method_call(
        bus: *mut c_void,
        message: *mut *mut c_void,
        destination: *const c_char,
        path: *const c_char,
        interface: *const c_char,
        member: *const c_char,
    ) -> c_int;
    fn sd_bus_message_append_basic(
        message: *mut c_void,
        kind: c_char,
        value: *const c_void,
    ) -> c_int;
    fn sd_bus_message_sensitive(message: *mut c_void) -> c_int;
    fn sd_bus_message_set_allow_interactive_authorization(
        message: *mut c_void,
        allow: c_int,
    ) -> c_int;
    fn sd_bus_call(
        bus: *mut c_void,
        message: *mut c_void,
        timeout_usec: u64,
        error: *mut SdBusError,
        reply: *mut *mut c_void,
    ) -> c_int;
    fn sd_bus_message_unref(message: *mut c_void) -> *mut c_void;
    fn sd_bus_error_free(error: *mut SdBusError);
}

/// homed creates a home or re-hashes a password: allow for slow disks.
const CALL_TIMEOUT_USEC: u64 = 120_000_000;

/// A failed call: the D-Bus error name (e.g. org.freedesktop.home1.BadPassword)
/// and its message.
#[derive(Debug)]
pub struct CallError {
    pub name: String,
    pub message: String,
}

/// A connection to the system bus.
pub struct Bus(*mut c_void);

impl Bus {
    pub fn system() -> Result<Self, String> {
        let mut bus = ptr::null_mut();
        // SAFETY: a valid out pointer; on success sd-bus owns the connection
        // until sd_bus_flush_close_unref.
        let r = unsafe { sd_bus_open_system(&mut bus) };
        if r < 0 {
            return Err(format!(
                "cannot connect to the system bus ({})",
                std::io::Error::from_raw_os_error(-r)
            ));
        }
        Ok(Self(bus))
    }

    /// Calls `member` of `interface` on `path` at `destination` with string
    /// arguments. `sensitive`: the arguments are secrets, which sd-bus then
    /// erases from memory when it frees the message. Polkit may ask the
    /// caller to authenticate.
    pub fn call(
        &self,
        destination: &str,
        path: &str,
        interface: &str,
        member: &str,
        args: &[&str],
        sensitive: bool,
    ) -> Result<(), CallError> {
        let local = |message: String| CallError {
            name: String::new(),
            message,
        };
        let c = |s: &str| CString::new(s).map_err(|_| local("a NUL byte in a call".into()));
        let (destination, path, interface, member) =
            (c(destination)?, c(path)?, c(interface)?, c(member)?);
        let mut message = ptr::null_mut();
        // SAFETY: valid connection and NUL-terminated names; the message is
        // unreferenced below on every path.
        let r = unsafe {
            sd_bus_message_new_method_call(
                self.0,
                &mut message,
                destination.as_ptr(),
                path.as_ptr(),
                interface.as_ptr(),
                member.as_ptr(),
            )
        };
        if r < 0 {
            return Err(local(format!("cannot create the call {member:?}")));
        }
        let result = self.send(message, args, sensitive);
        // SAFETY: the message created above, not used afterwards.
        unsafe { sd_bus_message_unref(message) };
        result
    }

    fn send(&self, message: *mut c_void, args: &[&str], sensitive: bool) -> Result<(), CallError> {
        let local = |message: &str| CallError {
            name: String::new(),
            message: message.to_owned(),
        };
        // SAFETY (all calls below): `message` is a valid, unsent method call.
        if sensitive && unsafe { sd_bus_message_sensitive(message) } < 0 {
            return Err(local("cannot mark the call as sensitive"));
        }
        unsafe { sd_bus_message_set_allow_interactive_authorization(message, 1) };
        for arg in args {
            let arg = CString::new(*arg).map_err(|_| local("a NUL byte in an argument"))?;
            // SAFETY: for 's', sd-bus copies the NUL-terminated string.
            let r = unsafe {
                sd_bus_message_append_basic(message, b's' as c_char, arg.as_ptr().cast())
            };
            let mut bytes = arg.into_bytes();
            bytes.fill(0);
            if r < 0 {
                return Err(local("cannot add an argument to the call"));
            }
        }
        let mut error = SdBusError {
            name: ptr::null(),
            message: ptr::null(),
            need_free: 0,
        };
        // SAFETY: valid connection, message and error; no reply is wanted.
        let r = unsafe {
            sd_bus_call(
                self.0,
                message,
                CALL_TIMEOUT_USEC,
                &mut error,
                ptr::null_mut(),
            )
        };
        if r >= 0 {
            return Ok(());
        }
        let text = |p: *const c_char| {
            if p.is_null() {
                String::new()
            } else {
                // SAFETY: sd-bus sets NUL-terminated strings.
                unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
            }
        };
        let failed = CallError {
            name: text(error.name),
            message: if error.message.is_null() {
                std::io::Error::from_raw_os_error(-r).to_string()
            } else {
                text(error.message)
            },
        };
        // SAFETY: the error filled in by sd_bus_call.
        unsafe { sd_bus_error_free(&mut error) };
        Err(failed)
    }
}

impl Drop for Bus {
    fn drop(&mut self) {
        // SAFETY: the connection opened in `system`, not used afterwards.
        unsafe { sd_bus_flush_close_unref(self.0) };
    }
}
