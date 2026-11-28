//! The windows of the desktop, for Beize's shell (beize-shell-v1, ADR 0028
//! §1): every app's windows with their app, title and state, as the
//! compositor tells them, and bringing one to the front. Only the shell is
//! offered the protocol.
//!
//! A window's changes take effect together at its `done`; the list changed
//! when `Dispatched::desktop` says so – as when the appearance changed.
//!
//! Glass (`Glasses`, `beize_glass_v1`) is for every app: the compositor draws
//! it, as macOS' WindowServer draws a material.

use beize_draw::Appearance;
use beize_protocols::app::client::beize_app_menu_v1::{self, BeizeAppMenuV1};
use beize_protocols::app::client::beize_glass_manager_v1::{self, BeizeGlassManagerV1};
use beize_protocols::app::client::beize_glass_v1::{self, BeizeGlassV1};
use beize_protocols::app::client::beize_menu_manager_v1::BeizeMenuManagerV1;
use beize_protocols::shell::client::beize_shell_v1::{self, BeizeShellV1};
use beize_protocols::shell::client::beize_window_v1::{self, BeizeWindowV1};
use wayland_client::protocol::wl_surface::WlSurface;
use wayland_client::{Connection, Dispatch, QueueHandle, WEnum, event_created_child};

use crate::Rect;
use crate::app::{State, WindowId};
use crate::menu::{AppMenu, Entry as MenuEntry};

/// Beize glass below parts of a window (`App::set_glass`; ADR 0033 §6):
/// where it lies, how dense it is, and whether each of its shapes casts a
/// shadow.
#[derive(Clone, Debug, PartialEq)]
pub struct Glass {
    pub shapes: Vec<GlassShape>,
    pub density: Density,
    pub shadow: bool,
}

/// One place of a glass: a rectangle in the window's coordinates with round
/// corners, and whether a rim of light runs along its edge.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GlassShape {
    pub rect: Rect,
    pub radius: f32,
    pub rim: bool,
}

/// How much of what lies behind a glass shows: one glass, three densities
/// of its tint (`beize_draw::tokens`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Density {
    /// What lies behind shows clearly: the menu bar's.
    Thin,
    /// The dock's.
    Medium,
    /// Calm, ink reads on it over anything: menus, dialogs, Alt+Tab, the
    /// control center.
    Dense,
}

impl Glass {
    /// Glass in one place, without a shadow.
    pub fn one(rect: Rect, radius: f32, rim: bool, density: Density) -> Glass {
        Glass {
            shapes: vec![GlassShape { rect, radius, rim }],
            density,
            shadow: false,
        }
    }
}

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
    /// The menus its app describes (`App::set_menus`).
    pub menus: Vec<AppMenu>,
}

struct Entry {
    proxy: BeizeWindowV1,
    /// As of its last `done`, and what came since.
    shown: Option<DesktopWindow>,
    pending: DesktopWindow,
}

/// Where the shell can move a window (the Window menu's Move & Resize).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DesktopPlace {
    Left,
    Right,
    /// Where it was before a half or maximized.
    Previous,
}

/// The switcher (Alt+Tab) as the compositor last said (`App::desktop_switcher`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SwitcherNews {
    pub entries: Vec<DesktopWindowId>,
    pub selected: Option<DesktopWindowId>,
    /// The room it may take.
    pub room: (u32, u32),
}

pub(crate) struct Desktop {
    shell: BeizeShellV1,
    /// The switcher's entries before it shows, and it while it does.
    switcher_entries: Vec<DesktopWindowId>,
    pub(crate) switcher: Option<SwitcherNews>,
    entries: Vec<Entry>,
    next: u64,
    /// The list or the appearance changed since the app last heard.
    pub(crate) changed: bool,
    pub(crate) appearance: Appearance,
    /// How light it is behind the menu bar, once the compositor said.
    pub(crate) menu_bar_backdrop: Option<Appearance>,
    /// The Beize menu's key was pressed since the app last heard.
    pub(crate) beize_menu_key: bool,
}

/// The glass asked for below the app's windows.
pub(crate) struct Glasses {
    manager: BeizeGlassManagerV1,
    /// The system's appearance, as the compositor tells every app.
    pub(crate) appearance: Appearance,
    list: Vec<(WindowId, BeizeGlassV1)>,
}

