//! An app's windows: toplevels, panels, overlays, popups, their size,
//! glass and input region, and drawing a frame into one.

use super::*;

impl App {
    /// Opens a window with Beize UI's title bar; it shows once its first
    /// `configured` is drawn.
    pub fn open(&mut self, title: &str, app_id: &str) -> Result<WindowId> {
        let id = self.add(
            Some(TitleBar::new(title)),
            false,
            |globals, qh, surface, id| {
                let wm_base: XdgWmBase = globals
                    .bind(qh, 1..=6, ())
                    .context("the compositor has no xdg_wm_base")?;
                let xdg_surface = wm_base.get_xdg_surface(surface, qh, id);
                let toplevel = xdg_surface.get_toplevel(qh, id);
                toplevel.set_title(title.into());
                toplevel.set_app_id(app_id.into());
                Ok(Role::Toplevel {
                    _wm_base: wm_base,
                    _xdg_surface: xdg_surface,
                    toplevel,
                })
            },
        )?;
        // The first window takes the token the app was started with.
        if let (Some(token), Some(activation), Some(surface)) = (
            self.startup_token.take(),
            &self.state.activation,
            self.state.surface(id),
        ) {
            activation.activate(token, &surface.wl);
        }
        Ok(id)
    }

    /// Opens a panel of Beize's shell. Only the shell may: the compositor
    /// offers the protocol to no other client.
    pub fn panel(&mut self, panel: &Panel<'_>) -> Result<WindowId> {
        use zwlr_layer_surface_v1::{Anchor, KeyboardInteractivity};
        self.add(None, panel.translucent, |globals, qh, surface, id| {
            let layer_shell: ZwlrLayerShellV1 = globals
                .bind(qh, 1..=4, ())
                .context("the compositor offers no panels (wlr-layer-shell) to this program")?;
            let layer = layer_shell.get_layer_surface(
                surface,
                None,
                zwlr_layer_shell_v1::Layer::Top,
                panel.namespace.into(),
                qh,
                id,
            );
            let along = match panel.edge {
                Edge::Top => Anchor::Top,
                Edge::Bottom => Anchor::Bottom,
            };
            // Anchored left and right with width 0, it is as wide as the
            // screen; otherwise centred.
            let across = if panel.width == 0 {
                Anchor::Left | Anchor::Right
            } else {
                Anchor::empty()
            };
            layer.set_anchor(across | along);
            layer.set_size(panel.width, panel.height);
            let margin = i32::try_from(panel.margin).unwrap_or(i32::MAX);
            match panel.edge {
                Edge::Top => layer.set_margin(margin, 0, 0, 0),
                Edge::Bottom => layer.set_margin(0, 0, margin, 0),
            }
            // The margin comes on top of the zone (the protocol's rule).
            layer.set_exclusive_zone(i32::try_from(panel.height).unwrap_or(i32::MAX));
            layer.set_keyboard_interactivity(KeyboardInteractivity::None);
            Ok(Role::Panel {
                _layer_shell: layer_shell,
                layer,
            })
        })
    }

    /// Opens an overlay (`Overlay`). Only the shell may, as a panel.
    pub fn overlay(&mut self, overlay: &Overlay<'_>) -> Result<WindowId> {
        use zwlr_layer_surface_v1::{Anchor, KeyboardInteractivity};
        self.add(None, true, |globals, qh, surface, id| {
            let layer_shell: ZwlrLayerShellV1 = globals
                .bind(qh, 4..=4, ())
                .context("the compositor offers no overlays to this program")?;
            let layer = layer_shell.get_layer_surface(
                surface,
                None,
                zwlr_layer_shell_v1::Layer::Overlay,
                overlay.namespace.into(),
                qh,
                id,
            );
            // Anchored nowhere it is centred; a zone of 0 keeps clear of the
            // panels' strips and takes none of its own.
            layer.set_anchor(Anchor::empty());
            layer.set_size(overlay.size.0, overlay.size.1);
            layer.set_exclusive_zone(0);
            layer.set_keyboard_interactivity(if overlay.keyboard {
                KeyboardInteractivity::OnDemand
            } else {
                KeyboardInteractivity::None
            });
            Ok(Role::Panel {
                _layer_shell: layer_shell,
                layer,
            })
        })
    }

