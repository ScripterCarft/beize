//! What the compositor sends, by object: configures, frame callbacks,
//! the seat's keyboard and pointer, activation tokens (wayland-client's
//! `Dispatch`).

use super::*;

impl Dispatch<WlRegistry, GlobalListContents> for State {
    fn event(
        _: &mut State,
        _: &WlRegistry,
        _: <WlRegistry as Proxy>::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
    }
}

impl Dispatch<XdgWmBase, ()> for State {
    fn event(
        _: &mut State,
        wm_base: &XdgWmBase,
        event: xdg_wm_base::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
        if let xdg_wm_base::Event::Ping { serial } = event {
            wm_base.pong(serial);
        }
    }
}

/// A window's frame is shown (`Window::draw_once` asked): the window is
/// ready for the next, with the latest configure.
impl Dispatch<WlCallback, (WindowId, u32)> for State {
    fn event(
        state: &mut State,
        _: &WlCallback,
        event: wl_callback::Event,
        (id, frame): &(WindowId, u32),
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
        let wl_callback::Event::Done { .. } = event else {
            return;
        };
        let Some(surface) = state.surface_mut(*id) else {
            return;
        };
        surface.shown.set(surface.shown.get().wrapping_add(1));
        if surface.on_its_way.get() == Some(*frame) {
            surface.on_its_way.set(None);
            surface.events.ready = true;
        }
    }
}

impl Dispatch<XdgSurface, WindowId> for State {
    fn event(
        state: &mut State,
        xdg_surface: &XdgSurface,
        event: xdg_surface::Event,
        id: &WindowId,
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
        // Acknowledged when the app takes it, before the frame for it is
        // presented (`State::apply_configures`).
        if let xdg_surface::Event::Configure { serial } = event
            && let Some(surface) = state.surface_mut(*id)
        {
            surface.next.ack = Some(Ack::Toplevel(xdg_surface.clone(), serial));
        }
    }
}

impl Dispatch<XdgToplevel, WindowId> for State {
    fn event(
        state: &mut State,
        _: &XdgToplevel,
        event: xdg_toplevel::Event,
        id: &WindowId,
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
        let Some(surface) = state.surface_mut(*id) else {
            return;
        };
        match event {
            xdg_toplevel::Event::Configure {
                width,
                height,
                states,
            } => {
                let has = |wanted: xdg_toplevel::State| {
                    states
                        .as_chunks::<4>()
                        .0
                        .iter()
                        .any(|s| u32::from_ne_bytes(*s) == u32::from(wanted))
                };
                // Taken with the xdg_surface's configure that follows.
                surface.next.asked = (width, height);
                surface.next.activated = has(xdg_toplevel::State::Activated);
                surface.next.maximized = has(xdg_toplevel::State::Maximized);
                surface.next.fullscreen = has(xdg_toplevel::State::Fullscreen);
            }
            xdg_toplevel::Event::Close => surface.events.closed = true,
            _ => {}
        }
    }
}

impl Dispatch<XdgPopup, WindowId> for State {
    fn event(
        state: &mut State,
        _: &XdgPopup,
        event: xdg_popup::Event,
        id: &WindowId,
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
        let Some(surface) = state.surface_mut(*id) else {
            return;
        };
        match event {
            // Its size, taken with the xdg_surface's configure that follows;
            // where it lies is the compositor's.
            xdg_popup::Event::Configure { width, height, .. } => {
                surface.next.asked = (width, height);
            }
            // A click elsewhere, or the grab was declined.
            xdg_popup::Event::PopupDone => surface.events.closed = true,
            _ => {}
        }
    }
}

impl Dispatch<ZwlrLayerSurfaceV1, WindowId> for State {
    fn event(
        state: &mut State,
        layer: &ZwlrLayerSurfaceV1,
        event: zwlr_layer_surface_v1::Event,
        id: &WindowId,
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
        let Some(surface) = state.surface_mut(*id) else {
            return;
        };
        match event {
            zwlr_layer_surface_v1::Event::Configure {
                serial,
                width,
                height,
            } => {
                // As for a toplevel (`XdgSurface`): acknowledged when the
                // app takes it.
                let size = |n: u32| i32::try_from(n).unwrap_or(i32::MAX);
                surface.next.asked = (size(width), size(height));
                surface.next.ack = Some(Ack::Panel(layer.clone(), serial));
            }
            // The compositor took the panel away (its screen went).
            zwlr_layer_surface_v1::Event::Closed => surface.events.closed = true,
            _ => {}
        }
    }
}

