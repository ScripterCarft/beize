//! The authentication dialog (ADR 0031): the shell as polkit's agent for the
//! user's session (`beize_systemd::polkit`), as GNOME Shell is. An action
//! that needs the user's password opens `beize_ui::dialog` with a field on an
//! overlay that holds the keyboard; the password goes to polkit's helper and
//! nowhere else. One request at a time, the others wait (GNOME).
//!
//! The dialog names the app that asks, from the unit of the process the
//! action is for (`polkit.subject-pid`; apps run as `app-<id>@….service`,
//! `beize_systemd::apps`), with its name and icon as the dock has them, and
//! says what it wants from the action's message, macOS 26's way: "Terminal
//! wants to manage system services or other units." A wrong password empties
//! the field and says so (GNOME's words); the next attempt is a new
//! conversation with the helper, as libpolkit-agent-1 starts one.

use std::collections::VecDeque;
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::path::PathBuf;

use beize_systemd::polkit::{self, Agent, Answer, Event, Helper, Identity, Request, Said};
use beize_ui::dialog::{self, Badge, Dialog, Note, Symbol};
use beize_ui::field::Field;
use beize_ui::{App, AppIcon, Appearance, Events, Overlay, WindowId};
use tracing::{info, warn};
use zeroize::Zeroizing;

use crate::apps;

/// The lock above the dialog's title (recipes/fluent-emoji.toml).
const LOCK: &str = "/usr/share/beize/images/locked.png";
/// The longest paste read: more than a field takes is cut off anyway.
const PASTE_BYTES: usize = 4096;
const TRY_AGAIN: &str = "Sorry, that didn't work. Please try again.";

/// The request on screen.
struct Asking {
    request: Request,
    /// The login name the helper authenticates.
    user: String,
    window: WindowId,
    dialog: Dialog,
    /// The conversation with the helper of this attempt.
    helper: Option<Helper>,
    /// PAM asked, and waits for the answer.
    asked: bool,
    /// PAM asked at least once in this attempt: only then is a failure the
    /// user's, worth another attempt.
    prompted: bool,
    /// The user's answer, given before PAM asked for it.
    answer: Option<Zeroizing<String>>,
    /// PAM said what went wrong in this attempt.
    pam_error: bool,
    /// Configured, and drawn again when something changed.
    configured: bool,
    redraw: bool,
    /// A paste on its way: the clipboard's pipe and what came so far.
    paste: Option<(OwnedFd, Zeroizing<Vec<u8>>)>,
}

/// The agent, the request on screen and those that wait.
pub struct Auth {
    agent: Agent,
    waiting: VecDeque<Request>,
    asking: Option<Asking>,
}

/// What `Auth::watches` gave, by place: to find them in `ready`.
pub struct Watched {
    pub agent: Option<usize>,
    pub helper: Option<usize>,
    pub paste: Option<usize>,
}

impl Auth {
    /// Registers the agent; without it actions that need a password are
    /// declined, as before, which the log says.
    pub fn new() -> Option<Auth> {
        match Agent::register() {
            Ok(agent) => {
                info!("polkit agent for session {}", agent.session());
                Some(Auth {
                    agent,
                    waiting: VecDeque::new(),
                    asking: None,
                })
            }
            Err(e) => {
                warn!("no polkit agent: {e}");
                None
            }
        }
    }

    /// The dialog's window, while it shows.
    pub fn window(&self) -> Option<WindowId> {
        self.asking.as_ref().map(|a| a.window)
    }

    /// What to wait on, after `first` others: the bus, the helper, a paste.
    pub fn watches(&self, first: usize) -> (Vec<BorrowedFd<'_>>, Watched) {
        let mut fds: Vec<BorrowedFd<'_>> = self.agent.fd().into_iter().collect();
        let mut watched = Watched {
            agent: (!fds.is_empty()).then_some(first),
            helper: None,
            paste: None,
        };
        if let Some(asking) = &self.asking {
            if let Some(helper) = &asking.helper {
                watched.helper = Some(first + fds.len());
                fds.push(helper.fd());
            }
            if let Some((pipe, _)) = &asking.paste {
                watched.paste = Some(first + fds.len());
                fds.push(pipe.as_fd());
            }
        }
        (fds, watched)
    }

