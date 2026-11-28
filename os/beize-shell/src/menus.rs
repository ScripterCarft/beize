//! The menus' entries and what they do: the Beize menu and its
//! confirmations, the app's name menu, Window and Move & Resize, the app's
//! own menus; starting apps and acting through logind.

use super::*;

/// The Beize menu, as macOS' Apple menu has these: the computer's state,
/// then the user's session. Log Out names the user, as macOS does.
pub(super) fn beize_menu_entries() -> Vec<Entry> {
    vec![
        Entry::item("Sleep"),
        Entry::item("Restart…"),
        Entry::item("Shut Down…"),
        Entry::Separator,
        Entry::item(format!("Log Out {}…", user_name())),
    ]
}

/// The command of the Beize menu's entry `i` (`beize_menu_entries`).
pub(super) fn command_at(i: usize) -> Option<Command> {
    match i {
        0 => Some(Command::Power(Power::Sleep)),
        1 => Some(Command::Power(Power::Restart)),
        2 => Some(Command::Power(Power::ShutDown)),
        4 => Some(Command::LogOut),
        _ => None,
    }
}

/// What asks before `command` happens.
pub(super) fn confirmation(command: Command) -> Dialog {
    let (title, action) = match command {
        Command::Power(Power::Restart) => ("Restart this computer?", "Restart"),
        Command::Power(_) => ("Shut down this computer?", "Shut Down"),
        Command::LogOut => ("Log out?", "Log Out"),
    };
    Dialog::new(title, "Unsaved work may be lost.", action)
}

/// Opens `dialog` in place of any open, for `command` if it confirms one.
pub(super) fn show_dialog(
    app: &mut App,
    asking: &mut Option<(WindowId, Dialog, Option<Command>)>,
    dialog: Dialog,
    command: Option<Command>,
) -> bool {
    if let Some((id, ..)) = asking.take() {
        app.close(id);
    }
    let overlay = Overlay {
        namespace: "confirmation",
        size: dialog.size(app.text()),
        keyboard: true,
    };
    match app.overlay(&overlay) {
        Ok(id) => {
            app.mark_dialog(id);
            *asking = Some((id, dialog, command));
            true
        }
        Err(e) => {
            warn!("a dialog: {e}");
            false
        }
    }
}

/// The system, as an app's About names it.
pub(super) fn system_name() -> String {
    let text = std::fs::read_to_string("/usr/lib/os-release").unwrap_or_default();
    let field = text
        .lines()
        .find_map(|line| line.strip_prefix("PRETTY_NAME="))
        .map(|name| name.trim_matches('"').to_owned());
    field.unwrap_or_else(|| "Beize".to_owned())
}

/// The active app's menu by its name, the system's for every app (ADR 0028 §3),
/// as macOS': about it, hiding, quitting. Hiding takes windows off the screen as
/// minimizing does.
pub(super) fn app_name_menu(
    window: &beize_ui::DesktopWindow,
    windows: &[beize_ui::DesktopWindow],
    names: &mut impl FnMut(&str) -> apps::App,
) -> (Vec<Entry>, Vec<Option<dock::Command>>) {
    let name = match names(&window.app_id).name {
        name if !name.is_empty() => name,
        _ if !window.title.is_empty() => window.title.clone(),
        _ => "App".to_owned(),
    };
    // A window whose app named none is an app of its own.
    let same_app = |w: &beize_ui::DesktopWindow| {
        w.id == window.id || (!w.app_id.is_empty() && w.app_id == window.app_id)
    };
    let of = |own: bool| -> Vec<_> {
        windows
            .iter()
            .filter(|w| same_app(w) == own && !w.minimized)
            .map(|w| w.id)
            .collect()
    };
    let (own, others) = (of(true), of(false));
    let hidden: Vec<_> = windows
        .iter()
        .filter(|w| w.minimized)
        .map(|w| w.id)
        .collect();
    let any_hidden = !hidden.is_empty();
    // The window in use stays in front of those that come back.
    let show_all = hidden.into_iter().chain([window.id]).collect();
    let all_own = windows
        .iter()
        .filter(|w| same_app(w))
        .map(|w| w.id)
        .collect();
    let entries = vec![
        Entry::item(format!("About {name}")),
        Entry::Separator,
        Entry::item(format!("Hide {name}")),
        Entry::item("Hide Others").enabled(!others.is_empty()),
        Entry::item("Show All").enabled(any_hidden),
        Entry::Separator,
        Entry::item(format!("Quit {name}")),
    ];
    let commands = vec![
        Some(dock::Command::About(name)),
        None,
        Some(dock::Command::Minimize(own)),
        Some(dock::Command::Minimize(others)),
        Some(dock::Command::Show(show_all)),
        None,
        Some(dock::Command::Quit(all_own)),
    ];
    (entries, commands)
}

