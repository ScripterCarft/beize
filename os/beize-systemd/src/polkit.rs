//! polkit's authentication agent for the shell (ADR 0031), as GNOME Shell
//! and COSMIC are one: registered for the user's display session on the
//! system bus, it hears `BeginAuthentication` and `CancelAuthentication`
//! from polkitd and answers once the user did (`Agent`). The password goes
//! to polkit's helper over its socket (`Helper`), never to polkitd or back
//! to the agent: the helper asks PAM and tells polkitd itself.
//!
//! Without libpolkit-agent-1 (GObject): both protocols are small and
//! stable, as polkit 128 has them (`polkitagentlistener.c`,
//! `polkitagentsession.c`, `polkitagenthelper-pam.c`). Who may call the
//! agent the bus decides, as for libpolkit-agent-1: polkit's bus policy
//! lets only polkitd's user send on the agent's interface.
//!
//! The session: the shell runs in the user's service manager, in no
//! session of its own, and polkitd takes the user's display session for
//! processes like it (`sd_uid_get_display`, polkit's session monitor) – for
//! the shell when it registers and for the apps that ask, the terminal's
//! run0 among them. So the agent registers for that session.

use std::collections::VecDeque;
use std::ffi::{CStr, c_char, c_int, c_void};
use std::io::{self, Read, Write};
use std::os::fd::{AsFd, BorrowedFd};
use std::os::unix::net::UnixStream;
use std::ptr;

use tracing::warn;

use super::{
    BusError, Message, MessageHandler, c, errno, failure, sd_bus_call, sd_bus_flush_close_unref,
    sd_bus_message_append_basic, sd_bus_message_enter_container, sd_bus_message_exit_container,
    sd_bus_message_new_method_call, sd_bus_message_read_basic, sd_bus_message_skip,
    sd_bus_slot_unref, take,
};

const AUTHORITY: &CStr = c"org.freedesktop.PolicyKit1";
const AUTHORITY_PATH: &CStr = c"/org/freedesktop/PolicyKit1/Authority";
const AUTHORITY_INTERFACE: &CStr = c"org.freedesktop.PolicyKit1.Authority";
const AGENT_INTERFACE: &CStr = c"org.freedesktop.PolicyKit1.AuthenticationAgent";
/// The agent's object, libpolkit-agent-1's default path.
const AGENT_PATH: &CStr = c"/org/freedesktop/PolicyKit1/AuthenticationAgent";
/// The error a dismissed dialog answers with (polkit's own name for it).
const CANCELLED: &CStr = c"org.freedesktop.PolicyKit1.Error.Cancelled";
/// The helper's socket (polkit-agent-helper.socket, polkit 126 on).
const HELPER: &str = "/run/polkit/agent-helper.socket";
/// A line from the helper longer than this ends the conversation: PAM's
/// messages are at most 512 bytes (`PAM_MAX_MSG_SIZE`), escaped up to four
/// times as long.
const LINE: usize = 4 * 512 + 64;

#[link(name = "systemd")]
unsafe extern "C" {
    fn sd_bus_open_system(ret: *mut *mut c_void) -> c_int;
    fn sd_uid_get_display(uid: u32, session: *mut *mut c_char) -> c_int;
    fn sd_pid_get_user_unit(pid: c_int, unit: *mut *mut c_char) -> c_int;
    fn sd_bus_add_object(
        bus: *mut c_void,
        slot: *mut *mut c_void,
        path: *const c_char,
        callback: MessageHandler,
        userdata: *mut c_void,
    ) -> c_int;
    fn sd_bus_get_fd(bus: *mut c_void) -> c_int;
    fn sd_bus_process(bus: *mut c_void, r: *mut *mut c_void) -> c_int;
    fn sd_bus_flush(bus: *mut c_void) -> c_int;
    fn sd_bus_message_ref(m: *mut c_void) -> *mut c_void;
    fn sd_bus_message_is_method_call(
        m: *mut c_void,
        interface: *const c_char,
        member: *const c_char,
    ) -> c_int;
    fn sd_bus_message_open_container(
        m: *mut c_void,
        kind: c_char,
        contents: *const c_char,
    ) -> c_int;
    fn sd_bus_message_close_container(m: *mut c_void) -> c_int;
    fn sd_bus_reply_method_return(call: *mut c_void, types: *const c_char, ...) -> c_int;
    fn sd_bus_reply_method_error(call: *mut c_void, e: *const BusError) -> c_int;
}

