//! libsystemd for Beize's desktop, which the image has anyway: sd-login for
//! the session beize-session runs in, sd-bus to the user's service manager
//! over its private socket (as `systemctl --user`, no session bus needed),
//! sd-event for the bus and the signals of the login. beize-session starts
//! the desktop with it, the compositor and the shell the apps (`apps`,
//! ADR 0027 §9).

pub mod apps;

use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::ptr;

const SIGHUP: c_int = 1;
const SIGINT: c_int = 2;
const SIGTERM: c_int = 15;
/// The signals that end a login: logind's SIGHUP and SIGTERM, Ctrl+C.
const SIGNALS: [c_int; 3] = [SIGHUP, SIGINT, SIGTERM];

const SYSTEMD: &CStr = c"org.freedesktop.systemd1";
const MANAGER_PATH: &CStr = c"/org/freedesktop/systemd1";
const MANAGER: &CStr = c"org.freedesktop.systemd1.Manager";
pub const UNIT: &str = "org.freedesktop.systemd1.Unit";

/// `sd_bus_error`; all null is SD_BUS_ERROR_NULL.
#[repr(C)]
struct BusError {
    name: *const c_char,
    message: *const c_char,
    need_free: c_int,
}

type MessageHandler = unsafe extern "C" fn(*mut c_void, *mut c_void, *mut BusError) -> c_int;
type SignalHandler = unsafe extern "C" fn(*mut c_void, *const c_void, *mut c_void) -> c_int;

#[link(name = "systemd")]
unsafe extern "C" {
    fn sd_pid_get_session(pid: c_int, session: *mut *mut c_char) -> c_int;
    fn sd_session_get_seat(session: *const c_char, seat: *mut *mut c_char) -> c_int;

    fn sd_bus_new(ret: *mut *mut c_void) -> c_int;
    fn sd_bus_set_address(bus: *mut c_void, address: *const c_char) -> c_int;
    fn sd_bus_start(bus: *mut c_void) -> c_int;
    fn sd_bus_attach_event(bus: *mut c_void, event: *mut c_void, priority: c_int) -> c_int;
    fn sd_bus_flush_close_unref(bus: *mut c_void) -> *mut c_void;
    fn sd_bus_message_new_method_call(
        bus: *mut c_void,
        ret: *mut *mut c_void,
        destination: *const c_char,
        path: *const c_char,
        interface: *const c_char,
        member: *const c_char,
    ) -> c_int;
    fn sd_bus_message_append_basic(m: *mut c_void, kind: c_char, value: *const c_void) -> c_int;
    fn sd_bus_message_append_strv(m: *mut c_void, list: *mut *mut c_char) -> c_int;
    fn sd_bus_message_read_basic(m: *mut c_void, kind: c_char, value: *mut c_void) -> c_int;
    fn sd_bus_message_enter_container(
        m: *mut c_void,
        kind: c_char,
        contents: *const c_char,
    ) -> c_int;
    fn sd_bus_message_exit_container(m: *mut c_void) -> c_int;
    fn sd_bus_message_skip(m: *mut c_void, types: *const c_char) -> c_int;
    fn sd_bus_message_unref(m: *mut c_void) -> *mut c_void;
    fn sd_bus_call(
        bus: *mut c_void,
        m: *mut c_void,
        usec: u64,
        error: *mut BusError,
        reply: *mut *mut c_void,
    ) -> c_int;
    fn sd_bus_get_property_string(
        bus: *mut c_void,
        destination: *const c_char,
        path: *const c_char,
        interface: *const c_char,
        member: *const c_char,
        error: *mut BusError,
        ret: *mut *mut c_char,
    ) -> c_int;
    fn sd_bus_match_signal(
        bus: *mut c_void,
        slot: *mut *mut c_void,
        sender: *const c_char,
        path: *const c_char,
        interface: *const c_char,
        member: *const c_char,
        callback: MessageHandler,
        userdata: *mut c_void,
    ) -> c_int;
    fn sd_bus_slot_unref(slot: *mut c_void) -> *mut c_void;
    fn sd_bus_error_free(e: *mut BusError);

    fn sd_event_default(ret: *mut *mut c_void) -> c_int;
    fn sd_event_add_signal(
        event: *mut c_void,
        source: *mut *mut c_void,
        signal: c_int,
        callback: SignalHandler,
        userdata: *mut c_void,
    ) -> c_int;
    fn sd_event_source_unref(source: *mut c_void) -> *mut c_void;
    fn sd_event_run(event: *mut c_void, timeout: u64) -> c_int;
    fn sd_event_unref(event: *mut c_void) -> *mut c_void;
}

