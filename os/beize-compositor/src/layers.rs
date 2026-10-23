//! Panels of Beize's shell (wlr-layer-shell, ADR 0028 §1): the menu bar,
//! later the dock. Only the shell gets the protocol (`trust`). Panels lie
//! above the windows (layers top and overlay) or below them (background,
//! bottom); a panel's exclusive zone keeps windows out of its strip, so
//! windows fill what is left of the screen.

use smithay::delegate_layer_shell;
use smithay::desktop::{LayerSurface, WindowSurfaceType, layer_map_for_output};
use smithay::output::Output;
use smithay::reexports::wayland_server::protocol::{wl_output::WlOutput, wl_surface::WlSurface};
use smithay::utils::{Logical, Point};
use smithay::wayland::compositor::with_states;
use smithay::wayland::shell::wlr_layer::{
    Layer, LayerSurface as WlrLayerSurface, LayerSurfaceData, WlrLayerShellHandler,
    WlrLayerShellState,
};
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

    fn layer_destroyed(&mut self, surface: WlrLayerSurface) {
        let Some(output) = self.output.clone() else {
            return;
        };
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

/// The panel surface under `at` on `layers`, front to back, and where that
/// surface lies.
pub fn surface_under(
    output: &Output,
    layers: &[Layer],
    at: Point<f64, Logical>,
) -> Option<(WlSurface, Point<f64, Logical>)> {
    let map = layer_map_for_output(output);
    layers.iter().find_map(|&layer| {
        let panel = map.layer_under(layer, at)?;
        let place = map.layer_geometry(panel)?.loc;
        panel
            .surface_under(at - place.to_f64(), WindowSurfaceType::ALL)
            .map(|(surface, offset)| (surface, (offset + place).to_f64()))
    })
}