    /// Handles what happened: polkitd's calls, the helper's lines, a paste,
    /// and the dialog's input in `events`. False once the system bus is
    /// gone: polkitd forgets the agent with its name, and the shell drops it
    /// (libpolkit-agent-1 does not register again either).
    pub fn handle(
        &mut self,
        app: &mut App,
        ready: &[usize],
        watched: &Watched,
        events: Option<Events>,
        names: &mut impl FnMut(&str) -> apps::App,
    ) -> bool {
        if watched.agent.is_none_or(|i| ready.contains(&i)) {
            match self.agent.process() {
                Ok(events) => {
                    for event in events {
                        match event {
                            Event::Begin(request) => {
                                info!("polkit asks: {}", request.action);
                                self.waiting.push_back(request);
                            }
                            Event::Cancel(cookie) => self.cancelled(app, &cookie),
                        }
                    }
                }
                Err(e) => {
                    warn!("polkit agent ends: {e}");
                    if let Some(asking) = self.asking.take() {
                        app.close(asking.window);
                    }
                    return false;
                }
            }
        }
        if watched.helper.is_some_and(|i| ready.contains(&i)) {
            self.helper_said(app);
        }
        if watched.paste.is_some_and(|i| ready.contains(&i)) {
            self.receive_paste();
        }
        if let Some(events) = events {
            self.input(app, events);
        }
        if self.asking.is_none() {
            self.next(app, names);
        }
        if let Some(asking) = &mut self.asking
            && asking.dialog.set_caps_lock(app.modifiers().caps_lock)
        {
            asking.redraw = true;
        }
        true
    }

    /// The dialog drawn, if it waits to be.
    pub fn draw(&mut self, app: &mut App, appearance: Appearance) -> Result<(), String> {
        let Some(asking) = &mut self.asking else {
            return Ok(());
        };
        if !(asking.configured && asking.redraw && app.ready(asking.window)) {
            return Ok(());
        }
        asking.redraw = false;
        let size = app.size(asking.window);
        app.set_glass(asking.window, Some(Dialog::glass(size)));
        let scene = asking.dialog.draw(app.text(), size, appearance);
        app.draw(asking.window, &scene).map_err(|e| e.to_string())
    }

    /// Draws again in the new appearance.
    pub fn appearance_changed(&mut self) {
        if let Some(asking) = &mut self.asking {
            asking.redraw = true;
        }
    }

    /// The next request that waits, on screen; one that cannot be asked is
    /// declined.
    fn next(&mut self, app: &mut App, names: &mut impl FnMut(&str) -> apps::App) {
        while self.asking.is_none() {
            let Some(request) = self.waiting.pop_front() else {
                return;
            };
            let (action, cookie) = (request.action.clone(), request.cookie.clone());
            match open(app, request, names) {
                Ok(asking) => self.asking = Some(asking),
                Err(e) => {
                    warn!("authentication for {action} declined: {e}");
                    self.agent.answer(&cookie, Answer::Cancelled);
                }
            }
        }
    }

    /// polkitd no longer waits for `cookie`.
    fn cancelled(&mut self, app: &mut App, cookie: &str) {
        if self
            .asking
            .as_ref()
            .is_some_and(|a| a.request.cookie == cookie)
        {
            info!("authentication cancelled by polkit");
            self.close(app, Answer::Cancelled);
        } else if let Some(i) = self.waiting.iter().position(|r| r.cookie == cookie) {
            self.waiting.remove(i);
            self.agent.answer(cookie, Answer::Cancelled);
        }
    }

    /// Answers the request on screen and closes its dialog.
    fn close(&mut self, app: &mut App, answer: Answer) {
        if let Some(asking) = self.asking.take() {
            app.close(asking.window);
            self.agent.answer(&asking.request.cookie, answer);
            // The helper's conversation ends with its socket.
        }
    }