/// glibc's `sigset_t`: 1024 bits.
#[repr(C)]
struct SigSet([u64; 16]);
const SIG_BLOCK: c_int = 0;

unsafe extern "C" {
    fn free(p: *mut c_void);
    fn sigemptyset(set: *mut SigSet) -> c_int;
    fn sigaddset(set: *mut SigSet, signal: c_int) -> c_int;
    fn sigprocmask(how: c_int, set: *const SigSet, old: *mut SigSet) -> c_int;
}

/// Blocks the signals that end a login for the rest of the process, as
/// sd_event_add_signal(3) asks: `watch` reads them through sd-event, and
/// outside it they wait unread. With SD_EVENT_SIGNAL_PROCMASK, sd-event
/// unblocked them when the watch ended; the terminal's SIGHUP a few
/// milliseconds after logind's SIGTERM (the login closes and hangs it up)
/// then ended beize-session before it took the environment back.
pub fn block_login_signals() -> Result<(), String> {
    let mut set = SigSet([0; 16]);
    // SAFETY: a set of our own; the mask of this (only) thread.
    let r = unsafe {
        sigemptyset(&mut set);
        for signal in SIGNALS {
            sigaddset(&mut set, signal);
        }
        sigprocmask(SIG_BLOCK, &set, ptr::null_mut())
    };
    if r < 0 {
        return Err(std::io::Error::last_os_error().to_string());
    }
    Ok(())
}

/// `-errno` as text.
fn errno(r: c_int) -> String {
    std::io::Error::from_raw_os_error(-r).to_string()
}

/// Takes a string libsystemd allocated.
///
/// # Safety
/// `p` is null or a NUL-terminated string from malloc, not used afterwards.
unsafe fn take(p: *mut c_char) -> Option<String> {
    if p.is_null() {
        return None;
    }
    // SAFETY: as the caller promises.
    let s = unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned();
    // SAFETY: as the caller promises.
    unsafe { free(p.cast()) };
    Some(s)
}

fn c(s: &str) -> Result<CString, String> {
    CString::new(s).map_err(|_| format!("a NUL byte in {s:?}"))
}

/// The logind session of this process: none for a service or a process
/// outside any login.
pub fn own_session() -> Result<String, String> {
    let mut id = ptr::null_mut();
    // SAFETY: valid out pointer; the result is ours (`take`).
    let r = unsafe { sd_pid_get_session(0, &mut id) };
    if r < 0 {
        return Err(errno(r));
    }
    // SAFETY: allocated by sd_pid_get_session.
    unsafe { take(id) }.ok_or_else(|| "no session".into())
}

/// The seat of `session`; a login over the network has none.
pub fn seat(session: &str) -> Result<String, String> {
    let session = c(session)?;
    let mut seat = ptr::null_mut();
    // SAFETY: NUL-terminated id, valid out pointer; the result is ours.
    let r = unsafe { sd_session_get_seat(session.as_ptr(), &mut seat) };
    if r < 0 {
        return Err(errno(r));
    }
    // SAFETY: allocated by sd_session_get_seat.
    unsafe { take(seat) }.ok_or_else(|| "no seat".into())
}