unsafe extern "C" {
    fn getuid() -> u32;
}

/// Who may allow an action, as polkitd names them; it has turned groups
/// into their users already.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Identity {
    User(u32),
    Group(u32),
    /// A kind this agent does not know.
    Other(String),
}

/// An action that waits for the user (`BeginAuthentication`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Request {
    pub action: String,
    /// What the action does, as its service says it ("Authentication is
    /// required to …"), its variables filled in by polkitd.
    pub message: String,
    pub icon: String,
    pub details: Vec<(String, String)>,
    pub cookie: String,
    pub identities: Vec<Identity>,
}

impl Request {
    pub fn detail(&self, key: &str) -> Option<&str> {
        self.details
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }

    /// The process the action is for: polkit's `polkit.subject-pid`.
    pub fn subject_pid(&self) -> Option<u32> {
        self.detail("polkit.subject-pid")?.parse().ok()
    }
}

/// What polkitd asked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    Begin(Request),
    /// The action with this cookie no longer waits: polkitd answers it
    /// itself (the app gave up).
    Cancel(String),
}

/// What the agent answers a request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Answer {
    /// The helper told polkitd the user authenticated.
    Done,
    /// The user dismissed the dialog, or it could not ask.
    Cancelled,
}

/// What the bus callback leaves for the agent: calls not yet answered by
/// their cookie, and what happened.
#[derive(Default)]
struct Inbox {
    waiting: Vec<(String, Message)>,
    events: VecDeque<Event>,
}

/// The shell as polkit's agent for its session.
pub struct Agent {
    bus: *mut c_void,
    slot: *mut c_void,
    /// Reached by the bus callback through a raw pointer while the agent
    /// lives; freed after the slot in Drop.
    inbox: *mut Inbox,
    session: String,
}

impl Agent {
    /// Registers with polkitd for the user's display session.
    pub fn register() -> Result<Agent, String> {
        let mut session = ptr::null_mut();
        // SAFETY: valid out pointer; the result is ours (`take`).
        let r = unsafe { sd_uid_get_display(getuid(), &mut session) };
        if r < 0 {
            return Err(format!("the user's display session: {}", errno(r)));
        }
        // SAFETY: allocated by sd_uid_get_display.
        let session = unsafe { take(session) }.ok_or("no display session")?;
        let mut agent = Agent {
            bus: ptr::null_mut(),
            slot: ptr::null_mut(),
            inbox: Box::into_raw(Box::default()),
            session,
        };
        // SAFETY: valid out pointers; the bus and slot are released in Drop,
        // also on the error paths; `inbox` outlives the slot (Drop).
        unsafe {
            let r = sd_bus_open_system(&mut agent.bus);
            if r < 0 {
                return Err(format!("the system bus: {}", errno(r)));
            }
            let r = sd_bus_add_object(
                agent.bus,
                &mut agent.slot,
                AGENT_PATH.as_ptr(),
                on_call,
                agent.inbox.cast(),
            );
            if r < 0 {
                return Err(format!("the agent's object: {}", errno(r)));
            }
        }
        agent.call_register()?;
        Ok(agent)
    }

    /// The session it is the agent of.
    pub fn session(&self) -> &str {
        &self.session
    }