    fn helper_said(&mut self, app: &mut App) {
        let Some(asking) = &mut self.asking else {
            return;
        };
        let said = asking.helper.as_mut().map(Helper::read).unwrap_or_default();
        for said in said {
            match said {
                Said::Ask { prompt, echo } => {
                    asking.asked = true;
                    asking.prompted = true;
                    asking.dialog.set_prompt(&prompt, echo);
                    asking.redraw = true;
                    match asking.answer.take() {
                        Some(answer) => send(asking, &answer),
                        // Asked again in the same conversation (another
                        // factor): the field takes the answer, as GNOME's.
                        None => asking.dialog.set_busy(false),
                    }
                }
                Said::Error(text) => {
                    asking.pam_error = true;
                    note(app, asking, Some((text, true)));
                }
                Said::Info(text) => note(app, asking, Some((text, false))),
                Said::Done(true) => {
                    info!("authentication: allowed");
                    self.close(app, Answer::Done);
                    return;
                }
                Said::Done(false) if !asking.prompted => {
                    // The helper failed before PAM asked anything: another
                    // attempt would fail alike, at once and again
                    // (pkttyagent never retries).
                    warn!("authentication: polkit's helper failed before asking; declined");
                    self.close(app, Answer::Cancelled);
                    return;
                }
                Said::Done(false) => {
                    info!("authentication: failed");
                    if !asking.pam_error {
                        note(app, asking, Some((TRY_AGAIN.to_owned(), true)));
                    }
                    // A new attempt, as GNOME's dialog starts one.
                    asking.dialog.set_busy(false);
                    // The field has the keys again, whatever Tab picked.
                    asking.dialog.focus_field();
                    asking.redraw = true;
                    asking.asked = false;
                    asking.prompted = false;
                    asking.answer = None;
                    asking.pam_error = false;
                    asking.helper = match Helper::start(&asking.user, &asking.request.cookie) {
                        Ok(helper) => Some(helper),
                        Err(e) => {
                            warn!("polkit's helper: {e}");
                            None
                        }
                    };
                    if asking.helper.is_none() {
                        self.close(app, Answer::Cancelled);
                        return;
                    }
                    break;
                }
            }
        }
    }

    fn input(&mut self, app: &mut App, events: Events) {
        let Some(asking) = &mut self.asking else {
            return;
        };
        asking.configured |= events.configured;
        asking.redraw |= events.configured;
        if events.closed {
            info!("authentication: dismissed");
            self.close(app, Answer::Cancelled);
            return;
        }
        let size = app.size(asking.window);
        for mut input in events.input {
            let outcome = asking.dialog.input(&input, size);
            // What a key typed, a character of the password perhaps: gone
            // with the key, not left in freed memory.
            if let beize_ui::Input::Key(key) = &mut input
                && let Some(text) = key.text.take()
            {
                drop(Zeroizing::new(text));
            }
            match outcome {
                dialog::Outcome::None => {}
                dialog::Outcome::Changed => asking.redraw = true,
                dialog::Outcome::Paste => match app.paste() {
                    Ok(Some(pipe)) => {
                        asking.paste =
                            Some((pipe, Zeroizing::new(Vec::with_capacity(PASTE_BYTES))));
                    }
                    Ok(None) => {}
                    Err(e) => warn!("pasting: {e}"),
                },
                dialog::Outcome::Confirmed => {
                    let answer = asking.dialog.take_answer().unwrap_or_default();
                    asking.dialog.set_busy(true);
                    asking.redraw = true;
                    if asking.asked {
                        send(asking, &answer);
                    } else {
                        asking.answer = Some(answer);
                    }
                }
                dialog::Outcome::Cancelled => {
                    info!("authentication: cancelled");
                    self.close(app, Answer::Cancelled);
                    return;
                }
            }
        }
    }

    /// Reads what the clipboard sent; once all is there, into the field.
    fn receive_paste(&mut self) {
        let Some(asking) = &mut self.asking else {
            return;
        };
        let Some((pipe, text)) = &mut asking.paste else {
            return;
        };
        let mut chunk = Zeroizing::new([0u8; 512]);
        loop {
            match rustix::io::read(&*pipe, &mut chunk[..]) {
                Ok(0) => break,
                Ok(n) => {
                    let room = PASTE_BYTES - text.len();
                    text.extend_from_slice(&chunk[..n.min(room)]);
                }
                Err(rustix::io::Errno::AGAIN) => return,
                Err(rustix::io::Errno::INTR) => continue,
                Err(e) => {
                    warn!("reading the clipboard: {e}");
                    asking.paste = None;
                    return;
                }
            }
        }
        if let Some((_, text)) = asking.paste.take() {
            let pasted = Zeroizing::new(String::from_utf8_lossy(&text).into_owned());
            if asking.dialog.paste(&pasted) == dialog::Outcome::Changed {
                asking.redraw = true;
            }
        }
    }
}

