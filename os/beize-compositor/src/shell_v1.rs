//! `beize-shell-v1` (os/beize-protocols): the windows as Beize's shell sees
//! them (ADR 0028 §1) – their app, title and state, for the dock and the
//! menu bar – and what the shell asks of them. Only the shell sees the
//! global (`trust`), as only it may draw panels.
//!
//! A window is listed once it is placed (its first content), numbered in
//! the order windows opened, and stays listed while minimized; it is gone
//! with its surface. What each window last told the shell is kept with it,
//! so a change sends only what changed – after anything that may change it
//! (`refresh`): focus, minimizing, a new title or app id.
//!
//! The shell also hears the appearance and says when its panels are drawn
//! (`opening`).

use std::cell::RefCell;

use beize_protocols::shell::server::beize_shell_v1::{self, BeizeShellV1};
use beize_protocols::shell::server::beize_window_v1::{self, BeizeWindowV1};
use smithay::desktop::Window;
use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel::State as XdgState;
use smithay::reexports::wayland_server::{
    Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource, WEnum,
};
use smithay::utils::Rectangle;
use smithay::wayland::compositor::with_states;
use smithay::wayland::shell::xdg::XdgToplevelSurfaceData;

use crate::backdrop::{Appearance, Tone};
use crate::state::{ClientState, State};

/// The shell's bound instances of the global.
#[derive(Default)]
pub struct ShellWindows {
    instances: Vec<BeizeShellV1>,
    /// The number the next listed window gets.
    next: u64,
    /// How light the background is behind the menu bar, once measured.
    tone: Option<Tone>,
}

impl ShellWindows {
    pub fn new(display: &DisplayHandle) -> ShellWindows {
        display.create_global::<State, BeizeShellV1, _>(1, ());
        ShellWindows::default()
    }

    /// The background behind the menu bar is `tone` now: the shell hears
    /// if that is news.
    pub fn set_tone(&mut self, tone: Tone) {
        if self.tone != Some(tone) {
            self.tone = Some(tone);
            for instance in &self.instances {
                send_tone(instance, tone);
            }
        }
    }
}

fn send_tone(instance: &BeizeShellV1, tone: Tone) {
    instance.menu_bar_backdrop(match tone {
        Tone::Light => beize_shell_v1::Tone::Light,
        Tone::Dark => beize_shell_v1::Tone::Dark,
    });
}

fn send_appearance(instance: &BeizeShellV1, appearance: Appearance) {
    instance.appearance(match appearance {
        Appearance::Light => beize_shell_v1::Appearance::Light,
        Appearance::Dark => beize_shell_v1::Appearance::Dark,
    });
}

impl State {
    /// The user chose `appearance` (the control center): kept for the next
    /// login, the background and glass drawn anew for it with the next
    /// frame, and the shell hears it.
    fn set_appearance(&mut self, appearance: Appearance) {
        if self.appearance == appearance {
            return;
        }
        tracing::info!("appearance {appearance:?}, chosen");
        self.appearance = appearance;
        appearance.keep();
        for instance in &self.shell_windows.instances {
            send_appearance(instance, appearance);
        }
        self.tell_appearance();
        self.changed();
    }
}

/// What a listed window told the shell's instances.
struct Listed {
    number: u64,
    handles: Vec<BeizeWindowV1>,
    sent: Shown,
}

#[derive(Clone, Default, PartialEq)]
struct Shown {
    app_id: String,
    title: String,
    state: u32,
}

fn with_listed<T>(window: &Window, f: impl FnOnce(&mut Listed) -> T) -> Option<T> {
    let listed = window.user_data().get::<RefCell<Listed>>()?;
    // Only touched from the event loop, so never borrowed twice.
    Some(f(&mut listed.borrow_mut()))
}

/// The window's app id and title, and its state for the shell.
fn shown(state: &State, window: &Window) -> Shown {
    let Some(toplevel) = window.toplevel() else {
        return Shown::default();
    };
    let (app_id, title) = with_states(toplevel.wl_surface(), |states| {
        states
            .data_map
            .get::<XdgToplevelSurfaceData>()
            .and_then(|d| d.lock().ok().map(|d| (d.app_id.clone(), d.title.clone())))
            .unwrap_or_default()
    });
    let mut bits = beize_window_v1::State::empty();
    if toplevel.with_pending_state(|s| s.states.contains(XdgState::Activated)) {
        bits |= beize_window_v1::State::Active;
    }
    if state.minimized.contains(window) {
        bits |= beize_window_v1::State::Minimized;
    }
    Shown {
        app_id: app_id.unwrap_or_default(),
        title: title.unwrap_or_default(),
        state: bits.bits(),
    }
}

