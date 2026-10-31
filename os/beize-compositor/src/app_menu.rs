//! App menus (ADR 0028 §3, beize-app-v1): an app describes its window's menus
//! here, as AppKit hands its menu to the WindowServer, and the shell shows
//! them in the menu bar (`shell_v1`), so the shell never talks to the app. They
//! are kept with the window, so a shell started anew hears them again.
//!
//! The shell's connection must not fill up: a server whose client does not
//! read ends that client. So only the active window's menus go there, and only
//! when they changed, and what a window describes has a budget.

use std::cell::RefCell;

use beize_protocols::app::server::beize_app_menu_v1::{self, BeizeAppMenuV1};
use beize_protocols::app::server::beize_menu_manager_v1::{self, BeizeMenuManagerV1};
use smithay::desktop::Window;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::{
    Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource,
};

use crate::state::State;

/// What a client may describe at most, so a client cannot make the shell's
/// work as large as it likes: more than any menu bar shows.
const MAX_MENUS: usize = 16;
const MAX_ENTRIES: usize = 64;
const MAX_TEXT: usize = 128;
/// The text of all of a window's menus, about what a wire message and a
/// socket's buffer take without the shell reading.
const MAX_BYTES: usize = 16 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Entry {
    Item {
        id: u32,
        title: String,
        enabled: bool,
        shortcut: String,
    },
    Separator,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Menu {
    pub title: String,
    pub entries: Vec<Entry>,
}

/// A window's menus: what applies, what is being described, and the app's
/// object to tell a choice.
#[derive(Default)]
struct Menus {
    current: Vec<Menu>,
    pending: Vec<Menu>,
    /// The text described so far, against `MAX_BYTES`.
    pending_bytes: usize,
    app: Option<BeizeAppMenuV1>,
}

impl Menus {
    /// Whether `bytes` more text fit; counted if so.
    fn takes(&mut self, bytes: usize) -> bool {
        let fits = self.pending_bytes + bytes <= MAX_BYTES;
        if fits {
            self.pending_bytes += bytes;
        }
        fits
    }
}

fn with_menus<T>(window: &Window, f: impl FnOnce(&mut Menus) -> T) -> T {
    let data = window.user_data();
    data.insert_if_missing(|| RefCell::new(Menus::default()));
    match data.get::<RefCell<Menus>>() {
        Some(menus) => f(&mut menus.borrow_mut()),
        None => f(&mut Menus::default()),
    }
}

/// The menus that apply to `window`.
pub fn current(window: &Window) -> Vec<Menu> {
    with_menus(window, |menus| menus.current.clone())
}

/// The app hears the user chose command `id`.
pub fn activate(window: &Window, id: u32) {
    with_menus(window, |menus| {
        if let Some(app) = &menus.app {
            app.activated(id);
        }
    });
}

/// Offers the menus to every client.
pub fn offer(display: &DisplayHandle) {
    display.create_global::<State, BeizeMenuManagerV1, _>(1, ());
}

impl GlobalDispatch<BeizeMenuManagerV1, ()> for State {
    fn bind(
        _: &mut State,
        _: &DisplayHandle,
        _: &Client,
        resource: New<BeizeMenuManagerV1>,
        _: &(),
        data_init: &mut DataInit<'_, State>,
    ) {
        data_init.init(resource, ());
    }
}

impl Dispatch<BeizeMenuManagerV1, ()> for State {
    fn request(
        state: &mut State,
        _: &Client,
        _: &BeizeMenuManagerV1,
        request: beize_menu_manager_v1::Request,
        _: &(),
        _: &DisplayHandle,
        data_init: &mut DataInit<'_, State>,
    ) {
        if let beize_menu_manager_v1::Request::GetMenu { id, surface } = request {
            let app = data_init.init(id, surface.clone());
            if let Some(window) = state.any_window(&surface) {
                with_menus(&window, |menus| menus.app = Some(app));
            }
        }
    }
}

fn cut(text: String) -> String {
    text.chars().take(MAX_TEXT).collect()
}

impl Dispatch<BeizeAppMenuV1, WlSurface> for State {
    fn request(
        state: &mut State,
        _: &Client,
        _: &BeizeAppMenuV1,
        request: beize_app_menu_v1::Request,
        surface: &WlSurface,
        _: &DisplayHandle,
        _: &mut DataInit<'_, State>,
    ) {
        let Some(window) = state.any_window(surface) else {
            return;
        };
        let add = |menus: &mut Menus, entry: Entry| {
            if let Some(menu) = menus.pending.last_mut()
                && menu.entries.len() < MAX_ENTRIES
            {
                menu.entries.push(entry);
            }
        };
        match request {
            beize_app_menu_v1::Request::Begin => {
                with_menus(&window, |menus| {
                    menus.pending.clear();
                    menus.pending_bytes = 0;
                });
            }
            beize_app_menu_v1::Request::AddMenu { title } => with_menus(&window, |menus| {
                let title = cut(title);
                if menus.pending.len() < MAX_MENUS && menus.takes(title.len()) {
                    menus.pending.push(Menu {
                        title,
                        entries: Vec::new(),
                    });
                }
            }),
            beize_app_menu_v1::Request::AddItem {
                id,
                title,
                enabled,
                shortcut,
            } => with_menus(&window, |menus| {
                let (title, shortcut) = (cut(title), cut(shortcut));
                if menus.takes(title.len() + shortcut.len()) {
                    add(
                        menus,
                        Entry::Item {
                            id,
                            title,
                            enabled: enabled != 0,
                            shortcut,
                        },
                    );
                }
            }),
            beize_app_menu_v1::Request::AddSeparator => {
                with_menus(&window, |menus| add(menus, Entry::Separator));
            }
            beize_app_menu_v1::Request::Commit => {
                let changed = with_menus(&window, |menus| {
                    let new = std::mem::take(&mut menus.pending);
                    menus.pending_bytes = 0;
                    let changed = new != menus.current;
                    menus.current = new;
                    changed
                });
                if changed {
                    state.menus_changed(&window);
                }
            }
            _ => {}
        }
    }

    fn destroyed(
        state: &mut State,
        _: smithay::reexports::wayland_server::backend::ClientId,
        _: &BeizeAppMenuV1,
        surface: &WlSurface,
    ) {
        if surface.is_alive()
            && let Some(window) = state.any_window(surface)
        {
            with_menus(&window, |menus| *menus = Menus::default());
            state.menus_changed(&window);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_a_window_describes_has_a_budget() {
        let mut menus = Menus::default();
        assert!(menus.takes(MAX_BYTES));
        assert!(!menus.takes(1));
        assert_eq!(menus.pending_bytes, MAX_BYTES);
    }
}
