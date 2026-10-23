//! The windows of the desktop, for Beize's shell (beize-shell-v1, ADR 0028
//! §1): every app's windows with their app, title and state, as the
//! compositor tells them, and bringing one to the front. Only the shell is
//! offered the protocol.
//!
//! A window's changes take effect together at its `done`; the list changed
//! when `Dispatched::desktop` says so.

use beize_protocols::shell::client::beize_shell_v1::{self, BeizeShellV1};
use beize_protocols::shell::client::beize_window_v1::{self, BeizeWindowV1};
use wayland_client::{Connection, Dispatch, QueueHandle, WEnum, event_created_child};

use crate::app::State;

/// A window of the desktop: the how-manieth it is in the list, so ids
/// follow the order windows opened.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct DesktopWindowId(pub u64);

#[derive(Clone, Debug, PartialEq)]
pub struct DesktopWindow {
    pub id: DesktopWindowId,
    /// Its app's id (the desktop file's name); empty if it named none.
    pub app_id: String,
    pub title: String,
    /// It has the keyboard.
    pub active: bool,
    pub minimized: bool,
}

struct Entry {
    proxy: BeizeWindowV1,
    /// As of its last `done`, and what came since.
    shown: Option<DesktopWindow>,
    pending: DesktopWindow,
}

pub(crate) struct Desktop {
    _shell: BeizeShellV1,
    entries: Vec<Entry>,
    next: u64,
    /// The list changed since the app last heard.
    pub(crate) changed: bool,
}

impl Desktop {
    pub(crate) fn new(shell: BeizeShellV1) -> Desktop {
        Desktop {
            _shell: shell,
            entries: Vec::new(),
            next: 0,
            changed: false,
        }
    }

    /// The windows, in the order they opened.
    pub(crate) fn windows(&self) -> impl Iterator<Item = &DesktopWindow> {
        self.entries.iter().filter_map(|e| e.shown.as_ref())
    }

    pub(crate) fn activate(&self, id: DesktopWindowId) {
        if let Some(entry) = self.entries.iter().find(|e| e.pending.id == id) {
            entry.proxy.activate();
        }
    }
}

impl Dispatch<BeizeShellV1, ()> for State {
    fn event(
        state: &mut State,
        _: &BeizeShellV1,
        event: beize_shell_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
        let (beize_shell_v1::Event::Window { window }, Some(desktop)) = (event, &mut state.desktop)
        else {
            return;
        };
        let id = DesktopWindowId(desktop.next);
        desktop.next += 1;
        desktop.entries.push(Entry {
            proxy: window,
            shown: None,
            pending: DesktopWindow {
                id,
                app_id: String::new(),
                title: String::new(),
                active: false,
                minimized: false,
            },
        });
    }

    event_created_child!(State, BeizeShellV1, [
        beize_shell_v1::EVT_WINDOW_OPCODE => (BeizeWindowV1, ()),
    ]);
}

impl Dispatch<BeizeWindowV1, ()> for State {
    fn event(
        state: &mut State,
        proxy: &BeizeWindowV1,
        event: beize_window_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
        let Some(desktop) = &mut state.desktop else {
            return;
        };
        let Some(i) = desktop.entries.iter().position(|e| &e.proxy == proxy) else {
            return;
        };
        let entry = &mut desktop.entries[i];
        match event {
            beize_window_v1::Event::AppId { app_id } => entry.pending.app_id = app_id,
            beize_window_v1::Event::Title { title } => entry.pending.title = title,
            beize_window_v1::Event::State {
                state: WEnum::Value(bits),
            } => {
                entry.pending.active = bits.contains(beize_window_v1::State::Active);
                entry.pending.minimized = bits.contains(beize_window_v1::State::Minimized);
            }
            beize_window_v1::Event::Done => {
                entry.shown = Some(entry.pending.clone());
                desktop.changed = true;
            }
            beize_window_v1::Event::Closed => {
                entry.proxy.destroy();
                desktop.entries.remove(i);
                desktop.changed = true;
            }
            _ => {}
        }
    }
}