/// The answer to the helper; the dialog waits for its verdict.
fn send(asking: &mut Asking, answer: &str) {
    asking.asked = false;
    if let Some(helper) = &mut asking.helper
        && let Err(e) = helper.answer(answer)
    {
        warn!("polkit's helper: {e}");
    }
}

/// The note below the field, the dialog's height with it.
fn note(app: &mut App, asking: &mut Asking, note: Option<(String, bool)>) {
    asking
        .dialog
        .set_note(note.map(|(text, error)| Note { text, error }));
    let size = asking.dialog.size(app.text());
    if size != app.size(asking.window) {
        // Drawn once the compositor configures the new size.
        app.set_panel_size(asking.window, size);
    }
    asking.redraw = true;
}

/// The dialog for `request`, its conversation with the helper begun.
fn open(
    app: &mut App,
    request: Request,
    names: &mut impl FnMut(&str) -> apps::App,
) -> Result<Asking, String> {
    let Some((uid, user)) = identity(&request.identities) else {
        return Err("no user polkit names has a login".into());
    };
    let helper = match Helper::start(&user, &request.cookie) {
        Ok(helper) => helper,
        Err(e) => return Err(format!("polkit's helper: {e}")),
    };
    let asker_id = request
        .subject_pid()
        .and_then(polkit::user_unit)
        .and_then(|unit| app_id(&unit));
    let asker = asker_id
        .as_deref()
        .map(&mut *names)
        .filter(|a| !a.name.is_empty());
    let title = asker
        .as_ref()
        .map_or("Authentication Required".to_owned(), |a| a.name.clone());
    let what = wants(asker.as_ref().map(|a| a.name.as_str()), &request.message);
    let how = if uid == polkit::own_uid() {
        "Enter your password to allow this.".to_owned()
    } else {
        format!("Enter the password of {user} to allow this.")
    };
    // The app's icon as the dock shows it, scaled to the badge.
    let badge = asker.as_ref().and_then(|a| {
        let own = asker_id.as_deref().and_then(AppIcon::of).map(Badge::Own);
        own.or_else(|| a.icon.clone().map(Badge::File))
    });
    info!("authentication: {title} | {what} | {how}");
    // For the test: which badge the lock carries.
    match &badge {
        Some(Badge::Own(icon)) => info!("authentication badge: {}", icon.motif),
        Some(Badge::File(file)) => info!("authentication badge: {}", file.display()),
        None => info!("authentication badge: none"),
    }
    let dialog = Dialog::new(title, what, "Allow")
        .with_line(how)
        .with_symbol(Symbol {
            image: PathBuf::from(LOCK),
            badge,
        })
        .with_field(Field::password());
    let overlay = Overlay {
        namespace: "authentication",
        size: dialog.size(app.text()),
        keyboard: true,
    };
    let window = match app.overlay(&overlay) {
        Ok(window) => window,
        Err(e) => return Err(format!("its window: {e}")),
    };
    Ok(Asking {
        request,
        user,
        window,
        dialog,
        helper: Some(helper),
        asked: false,
        prompted: false,
        answer: None,
        pam_error: false,
        configured: false,
        redraw: false,
        paste: None,
    })
}

/// Who authenticates: the user, if polkit names them (the owner is in
/// `wheel`), else the one user it names first, by login.
fn identity(identities: &[Identity]) -> Option<(u32, String)> {
    let uid = chosen(identities, polkit::own_uid())?;
    login(uid).map(|name| (uid, name))
}

/// Of the users polkit names, `own` if it is one, else the first.
fn chosen(identities: &[Identity], own: u32) -> Option<u32> {
    let users: Vec<u32> = identities
        .iter()
        .filter_map(|i| match i {
            Identity::User(uid) => Some(*uid),
            _ => None,
        })
        .collect();
    if users.contains(&own) {
        Some(own)
    } else {
        users.first().copied()
    }
}

