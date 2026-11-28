//! The shell's dialogs and menus: they appear live (`Pose`), shake at a
//! wrong password and close as a picture with their glass's ghost.

use super::*;

impl Pose {
    /// As it is.
    pub fn still() -> Pose {
        Pose {
            scale: 1.0,
            about: Point::default(),
            opacity: 1.0,
            shift: 0,
        }
    }

    pub fn is_still(&self) -> bool {
        self.scale == 1.0 && self.opacity == 1.0 && self.shift == 0
    }

    /// Where `rect` shows: moved by the shift, scaled about the anchor moved
    /// with it.
    pub fn rect(&self, rect: Rectangle<i32, Physical>) -> Rectangle<i32, Physical> {
        let mut moved = rect;
        moved.loc.x += self.shift;
        scaled(moved, self.origin().to_f64(), self.scale)
    }

    /// The anchor, moved by the shift, on a whole pixel: what a surface's
    /// elements scale about (`Posed`).
    pub fn origin(&self) -> Point<i32, Physical> {
        Point::from((
            (self.about.x + f64::from(self.shift)).round() as i32,
            self.about.y.round() as i32,
        ))
    }
}

/// The centre of `place`, on the screen.
pub fn centre(place: Rectangle<i32, Logical>) -> Point<f64, Physical> {
    let place = place.to_physical(1);
    Point::from((
        f64::from(place.loc.x) + f64::from(place.size.w) / 2.0,
        f64::from(place.loc.y) + f64::from(place.size.h) / 2.0,
    ))
}

pub fn mark_grows(surface: &WlSurface) {
    with_states(surface, |states| {
        states.data_map.insert_if_missing_threadsafe(|| GrowsMark);
    });
}

pub fn grows(surface: &WlSurface) -> bool {
    with_states(surface, |states| {
        states.data_map.get::<GrowsMark>().is_some()
    })
}

pub fn mark_dialog(surface: &WlSurface) {
    with_states(surface, |states| {
        states.data_map.insert_if_missing_threadsafe(|| DialogMark);
    });
}

pub fn is_dialog(surface: &WlSurface) -> bool {
    with_states(surface, |states| {
        states.data_map.get::<DialogMark>().is_some()
    })
}

impl Animations {
    /// A dialog of the shell's shows for the first time: it appears from
    /// its next frame.
    pub fn appear(&mut self, surface: &WlSurface) {
        if self.dialogs.iter().any(|d| d.surface == *surface) {
            return;
        }
        info!("animation: dialog appears");
        self.dialogs.push(Dialog {
            surface: surface.clone(),
            what: "dialog",
            small: SMALL,
            about: None,
            open: DIALOG_OPEN,
            shown: Motion::new(0.0, UNSEEN),
            started: false,
            appearing: true,
            shake: Motion::new(0.0, SHAKE_UNSEEN),
            shaking: false,
            kick: false,
        });
    }

    /// A popup of the shell's that grows out of its button (the control
    /// center) shows for the first time: from `small` of its size about
    /// `about`, the button's centre, from its next frame.
    pub fn grow(&mut self, surface: &WlSurface, about: Point<f64, Physical>, small: f64) {
        if self.dialogs.iter().any(|d| d.surface == *surface) {
            return;
        }
        info!("animation: control center appears");
        self.dialogs.push(Dialog {
            surface: surface.clone(),
            what: "control center",
            small,
            about: Some(about),
            open: GROW,
            shown: Motion::new(0.0, UNSEEN),
            started: false,
            appearing: true,
            shake: Motion::new(0.0, SHAKE_UNSEEN),
            shaking: false,
            kick: false,
        });
    }

    /// The shell asks a dialog to shake: from its next frame.
    pub fn shake(&mut self, surface: &WlSurface) {
        info!("animation: dialog shakes");
        if let Some(dialog) = self.dialogs.iter_mut().find(|d| d.surface == *surface) {
            dialog.kick = true;
            dialog.shaking = true;
            return;
        }
        self.dialogs.push(Dialog {
            surface: surface.clone(),
            what: "dialog",
            small: SMALL,
            about: None,
            open: DIALOG_OPEN,
            shown: Motion::new(1.0, UNSEEN),
            started: true,
            appearing: false,
            shake: Motion::new(0.0, SHAKE_UNSEEN),
            shaking: true,
            kick: true,
        });
    }