    /// `RegisterAuthenticationAgent((sa{sv}) subject, s locale, s path)`,
    /// the subject the session.
    fn call_register(&self) -> Result<(), String> {
        let session = c(&self.session)?;
        // English, as Beize's text (ADR 0017); polkitd translates actions'
        // messages into it.
        let locale = c(&std::env::var("LANG").unwrap_or_else(|_| "C.UTF-8".into()))?;
        let mut m = ptr::null_mut();
        // SAFETY: an open bus, NUL-terminated names, valid out pointer.
        let r = unsafe {
            sd_bus_message_new_method_call(
                self.bus,
                &mut m,
                AUTHORITY.as_ptr(),
                AUTHORITY_PATH.as_ptr(),
                AUTHORITY_INTERFACE.as_ptr(),
                c"RegisterAuthenticationAgent".as_ptr(),
            )
        };
        if r < 0 {
            return Err(errno(r));
        }
        let message = Message(m);
        let s = |value: &CStr| {
            // SAFETY: a message being built; for 's' the value is the
            // string, copied by sd-bus.
            unsafe { sd_bus_message_append_basic(m, b's' as c_char, value.as_ptr().cast()) }
        };
        let open = |kind: u8, contents: &CStr| {
            // SAFETY: a message being built, NUL-terminated signature.
            unsafe { sd_bus_message_open_container(m, kind as c_char, contents.as_ptr()) }
        };
        // SAFETY: a message being built; closes what `open` opened.
        let close = || unsafe { sd_bus_message_close_container(m) };
        let steps = [
            open(b'r', c"sa{sv}"),
            s(c"unix-session"),
            open(b'a', c"{sv}"),
            open(b'e', c"sv"),
            s(c"session-id"),
            open(b'v', c"s"),
            s(&session),
            close(),
            close(),
            close(),
            close(),
            s(&locale),
            // The object path as a string: polkit's interface says `s`
            // (org.freedesktop.PolicyKit1.Authority.xml), not `o`.
            s(AGENT_PATH),
        ];
        if let Some(r) = steps.into_iter().find(|r| *r < 0) {
            return Err(errno(r));
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
            return Err(format!("registering: {}", failure(r, &mut error)));
        }
        drop(Message(reply));
        Ok(())
    }

    /// The bus connection, to wait on until it can be read.
    /// None once the connection is gone (`process` then fails).
    pub fn fd(&self) -> Option<BorrowedFd<'_>> {
        // SAFETY: an open bus.
        let fd = unsafe { sd_bus_get_fd(self.bus) };
        // SAFETY: the bus' socket, open as long as the bus (Drop).
        (fd >= 0).then(|| unsafe { BorrowedFd::borrow_raw(fd) })
    }

    /// Handles what arrived; what polkitd asked, in order.
    pub fn process(&mut self) -> Result<Vec<Event>, String> {
        loop {
            // SAFETY: an open bus; callbacks run in here, on this thread.
            let r = unsafe { sd_bus_process(self.bus, ptr::null_mut()) };
            if r < 0 {
                return Err(format!("the system bus: {}", errno(r)));
            }
            if r == 0 {
                break;
            }
        }
        // SAFETY: no callback runs outside sd_bus_process.
        let inbox = unsafe { &mut *self.inbox };
        Ok(inbox.events.drain(..).collect())
    }

    /// Answers the request with `cookie`, if it still waits.
    pub fn answer(&mut self, cookie: &str, answer: Answer) {
        // SAFETY: no callback runs outside sd_bus_process.
        let inbox = unsafe { &mut *self.inbox };
        let Some(i) = inbox.waiting.iter().position(|(c, _)| c == cookie) else {
            return;
        };
        let (_, call) = inbox.waiting.swap_remove(i);
        let r = match answer {
            // SAFETY: a method call of ours, answered once; no arguments.
            Answer::Done => unsafe { sd_bus_reply_method_return(call.0, c"".as_ptr()) },
            Answer::Cancelled => {
                let error = BusError {
                    name: CANCELLED.as_ptr(),
                    message: c"The authentication was cancelled".as_ptr(),
                    need_free: 0,
                };
                // SAFETY: as above; sd-bus copies the error.
                unsafe { sd_bus_reply_method_error(call.0, &error) }
            }
        };
        // SAFETY: an open bus.
        let flushed = unsafe { sd_bus_flush(self.bus) };
        if r < 0 || flushed < 0 {
            warn!("answering polkitd: {}", errno(r.min(flushed)));
        }
    }
}

impl Drop for Agent {
    fn drop(&mut self) {
        // SAFETY: created in `register` (or null, which both accept); the
        // slot goes before the inbox its callback reaches; leaving the bus
        // unregisters the agent (polkitd watches its name).
        unsafe {
            sd_bus_slot_unref(self.slot);
            drop(Box::from_raw(self.inbox));
            sd_bus_flush_close_unref(self.bus);
        }
    }
}

