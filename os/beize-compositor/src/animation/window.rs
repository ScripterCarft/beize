//! Windows opening and closing in place: from 94 % with a fade, to 94 %
//! with a fade (ADR 0032 §5).

use super::*;

impl Animations {
    pub(super) fn stop_opening(&mut self, window: &Window) {
        if let Some(i) = self.opening.iter().position(|o| o.window == *window) {
            let opening = self.opening.remove(i);
            info!("animation: {} stopped opening", opening.app);
        }
    }

    /// `window` of `app` was mapped: it grows in from its next frame.
    pub fn open(&mut self, window: &Window, app: String) {
        if self.opening.iter().any(|o| o.window == *window) {
            return;
        }
        info!("animation: {app} opens");
        self.opening.push(Opening {
            window: window.clone(),
            app,
            shown: Motion::new(0.0, UNSEEN),
            started: false,
            picture: Picture::new(),
        });
    }

    /// `window` stops opening or coming out of its symbol (closed,
    /// unmapped): how far it was shown, for what follows. Going into its
    /// symbol it goes on: its picture is its own.
    pub fn stop(&mut self, window: &Window) -> Option<Motion> {
        self.stop_reshape(window);
        self.genies.retain(|g| {
            let stops = g.live && g.window == *window;
            if stops {
                info!("animation: {} stopped coming back", g.app);
            }
            !stops
        });
        let i = self.opening.iter().position(|o| o.window == *window)?;
        let opening = self.opening.remove(i);
        info!("animation: {} stopped opening", opening.app);
        Some(opening.shown)
    }

    /// A window of `app` closed, drawn into `picture` with its buffers
    /// `kept`; it fades from where its opening was (`from`), or from whole.
    pub fn close(
        &mut self,
        app: String,
        picture: Picture,
        from: Option<Motion>,
        behind: Vec<Window>,
        kept: Vec<Held>,
    ) {
        info!("animation: {app} closes");
        self.closing.push(Closing {
            app,
            shown: from.unwrap_or_else(|| Motion::new(1.0, UNSEEN)),
            started: false,
            picture,
            behind,
            kept,
            placed: false,
        });
    }
}