impl Desktop {
    pub(crate) fn new(shell: BeizeShellV1) -> Desktop {
        Desktop {
            shell,
            switcher_entries: Vec::new(),
            switcher: None,
            entries: Vec::new(),
            next: 0,
            changed: false,
            appearance: Appearance::Light,
            menu_bar_backdrop: None,
            beize_menu_key: false,
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

    pub(crate) fn switcher_hover(&self, id: DesktopWindowId) {
        if let Some(entry) = self.entries.iter().find(|e| e.pending.id == id) {
            entry.proxy.switcher_hover();
        }
    }

    pub(crate) fn switcher_choose(&self, id: DesktopWindowId) {
        if let Some(entry) = self.entries.iter().find(|e| e.pending.id == id) {
            entry.proxy.switcher_choose();
        }
    }

    /// The windows' pictures on `surface`, with its next commit.
    pub(crate) fn show_thumbnails(
        &self,
        surface: &WlSurface,
        pictures: &[(DesktopWindowId, Rect)],
    ) {
        self.shell.clear_thumbnails(surface);
        for (id, r) in pictures {
            if let Some(entry) = self.entries.iter().find(|e| e.pending.id == *id) {
                entry.proxy.show_thumbnail(
                    surface,
                    r.x.round() as i32,
                    r.y.round() as i32,
                    r.w.round() as i32,
                    r.h.round() as i32,
                );
            }
        }
    }

    /// Where the apps' symbols lie on `surface` (the dock), with its next
    /// commit: the compositor minimizes windows into them (ADR 0032).
    pub(crate) fn show_dock_icons(&self, surface: &WlSurface, icons: &[(String, Rect)]) {
        self.shell.clear_dock_icons(surface);
        for (app_id, r) in icons {
            self.shell.dock_icon(
                surface,
                app_id.clone(),
                r.x.round() as i32,
                r.y.round() as i32,
                r.w.round() as i32,
                r.h.round() as i32,
            );
        }
    }

    /// `surface` is a dialog: it appears and goes as one (ADR 0032).
    pub(crate) fn dialog(&self, surface: &WlSurface) {
        self.shell.dialog(surface);
    }

    /// The popup on `surface` grows out of its button and goes back
    /// there (ADR 0032).
    pub(crate) fn grows(&self, surface: &WlSurface) {
        self.shell.grows(surface);
    }

    /// The dialog on `surface` shakes: a wrong answer.
    pub(crate) fn shake(&self, surface: &WlSurface) {
        self.shell.shake(surface);
    }

    /// The user chose command `command` of the window's menus.
    pub(crate) fn menu_activate(&self, id: DesktopWindowId, command: u32) {
        if let Some(entry) = self.entries.iter().find(|e| e.pending.id == id) {
            entry.proxy.menu_activate(command);
        }
    }

    pub(crate) fn minimize(&self, id: DesktopWindowId) {
        if let Some(entry) = self.entries.iter().find(|e| e.pending.id == id) {
            entry.proxy.minimize();
        }
    }

    pub(crate) fn zoom(&self, id: DesktopWindowId) {
        if let Some(entry) = self.entries.iter().find(|e| e.pending.id == id) {
            entry.proxy.zoom();
        }
    }

    pub(crate) fn move_to(&self, id: DesktopWindowId, place: DesktopPlace) {
        if let Some(entry) = self.entries.iter().find(|e| e.pending.id == id) {
            entry.proxy.move_to(match place {
                DesktopPlace::Left => beize_window_v1::Place::Left,
                DesktopPlace::Right => beize_window_v1::Place::Right,
                DesktopPlace::Previous => beize_window_v1::Place::Previous,
            });
        }
    }

    /// Asks the window to close, as its own close button does.
    pub(crate) fn close(&self, id: DesktopWindowId) {
        if let Some(entry) = self.entries.iter().find(|e| e.pending.id == id) {
            entry.proxy.close();
        }
    }

    pub(crate) fn ready(&self) {
        self.shell.ready();
    }

    pub(crate) fn choose_appearance(&self, appearance: Appearance) {
        self.shell.set_appearance(match appearance {
            Appearance::Light => beize_shell_v1::Appearance::Light,
            Appearance::Dark => beize_shell_v1::Appearance::Dark,
        });
    }
}

impl Glasses {
    pub(crate) fn new(manager: BeizeGlassManagerV1) -> Glasses {
        Glasses {
            manager,
            appearance: Appearance::Light,
            list: Vec::new(),
        }
    }

    /// Glass below the window `id` (its surface `surface`), or none; it
    /// applies with the window's next commit.
    pub(crate) fn set_glass(
        &mut self,
        id: WindowId,
        surface: &WlSurface,
        glass: Option<Glass>,
        qh: &QueueHandle<State>,
    ) {
        let Some(glass) = glass else {
            if let Some(i) = self.list.iter().position(|(w, _)| *w == id) {
                self.list.swap_remove(i).1.destroy();
            }
            return;
        };
        let proxy = match self.list.iter().find(|(w, _)| *w == id) {
            Some((_, proxy)) => proxy.clone(),
            None => {
                let proxy = self.manager.get_glass(surface, qh, ());
                self.list.push((id, proxy.clone()));
                proxy
            }
        };
        if glass.shapes.is_empty() {
            // No place: none left of the shapes before.
            proxy.set_shape(0, 0, 0, 0, 0.0, 0);
        }
        for (i, shape) in glass.shapes.iter().enumerate() {
            let r = shape.rect;
            let (x, y, w, h) = (
                r.x.round() as i32,
                r.y.round() as i32,
                r.w.round() as i32,
                r.h.round() as i32,
            );
            let (radius, rim) = (f64::from(shape.radius), u32::from(shape.rim));
            if i == 0 {
                proxy.set_shape(x, y, w, h, radius, rim);
            } else {
                proxy.add_shape(x, y, w, h, radius, rim);
            }
        }
        proxy.set_density(match glass.density {
            Density::Thin => beize_glass_v1::Density::Thin,
            Density::Medium => beize_glass_v1::Density::Medium,
            Density::Dense => beize_glass_v1::Density::Dense,
        });
        proxy.set_shadow(u32::from(glass.shadow));
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
        let Some(desktop) = &mut state.desktop else {
            return;
        };
        let window = match event {
            beize_shell_v1::Event::Window { window } => window,
            beize_shell_v1::Event::Appearance { appearance } => {
                let appearance = match appearance {
                    WEnum::Value(beize_shell_v1::Appearance::Dark) => Appearance::Dark,
                    _ => Appearance::Light,
                };
                if appearance != desktop.appearance {
                    desktop.appearance = appearance;
                    desktop.changed = true;
                }
                return;
            }
            beize_shell_v1::Event::SwitcherEntry { window } => {
                if let Some(entry) = desktop.entries.iter().find(|e| e.proxy == window) {
                    desktop.switcher_entries.push(entry.pending.id);
                }
                return;
            }
            beize_shell_v1::Event::SwitcherAdd { window } => {
                if let (Some(entry), Some(switcher)) = (
                    desktop.entries.iter().find(|e| e.proxy == window),
                    &mut desktop.switcher,
                ) {
                    switcher.entries.push(entry.pending.id);
                    desktop.changed = true;
                }
                return;
            }
            beize_shell_v1::Event::SwitcherShow { width, height } => {
                let size = |n: i32| u32::try_from(n).unwrap_or(0);
                desktop.switcher = Some(SwitcherNews {
                    entries: std::mem::take(&mut desktop.switcher_entries),
                    selected: None,
                    room: (size(width), size(height)),
                });
                desktop.changed = true;
                return;
            }
            beize_shell_v1::Event::SwitcherSelect { window } => {
                let id = desktop
                    .entries
                    .iter()
                    .find(|e| e.proxy == window)
                    .map(|e| e.pending.id);
                if let Some(switcher) = &mut desktop.switcher {
                    switcher.selected = id;
                    desktop.changed = true;
                }
                return;
            }
            beize_shell_v1::Event::SwitcherEnd => {
                desktop.switcher = None;
                desktop.switcher_entries.clear();
                desktop.changed = true;
                return;
            }
            beize_shell_v1::Event::BeizeMenu => {
                desktop.beize_menu_key = true;
                return;
            }
            beize_shell_v1::Event::MenuBarBackdrop { tone } => {
                let tone = match tone {
                    WEnum::Value(beize_shell_v1::Tone::Dark) => Appearance::Dark,
                    _ => Appearance::Light,
                };
                if desktop.menu_bar_backdrop != Some(tone) {
                    desktop.menu_bar_backdrop = Some(tone);
                    desktop.changed = true;
                }
                return;
            }
            _ => return,
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
                menus: Vec::new(),
            },
        });
    }