/// A D-Bus address with `path` escaped as systemd's bus_address_escape does.
fn unix_address(path: &str) -> String {
    let mut address = String::from("unix:path=");
    for b in path.bytes() {
        if b.is_ascii_alphanumeric() || b"-_/\\.".contains(&b) {
            address.push(char::from(b));
        } else {
            address.push_str(&format!("%{b:02x}"));
        }
    }
    address
}

/// The call failed: systemd's error message, else errno.
fn failure(r: c_int, error: &mut BusError) -> String {
    let text = if error.message.is_null() {
        errno(r)
    } else {
        // SAFETY: set by sd-bus, NUL-terminated, alive until freed below.
        unsafe { CStr::from_ptr(error.message) }
            .to_string_lossy()
            .into_owned()
    };
    // SAFETY: an error sd-bus filled in (or left empty), freed once.
    unsafe { sd_bus_error_free(error) };
    text
}

/// What [`Manager::watch`] reports to.
pub trait Watcher {
    /// The start job ended with `result`, `done` when the unit is up;
    /// false ends the watch.
    fn started(&mut self, result: &str) -> bool;
    /// The watched unit's ActiveState is now `state`; false ends the watch.
    fn changed(&mut self, state: &str) -> bool;
    /// The login ends (SIGHUP, SIGTERM) or Ctrl+C; false ends the watch.
    fn signal(&mut self, manager: &Manager) -> bool;
}

/// The user's service manager.
pub struct Manager {
    bus: *mut c_void,
    event: *mut c_void,
}

impl Manager {
    /// Connects to `$XDG_RUNTIME_DIR/systemd/private`.
    pub fn connect() -> Result<Self, String> {
        let dir = std::env::var("XDG_RUNTIME_DIR").map_err(|_| "XDG_RUNTIME_DIR is not set")?;
        let address = c(&unix_address(&format!("{dir}/systemd/private")))?;
        let mut manager = Self {
            bus: ptr::null_mut(),
            event: ptr::null_mut(),
        };
        // SAFETY: valid out pointers; what they get is released in Drop,
        // also on the error paths.
        unsafe {
            let r = sd_bus_new(&mut manager.bus);
            if r < 0 {
                return Err(errno(r));
            }
            let r = sd_bus_set_address(manager.bus, address.as_ptr());
            if r < 0 {
                return Err(errno(r));
            }
            let r = sd_bus_start(manager.bus);
            if r < 0 {
                return Err(errno(r));
            }
            let r = sd_event_default(&mut manager.event);
            if r < 0 {
                return Err(errno(r));
            }
            let r = sd_bus_attach_event(manager.bus, manager.event, 0);
            if r < 0 {
                return Err(errno(r));
            }
        }
        Ok(manager)
    }

    /// Calls `member` of the manager with string arguments, then a string
    /// list if given.
    fn call(&self, member: &CStr, args: &[&str], list: Option<&[String]>) -> Result<(), String> {
        self.call_reply(member, args, list, |_| Ok(()))
    }