impl Dispatch<WlSeat, ()> for State {
    fn event(
        state: &mut State,
        seat: &WlSeat,
        event: wl_seat::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<State>,
    ) {
        let wl_seat::Event::Capabilities {
            capabilities: WEnum::Value(caps),
        } = event
        else {
            return;
        };
        let keyboard = caps.contains(wl_seat::Capability::Keyboard);
        match (state.wl_keyboard.take(), keyboard) {
            (None, true) => state.wl_keyboard = Some(seat.get_keyboard(qh, ())),
            (Some(gone), false) => {
                gone.release();
                // As if the window lost it: no key stays held.
                let on = state.keyboard_on.take();
                if let Some(surface) = on.and_then(|id| state.surface(id)) {
                    let leave = wl_keyboard::Event::Leave {
                        serial: 0,
                        surface: surface.wl.clone(),
                    };
                    let mut input = Vec::new();
                    state.keyboard.event(leave, &mut input);
                    state.deliver(on, input);
                }
            }
            (kept, _) => state.wl_keyboard = kept,
        }
        let pointer = caps.contains(wl_seat::Capability::Pointer);
        match (state.wl_pointer.take(), pointer) {
            (None, true) => {
                let pointer = seat.get_pointer(qh, ());
                state.cursor = state
                    .cursor_shapes
                    .as_ref()
                    .map(|shapes| shapes.get_pointer(&pointer, qh, ()));
                state.wl_pointer = Some(pointer);
            }
            (Some(gone), false) => {
                if let Some(device) = state.cursor.take() {
                    device.destroy();
                }
                gone.release();
            }
            (kept, _) => state.wl_pointer = kept,
        }
    }
}

impl Dispatch<WlKeyboard, ()> for State {
    fn event(
        state: &mut State,
        _: &WlKeyboard,
        event: wl_keyboard::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
        if let wl_keyboard::Event::Key { serial, .. } | wl_keyboard::Event::Enter { serial, .. } =
            &event
        {
            state.serial = *serial;
        }
        if let wl_keyboard::Event::Enter { surface, .. } = &event {
            state.keyboard_on = state.id_of(surface);
        }
        let leave = matches!(event, wl_keyboard::Event::Leave { .. });
        let mut input = Vec::new();
        state.keyboard.event(event, &mut input);
        state.deliver(state.keyboard_on, input);
        if leave {
            state.keyboard_on = None;
        }
    }
}

impl Dispatch<WlPointer, ()> for State {
    fn event(
        state: &mut State,
        _: &WlPointer,
        event: wl_pointer::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
        if let wl_pointer::Event::Button { serial, .. } = &event {
            state.serial = *serial;
        }
        if let wl_pointer::Event::Enter {
            surface, serial, ..
        } = &event
        {
            state.pointer_on = state.id_of(surface);
            // The image is the app's to name again (`dispatch`).
            state.entered = *serial;
            state.cursor_shown = None;
        }
        let leave = matches!(event, wl_pointer::Event::Leave { .. });
        let mut input = Vec::new();
        state.pointer.event(event, &mut input);
        state.deliver(state.pointer_on, input);
        if leave {
            state.pointer_on = None;
        }
    }
}

impl Dispatch<XdgActivationTokenV1, String> for State {
    /// The token for the app `app_id`, once made.
    fn event(
        state: &mut State,
        token: &XdgActivationTokenV1,
        event: xdg_activation_token_v1::Event,
        app_id: &String,
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
        if let xdg_activation_token_v1::Event::Done { token: made } = event {
            state.tokens.push((app_id.clone(), made));
            token.destroy();
        }
    }
}

delegate_noop!(State: ignore WlCompositor);
delegate_noop!(State: ignore WlSurface);
delegate_noop!(State: ignore WlRegion);
delegate_noop!(State: ignore ZwlrLayerShellV1);
delegate_noop!(State: ignore XdgPositioner);
delegate_noop!(State: WpCursorShapeManagerV1);
delegate_noop!(State: WpCursorShapeDeviceV1);
delegate_noop!(State: XdgActivationV1);