/// The agent's object: polkitd's calls.
unsafe extern "C" fn on_call(m: *mut c_void, data: *mut c_void, _error: *mut BusError) -> c_int {
    // SAFETY: `data` is the Agent's inbox, alive while the slot is; sd-bus
    // calls back on this thread only, from within `Agent::process`.
    let inbox = unsafe { &mut *data.cast::<Inbox>() };
    let is = |member: &CStr| {
        // SAFETY: the message this callback was given.
        unsafe { sd_bus_message_is_method_call(m, AGENT_INTERFACE.as_ptr(), member.as_ptr()) > 0 }
    };
    if is(c"BeginAuthentication") {
        // SAFETY: a BeginAuthentication call, read once from its start.
        match unsafe { read_begin(m) } {
            Some(request) => {
                // SAFETY: kept until answered (`Agent::answer`) or the inbox goes.
                let call = Message(unsafe { sd_bus_message_ref(m) });
                inbox.waiting.push((request.cookie.clone(), call));
                inbox.events.push_back(Event::Begin(request));
                1
            }
            // sd-bus answers with EINVAL's error.
            None => -22,
        }
    } else if is(c"CancelAuthentication") {
        // SAFETY: a CancelAuthentication call: its one string.
        let Some(cookie) = (unsafe { read_str(m) }) else {
            return -22;
        };
        inbox.events.push_back(Event::Cancel(cookie));
        // SAFETY: the call, answered once; no arguments.
        unsafe { sd_bus_reply_method_return(m, c"".as_ptr()) };
        1
    } else {
        // Not ours: sd-bus says it is unknown.
        0
    }
}

/// The next string or object path of `m`, copied.
///
/// # Safety
/// `m` is a message being read, its next field a string.
unsafe fn read_str(m: *mut c_void) -> Option<String> {
    let mut value: *const c_char = ptr::null();
    // SAFETY: as the caller promises; the string lives as long as the
    // message and is copied at once.
    unsafe {
        if sd_bus_message_read_basic(m, b's' as c_char, (&raw mut value).cast()) <= 0
            || value.is_null()
        {
            return None;
        }
        Some(CStr::from_ptr(value).to_string_lossy().into_owned())
    }
}

/// `BeginAuthentication(s action_id, s message, s icon_name, a{ss} details,
/// s cookie, a(sa{sv}) identities)`.
///
/// # Safety
/// `m` is that call, not read yet.
unsafe fn read_begin(m: *mut c_void) -> Option<Request> {
    // SAFETY: as the caller promises; each read follows the signature.
    unsafe {
        let action = read_str(m)?;
        let message = read_str(m)?;
        let icon = read_str(m)?;
        let mut details = Vec::new();
        if sd_bus_message_enter_container(m, b'a' as c_char, c"{ss}".as_ptr()) <= 0 {
            return None;
        }
        while sd_bus_message_enter_container(m, b'e' as c_char, c"ss".as_ptr()) > 0 {
            let key = read_str(m)?;
            let value = read_str(m)?;
            details.push((key, value));
            sd_bus_message_exit_container(m);
        }
        sd_bus_message_exit_container(m);
        let cookie = read_str(m)?;
        let mut identities = Vec::new();
        if sd_bus_message_enter_container(m, b'a' as c_char, c"(sa{sv})".as_ptr()) <= 0 {
            return None;
        }
        while sd_bus_message_enter_container(m, b'r' as c_char, c"sa{sv}".as_ptr()) > 0 {
            let kind = read_str(m)?;
            let mut id = None;
            if sd_bus_message_enter_container(m, b'a' as c_char, c"{sv}".as_ptr()) <= 0 {
                return None;
            }
            while sd_bus_message_enter_container(m, b'e' as c_char, c"sv".as_ptr()) > 0 {
                let key = read_str(m)?;
                let wanted = matches!(
                    (kind.as_str(), key.as_str()),
                    ("unix-user", "uid") | ("unix-group", "gid")
                );
                if wanted && sd_bus_message_enter_container(m, b'v' as c_char, c"u".as_ptr()) > 0 {
                    let mut value: u32 = 0;
                    if sd_bus_message_read_basic(m, b'u' as c_char, (&raw mut value).cast()) > 0 {
                        id = Some(value);
                    }
                    sd_bus_message_exit_container(m);
                } else if sd_bus_message_skip(m, c"v".as_ptr()) < 0 {
                    return None;
                }
                sd_bus_message_exit_container(m);
            }
            sd_bus_message_exit_container(m);
            sd_bus_message_exit_container(m);
            identities.push(match (kind.as_str(), id) {
                ("unix-user", Some(uid)) => Identity::User(uid),
                ("unix-group", Some(gid)) => Identity::Group(gid),
                _ => Identity::Other(kind),
            });
        }
        sd_bus_message_exit_container(m);
        Some(Request {
            action,
            message,
            icon,
            details,
            cookie,
            identities,
        })
    }
}