    fn call_reply<T>(
        &self,
        member: &CStr,
        args: &[&str],
        list: Option<&[String]>,
        read: impl FnOnce(*mut c_void) -> Result<T, String>,
    ) -> Result<T, String> {
        let args = args.iter().map(|a| c(a)).collect::<Result<Vec<_>, _>>()?;
        let list = list
            .unwrap_or_default()
            .iter()
            .map(|a| c(a))
            .collect::<Result<Vec<_>, _>>()?;
        let mut m = ptr::null_mut();
        // SAFETY: a started bus, NUL-terminated names, valid out pointer.
        let r = unsafe {
            sd_bus_message_new_method_call(
                self.bus,
                &mut m,
                SYSTEMD.as_ptr(),
                MANAGER_PATH.as_ptr(),
                MANAGER.as_ptr(),
                member.as_ptr(),
            )
        };
        if r < 0 {
            return Err(errno(r));
        }
        let message = Message(m);
        for arg in &args {
            // SAFETY: a message being built; for 's' the value is the string.
            let r = unsafe { sd_bus_message_append_basic(m, b's' as c_char, arg.as_ptr().cast()) };
            if r < 0 {
                return Err(errno(r));
            }
        }
        if !list.is_empty() {
            let mut strv: Vec<*mut c_char> = list.iter().map(|s| s.as_ptr().cast_mut()).collect();
            strv.push(ptr::null_mut());
            // SAFETY: a NULL-terminated array of strings that outlive the
            // call; sd-bus only reads them.
            let r = unsafe { sd_bus_message_append_strv(m, strv.as_mut_ptr()) };
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
        let r = unsafe { sd_bus_call(self.bus, message.0, 0, &mut error, &mut reply) };
        if r < 0 {
            return Err(failure(r, &mut error));
        }
        let reply = Message(reply);
        read(reply.0)
    }

    /// Loads `unit` and gives its object path.
    pub fn unit_path(&self, unit: &str) -> Result<CString, String> {
        self.call_reply(c"LoadUnit", &[unit], None, |reply| {
            let mut path: *const c_char = ptr::null();
            // SAFETY: the reply of LoadUnit, one object path; it lives as
            // long as the reply, so it is copied right away.
            unsafe {
                let r = sd_bus_message_read_basic(reply, b'o' as c_char, (&raw mut path).cast());
                if r <= 0 || path.is_null() {
                    return Err("LoadUnit gave no path".into());
                }
                Ok(CStr::from_ptr(path).to_owned())
            }
        })
    }

    /// A string property of the unit at `path`.
    pub fn property(&self, path: &CStr, interface: &str, name: &str) -> Result<String, String> {
        let (interface, name) = (c(interface)?, c(name)?);
        let mut error = BusError {
            name: ptr::null(),
            message: ptr::null(),
            need_free: 0,
        };
        let mut value = ptr::null_mut();
        // SAFETY: NUL-terminated names, valid out pointers; the value is ours.
        let r = unsafe {
            sd_bus_get_property_string(
                self.bus,
                SYSTEMD.as_ptr(),
                path.as_ptr(),
                interface.as_ptr(),
                name.as_ptr(),
                &mut error,
                &mut value,
            )
        };
        if r < 0 {
            return Err(failure(r, &mut error));
        }
        // SAFETY: allocated by sd-bus.
        unsafe { take(value) }.ok_or_else(|| format!("no {name:?}"))
    }

    pub fn set_environment(&self, assignments: &[String]) -> Result<(), String> {
        self.call(c"SetEnvironment", &[], Some(assignments))
    }

    pub fn unset_environment(&self, names: &[String]) -> Result<(), String> {
        self.call(c"UnsetEnvironment", &[], Some(names))
    }

    /// Queues the start of `unit` and gives the job's object path; how the
    /// job ends, [`Watcher::started`] hears.
    pub fn start(&self, unit: &str) -> Result<CString, String> {
        self.call_reply(c"StartUnit", &[unit, "replace"], None, |reply| {
            let mut job: *const c_char = ptr::null();
            // SAFETY: the reply of StartUnit, one object path; it lives as
            // long as the reply, so it is copied right away.
            unsafe {
                let r = sd_bus_message_read_basic(reply, b'o' as c_char, (&raw mut job).cast());
                if r <= 0 || job.is_null() {
                    return Err("StartUnit gave no job".into());
                }
                Ok(CStr::from_ptr(job).to_owned())
            }
        })
    }

    pub fn stop(&self, unit: &str) -> Result<(), String> {
        self.call(c"StopUnit", &[unit, "replace"], None)
    }

    /// Tells `watcher` how the start job that `begin` queues ends, every
    /// change of the ActiveState of the unit at `path`, and the signals that
    /// end a login, until it says stop. `begin` runs once all are watched,
    /// so nothing is missed: what arrives during a call waits in the queue.
    ///
    /// The loop runs with sd_event_run rather than sd_event_loop: sd-bus
    /// closes a bus when the event loop it is attached to exits, and the
    /// caller still talks to the manager afterwards.
    pub fn watch(
        &self,
        path: &CStr,
        watcher: &mut dyn Watcher,
        begin: impl FnOnce(&Self) -> Result<CString, String>,
    ) -> Result<(), String> {
        let context = Box::into_raw(Box::new(Context {
            manager: self,
            watcher,
            job: None,
            done: false,
        }));
        // From here on the context is reached only through this pointer,
        // by the callbacks and by the loop below.
        let data = context.cast::<c_void>();
        let mut slots = Vec::new();
        let mut result = Ok(());
        for (path, interface, member, callback) in [
            (
                path,
                c"org.freedesktop.DBus.Properties",
                c"PropertiesChanged",
                properties_changed as MessageHandler,
            ),
            (MANAGER_PATH, MANAGER, c"JobRemoved", job_removed),
        ] {
            let mut slot = ptr::null_mut();
            // SAFETY: `data` outlives the slots and the sources: they are
            // released below before the context is freed, and the
            // callbacks only run inside sd_event_run.
            let r = unsafe {
                sd_bus_match_signal(
                    self.bus,
                    &mut slot,
                    // A direct connection has no sender names; only
                    // systemd is at the other end.
                    ptr::null(),
                    path.as_ptr(),
                    interface.as_ptr(),
                    member.as_ptr(),
                    callback,
                    data,
                )
            };
            if r < 0 {
                result = Err(errno(r));
                break;
            }
            slots.push(slot);
        }
        let mut sources = Vec::new();
        for signal in SIGNALS {
            if result.is_err() {
                break;
            }
            let mut source = ptr::null_mut();
            // SAFETY: as above.
            let r = unsafe {
                sd_event_add_signal(
                    self.event,
                    &mut source,
                    // Blocked since `block_login_signals`.
                    signal,
                    login_ends,
                    data,
                )
            };
            if r < 0 {
                result = Err(errno(r));
                break;
            }
            sources.push(source);
        }
        let result = result.and_then(|()| begin(self)).and_then(|job| {
            // SAFETY: no callback runs outside sd_event_run.
            unsafe { (*context).job = Some(job) };
            loop {
                // SAFETY: as above. The callbacks set `done` from within
                // sd_event_run, which got the pointer through the matches.
                if unsafe { (*context).done } {
                    return Ok(());
                }
                // SAFETY: the bus is attached to this event loop.
                let r = unsafe { sd_event_run(self.event, u64::MAX) };
                if r < 0 {
                    return Err(format!("waiting for the desktop: {}", errno(r)));
                }
            }
        });
        // SAFETY: created above, released once; nothing calls back after,
        // so the context can go.
        unsafe {
            for source in sources {
                sd_event_source_unref(source);
            }
            for slot in slots {
                sd_bus_slot_unref(slot);
            }
            drop(Box::from_raw(context));
        }
        result
    }
}

impl Drop for Manager {
    fn drop(&mut self) {
        // SAFETY: created in `connect` (or null, which both accept); the
        // bus goes first, as it holds the event loop.
        unsafe {
            sd_bus_flush_close_unref(self.bus);
            sd_event_unref(self.event);
        }
    }
}

struct Message(*mut c_void);

impl Drop for Message {
    fn drop(&mut self) {
        // SAFETY: a message we own (or null).
        unsafe { sd_bus_message_unref(self.0) };
    }
}

struct Context<'a> {
    manager: &'a Manager,
    watcher: &'a mut dyn Watcher,
    /// The start job, once `begin` queued it.
    job: Option<CString>,
    /// The watcher said stop.
    done: bool,
}

