//! Pictures taken for animations (ADR 0032): a window closing, minimized
//! or reshaped, a dialog or menu closing, a popup growing out of its
//! button, and where a popup sits.

use super::*;

impl State {
    /// `window` closes – its toplevel destroyed, or a commit without a
    /// buffer –: drawn into a picture that fades where it was, while its
    /// buffers are still there (`animation`). Without a screen, nothing.
    pub fn close_picture(&mut self, window: &Window) {
        let from = self.animations.stop(window);
        let Some(shot) = self.window_picture(window, false) else {
            return;
        };
        self.animations
            .close(shot.app, shot.picture, from, shot.behind, shot.kept);
        self.changed();
    }

    /// `window` is minimized, still placed: drawn into a picture without
    /// its shadow, it goes into its app's symbol in the dock (`animation`,
    /// `genie`). Whether it does: without a screen or a symbol it goes at
    /// once.
    pub fn minimize_picture(&mut self, window: &Window) -> bool {
        let app = crate::windows::app_id(window);
        let Some(icon) = self.dock_icon(&app) else {
            return false;
        };
        let Some(shot) = self.window_picture(window, true) else {
            return false;
        };
        if crate::genie::Genie::new(shot.picture.place(), icon).is_none() {
            return false;
        }
        self.animations.minimize(
            window,
            shot.app,
            icon,
            shot.picture,
            shot.shadow,
            shot.behind,
            shot.kept,
        );
        self.changed();
        true
    }

    /// `window` was asked for a new size to snap, maximize or come back
    /// from either (`asked`): drawn as it shows now, with its shadow, it
    /// fades into the new once that commit moves its frame (`animation`).
    /// Without a screen, and fullscreen, it changes at once.
    pub fn reshape_picture(&mut self, window: &Window, asked: Serial) {
        let Some(geometry) = self.space.element_geometry(window) else {
            return;
        };
        let frame = crate::decorations::frame(window, geometry).to_physical(1);
        let Some(shot) = self.window_picture(window, false) else {
            self.animations.stop_reshape(window);
            return;
        };
        self.animations
            .reshape(window, shot.app, shot.picture, frame, shot.kept, asked);
    }

    /// `window` as it shows now, drawn into a picture while its buffers are
    /// there – its shadow in it, or `apart` –, its buffers kept until the
    /// picture is drawn (`syncobj::keep`), and the windows that lie behind
    /// it. None without a screen, for fullscreen, which comes and goes at
    /// once (ADR 0032 §5), or once its client is gone – a disconnect
    /// destroys the surface before its toplevel: nothing is left to draw,
    /// as a crashed app's window goes on macOS.
    pub(super) fn window_picture(&mut self, window: &Window, apart: bool) -> Option<WindowShot> {
        let geometry = self.space.element_geometry(window)?;
        if crate::windows::fullscreen(window) {
            return None;
        }
        let (Some(toplevel), Some(gpu)) = (window.toplevel(), self.gpu.as_mut()) else {
            return None;
        };
        let surface = toplevel.wl_surface();
        if !gpu.shows()
            || !surface.is_alive()
            || !with_renderer_surface_state(surface, |s| s.buffer().is_some()).unwrap_or(false)
        {
            return None;
        }
        let app = crate::windows::app_id(window);
        let renderer = gpu.renderer();
        let bar = self
            .decorations
            .elements(renderer, &[(window.clone(), geometry)])
            .pop()
            .map(|(_, bar)| bar);
        let (mut own, mut shadow) = scene::own(renderer, window, geometry, bar);
        if !apart {
            own.extend(shadow.take().map(Element::Shadow));
        }
        let mut picture = Picture::new();
        if let Err(e) = picture.draw(renderer, own, scene::anchor(window, geometry)) {
            warn!("animation: {app} without its picture: {e}");
            return None;
        }
        let mut kept = Vec::new();
        with_surface_tree_downward(
            toplevel.wl_surface(),
            (),
            |_, _, &()| TraversalAction::DoChildren(()),
            |_, states, &()| kept.extend(syncobj::keep(states)),
            |_, _, &()| true,
        );
        let behind = self
            .space
            .elements()
            .take_while(|w| *w != window)
            .cloned()
            .collect();
        Some(WindowShot {
            app,
            picture,
            shadow,
            kept,
            behind,
        })
    }