/// The Window menu, the system's for every app, as macOS': the active window's,
/// then its app's windows to the front; the group with a symbol last, as Apple's
/// HIG has symbols for a whole group or none.
pub(super) fn window_menu(
    window: &beize_ui::DesktopWindow,
    dock: &Dock,
) -> (Vec<Entry>, Vec<Option<dock::Command>>) {
    let own = if window.app_id.is_empty() {
        vec![window.id]
    } else {
        dock.front_to_top(&window.app_id)
    };
    let entries = vec![
        Entry::item("Minimize"),
        Entry::item("Zoom"),
        Entry::item("Bring All to Front"),
        Entry::Separator,
        Entry::item("Move & Resize").symbol("square-half").submenu(),
    ];
    let commands = vec![
        Some(dock::Command::Minimize(vec![window.id])),
        Some(dock::Command::Zoom(window.id)),
        Some(dock::Command::Show(own)),
        None,
        None,
    ];
    (entries, commands)
}

/// Move & Resize's menu.
pub(super) fn move_and_resize_menu(
    window: beize_ui::DesktopWindowId,
) -> (Vec<Entry>, Vec<Option<dock::Command>>) {
    let entries = vec![
        Entry::item("Left").shortcut("Super+Left"),
        Entry::item("Right").shortcut("Super+Right"),
        Entry::item("Return to Previous Size"),
    ];
    let to = |place| Some(dock::Command::MoveTo(window, place));
    let commands = vec![
        to(DesktopPlace::Left),
        to(DesktopPlace::Right),
        to(DesktopPlace::Previous),
    ];
    (entries, commands)
}

/// A menu an app describes, with each entry's command.
pub(super) fn app_menu_commands(
    window: beize_ui::DesktopWindowId,
    menu: &beize_ui::menu::AppMenu,
) -> (Vec<Entry>, Vec<Option<dock::Command>>) {
    let entries = menu
        .entries
        .iter()
        .map(|(entry, _)| entry.clone())
        .collect();
    let commands = menu
        .entries
        .iter()
        .map(|(entry, id)| match entry {
            Entry::Item(_) => Some(dock::Command::App(window, *id)),
            Entry::Separator => None,
        })
        .collect();
    (entries, commands)
}

/// What logind lets the Beize menu do now, for the journal: polkit's
/// answer for this session ("yes" without a password).
pub(super) fn may() -> String {
    [
        ("sleep", Power::Sleep),
        ("restart", Power::Restart),
        ("shut down", Power::ShutDown),
    ]
    .iter()
    .map(|(name, power)| {
        let answer = login::can(*power).unwrap_or_else(|e| format!("({e})"));
        format!("{name} {answer}")
    })
    .collect::<Vec<_>>()
    .join(", ")
}

/// Starts the app `app_id` from the dock (`dock`): with an activation
/// token for the click, which its first window shows with and grows out of
/// the app's symbol (ADR 0032 §4); the app starts once the token is made,
/// after the next dispatch. Where the compositor makes none, at once.
pub(super) fn start(app: &mut App, dock: WindowId, app_id: &str) {
    if !app.ask_activation_token(dock, app_id) {
        beize_systemd::apps::start(app_id, None);
    }
}

/// Does what the Beize menu chose.
pub(super) fn choose(command: Option<Command>) {
    let Some(command) = command else {
        return;
    };
    info!("Beize menu: {command:?}");
    on_the_side(move || {
        let done = match command {
            Command::Power(power) => login::power(power),
            Command::LogOut => login::log_out(),
        };
        if let Err(e) = done {
            warn!("{command:?}: {e}");
        }
    });
}

/// A call that waits for another service, on a thread of its own with its own
/// bus connection: logind or polkit may take up to sd-bus' 25 s to answer, and
/// the bar and dock must go on meanwhile (`beize_systemd::apps::start` too).
pub(super) fn on_the_side(job: impl FnOnce() + Send + 'static) {
    if let Err(e) = std::thread::Builder::new().spawn(job) {
        warn!("starting a thread: {e}");
    }
}

/// The user's name as the account has it (its full name, else the login),
/// as macOS names the user in "Log Out".
pub(super) fn user_name() -> String {
    // SAFETY: getpwuid's entry is read at once, before any other call to it
    // (only this thread calls it; `on_the_side` does not).
    unsafe {
        let entry = libc::getpwuid(libc::getuid());
        if entry.is_null() {
            return String::new();
        }
        let field = |p: *const libc::c_char| {
            (!p.is_null()).then(|| std::ffi::CStr::from_ptr(p).to_string_lossy().into_owned())
        };
        let full = field((*entry).pw_gecos)
            .and_then(|g| g.split(',').next().map(str::to_owned))
            .filter(|g| !g.is_empty());
        full.or_else(|| field((*entry).pw_name)).unwrap_or_default()
    }
}
