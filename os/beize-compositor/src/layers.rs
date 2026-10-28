//! Panels of Beize's shell (wlr-layer-shell, ADR 0028 §1): the menu bar,
//! later the dock. Only the shell gets the protocol (`trust`). Panels lie
//! above the windows (layers top and overlay) or below them (background,
//! bottom); a panel's exclusive zone keeps windows out of its strip, so
//! windows fill what is left of the screen.
//!
//! With a fullscreen window in front, the panels above the windows hide
//! and their strips stay where they are (no window moves); the pointer at
//! the screen's top or bottom edge shows the panels along it until it
//! leaves their strip, as macOS shows its menu bar and dock (`reveal`).
//!
//! When the shell fails, its panels stay as last drawn until a new shell
//! is ready (`State::keep_panels`).

use smithay::delegate_layer_shell;
use smithay::desktop::{LayerSurface, WindowSurfaceType, layer_map_for_output};
use smithay::output::Output;
use smithay::reexports::wayland_server::Resource;
use smithay::reexports::wayland_server::protocol::{wl_output::WlOutput, wl_surface::WlSurface};
use smithay::utils::{Logical, Point, Rectangle};
use smithay::wayland::compositor::with_states;
use smithay::wayland::shell::wlr_layer::{
    Layer, LayerSurface as WlrLayerSurface, LayerSurfaceData, WlrLayerShellHandler,
    WlrLayerShellState,
};
use smithay::wayland::shell::xdg::PopupSurface;
use tracing::{info, warn};

use crate::state::State;

impl WlrLayerShellHandler for State {
    fn shell_state(&mut self) -> &mut WlrLayerShellState {
        &mut self.layer_shell
    }

    fn new_layer_surface(
        &mut self,
        surface: WlrLayerSurface,
        _output: Option<WlOutput>,
        layer: Layer,
        namespace: String,
    ) {
        // One screen: a panel goes there, whichever it named.
        let Some(output) = self.output.clone() else {
            surface.send_close();
            return;
        };
        info!("panel {namespace} ({layer:?})");
        let mut map = layer_map_for_output(&output);
        if let Err(e) = map.map_layer(&LayerSurface::new(surface, namespace)) {
            warn!("a panel: {e}");
        }
    }

    /// A menu of the shell's: placed now that its parent, the panel, is
    /// known (`get_popup` comes after the popup was made).
    fn new_popup(&mut self, _parent: WlrLayerSurface, popup: PopupSurface) {
        crate::shell::unconstrain(self, &popup);
    }

    fn layer_destroyed(&mut self, surface: WlrLayerSurface) {
        let Some(output) = self.output.clone() else {
            return;
        };
        // The shell failed: its connection is gone before its objects
        // (wayland-backend), its panels stay until a new one is ready.
        if self.display.get_client(surface.wl_surface().id()).is_err() {
            self.keep_panels();
        }
        {
            let mut map = layer_map_for_output(&output);
            let gone = map
                .layers()
                .find(|l| l.layer_surface() == &surface)
                .cloned();
            if let Some(gone) = gone {
                map.unmap_layer(&gone);
            }
        }
        self.arrange_windows();
        self.changed();
    }
}

delegate_layer_shell!(State);

/// After a commit: the first configure of a new panel, which the protocol
/// wants only after the client's initial commit, and the panels' places –
/// which may change the room left for windows.
pub fn committed(state: &mut State, surface: &WlSurface) {
    let Some(output) = state.output.clone() else {
        return;
    };
    let zone_changed = {
        let mut map = layer_map_for_output(&output);
        if map
            .layer_for_surface(surface, WindowSurfaceType::TOPLEVEL)
            .is_none()
        {
            return;
        }
        let before = map.non_exclusive_zone();
        map.arrange();
        let initial_sent = with_states(surface, |states| {
            states
                .data_map
                .get::<LayerSurfaceData>()
                .and_then(|data| data.lock().ok().map(|d| d.initial_configure_sent))
                .unwrap_or(true)
        });
        if !initial_sent
            && let Some(layer) = map.layer_for_surface(surface, WindowSurfaceType::TOPLEVEL)
        {
            layer.layer_surface().send_configure();
        }
        map.non_exclusive_zone() != before
    };
    if zone_changed {
        state.arrange_windows();
    }
}

/// The edge of the screen a panel lies along.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Top,
    Bottom,
}

/// Which edge of `screen` a panel at `place` lies along: the nearer one.
pub fn side(screen: Rectangle<i32, Logical>, place: Rectangle<i32, Logical>) -> Side {
    if place.loc.y + place.size.h / 2 < screen.loc.y + screen.size.h / 2 {
        Side::Top
    } else {
        Side::Bottom
    }
}