    /// A dialog of the shell's goes (its panel destroyed, `layers`), at
    /// `place`: its content drawn into a picture while its buffer is there,
    /// its glass's shapes kept, both fading out (`animation`).
    pub fn close_dialog_picture(&mut self, surface: &WlSurface, place: Rectangle<i32, Logical>) {
        let from = self.animations.stop_dialog(surface);
        if !surface.is_alive() || !crate::animation::is_dialog(surface) {
            return;
        }
        let Some(gpu) = self.gpu.as_mut() else {
            return;
        };
        if !gpu.shows()
            || !with_renderer_surface_state(surface, |s| s.buffer().is_some()).unwrap_or(false)
        {
            return;
        }
        let renderer = gpu.renderer();
        let content = scene::surface(renderer, surface, place.loc);
        let mut picture = Picture::new();
        let anchor = crate::animation::Pose {
            about: crate::animation::centre(place),
            ..crate::animation::Pose::still()
        }
        .origin()
        .to_f64();
        if let Err(e) = picture.draw(renderer, content, anchor) {
            warn!("animation: a dialog closes without its picture: {e}");
            return;
        }
        let ghost = self
            .backdrop
            .as_ref()
            .and_then(|b| b.ghost(surface, place.loc));
        let mut kept = Vec::new();
        with_surface_tree_downward(
            surface,
            (),
            |_, _, &()| TraversalAction::DoChildren(()),
            |_, states, &()| kept.extend(syncobj::keep(states)),
            |_, _, &()| true,
        );
        self.animations
            .close_dialog(picture, ghost, from, place, kept);
        self.changed();
    }

    /// A menu goes (its popup destroyed, `shell`), an app's or the shell's:
    /// its content drawn into a picture where it lay while its buffer is
    /// there, its glass's shapes kept, both fading out (`animation`). Its
    /// popup and grab are gone at once (ADR 0032 §5).
    pub fn close_menu_picture(&mut self, popup: &smithay::wayland::shell::xdg::PopupSurface) {
        let surface = popup.wl_surface();
        // The control center goes back into its button, from how far it
        // had grown.
        let grown = crate::animation::grows(surface);
        let from = if grown {
            self.animations.stop_dialog(surface)
        } else {
            None
        };
        let back = if grown { self.grown_from(popup) } else { None };
        if !surface.is_alive() {
            return;
        }
        let Some(place) = self.popup_place(popup) else {
            return;
        };
        let Some(gpu) = self.gpu.as_mut() else {
            return;
        };
        if !gpu.shows()
            || !with_renderer_surface_state(surface, |s| s.buffer().is_some()).unwrap_or(false)
        {
            return;
        }
        let renderer = gpu.renderer();
        let content = scene::surface(renderer, surface, place.loc);
        let mut picture = Picture::new();
        if let Err(e) = picture.draw(renderer, content, crate::animation::centre(place)) {
            warn!("animation: a menu closes without its picture: {e}");
            return;
        }
        let ghost = self
            .backdrop
            .as_ref()
            .and_then(|b| b.ghost(surface, place.loc));
        let mut kept = Vec::new();
        with_surface_tree_downward(
            surface,
            (),
            |_, _, &()| TraversalAction::DoChildren(()),
            |_, states, &()| kept.extend(syncobj::keep(states)),
            |_, _, &()| true,
        );
        match back {
            Some((about, small)) => self
                .animations
                .close_grown(picture, ghost, from, place, kept, about, small),
            None => self.animations.close_menu(picture, ghost, place, kept),
        }
        self.changed();
    }

    /// After a commit: a popup of the shell's that grows out of its button
    /// (the control center, `beize_shell_v1.grows`) does with its first
    /// content (ADR 0032 §5).
    pub(super) fn grown_committed(&mut self, surface: &WlSurface) {
        if !crate::animation::grows(surface)
            || !with_renderer_surface_state(surface, |s| s.buffer().is_some()).unwrap_or(false)
        {
            return;
        }
        // Its first content, whether a screen shows it or not: a later one
        // never grows an open control center.
        let first = smithay::wayland::compositor::with_states(surface, |states| {
            states.data_map.insert_if_missing_threadsafe(|| Grown)
        });
        if !first || !self.shows() {
            return;
        }
        let Some(smithay::desktop::PopupKind::Xdg(popup)) = self.popups.find_popup(surface) else {
            return;
        };
        if let Some((about, small)) = self.grown_from(&popup) {
            self.animations.grow(surface, about, small);
            self.changed();
        }
    }

