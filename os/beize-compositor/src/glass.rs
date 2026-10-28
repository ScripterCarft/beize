//! Where the shell wants glass (beize-shell-v1, `beize_glass_v1`; ADR 0028
//! §6): rounded rectangles below parts of a panel or of its popup – one
//! below a menu, several below the control center's tiles –, base, live or
//! menu glass, with a shadow or not, as macOS' WindowServer draws a
//! material below a view. Shapes, material and shadow are the surface's
//! double-buffered state, applied with its commit (Smithay's cached state);
//! the glass itself is drawn by `backdrop`.

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

/// What a glass shows (`beize_glass_v1.material`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Material {
    /// The background, blurred once.
    #[default]
    Base,
    /// What lies behind it, windows too.
    Live,
    /// Live, tinted thickly: a menu's.
    Menu,
}

/// A surface's glass, double-buffered: its shapes (none: no glass), what
/// it shows, and whether each shape casts a shadow.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct GlassState {
    pub shapes: Vec<Shape>,
    pub material: Material,
    pub shadow: bool,
}

impl Cacheable for GlassState {
    fn commit(&mut self, _dh: &DisplayHandle) -> GlassState {
        self.clone()
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
        let shape = |x, y, width, height, radius: f64, rim| {
            (width > 0 && height > 0).then(|| Shape {
                rect: Rectangle::new((x, y).into(), (width, height).into()),
                radius: (radius as f32).max(0.0),
                rim: rim != 0,
            })
        };
        match request {
            beize_glass_v1::Request::SetShape {
                x,
                y,
                width,
                height,
                radius,
                rim,
            } => {
                let shape = shape(x, y, width, height, radius, rim);
                set_pending(surface, |state| state.shapes = shape.into_iter().collect());
            }
            beize_glass_v1::Request::AddShape {
                x,
                y,
                width,
                height,
                radius,
                rim,
            } => {
                let shape = shape(x, y, width, height, radius, rim);
                set_pending(surface, |state| state.shapes.extend(shape));
            }
            beize_glass_v1::Request::SetMaterial { material } => {
                let material = match material {
                    WEnum::Value(beize_glass_v1::Material::Live) => Material::Live,
                    WEnum::Value(beize_glass_v1::Material::Menu) => Material::Menu,
                    _ => Material::Base,
                };
                set_pending(surface, |state| state.material = material);
            }
            beize_glass_v1::Request::SetShadow { shadow } => {
                set_pending(surface, |state| state.shadow = shadow != 0);
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