/// Tells `handle` all of `shown`, or what differs from `before`.
fn send(handle: &BeizeWindowV1, shown: &Shown, before: Option<&Shown>) {
    if before.is_none_or(|b| b.app_id != shown.app_id) {
        handle.app_id(shown.app_id.clone());
    }
    if before.is_none_or(|b| b.title != shown.title) {
        handle.title(shown.title.clone());
    }
    if before.is_none_or(|b| b.state != shown.state) {
        handle.state(beize_window_v1::State::from_bits_truncate(shown.state));
    }
    handle.done();
}

/// Tells `handle` the window's menus, anew.
fn send_menus(handle: &BeizeWindowV1, window: &Window) {
    handle.menu_clear();
    for menu in crate::app_menu::current(window) {
        handle.menu_add(menu.title);
        for entry in menu.entries {
            match entry {
                crate::app_menu::Entry::Item {
                    id,
                    title,
                    enabled,
                    shortcut,
                } => handle.menu_item(id, title, u32::from(enabled), shortcut),
                crate::app_menu::Entry::Separator => handle.menu_separator(),
            }
        }
    }
}

/// A handle for `window` (`number`) on `instance`, announced there.
fn announce(
    display: &DisplayHandle,
    instance: &BeizeShellV1,
    number: u64,
) -> Option<BeizeWindowV1> {
    let client = display.get_client(instance.id()).ok()?;
    let handle = client
        .create_resource::<BeizeWindowV1, u64, State>(display, instance.version(), number)
        .ok()?;
    instance.window(&handle);
    Some(handle)
}

impl State {
    /// The windows the shell knows of: placed or minimized.
    fn listed_windows(&self) -> Vec<Window> {
        let mut windows: Vec<(u64, Window)> = self
            .space
            .elements()
            .chain(&self.minimized)
            .filter_map(|w| Some((with_listed(w, |l| l.number)?, w.clone())))
            .collect();
        windows.sort_by_key(|(number, _)| *number);
        windows.into_iter().map(|(_, w)| w).collect()
    }

    /// `window` was placed: the shell hears of it.
    pub fn list_window(&mut self, window: &Window) {
        if with_listed(window, |_| ()).is_some() {
            return;
        }
        let number = self.shell_windows.next;
        self.shell_windows.next += 1;
        let shown = shown(self, window);
        let handles: Vec<BeizeWindowV1> = self
            .shell_windows
            .instances
            .iter()
            .filter_map(|instance| announce(&self.display, instance, number))
            .collect();
        for handle in &handles {
            send_menus(handle, window);
            send(handle, &shown, None);
        }
        window.user_data().insert_if_missing(|| {
            RefCell::new(Listed {
                number,
                handles,
                sent: shown,
            })
        });
    }

    /// What may have changed for the listed windows reaches the shell.
    pub fn refresh_listing(&mut self) {
        for window in self.listed_windows() {
            let now = shown(self, &window);
            with_listed(&window, |listed| {
                if listed.sent == now {
                    return;
                }
                listed.handles.retain(Resource::is_alive);
                for handle in &listed.handles {
                    send(handle, &now, Some(&listed.sent));
                }
                listed.sent = now;
            });
        }
    }

    /// `window`'s menus changed.
    pub fn menus_changed(&mut self, window: &Window) {
        with_listed(window, |listed| {
            listed.handles.retain(Resource::is_alive);
            for handle in &listed.handles {
                send_menus(handle, window);
                handle.done();
            }
        });
    }

    /// `window` is gone: the shell hears of it last.
    pub fn unlist_window(&mut self, window: &Window) {
        with_listed(window, |listed| {
            for handle in listed.handles.drain(..).filter(Resource::is_alive) {
                handle.closed();
            }
        });
    }

    /// The listed windows with their numbers.
    pub fn numbered_windows(&self) -> Vec<(u64, Window)> {
        self.listed_windows()
            .into_iter()
            .filter_map(|w| Some((with_listed(&w, |l| l.number)?, w)))
            .collect()
    }

    /// The handle of `window` on `instance`.
    fn handle_on(window: &Window, instance: &BeizeShellV1) -> Option<BeizeWindowV1> {
        with_listed(window, |listed| {
            listed
                .handles
                .iter()
                .find(|h| h.id().same_client_as(&instance.id()))
                .cloned()
        })
        .flatten()
    }

    /// The shell shows the switcher: `entries`, `selected` of them, in the
    /// room the windows have.
    pub fn tell_switcher_show(&mut self, entries: &[Window], selected: &Window) {
        let room = self.window_area().size;
        for instance in &self.shell_windows.instances {
            for window in entries {
                if let Some(handle) = Self::handle_on(window, instance) {
                    instance.switcher_entry(&handle);
                }
            }
            instance.switcher_show(room.w, room.h);
            if let Some(handle) = Self::handle_on(selected, instance) {
                instance.switcher_select(&handle);
            }
        }
    }