    /// Opens a popup of the window `parent` (`Popup`), with the click just
    /// handled; it shows once its first `configured` is drawn. Its content
    /// is translucent: glass may lie below it (`set_glass`).
    pub fn popup(&mut self, parent: WindowId, popup: &Popup) -> Result<WindowId> {
        use xdg_positioner::{Anchor, ConstraintAdjustment, Gravity};
        let parent = self
            .windows
            .iter()
            .find(|w| w.id == parent)
            .context("no such window")?
            .popup_parent();
        let seat = self.seat.clone();
        let serial = self.state.serial;
        self.add(None, true, |globals, qh, surface, id| {
            let wm_base: XdgWmBase = globals
                .bind(qh, 1..=6, ())
                .context("the compositor has no xdg_wm_base")?;
            let positioner = wm_base.create_positioner(qh, ());
            let px = |v: u32| i32::try_from(v.max(1)).unwrap_or(i32::MAX);
            positioner.set_size(px(popup.size.0), px(popup.size.1));
            let a = popup.anchor;
            positioner.set_anchor_rect(
                a.x.round() as i32,
                a.y.round() as i32,
                (a.w.round() as i32).max(1),
                (a.h.round() as i32).max(1),
            );
            let (anchor, gravity) = match (popup.align, popup.above) {
                (Align::Left, false) => (Anchor::BottomLeft, Gravity::BottomRight),
                (Align::Right, false) => (Anchor::BottomRight, Gravity::BottomLeft),
                (Align::Left, true) => (Anchor::TopLeft, Gravity::TopRight),
                (Align::Right, true) => (Anchor::TopRight, Gravity::TopLeft),
                (Align::Centre, false) => (Anchor::Bottom, Gravity::Bottom),
                (Align::Centre, true) => (Anchor::Top, Gravity::Top),
                (Align::Beside, _) => (Anchor::TopRight, Gravity::BottomRight),
            };
            positioner.set_anchor(anchor);
            positioner.set_gravity(gravity);
            if popup.align == Align::Beside {
                positioner.set_offset(-BESIDE_OVERLAP, -BESIDE_RISE);
                // No flip: Smithay flips anchor and gravity but not the offset,
                // which would leave a gap where a press closes both menus.
                positioner.set_constraint_adjustment(
                    ConstraintAdjustment::SlideX | ConstraintAdjustment::SlideY,
                );
            } else {
                positioner.set_constraint_adjustment(
                    ConstraintAdjustment::SlideX
                        | ConstraintAdjustment::SlideY
                        | ConstraintAdjustment::FlipY,
                );
            }
            let xdg_surface = wm_base.get_xdg_surface(surface, qh, id);
            let xdg_parent = match &parent {
                PopupParent::Xdg(xdg) => Some(xdg),
                PopupParent::Panel(_) => None,
            };
            let xdg_popup = xdg_surface.get_popup(xdg_parent, &positioner, qh, id);
            if let PopupParent::Panel(layer) = &parent {
                layer.get_popup(&xdg_popup);
            }
            positioner.destroy();
            // Before the first commit, as the protocol wants.
            if let Some(seat) = &seat {
                xdg_popup.grab(seat, serial);
            }
            Ok(Role::Popup {
                _wm_base: wm_base,
                xdg_surface,
                popup: xdg_popup,
            })
        })
    }

    /// A context menu's popup at `at` in the content of the window `parent`
    /// (`Popup`), with the click just handled: its top left corner at the
    /// pointer, kept on the screen.
    pub fn context_popup(
        &mut self,
        parent: WindowId,
        at: (f32, f32),
        size: (u32, u32),
    ) -> Result<WindowId> {
        let below_bar = match (
            self.windows.iter().find(|w| w.id == parent),
            self.state.surface(parent),
        ) {
            (Some(window), Some(surface)) => window.bar_height(surface) as f32,
            _ => 0.0,
        };
        self.popup(
            parent,
            &Popup {
                anchor: beize_draw::Rect {
                    x: at.0,
                    y: at.1 + below_bar,
                    w: 1.0,
                    h: 1.0,
                },
                size,
                align: Align::Left,
                above: false,
            },
        )
    }

