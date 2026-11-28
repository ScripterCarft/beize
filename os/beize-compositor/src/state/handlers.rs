//! Smithay's handler traits for `State` and their delegates: what the
//! protocols call into (commits, buffers, seat, selection, idle inhibit).

use super::*;

impl ClientData for ClientState {
    fn initialized(&self, _client: ClientId) {}
    fn disconnected(&self, _client: ClientId, _reason: DisconnectReason) {
        if let Some(ended) = &self.ended {
            // The event loop is gone only with the compositor.
            let _ = ended.send(self.ready.load(Ordering::Relaxed));
        }
    }
}

impl CompositorHandler for State {
    fn compositor_state(&mut self) -> &mut CompositorState {
        &mut self.compositor
    }

    fn new_surface(&mut self, surface: &WlSurface) {
        syncobj::watch(surface);
        crate::timing::watch(surface);
    }

    fn client_compositor_state<'a>(&self, client: &'a Client) -> &'a CompositorClientState {
        // Every client is inserted with ClientState (`insert_client`); the
        // fallback only keeps a bug from being a crash.
        static NONE: LazyLock<CompositorClientState> = LazyLock::new(Default::default);
        client
            .get_data::<ClientState>()
            .map_or(&*NONE, |state| &state.compositor)
    }

    fn commit(&mut self, surface: &WlSurface) {
        // A window's commit without a buffer closes it (`shell::committed`):
        // its picture first, while the renderer still has its buffer.
        if let Some(window) = self.window_for(surface)
            && self.space.element_geometry(&window).is_some()
            && smithay::wayland::compositor::with_states(surface, |states| {
                matches!(
                    states
                        .cached_state
                        .get::<SurfaceAttributes>()
                        .current()
                        .buffer,
                    Some(smithay::wayland::compositor::BufferAssignment::Removed)
                )
            })
        {
            self.close_picture(&window);
        }
        if let Some(gpu) = &self.gpu {
            let replaced = syncobj::applied(gpu.renderer_ref(), surface);
            let submission = gpu.renderer_ref().submitted();
            self.holds.add(submission, replaced);
        }
        on_commit_buffer_handler::<State>(surface);
        if !is_sync_subsurface(surface) {
            let mut root = surface.clone();
            while let Some(parent) = get_parent(&root) {
                root = parent;
            }
            // Also minimized: `bring_back` places it by this geometry.
            if let Some(window) = self.any_window(&root) {
                window.on_commit();
            }
        }
        self.popups.commit(surface);
        self.grown_committed(surface);
        crate::shell::committed(self, surface);
        crate::layers::committed(self, surface);
        self.cursor.committed(surface);
        self.changed();
    }
}

impl DrmSyncobjHandler for State {
    fn drm_syncobj_state(&mut self) -> Option<&mut DrmSyncobjState> {
        self.syncobj.as_mut()
    }
}

impl BufferHandler for State {
    fn buffer_destroyed(&mut self, _buffer: &WlBuffer) {}
}

impl ShmHandler for State {
    fn shm_state(&self) -> &ShmState {
        &self.shm
    }
}

impl DmabufHandler for State {
    fn dmabuf_state(&mut self) -> &mut DmabufState {
        &mut self.dmabuf
    }

    fn dmabuf_imported(
        &mut self,
        _global: &DmabufGlobal,
        dmabuf: Dmabuf,
        notifier: ImportNotifier,
    ) {
        // Imported once, when the client makes it, and kept with it (Smithay's
        // anvil; Weston imports at creation too): its first frame finds it.
        let imported = match self.gpu.as_mut() {
            Some(gpu) => gpu.renderer().import_dmabuf(&dmabuf, None).map(drop),
            None => Err(error("no GPU")),
        };
        match imported {
            Ok(()) => {
                let _ = notifier.successful::<State>();
            }
            // Said, or the client's protocol error is all that shows (0.5.105).
            Err(e) => {
                warn!("a client's dmabuf cannot be shown: {e}");
                notifier.failed();
            }
        }
    }
}

impl SeatHandler for State {
    type KeyboardFocus = WlSurface;
    type PointerFocus = WlSurface;
    type TouchFocus = WlSurface;

    fn seat_state(&mut self) -> &mut SeatState<State> {
        &mut self.seat_state
    }

    fn focus_changed(&mut self, seat: &Seat<State>, focused: Option<&WlSurface>) {
        let client = focused.and_then(|surface| self.display.get_client(surface.id()).ok());
        set_data_device_focus(&self.display, seat, client);
    }

    fn cursor_image(&mut self, _seat: &Seat<State>, image: CursorImageStatus) {
        let now = self.clock.now().into();
        self.cursor.set(image, now);
        self.changed();
    }

    fn led_state_changed(&mut self, _seat: &Seat<State>, leds: LedState) {
        for keyboard in &mut self.keyboards {
            keyboard.led_update(leds.into());
        }
    }
}

impl SelectionHandler for State {
    type SelectionUserData = ();
}

impl DataDeviceHandler for State {
    fn data_device_state(&self) -> &DataDeviceState {
        &self.data_device
    }
}

impl ClientDndGrabHandler for State {}

impl ServerDndGrabHandler for State {}

impl OutputHandler for State {}

impl IdleInhibitHandler for State {
    fn inhibit(&mut self, surface: WlSurface) {
        self.inhibitors.push(surface);
    }

    /// Destroyed by its client: shown until now, if it was. Inhibitors of
    /// a client that ends go without this (`look_at_inhibitors`).
    fn uninhibit(&mut self, surface: WlSurface) {
        self.look_at_inhibitors();
        if let Some(i) = self.inhibitors.iter().position(|s| *s == surface) {
            self.inhibitors.swap_remove(i);
        }
    }
}

/// cursor-shape-v1 also names tablet tools' images; Beize has no tablets
/// yet, so nothing is needed of it.
impl TabletSeatHandler for State {}

delegate_compositor!(State);
delegate_shm!(State);
delegate_dmabuf!(State);
delegate_drm_syncobj!(State);
delegate_seat!(State);
delegate_data_device!(State);
delegate_output!(State);
delegate_xdg_shell!(State);
delegate_xdg_decoration!(State);
delegate_cursor_shape!(State);
delegate_presentation!(State);
delegate_fifo!(State);
delegate_idle_inhibit!(State);
delegate_commit_timing!(State);
delegate_viewporter!(State);