    pub fn tell_switcher_select(&mut self, window: &Window) {
        for instance in &self.shell_windows.instances {
            if let Some(handle) = Self::handle_on(window, instance) {
                instance.switcher_select(&handle);
            }
        }
    }

    pub fn tell_switcher_end(&mut self) {
        for instance in &self.shell_windows.instances {
            instance.switcher_end();
        }
    }

    /// The listed window `number`.
    fn listed(&self, number: u64) -> Option<Window> {
        self.space
            .elements()
            .chain(&self.minimized)
            .find(|w| with_listed(w, |l| l.number) == Some(number))
            .cloned()
    }
}

impl GlobalDispatch<BeizeShellV1, ()> for State {
    fn bind(
        state: &mut State,
        display: &DisplayHandle,
        _client: &Client,
        resource: New<BeizeShellV1>,
        _: &(),
        data_init: &mut DataInit<'_, State>,
    ) {
        let instance = data_init.init(resource, ());
        send_appearance(&instance, state.appearance);
        if let Some(tone) = state.shell_windows.tone {
            send_tone(&instance, tone);
        }
        for window in state.listed_windows() {
            let Some(number) = with_listed(&window, |l| l.number) else {
                continue;
            };
            let Some(handle) = announce(display, &instance, number) else {
                continue;
            };
            let shown = shown(state, &window);
            // The menus an app described are kept with its window: a shell
            // started anew hears them too.
            send_menus(&handle, &window);
            send(&handle, &shown, None);
            with_listed(&window, |listed| listed.handles.push(handle));
        }
        state.shell_windows.instances.push(instance);
    }

    /// Only the shell (`trust`); other clients do not see the global.
    fn can_view(client: Client, _: &()) -> bool {
        client.get_data::<ClientState>().is_some_and(|c| c.shell)
    }
}

impl Dispatch<BeizeShellV1, ()> for State {
    /// The appearance chosen, the shell ready (`opening`); `destroy` ends in
    /// `destroyed`.
    fn request(
        state: &mut State,
        client: &Client,
        _: &BeizeShellV1,
        request: beize_shell_v1::Request,
        _: &(),
        _: &DisplayHandle,
        _: &mut DataInit<'_, State>,
    ) {
        match request {
            beize_shell_v1::Request::SetAppearance { appearance } => {
                state.set_appearance(match appearance {
                    WEnum::Value(beize_shell_v1::Appearance::Dark) => Appearance::Dark,
                    _ => Appearance::Light,
                });
            }
            beize_shell_v1::Request::ClearThumbnails { surface } => {
                crate::switcher::set_pending(&surface, |t| t.0.clear());
            }
            beize_shell_v1::Request::Ready => {
                if let Some(data) = client.get_data::<ClientState>() {
                    data.ready.store(true, std::sync::atomic::Ordering::Relaxed);
                }
                state.shell_ready();
            }
            _ => {}
        }
    }

    fn destroyed(
        state: &mut State,
        _: smithay::reexports::wayland_server::backend::ClientId,
        resource: &BeizeShellV1,
        _: &(),
    ) {
        state.shell_windows.instances.retain(|i| i != resource);
    }
}

impl Dispatch<BeizeWindowV1, u64> for State {
    fn request(
        state: &mut State,
        _: &Client,
        _: &BeizeWindowV1,
        request: beize_window_v1::Request,
        number: &u64,
        _: &DisplayHandle,
        _: &mut DataInit<'_, State>,
    ) {
        // `destroy` ends in `destroyed`.
        let Some(window) = state.listed(*number) else {
            return;
        };
        match request {
            beize_window_v1::Request::Activate => state.bring_back(window),
            beize_window_v1::Request::MenuActivate { id } => {
                crate::app_menu::activate(&window, id);
            }
            beize_window_v1::Request::SwitcherHover => state.switcher_hover(&window),
            beize_window_v1::Request::SwitcherChoose => state.switcher_click(&window),
            beize_window_v1::Request::ShowThumbnail {
                surface,
                x,
                y,
                width,
                height,
            } => {
                if width > 0 && height > 0 {
                    let place = Rectangle::new((x, y).into(), (width, height).into());
                    crate::switcher::set_pending(&surface, |t| t.0.push((*number, place)));
                }
            }
            beize_window_v1::Request::Close => {
                if let Some(toplevel) = window.toplevel() {
                    toplevel.send_close();
                }
            }
            _ => {}
        }
    }

    fn destroyed(
        state: &mut State,
        _: smithay::reexports::wayland_server::backend::ClientId,
        resource: &BeizeWindowV1,
        number: &u64,
    ) {
        if let Some(window) = state.listed(*number) {
            with_listed(&window, |listed| listed.handles.retain(|h| h != resource));
        }
    }
}
