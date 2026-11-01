//! `--protocol-errors`: the errors Beize's own protocols promise, each on a
//! connection of its own (an error ends it). One line each,
//! `protocol <case>: error <code>` or `protocol <case>: none`:
//! - `second menu`: a second beize_app_menu_v1 for one window;
//! - `menu without window`: one for a surface with no toplevel role;
//! - `glass for a toplevel`: a beize_glass_v1 for a toplevel;
//! - `second glass`: a second one for a surface.

use beize_protocols::app::client::beize_app_menu_v1::{self, BeizeAppMenuV1};
use beize_protocols::app::client::beize_glass_manager_v1::{self, BeizeGlassManagerV1};
use beize_protocols::app::client::beize_glass_v1::{self, BeizeGlassV1};
use beize_protocols::app::client::beize_menu_manager_v1::BeizeMenuManagerV1;
use wayland_client::backend::WaylandError;
use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::{
    wl_compositor::WlCompositor, wl_registry::WlRegistry, wl_surface::WlSurface,
};
use wayland_client::{Connection, Dispatch, DispatchError, QueueHandle, delegate_noop};
use wayland_protocols::xdg::shell::client::{
    xdg_surface::{self, XdgSurface},
    xdg_toplevel::{self, XdgToplevel},
    xdg_wm_base::{self, XdgWmBase},
};

struct App;

#[derive(Clone, Copy)]
enum Case {
    SecondMenu,
    MenuWithoutWindow,
    GlassForToplevel,
    SecondGlass,
}

pub fn run() -> Result<(), String> {
    for (name, case) in [
        ("second menu", Case::SecondMenu),
        ("menu without window", Case::MenuWithoutWindow),
        ("glass for a toplevel", Case::GlassForToplevel),
        ("second glass", Case::SecondGlass),
    ] {
        println!("protocol {name}: {}", one(case)?);
    }
    Ok(())
}

fn one(case: Case) -> Result<String, String> {
    let connection = Connection::connect_to_env().map_err(|e| format!("connecting: {e}"))?;
    let (globals, mut queue) =
        registry_queue_init::<App>(&connection).map_err(|e| format!("registry: {e}"))?;
    let qh = queue.handle();
    let compositor: WlCompositor = globals
        .bind(&qh, 4..=4, ())
        .map_err(|e| format!("no wl_compositor: {e}"))?;
    let wm_base: XdgWmBase = globals
        .bind(&qh, 1..=1, ())
        .map_err(|e| format!("no xdg_wm_base: {e}"))?;
    let menus: BeizeMenuManagerV1 = globals
        .bind(&qh, 1..=1, ())
        .map_err(|e| format!("no beize_menu_manager_v1: {e}"))?;
    let glass: BeizeGlassManagerV1 = globals
        .bind(&qh, 1..=1, ())
        .map_err(|e| format!("no beize_glass_manager_v1: {e}"))?;
    let surface = compositor.create_surface(&qh, ());
    let toplevel = |surface: &WlSurface| {
        let xdg = wm_base.get_xdg_surface(surface, &qh, ());
        xdg.get_toplevel(&qh, ())
    };
    match case {
        Case::SecondMenu => {
            let _toplevel = toplevel(&surface);
            menus.get_menu(&surface, &qh, ());
            menus.get_menu(&surface, &qh, ());
        }
        Case::MenuWithoutWindow => {
            menus.get_menu(&surface, &qh, ());
        }
        Case::GlassForToplevel => {
            let _toplevel = toplevel(&surface);
            glass.get_glass(&surface, &qh, ());
        }
        Case::SecondGlass => {
            glass.get_glass(&surface, &qh, ());
            glass.get_glass(&surface, &qh, ());
        }
    }
    Ok(match queue.roundtrip(&mut App) {
        Ok(_) => "none".to_owned(),
        Err(DispatchError::Backend(WaylandError::Protocol(e))) => format!("error {}", e.code),
        Err(e) => return Err(format!("roundtrip: {e}")),
    })
}

impl Dispatch<WlRegistry, GlobalListContents> for App {
    fn event(
        _: &mut App,
        _: &WlRegistry,
        _: <WlRegistry as wayland_client::Proxy>::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<App>,
    ) {
    }
}

impl Dispatch<XdgWmBase, ()> for App {
    fn event(
        _: &mut App,
        wm_base: &XdgWmBase,
        event: xdg_wm_base::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<App>,
    ) {
        if let xdg_wm_base::Event::Ping { serial } = event {
            wm_base.pong(serial);
        }
    }
}

impl Dispatch<XdgSurface, ()> for App {
    fn event(
        _: &mut App,
        _: &XdgSurface,
        _: xdg_surface::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<App>,
    ) {
    }
}

impl Dispatch<XdgToplevel, ()> for App {
    fn event(
        _: &mut App,
        _: &XdgToplevel,
        _: xdg_toplevel::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<App>,
    ) {
    }
}

impl Dispatch<BeizeGlassManagerV1, ()> for App {
    fn event(
        _: &mut App,
        _: &BeizeGlassManagerV1,
        _: beize_glass_manager_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<App>,
    ) {
    }
}

impl Dispatch<BeizeGlassV1, ()> for App {
    fn event(
        _: &mut App,
        _: &BeizeGlassV1,
        _: beize_glass_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<App>,
    ) {
    }
}

impl Dispatch<BeizeAppMenuV1, ()> for App {
    fn event(
        _: &mut App,
        _: &BeizeAppMenuV1,
        _: beize_app_menu_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<App>,
    ) {
    }
}

delegate_noop!(App: ignore WlCompositor);
delegate_noop!(App: ignore WlSurface);
delegate_noop!(App: ignore BeizeMenuManagerV1);