/// The login name of `uid`.
fn login(uid: u32) -> Option<String> {
    // SAFETY: `passwd` is plain data (pointers and integers); all zero is
    // valid for it, and getpwuid_r fills it in.
    let mut entry: libc::passwd = unsafe { std::mem::zeroed() };
    let mut buffer = vec![0 as libc::c_char; 4096];
    let mut found = std::ptr::null_mut();
    // SAFETY: valid pointers to an entry, a buffer of its length and the
    // result; the entry's strings point into the buffer, read before it goes.
    let r = unsafe {
        libc::getpwuid_r(
            uid,
            &mut entry,
            buffer.as_mut_ptr(),
            buffer.len(),
            &mut found,
        )
    };
    if r != 0 || found.is_null() || entry.pw_name.is_null() {
        return None;
    }
    // SAFETY: set by getpwuid_r, NUL-terminated, in `buffer`.
    let name = unsafe { std::ffi::CStr::from_ptr(entry.pw_name) };
    Some(name.to_string_lossy().into_owned())
}

/// The app of a unit in the user's service manager: `app-<id>@<random>.service`
/// as Beize starts apps (systemd's desktop integration), its id unescaped
/// as `systemd-escape` escaped it.
fn app_id(unit: &str) -> Option<String> {
    let name = unit.strip_prefix("app-")?;
    let (escaped, _) = name.split_once('@')?;
    let mut bytes = Vec::with_capacity(escaped.len());
    let mut rest = escaped.as_bytes();
    while let Some((&b, tail)) = rest.split_first() {
        match (b, tail) {
            // Only `\xNN`, as systemd's unescaping takes it.
            (b'\\', tail) => {
                let [b'x', h, l, more @ ..] = tail else {
                    return None;
                };
                let hex = std::str::from_utf8(&[*h, *l]).ok()?.to_owned();
                bytes.push(u8::from_str_radix(&hex, 16).ok()?);
                rest = more;
            }
            (b'-', _) => {
                bytes.push(b'/');
                rest = tail;
            }
            _ => {
                bytes.push(b);
                rest = tail;
            }
        }
    }
    String::from_utf8(bytes).ok().filter(|id| !id.is_empty())
}

/// What the app wants, from the action's message: "Authentication is
/// required to …" and "… required for an application to …" become "<App>
/// wants to …" (133 of Beize's 134 actions); another stays as it is, and
/// so does every one without an app.
fn wants(app: Option<&str>, message: &str) -> String {
    let Some(app) = app else {
        return message.to_owned();
    };
    let rest = message
        .strip_prefix("Authentication is required to ")
        .or_else(|| {
            const FOR_AN_APP: &str = "required for an application to ";
            message
                .find(FOR_AN_APP)
                .map(|i| &message[i + FOR_AN_APP.len()..])
        });
    match rest {
        Some(rest) => format!("{app} wants to {rest}"),
        None => message.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn says_what_the_app_wants() {
        assert_eq!(
            wants(
                Some("Terminal"),
                "Authentication is required to manage system services or other units."
            ),
            "Terminal wants to manage system services or other units."
        );
        assert_eq!(
            wants(
                Some("Sift"),
                "Authentication is required for an application to mount a file system image."
            ),
            "Sift wants to mount a file system image."
        );
        let other = "Explicit request is required to run programs as a non-logged-in user.";
        assert_eq!(wants(Some("Terminal"), other), other);
        let message = "Authentication is required to set the system time.";
        assert_eq!(wants(None, message), message);
    }

    #[test]
    fn finds_the_app_of_its_unit() {
        assert_eq!(
            app_id("app-beize\\x2dterminal@0123456789abcdef.service").as_deref(),
            Some("beize-terminal")
        );
        assert_eq!(
            app_id("app-org.gnome.Nautilus@1.service").as_deref(),
            Some("org.gnome.Nautilus")
        );
        assert_eq!(app_id("beize-shell.service"), None);
        assert_eq!(app_id("app-@1.service"), None);
        assert_eq!(app_id("app-x\\x2@1.service"), None);
    }

    #[test]
    fn the_user_authenticates_if_polkit_names_them() {
        let ids = [Identity::User(0), Identity::User(1000)];
        assert_eq!(chosen(&ids, 1000), Some(1000));
        assert_eq!(chosen(&ids, 1001), Some(0));
        assert_eq!(chosen(&[Identity::Group(10)], 1000), None);
    }
}