    event_created_child!(State, BeizeShellV1, [
        beize_shell_v1::EVT_WINDOW_OPCODE => (BeizeWindowV1, ()),
    ]);
}

impl Dispatch<BeizeGlassManagerV1, ()> for State {
    fn event(
        state: &mut State,
        _: &BeizeGlassManagerV1,
        event: beize_glass_manager_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
        if let (beize_glass_manager_v1::Event::Appearance { appearance }, Some(glasses)) =
            (event, &mut state.glasses)
        {
            glasses.appearance = match appearance {
                WEnum::Value(beize_glass_manager_v1::Appearance::Dark) => Appearance::Dark,
                _ => Appearance::Light,
            };
        }
    }
}

/// Glass has no events.
impl Dispatch<BeizeGlassV1, ()> for State {
    fn event(
        _: &mut State,
        _: &BeizeGlassV1,
        _: <BeizeGlassV1 as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
    }
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
            beize_window_v1::Event::MenuClear => entry.pending.menus.clear(),
            beize_window_v1::Event::MenuAdd { title } => entry.pending.menus.push(AppMenu {
                title,
                entries: Vec::new(),
            }),
            beize_window_v1::Event::MenuItem {
                id,
                title,
                enabled,
                shortcut,
            } => {
                if let Some(menu) = entry.pending.menus.last_mut() {
                    let mut item = MenuEntry::item(title).enabled(enabled != 0);
                    if !shortcut.is_empty() {
                        item = item.shortcut(shortcut);
                    }
                    menu.entries.push((item, id));
                }
            }
            beize_window_v1::Event::MenuSeparator => {
                if let Some(menu) = entry.pending.menus.last_mut() {
                    menu.entries.push((MenuEntry::Separator, 0));
                }
            }
            beize_window_v1::Event::Done => {
                entry.shown = Some(entry.pending.clone());
                desktop.changed = true;
            }
            beize_window_v1::Event::Closed => {
                let id = entry.pending.id;
                entry.proxy.destroy();
                desktop.entries.remove(i);
                desktop.switcher_entries.retain(|e| *e != id);
                if let Some(switcher) = &mut desktop.switcher {
                    switcher.entries.retain(|e| *e != id);
                }
                desktop.changed = true;
            }
            _ => {}
        }
    }
}

impl Dispatch<BeizeMenuManagerV1, ()> for State {
    fn event(
        _: &mut State,
        _: &BeizeMenuManagerV1,
        _: <BeizeMenuManagerV1 as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
    }
}

/// The user chose a command of a window's menus.
impl Dispatch<BeizeAppMenuV1, WindowId> for State {
    fn event(
        state: &mut State,
        _: &BeizeAppMenuV1,
        event: beize_app_menu_v1::Event,
        id: &WindowId,
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
        if let beize_app_menu_v1::Event::Activated { id: command } = event {
            state.command(*id, command);
        }
    }
}

/// Describes `menus` on `proxy`, anew.
pub(crate) fn describe_menus(proxy: &BeizeAppMenuV1, menus: &[AppMenu]) {
    proxy.begin();
    for menu in menus {
        proxy.add_menu(menu.title.clone());
        for (entry, id) in &menu.entries {
            match entry {
                MenuEntry::Item(item) => proxy.add_item(
                    *id,
                    item.title.clone(),
                    u32::from(item.enabled),
                    item.shortcut.clone().unwrap_or_default(),
                ),
                MenuEntry::Separator => proxy.add_separator(),
            }
        }
    }
    proxy.commit();
}