/// The user's own uid.
pub fn own_uid() -> u32 {
    // SAFETY: always succeeds.
    unsafe { getuid() }
}

/// The unit in the user's service manager the process `pid` runs in.
pub fn user_unit(pid: u32) -> Option<String> {
    let pid = c_int::try_from(pid).ok()?;
    let mut unit = ptr::null_mut();
    // SAFETY: valid out pointer; the result is ours (`take`).
    let r = unsafe { sd_pid_get_user_unit(pid, &mut unit) };
    if r < 0 {
        return None;
    }
    // SAFETY: allocated by sd_pid_get_user_unit.
    unsafe { take(unit) }
}

/// What the helper said.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Said {
    /// PAM asks: `echo` false for a password.
    Ask { prompt: String, echo: bool },
    /// `PAM_ERROR_MSG`.
    Error(String),
    /// `PAM_TEXT_INFO`.
    Info(String),
    /// It ended: authenticated (polkitd heard it from the helper) or not.
    Done(bool),
}

/// One conversation with polkit's helper: one attempt, as
/// libpolkit-agent-1's `PolkitAgentSession`; after a failure the next one
/// starts anew with the same cookie.
pub struct Helper {
    stream: UnixStream,
    read: Vec<u8>,
    ended: bool,
}

impl Helper {
    /// Connects and names the user to authenticate and the cookie; the
    /// helper knows the caller from the socket (`SO_PEERPIDFD`).
    pub fn start(user: &str, cookie: &str) -> io::Result<Helper> {
        let bad = |what| io::Error::new(io::ErrorKind::InvalidInput, what);
        if user.contains(['\n', '\0']) || cookie.contains(['\n', '\0']) {
            return Err(bad("a line break in the user or the cookie"));
        }
        let mut stream = UnixStream::connect(HELPER)?;
        stream.write_all(format!("{user}\n{cookie}\n").as_bytes())?;
        stream.set_nonblocking(true)?;
        Ok(Helper {
            stream,
            read: Vec::new(),
            ended: false,
        })
    }

    /// The socket, to wait on until it can be read.
    pub fn fd(&self) -> BorrowedFd<'_> {
        self.stream.as_fd()
    }

    /// What it said since last asked, in order; `Done` last.
    pub fn read(&mut self) -> Vec<Said> {
        let mut said = Vec::new();
        let mut buffer = [0u8; 1024];
        while !self.ended {
            match self.stream.read(&mut buffer) {
                Ok(0) => {
                    // Closed without its verdict: not authenticated.
                    said.push(Said::Done(false));
                    self.ended = true;
                }
                Ok(n) => {
                    self.read.extend_from_slice(&buffer[..n]);
                    while let Some(end) = self.read.iter().position(|&b| b == b'\n') {
                        let line: Vec<u8> = self.read.drain(..=end).collect();
                        let said_now = parse(&compress(&line[..end]));
                        self.ended = matches!(said_now, Said::Done(_));
                        said.push(said_now);
                        if self.ended {
                            break;
                        }
                    }
                    if self.read.len() > LINE {
                        warn!("polkit's helper: a line of more than {LINE} bytes");
                        said.push(Said::Done(false));
                        self.ended = true;
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => {
                    warn!("polkit's helper: {e}");
                    said.push(Said::Done(false));
                    self.ended = true;
                }
            }
        }
        said
    }

    /// Answers what PAM asked: one line. Nothing of it is kept here.
    pub fn answer(&mut self, text: &str) -> io::Result<()> {
        if text.contains(['\n', '\0']) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "a line break in the answer",
            ));
        }
        // Small, to a socket that holds far more: written at once, as
        // libpolkit-agent-1 writes it (`polkit_agent_session_response`).
        self.stream.set_nonblocking(false)?;
        let written = self
            .stream
            .write_all(text.as_bytes())
            .and_then(|()| self.stream.write_all(b"\n"));
        self.stream.set_nonblocking(true)?;
        written
    }
}