impl State {
    /// Whether the topmost window is fullscreen: the panels above the
    /// windows show only when revealed.
    pub fn fullscreen_in_front(&self) -> bool {
        self.space
            .elements()
            .last()
            .is_some_and(crate::windows::fullscreen)
    }

    /// Whether a panel above the windows at `place` shows now.
    pub fn panel_shown(&self, place: Rectangle<i32, Logical>) -> bool {
        !self.fullscreen_in_front() || self.revealed == Some(side(self.screen_area(), place))
    }

    /// The strip the panels above the windows along `edge` take, from the
    /// screen's edge across; None without such panels.
    pub fn band(&self, edge: Side) -> Option<Rectangle<i32, Logical>> {
        let output = self.output.as_ref()?;
        let screen = self.screen_area();
        let map = layer_map_for_output(output);
        let mut band: Option<Rectangle<i32, Logical>> = None;
        for layer in [Layer::Top, Layer::Overlay] {
            for panel in map.layers_on(layer) {
                let Some(place) = map.layer_geometry(panel) else {
                    continue;
                };
                if side(screen, place) != edge {
                    continue;
                }
                let strip = match edge {
                    Side::Top => Rectangle::new(
                        screen.loc,
                        (screen.size.w, place.loc.y + place.size.h - screen.loc.y).into(),
                    ),
                    Side::Bottom => Rectangle::new(
                        (screen.loc.x, place.loc.y).into(),
                        (screen.size.w, screen.loc.y + screen.size.h - place.loc.y).into(),
                    ),
                };
                band = Some(band.map_or(strip, |b| b.merge(strip)));
            }
        }
        band
    }

    /// With a fullscreen window in front: the pointer at the screen's top
    /// or bottom edge shows the panels along it, until it leaves their
    /// strip. True if what shows changed.
    pub fn reveal(&mut self) -> bool {
        let front = self.fullscreen_in_front();
        let at = self.pointer;
        let screen = self.screen_area().to_f64();
        let inside = |edge| self.band(edge).is_some_and(|b| b.to_f64().contains(at));
        // On the edge's row of pixels: a mouse stops at its last pixel, an
        // absolute device (tablet, remote desktop) lands within it.
        let at_edge = if at.y < screen.loc.y + 1.0 {
            Some(Side::Top)
        } else if at.y >= screen.loc.y + screen.size.h - 1.0 {
            Some(Side::Bottom)
        } else {
            None
        };
        let revealed = if !front {
            None
        } else if let Some(kept) = self.revealed.filter(|&edge| inside(edge)) {
            Some(kept)
        } else if !self.fullscreen_front {
            // Fullscreen just came to the front – a click in the dock
            // brought it: the panels under the pointer stay.
            [Side::Top, Side::Bottom]
                .into_iter()
                .find(|&edge| inside(edge))
                .or(at_edge)
        } else {
            at_edge
        };
        self.fullscreen_front = front;
        if revealed == self.revealed {
            return false;
        }
        self.revealed = revealed;
        info!("panels over fullscreen: {revealed:?}");
        self.changed();
        true
    }
}

/// Where the panel with the surface `surface` lies, if it is one.
pub fn place_of(state: &State, surface: &WlSurface) -> Option<Rectangle<i32, Logical>> {
    let map = layer_map_for_output(state.output.as_ref()?);
    let panel = map.layer_for_surface(surface, WindowSurfaceType::TOPLEVEL)?;
    map.layer_geometry(panel)
}

/// The panel surface under `at` on `layers`, front to back, and where that
/// surface lies; only panels `shown` at their place. A panel's popups – the
/// shell's menus – reach beyond its place and lie in front of it.
pub fn surface_under(
    output: &Output,
    layers: &[Layer],
    at: Point<f64, Logical>,
    shown: impl Fn(Rectangle<i32, Logical>) -> bool,
) -> Option<(WlSurface, Point<f64, Logical>)> {
    let map = layer_map_for_output(output);
    layers.iter().find_map(|&layer| {
        map.layers_on(layer).rev().find_map(|panel| {
            let place = map.layer_geometry(panel)?;
            if !shown(place) {
                return None;
            }
            let within = at - place.loc.to_f64();
            panel
                .surface_under(within, WindowSurfaceType::POPUP)
                .or_else(|| {
                    place.to_f64().contains(at).then(|| {
                        panel.surface_under(
                            within,
                            WindowSurfaceType::TOPLEVEL | WindowSurfaceType::SUBSURFACE,
                        )
                    })?
                })
                .map(|(surface, offset)| (surface, (offset + place.loc).to_f64()))
        })
    })
}
