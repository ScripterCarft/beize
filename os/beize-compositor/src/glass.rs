//! Where the shell wants glass (beize-shell-v1, `beize_glass_v1`; ADR 0028
//! §6): rounded rectangles below parts of a panel or of its popup – one
//! below a menu, several below the control center's tiles –, base, live,
//! menu or liquid glass, with a shadow or not, as macOS' WindowServer draws
//! a material below a view; around the shapes a smoke if asked for: what
//! lies behind blurred less and less away from them. Shapes, material,
//! shadow and smoke are the surface's double-buffered state, applied with
//! its commit (Smithay's cached state); the glass itself is drawn by
//! `backdrop`.

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
    /// Liquid glass with white ink on it, its tone held for the ink: the
    /// control center's tiles.
    Clear,
    /// Liquid glass without ink on it: the dock.
    Liquid,
}

/// The smoke around a glass's shapes (`beize_glass_v1.set_smoke`): over
/// `falloff` px from their edges, blurred by `strength` px at most.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Smoke {
    pub falloff: f32,
    pub strength: f32,
}

/// The most a smoke reaches and blurs: twice what the control center asks
/// (130 px, 4 px), so its kernel and the region it reads stay small
/// whatever a client asks.
const MAX_FALLOFF: f32 = 260.0;
pub const MAX_STRENGTH: f32 = 8.0;

/// A surface's glass, double-buffered: its shapes (none: no glass), what
/// it shows, whether each shape casts a shadow, and the smoke around them.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct GlassState {
    pub shapes: Vec<Shape>,
    pub material: Material,
    pub shadow: bool,
    pub smoke: Option<Smoke>,
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
                    WEnum::Value(beize_glass_v1::Material::Clear) => Material::Clear,
                    WEnum::Value(beize_glass_v1::Material::Liquid) => Material::Liquid,
                    _ => Material::Base,
                };
                set_pending(surface, |state| state.material = material);
            }
            beize_glass_v1::Request::SetShadow { shadow } => {
                set_pending(surface, |state| state.shadow = shadow != 0);
            }
            beize_glass_v1::Request::SetSmoke { falloff, strength } => {
                let (falloff, strength) = (falloff as f32, strength as f32);
                let smoke = (falloff > 0.0 && strength > 0.0).then(|| Smoke {
                    falloff: falloff.min(MAX_FALLOFF),
                    strength: strength.min(MAX_STRENGTH),
                });
                set_pending(surface, |state| state.smoke = smoke);
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