/// A line of the helper, unescaped as `g_strcompress` does: `\b \f \n \r
/// \t \v`, up to three octal digits, any other character after a backslash
/// as itself; a trailing backslash is dropped. As in C, the text ends at a
/// NUL.
fn compress(line: &[u8]) -> String {
    let mut out = Vec::with_capacity(line.len());
    let mut i = 0;
    while i < line.len() {
        let b = line[i];
        i += 1;
        if b != b'\\' {
            out.push(b);
            continue;
        }
        let Some(&next) = line.get(i) else {
            break;
        };
        i += 1;
        out.push(match next {
            b'0'..=b'7' => {
                let mut value = u32::from(next - b'0');
                let mut digits = 1;
                while digits < 3
                    && let Some(&d @ b'0'..=b'7') = line.get(i)
                {
                    value = value * 8 + u32::from(d - b'0');
                    i += 1;
                    digits += 1;
                }
                value as u8
            }
            b'b' => 0x08,
            b'f' => 0x0c,
            b'n' => b'\n',
            b'r' => b'\r',
            b't' => b'\t',
            b'v' => 0x0b,
            other => other,
        });
    }
    if let Some(nul) = out.iter().position(|&b| b == 0) {
        out.truncate(nul);
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// One unescaped line, as `io_watch_have_data` reads it.
fn parse(line: &str) -> Said {
    if let Some(prompt) = line.strip_prefix("PAM_PROMPT_ECHO_OFF ") {
        Said::Ask {
            prompt: prompt.to_owned(),
            echo: false,
        }
    } else if let Some(prompt) = line.strip_prefix("PAM_PROMPT_ECHO_ON ") {
        Said::Ask {
            prompt: prompt.to_owned(),
            echo: true,
        }
    } else if let Some(text) = line.strip_prefix("PAM_ERROR_MSG ") {
        Said::Error(text.to_owned())
    } else if let Some(text) = line.strip_prefix("PAM_TEXT_INFO ") {
        Said::Info(text.to_owned())
    } else if line.starts_with("SUCCESS") {
        Said::Done(true)
    } else {
        // FAILURE, and what libpolkit-agent-1 calls an unknown line.
        if !line.starts_with("FAILURE") {
            warn!("polkit's helper: an unknown line");
        }
        Said::Done(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unescapes_as_g_strcompress() {
        assert_eq!(compress(br"Password: "), "Password: ");
        assert_eq!(compress(br#"a\nb\tc\\d\"e"#), "a\nb\tc\\d\"e");
        assert_eq!(compress(br"\303\244"), "ä");
        assert_eq!(compress(br"\101BC"), "ABC");
        assert_eq!(compress(br"x\q"), "xq");
        assert_eq!(compress(br"end\"), "end");
        assert_eq!(compress(br"cut\0off"), "cut");
    }

    #[test]
    fn reads_the_helpers_lines() {
        assert_eq!(
            parse("PAM_PROMPT_ECHO_OFF Password: "),
            Said::Ask {
                prompt: "Password: ".into(),
                echo: false
            }
        );
        assert_eq!(
            parse("PAM_PROMPT_ECHO_ON Code: "),
            Said::Ask {
                prompt: "Code: ".into(),
                echo: true
            }
        );
        assert_eq!(parse("PAM_ERROR_MSG locked"), Said::Error("locked".into()));
        assert_eq!(parse("PAM_TEXT_INFO hi"), Said::Info("hi".into()));
        assert_eq!(parse("SUCCESS"), Said::Done(true));
        assert_eq!(parse("FAILURE"), Said::Done(false));
        assert_eq!(parse("what"), Said::Done(false));
    }

    #[test]
    fn a_conversation_ends_with_its_verdict() {
        let (ours, theirs) = UnixStream::pair().unwrap();
        ours.set_nonblocking(true).unwrap();
        let mut helper = Helper {
            stream: ours,
            read: Vec::new(),
            ended: false,
        };
        let mut other = theirs;
        other.write_all(b"PAM_PROMPT_ECHO_OFF Pass").unwrap();
        assert_eq!(helper.read(), vec![]);
        other.write_all(b"word: \n").unwrap();
        assert_eq!(
            helper.read(),
            vec![Said::Ask {
                prompt: "Password: ".into(),
                echo: false
            }]
        );
        helper.answer("secret").unwrap();
        let mut got = [0u8; 7];
        other.read_exact(&mut got).unwrap();
        assert_eq!(&got, b"secret\n");
        other.write_all(b"FAILURE\n").unwrap();
        drop(other);
        assert_eq!(helper.read(), vec![Said::Done(false)]);
        // Once ended, nothing more.
        assert_eq!(helper.read(), vec![]);
        assert!(helper.answer("a\nb").is_err());
    }
}
