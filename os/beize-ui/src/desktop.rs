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
use beize_protocols::app::client::beize_glass_manager_v1::{self, BeizeGlassManagerV1};
use beize_protocols::app::client::beize_glass_v1::{self, BeizeGlassV1};
use beize_protocols::shell::client::beize_shell_v1::{self, BeizeShellV1};
use beize_protocols::shell::client::beize_window_v1::{self, BeizeWindowV1};
use wayland_client::protocol::wl_surface::WlSurface;
use wayland_client::{Connection, Dispatch, QueueHandle, WEnum, event_created_child};

use crate::Rect;
use crate::app::{State, WindowId};

/// Glass below parts of a window (`App::set_glass`): where it lies, what it
/// shows, whether each of its shapes casts a shadow, and the smoke around
/// them.
#[derive(Clone, Debug, PartialEq)]
pub struct Glass {
    pub shapes: Vec<GlassShape>,
    pub material: Material,
    pub shadow: bool,
    pub smoke: Option<Smoke>,
}

/// What lies behind a glass's shapes and around them, blurred by the
/// compositor beyond the window: less and less over `falloff` px from their
/// edges, by `strength` px at most (beize_glass_v1.set_smoke).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Smoke {
    pub falloff: f32,
    pub strength: f32,
}

/// One place of a glass: a rectangle in the window's coordinates with round
/// corners, and whether a rim of light runs along its edge.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GlassShape {
    pub rect: Rect,
    pub radius: f32,
    pub rim: bool,
}

/// What a glass shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Material {
    /// The background, blurred once: costs next to nothing.
    Base,
    /// What lies behind it, windows too.
    Live,
    /// Live, tinted thickly: calm, as macOS' menus.
    Menu,
    /// Liquid glass, as macOS 26's: what lies behind frosted, a curved edge
    /// with a line of light, its tone held so white ink reads on it – the
    /// control center's tiles.
    Clear,
    /// Liquid glass without ink on it: the dock's.
    Liquid,
}

impl Glass {
    /// Glass in one place, without a shadow.
    pub fn one(rect: Rect, radius: f32, rim: bool, material: Material) -> Glass {
        Glass {
            shapes: vec![GlassShape { rect, radius, rim }],
            material,
            shadow: false,
            smoke: None,
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
}

struct Entry {
    proxy: BeizeWindowV1,
    /// As of its last `done`, and what came since.
    shown: Option<DesktopWindow>,
    pending: DesktopWindow,
}

pub(crate) struct Desktop {
    shell: BeizeShellV1,
    entries: Vec<Entry>,
    next: u64,
    /// The list or the appearance changed since the app last heard.
    pub(crate) changed: bool,
    pub(crate) appearance: Appearance,
    /// How light it is behind the menu bar, once the compositor said.
    pub(crate) menu_bar_backdrop: Option<Appearance>,
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
            entries: Vec::new(),
            next: 0,
            changed: false,
            appearance: Appearance::Light,
            menu_bar_backdrop: None,
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
        proxy.set_material(match glass.material {
            Material::Base => beize_glass_v1::Material::Base,
            Material::Live => beize_glass_v1::Material::Live,
            Material::Menu => beize_glass_v1::Material::Menu,
            Material::Clear => beize_glass_v1::Material::Clear,
            Material::Liquid => beize_glass_v1::Material::Liquid,
        });
        proxy.set_shadow(u32::from(glass.shadow));
        let smoke = glass.smoke.unwrap_or(Smoke {
            falloff: 0.0,
            strength: 0.0,
        });
        proxy.set_smoke(f64::from(smoke.falloff), f64::from(smoke.strength));
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