/// `PropertiesChanged(s interface, a{sv} changed, as invalidated)`: the new
/// ActiveState, if this is the unit interface and it changed.
///
/// # Safety
/// `m` is a PropertiesChanged signal being read.
unsafe fn active_state(m: *mut c_void) -> Option<String> {
    let unit = c(UNIT).ok()?;
    // SAFETY: as the caller promises; strings read from the message live
    // as long as it and are copied before it goes.
    unsafe {
        let mut interface: *const c_char = ptr::null();
        if sd_bus_message_read_basic(m, b's' as c_char, (&raw mut interface).cast()) <= 0
            || CStr::from_ptr(interface) != unit.as_c_str()
        {
            return None;
        }
        if sd_bus_message_enter_container(m, b'a' as c_char, c"{sv}".as_ptr()) <= 0 {
            return None;
        }
        while sd_bus_message_enter_container(m, b'e' as c_char, c"sv".as_ptr()) > 0 {
            let mut name: *const c_char = ptr::null();
            if sd_bus_message_read_basic(m, b's' as c_char, (&raw mut name).cast()) <= 0 {
                return None;
            }
            if CStr::from_ptr(name) == c"ActiveState" {
                let mut value: *const c_char = ptr::null();
                if sd_bus_message_enter_container(m, b'v' as c_char, c"s".as_ptr()) <= 0
                    || sd_bus_message_read_basic(m, b's' as c_char, (&raw mut value).cast()) <= 0
                {
                    return None;
                }
                return Some(CStr::from_ptr(value).to_string_lossy().into_owned());
            }
            if sd_bus_message_skip(m, c"v".as_ptr()) < 0 || sd_bus_message_exit_container(m) < 0 {
                return None;
            }
        }
    }
    None
}