    /// Closes the window `id`: a popup the app is done with (a menu's choice
    /// made), or one the compositor closed (`Events::closed`).
    pub fn close(&mut self, id: WindowId) {
        let Some(i) = self.windows.iter().position(|w| w.id == id) else {
            return;
        };
        let qh = self.queue.handle();
        if let Some(wl) = self.state.surface(id).map(|s| s.wl.clone())
            && let Some(glasses) = &mut self.state.glasses
        {
            glasses.set_glass(id, &wl, None, &qh);
        }
        if let Some(j) = self.state.app_menus.iter().position(|(w, _)| *w == id) {
            self.state.app_menus.swap_remove(j).1.destroy();
        }
        let window = self.windows.swap_remove(i);
        let gpu = &self.renderer.gpu;
        // SAFETY: nothing records now; after the wait the GPU uses nothing
        // of the window.
        if let Err(e) = unsafe { gpu.shared.device.device_wait_idle() } {
            warn!("waiting for the GPU: {e}");
        }
        window.unmap();
        window.destroy(gpu);
        self.renderer.forget(id);
        if let Some(j) = self.state.surfaces.iter().position(|s| s.id == id) {
            self.state.surfaces.swap_remove(j).wl.destroy();
        }
        if self.state.keyboard_on == Some(id) {
            self.state.keyboard_on = None;
        }
        if self.state.pointer_on == Some(id) {
            self.state.pointer_on = None;
        }
    }

    /// A new surface with its `role`, its Vulkan surface, and the first
    /// commit, which asks the compositor for a configure.
    pub(super) fn add(
        &mut self,
        bar: Option<TitleBar>,
        translucent: bool,
        role: impl FnOnce(&GlobalList, &QueueHandle<State>, &WlSurface, WindowId) -> Result<Role>,
    ) -> Result<WindowId> {
        let id = WindowId(self.next_id);
        self.next_id += 1;
        let qh = self.queue.handle();
        let wl = self.state.compositor.create_surface(&qh, ());
        let role = role(&self.globals, &qh, &wl, id)?;
        wl.commit();
        let info = vk::WaylandSurfaceCreateInfoKHR::default()
            .display(self.connection.backend().display_ptr().cast())
            .surface(wl.id().as_ptr().cast());
        // SAFETY: display and surface are libwayland's and live as long as
        // the connection, which outlives the Vulkan surface (`Drop`).
        let vk_surface = unsafe {
            self.renderer
                .gpu
                .instance
                .wayland
                .create_wayland_surface(&info, None)
        }
        .context("a Vulkan surface for the window")?;
        self.windows
            .push(Window::new(id, role, vk_surface, bar, translucent));
        self.state.surfaces.push(Surface {
            id,
            wl,
            asked: (0, 0),
            activated: false,
            maximized: false,
            fullscreen: false,
            events: Events::default(),
            next: Next::default(),
            on_its_way: Cell::new(None),
            frames: Cell::new(0),
            shown: Cell::new(0),
        });
        Ok(id)
    }

    /// The window `id`; settings for a window that is not (any more) do
    /// nothing.
    pub(super) fn window(&mut self, id: WindowId) -> Option<&mut Window> {
        self.windows.iter_mut().find(|w| w.id == id)
    }

    /// A panel's new size: the compositor configures it, then it is drawn
    /// in it.
    pub fn set_panel_size(&mut self, id: WindowId, size: (u32, u32)) {
        let Some(wl) = self.state.surface(id).map(|s| s.wl.clone()) else {
            return;
        };
        if let Some(window) = self.window(id) {
            window.set_panel_size(size);
            wl.commit();
        }
    }

    /// Glass below part of the panel or popup `id` (or none), drawn by the
    /// compositor (ADR 0028 §6): what the scene leaves transparent there
    /// shows it. Applies with its next draw.
    pub fn set_glass(&mut self, id: WindowId, glass: Option<Glass>) {
        let qh = self.queue.handle();
        let Some(wl) = self.state.surface(id).map(|s| s.wl.clone()) else {
            return;
        };
        if let Some(glasses) = &mut self.state.glasses {
            glasses.set_glass(id, &wl, glass, &qh);
        }
    }

