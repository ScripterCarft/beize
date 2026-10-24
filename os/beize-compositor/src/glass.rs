//! Where the shell wants glass (beize-shell-v1, `beize_glass_v1`; ADR 0028
//! §6): a rounded rectangle below part of a panel, base or live glass, as
//! macOS' WindowServer draws a material below a view. Shape and material
//! are the surface's double-buffered state, applied with its commit
//! (Smithay's cached state); the glass itself is drawn by `backdrop`.

use beize_protocols::shell::server::beize_glass_v1::{self, BeizeGlassV1};
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::{
    Client, DataInit, Dispatch, DisplayHandle, Resource, WEnum,
};
use smithay::utils::{Logical, Rectangle};
use smithay::wayland::compositor::{Cacheable, with_states};

use crate::state::State;

/// A glass: in the surface's coordinates, with round corners, and whether
/// a rim of light runs along it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Shape {
    pub rect: Rectangle<i32, Logical>,
    pub radius: f32,
    pub rim: bool,
}

/// A surface's glass, double-buffered: its shape, and whether it is live
/// (shows what lies behind it) rather than base glass.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct GlassState {
    pub shape: Option<Shape>,
    pub live: bool,
}

impl Cacheable for GlassState {
    fn commit(&mut self, _dh: &DisplayHandle) -> GlassState {
        *self
    }

    fn merge_into(self, into: &mut GlassState, _dh: &DisplayHandle) {
        *into = self;
    }
}

/// Changes the glass `surface` gets with its next commit.
fn set_pending(surface: &WlSurface, change: impl FnOnce(&mut GlassState)) {
    if !surface.is_alive() {
        return;
    }
    with_states(surface, |states| {
        change(states.cached_state.get::<GlassState>().pending());
    });
}

impl Dispatch<BeizeGlassV1, WlSurface> for State {
    fn request(
        _: &mut State,
        _: &Client,
        _: &BeizeGlassV1,
        request: beize_glass_v1::Request,
        surface: &WlSurface,
        _: &DisplayHandle,
        _: &mut DataInit<'_, State>,
    ) {
        // `destroy` ends in `destroyed`.
        match request {
            beize_glass_v1::Request::SetShape {
                x,
                y,
                width,
                height,
                radius,
                rim,
            } => {
                let shape = (width > 0 && height > 0).then(|| Shape {
                    rect: Rectangle::new((x, y).into(), (width, height).into()),
                    radius: (radius as f32).max(0.0),
                    rim: rim != 0,
                });
                set_pending(surface, |state| state.shape = shape);
            }
            beize_glass_v1::Request::SetMaterial { material } => {
                let live = matches!(material, WEnum::Value(beize_glass_v1::Material::Live));
                set_pending(surface, |state| state.live = live);
            }
            _ => {}
        }
    }

    fn destroyed(
        _: &mut State,
        _: smithay::reexports::wayland_server::backend::ClientId,
        _: &BeizeGlassV1,
        surface: &WlSurface,
    ) {
        set_pending(surface, |state| *state = GlassState::default());
    }
}