unsafe extern "C" fn properties_changed(
    m: *mut c_void,
    data: *mut c_void,
    _error: *mut BusError,
) -> c_int {
    // SAFETY: `data` is the Context of the running `watch`; sd-event calls
    // back on its thread only, one callback at a time.
    let context = unsafe { &mut *data.cast::<Context>() };
    // SAFETY: the signal this match was made for.
    if let Some(state) = unsafe { active_state(m) }
        && !context.watcher.changed(&state)
    {
        context.done = true;
    }
    0
}

/// `JobRemoved(u id, o job, s unit, s result)`: the start job ended.
unsafe extern "C" fn job_removed(
    m: *mut c_void,
    data: *mut c_void,
    _error: *mut BusError,
) -> c_int {
    // SAFETY: as in `properties_changed`.
    let context = unsafe { &mut *data.cast::<Context>() };
    let Some(ours) = &context.job else {
        return 0;
    };
    let mut job: *const c_char = ptr::null();
    let mut result: *const c_char = ptr::null();
    // SAFETY: the signal this match was made for; its strings live as long
    // as the message and are only compared and copied here.
    let result = unsafe {
        if sd_bus_message_skip(m, c"u".as_ptr()) < 0
            || sd_bus_message_read_basic(m, b'o' as c_char, (&raw mut job).cast()) <= 0
            || CStr::from_ptr(job) != ours.as_c_str()
            || sd_bus_message_skip(m, c"s".as_ptr()) < 0
            || sd_bus_message_read_basic(m, b's' as c_char, (&raw mut result).cast()) <= 0
        {
            return 0;
        }
        CStr::from_ptr(result).to_string_lossy().into_owned()
    };
    if !context.watcher.started(&result) {
        context.done = true;
    }
    0
}

unsafe extern "C" fn login_ends(
    _source: *mut c_void,
    _info: *const c_void,
    data: *mut c_void,
) -> c_int {
    // SAFETY: as in `properties_changed`.
    let context = unsafe { &mut *data.cast::<Context>() };
    if !context.watcher.signal(context.manager) {
        context.done = true;
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_the_socket_path_as_systemd_does() {
        assert_eq!(
            unix_address("/run/user/1000/systemd/private"),
            "unix:path=/run/user/1000/systemd/private"
        );
        assert_eq!(unix_address("/a b;c"), "unix:path=/a%20b%3bc");
    }
}
