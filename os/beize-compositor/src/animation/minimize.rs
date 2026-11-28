//! Minimizing into the dock and coming back: the genie (`crate::genie`
//! bends the mesh).

use super::*;

impl Genie {
    /// Its picture bent as at `p`.
    pub(super) fn element(&mut self, p: f64) -> Option<MeshElement> {
        let texture = self.picture.target.as_ref()?.picture()?.clone();
        let mesh = genie::Genie::new(self.picture.place, self.icon)?.mesh(p);
        self.commit.increment();
        Some(MeshElement {
            id: self.id.clone(),
            commit: self.commit,
            bounds: genie::bounds(&mesh),
            texture,
            mesh,
        })
    }

    /// What it shows at `p`: its picture bent; going, its shadow fading.
    /// Coming back it has none: the shadow belongs to the rectangle it
    /// lands as, and comes with it (macOS shows none around the bent
    /// picture).
    pub(super) fn elements(&mut self, p: f64, shadow: Option<Shadow>) -> Vec<scene::Element> {
        let p = p.max(0.0);
        let mut elements: Vec<scene::Element> = self
            .element(p)
            .map(scene::Element::Mesh)
            .into_iter()
            .collect();
        let faded = genie::Genie::shadow(p);
        if self.toward >= 1.0 && faded > 0.0 {
            elements.extend(shadow.map(|s| scene::Element::Shadow(s.faded(faded))));
        }
        elements
    }

    /// Whether it has ended by its last value: going, in its symbol;
    /// coming out, at its place.
    pub(super) fn done(&self, p: f64) -> bool {
        let arrived = if self.toward >= 1.0 {
            p >= END
        } else {
            p <= 0.0
        };
        self.started && (!self.p.moving() || arrived)
    }

    pub(super) fn ended(&self) {
        let app = &self.app;
        match (self.toward >= 1.0, self.opening) {
            (true, _) => info!("animation: {app} minimized"),
            (false, true) => info!("animation: {app} opened"),
            (false, false) => info!("animation: {app} came back"),
        }
    }
}

/// How far a quarter pixel is of the genie from `place` into `icon`.
pub(super) fn genie_unseen(place: Rectangle<i32, Physical>, icon: Rectangle<i32, Physical>) -> f64 {
    genie::Genie::new(place, icon).map_or(GENIE_UNSEEN, |g| GENIE_UNSEEN / g.longest_way())
}

impl Animations {
    /// `window` of `app` was minimized: drawn into `picture` (without its
    /// shadow, which is `shadow`), it goes into its symbol at `icon` in
    /// front of the windows that lay `behind` it, its buffers `kept` until
    /// the picture is drawn. From where it was coming back, if it was.
    #[allow(clippy::too_many_arguments)]
    pub fn minimize(
        &mut self,
        window: &Window,
        app: String,
        icon: Rectangle<i32, Physical>,
        picture: Picture,
        shadow: Option<Shadow>,
        behind: Vec<Window>,
        kept: Vec<Held>,
    ) {
        self.stop_opening(window);
        self.stop_reshape(window);
        let unseen = genie_unseen(picture.place, icon);
        let p = match self.genies.iter().position(|g| g.window == *window) {
            Some(i) => self.genies.remove(i).p,
            None => Motion::new(0.0, unseen),
        };
        info!("animation: {app} minimizes");
        self.genies.push(Genie {
            window: window.clone(),
            app,
            p,
            toward: 1.0,
            started: false,
            icon,
            picture,
            live: false,
            shadow,
            behind,
            kept,
            placed: false,
            opening: false,
            id: Id::new(),
            commit: CommitCounter::default(),
        });
    }

    /// `window` of `app`, at `place` on the screen, comes back out of its
    /// symbol at `icon` – or, `opening`, a new window grows out of it –,
    /// drawn anew each frame. From where it was going, if it was.
    pub fn come_back(
        &mut self,
        window: &Window,
        app: String,
        place: Rectangle<i32, Physical>,
        icon: Rectangle<i32, Physical>,
        opening: bool,
    ) {
        self.stop_opening(window);
        if opening {
            info!("animation: {app} opens from its symbol");
        } else {
            info!("animation: {app} comes back");
        }
        if let Some(genie) = self.genies.iter_mut().find(|g| g.window == *window) {
            genie.toward = BACK;
            genie.started = false;
            genie.icon = icon;
            genie.live = true;
            genie.shadow = None;
            genie.behind.clear();
            genie.opening |= opening;
            return;
        }
        self.genies.push(Genie {
            window: window.clone(),
            app,
            p: Motion::new(END, genie_unseen(place, icon)),
            toward: BACK,
            started: false,
            icon,
            picture: Picture::new(),
            live: true,
            shadow: None,
            behind: Vec::new(),
            kept: Vec::new(),
            placed: false,
            opening,
            id: Id::new(),
            commit: CommitCounter::default(),
        });
    }

    /// Whether `window` grows in from its next frame, not yet under way.
    pub fn opens_next(&self, window: &Window) -> bool {
        self.opening
            .iter()
            .any(|o| o.window == *window && !o.started)
    }

    /// `window` shows as it is at once: no genie of it, going or coming.
    pub fn stop_genie(&mut self, window: &Window) {
        self.genies.retain(|g| {
            let stops = g.window == *window;
            if stops {
                info!("animation: {} stopped its genie", g.app);
            }
            !stops
        });
    }
}