    /// Where the control center appears from and how small: under its
    /// button – the rectangle its popup is anchored to –, at the popup's top
    /// edge, from 97 %, as a menu with a small pop. Until 0.5.130 it grew
    /// out of the button's centre from the button's width (`GROW`).
    pub(super) fn grown_from(
        &self,
        popup: &smithay::wayland::shell::xdg::PopupSurface,
    ) -> Option<(Point<f64, Physical>, f64)> {
        let place = self.popup_place(popup)?;
        let (parent, anchor) = self.popup_parent(popup)?;
        let button = Rectangle::new(parent + anchor.loc, anchor.size).to_physical(1);
        let about = Point::<f64, Physical>::from((
            f64::from(button.loc.x) + f64::from(button.size.w) / 2.0,
            f64::from(button.loc.y) + f64::from(button.size.h) / 2.0,
        ));
        let place = place.to_physical(1);
        let left = f64::from(place.loc.x);
        let right = left + f64::from(place.size.w);
        let about = Point::from((about.x.clamp(left, right), f64::from(place.loc.y)));
        Some((about, 0.97))
    }

    /// Where the parent of `popup` lies on the screen – the surface its
    /// positioner places it against – and its anchor rectangle there.
    pub(super) fn popup_parent(
        &self,
        popup: &smithay::wayland::shell::xdg::PopupSurface,
    ) -> Option<(Point<i32, Logical>, Rectangle<i32, Logical>)> {
        let kind = smithay::desktop::PopupKind::Xdg(popup.clone());
        let anchor = smithay::wayland::compositor::with_states(popup.wl_surface(), |states| {
            let data = states
                .data_map
                .get::<smithay::wayland::shell::xdg::XdgPopupSurfaceData>()?;
            Some(data.lock().ok()?.current.positioner.anchor_rect)
        })?;
        Some((
            self.popup_root(&kind)? + smithay::desktop::get_popup_toplevel_coords(&kind),
            anchor,
        ))
    }

    /// Where the window or panel `kind` belongs to lies on the screen.
    pub(super) fn popup_root(
        &self,
        kind: &smithay::desktop::PopupKind,
    ) -> Option<Point<i32, Logical>> {
        let root = find_popup_root_surface(kind).ok()?;
        if let Some(window) = self.window_for(&root) {
            return Some(self.space.element_geometry(&window)?.loc);
        }
        let output = self.output.as_ref()?;
        let map = layer_map_for_output(output);
        let panel = map.layer_for_surface(&root, smithay::desktop::WindowSurfaceType::TOPLEVEL)?;
        Some(map.layer_geometry(panel)?.loc)
    }

    /// Where `popup`'s surface lies on the screen and how far its tree
    /// reaches, as the scene places it: from its window's place or its
    /// panel's, by its parents' offsets and its own.
    pub(super) fn popup_place(
        &self,
        popup: &smithay::wayland::shell::xdg::PopupSurface,
    ) -> Option<Rectangle<i32, Logical>> {
        let kind = smithay::desktop::PopupKind::Xdg(popup.clone());
        let own = smithay::wayland::compositor::with_states(popup.wl_surface(), |states| {
            let data = states
                .data_map
                .get::<smithay::wayland::shell::xdg::XdgPopupSurfaceData>()?;
            Some(data.lock().ok()?.current.geometry.loc)
        })?;
        let offset = smithay::desktop::get_popup_toplevel_coords(&kind) + own;
        let at = self.popup_root(&kind)? + offset - kind.geometry().loc;
        let reach = smithay::desktop::utils::bbox_from_surface_tree(popup.wl_surface(), at);
        (!reach.is_empty()).then(|| Rectangle::new(at, reach.size))
    }
}