    /// Where the window `id` takes the pointer: these rectangles (a popup's
    /// tiles, a menu's shape), or all of it (`None`). The rest is as if not
    /// there – a click falls through to what lies below, or closes the
    /// popup. Applies with the next draw.
    pub fn set_input(&mut self, id: WindowId, rects: Option<&[beize_draw::Rect]>) {
        let qh = self.queue.handle();
        let Some(surface) = self.state.surface(id) else {
            return;
        };
        let Some(rects) = rects else {
            surface.wl.set_input_region(None);
            return;
        };
        let region = self.state.compositor.create_region(&qh, ());
        for r in rects {
            region.add(
                r.x.round() as i32,
                r.y.round() as i32,
                r.w.round() as i32,
                r.h.round() as i32,
            );
        }
        surface.wl.set_input_region(Some(&region));
        region.destroy();
    }

    /// The window's title, in its title bar and the shell.
    /// The pointer's image over `id`'s content (over the title bar it is
    /// the arrow): the text cursor over text, as AppKit's views name theirs.
    pub fn set_cursor(&mut self, id: WindowId, shape: CursorShape) {
        if let Some(window) = self.windows.iter_mut().find(|w| w.id == id) {
            window.content_cursor = shape;
        }
    }

    pub fn set_title(&mut self, id: WindowId, title: &str) {
        if let Some(window) = self.window(id) {
            window.set_title(title);
        }
    }

    /// The title bar light or dark, to go with the content.
    pub fn set_appearance(&mut self, id: WindowId, appearance: Appearance) {
        if let Some(window) = self.window(id) {
            window.set_appearance(appearance);
        }
    }

    /// The content's size where the compositor leaves the choice to the
    /// app: when the window opens, and when it comes back from being
    /// maximized without a size from before (AppKit's content size).
    pub fn prefer_size(&mut self, id: WindowId, size: (u32, u32)) {
        if let Some(window) = self.window(id) {
            window.preferred = size;
        }
    }

    /// The size the window's next frame is drawn in: the content's, below
    /// the title bar.
    pub fn size(&self, id: WindowId) -> (u32, u32) {
        match (
            self.windows.iter().find(|w| w.id == id),
            self.state.surface(id),
        ) {
            (Some(window), Some(surface)) => window.size(surface),
            _ => (1, 1),
        }
    }

    /// Whether `id` takes a frame now: its last one is shown. The app's
    /// clock, as GTK's frame clock and macOS' display link are: an app
    /// draws what changed once its window is ready – never before, as a
    /// frame drawn ahead waits in line behind the one on its way, and
    /// everything after it later still. `dispatch` returns when a window
    /// becomes ready.
    pub fn ready(&self, id: WindowId) -> bool {
        self.state.surface(id).is_some_and(Surface::ready)
    }

    /// How many frames `id` drew, and how many of them the compositor
    /// showed: the difference is on its way, at most one (`ready`).
    pub fn frames(&self, id: WindowId) -> (u32, u32) {
        self.state
            .surface(id)
            .map_or((0, 0), |s| (s.frames.get(), s.shown.get()))
    }

    /// Draws `scene` and shows it with the compositor's next frame; only
    /// when the window is `ready`. The scene covers the whole content
    /// (`size`): what it leaves is undefined.
    pub fn draw(&mut self, id: WindowId, scene: &Scene) -> Result<()> {
        let App {
            windows,
            renderer,
            text,
            queue,
            state,
            ..
        } = self;
        let window = windows
            .iter_mut()
            .find(|w| w.id == id)
            .context("no such window")?;
        let surface = state.surface(id).context("no such window")?;
        if !surface.ready() {
            return Err(error("drawn while the window's last frame is on its way"));
        }
        window.draw(
            scene,
            renderer,
            text,
            surface,
            &state.compositor,
            &queue.handle(),
        )
    }
}