    /// How a frame shown `at` draws `surface`, a panel at `place`: as it
    /// is, or as its dialog moves.
    pub fn pose(
        &mut self,
        surface: &WlSurface,
        place: Rectangle<i32, Logical>,
        at: Duration,
    ) -> Pose {
        let Some(dialog) = self.dialogs.iter_mut().find(|d| d.surface == *surface) else {
            return Pose::still();
        };
        // Its size may swing past whole (the control center); its
        // opacity stops there.
        let shown = dialog.shown.at(at).max(0.0);
        let shift = dialog.shake.at(at).round() as i32;
        Pose {
            scale: dialog.small + (1.0 - dialog.small) * shown,
            about: dialog.about.unwrap_or_else(|| centre(place)),
            opacity: shown.min(1.0) as f32,
            shift,
        }
    }

    /// A dialog goes: how far it was shown, for its closing.
    pub fn stop_dialog(&mut self, surface: &WlSurface) -> Option<Motion> {
        let i = self.dialogs.iter().position(|d| d.surface == *surface)?;
        Some(self.dialogs.remove(i).shown)
    }

    /// A dialog closed at `place`: its content drawn into `picture`, its
    /// glass's `ghost`, its buffers `kept` until the picture is drawn.
    pub fn close_dialog(
        &mut self,
        picture: Picture,
        ghost: Option<Ghost>,
        from: Option<Motion>,
        place: Rectangle<i32, Logical>,
        kept: Vec<Held>,
    ) {
        info!("animation: dialog closes");
        self.dialogs_closing.push(DialogClosing {
            what: "dialog",
            small: SMALL,
            about: None,
            spring: DIALOG_CLOSE,
            shown: from.unwrap_or_else(|| Motion::new(1.0, UNSEEN)),
            started: false,
            picture,
            ghost,
            place,
            kept,
        });
    }

    /// The closed dialogs as a frame shown `at` draws them, front to back,
    /// each with its place: content, then its glass's shadows and glass.
    /// A menu closed at `place`: its content drawn into `picture`, its
    /// glass's `ghost`, its buffers `kept` until the picture is drawn. It
    /// fades where it was, its popup gone at once (ADR 0032 §5).
    pub fn close_menu(
        &mut self,
        picture: Picture,
        ghost: Option<Ghost>,
        place: Rectangle<i32, Logical>,
        kept: Vec<Held>,
    ) {
        info!("animation: menu closes");
        self.dialogs_closing.push(DialogClosing {
            what: "menu",
            small: 1.0,
            about: None,
            spring: MENU_CLOSE,
            shown: Motion::new(1.0, UNSEEN),
            started: false,
            picture,
            ghost,
            place,
            kept,
        });
    }

    /// The control center closed at `place`, as a menu does: it goes back
    /// into its button, to `small` of its size about `about`, from how far
    /// it had grown (`from`).
    #[allow(clippy::too_many_arguments)]
    pub fn close_grown(
        &mut self,
        picture: Picture,
        ghost: Option<Ghost>,
        from: Option<Motion>,
        place: Rectangle<i32, Logical>,
        kept: Vec<Held>,
        about: Point<f64, Physical>,
        small: f64,
    ) {
        info!("animation: control center closes");
        self.dialogs_closing.push(DialogClosing {
            what: "control center",
            small,
            about: Some(about),
            spring: SHRINK_BACK,
            shown: from.unwrap_or_else(|| Motion::new(1.0, UNSEEN)),
            started: false,
            picture,
            ghost,
            place,
            kept,
        });
    }

    pub fn dialogs_closing(
        &mut self,
        mut backdrop: Option<&mut Backdrop>,
        at: Duration,
    ) -> Vec<(Rectangle<i32, Logical>, Vec<scene::Element>)> {
        let mut drawn = Vec::new();
        for closing in self.dialogs_closing.iter_mut().rev() {
            let shown = closing.shown.at(at).clamp(0.0, 1.0);
            if !closing.shown.moving() {
                continue;
            }
            let scale = closing.small + (1.0 - closing.small) * shown;
            let about = closing.about.unwrap_or_else(|| centre(closing.place));
            let pose = Pose {
                scale,
                about,
                opacity: shown as f32,
                shift: 0,
            };
            let place = closing.picture.place();
            let picture = closing
                .picture
                .element_at(scaled(place, about, scale), shown as f32);
            let mut elements: Vec<scene::Element> =
                picture.map(scene::Element::Picture).into_iter().collect();
            if let (Some(backdrop), Some(ghost)) = (backdrop.as_deref_mut(), &mut closing.ghost)
                && let Some((glass, shadows)) = backdrop.ghost_glass(ghost, closing.place.loc, pose)
            {
                elements.extend(shadows.into_iter().map(scene::Element::Shadow));
                elements.push(scene::Element::Glass(glass));
            }
            drawn.push((closing.place, elements));
        }
        drawn
    }
}
