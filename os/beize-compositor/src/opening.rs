//! The desktop appears in one piece (ADR 0028 §6), as macOS' does: the
//! mode is set with a black frame at once – the monitor locks on to it
//! while the shell starts –, the desktop shows once the shell's panels are
//! drawn (`beize_shell_v1.ready`) and fades in from that black. No frame
//! shows half a desktop. Without the shell only if it ends before it is
//! ready, or after `SHELL_WAIT` if it hangs.
//!
//! The fade is a spring without bounce, SwiftUI's `.smooth` (duration
//! 0.5 s, bounce 0; `beize_draw::spring`): the black's opacity
//! `(1 + ωt)·e^(−ωt)` with ω = 2π / duration.
//!
//! A shell that fails leaves its panels' last frame on the screen until
//! the new one is ready (`State::keep_panels`).

use std::time::Duration;

use beize_draw::{Motion, Spring};

/// How long the desktop waits for the shell: at the start, and for a new
/// shell after one failed. A shell that ends before it is ready is not
/// waited for (`State::shell_ended`): this is for one that hangs. A working
/// shell is ready in 0.17 s, on lavapipe with Mesa's shader cache still
/// empty (the first sign-in on a PC without a Vulkan driver) in 0.9–1.2 s,
/// once 2.6 s with the smoke test's other machines beside it (24.10.):
/// well over that (owner, 24.10.).
pub const SHELL_WAIT: Duration = Duration::from_secs(10);

/// Below this the black is no longer seen: half a step of 8 bits.
const UNSEEN: f64 = 0.5 / 255.0;

/// Where the desktop's start is.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Opening {
    /// Black: the screen is not taken yet, or the shell not ready. Whether
    /// it said it is.
    Waiting {
        shell: bool,
    },
    /// The desktop fades in: the black's opacity.
    Fading(Motion),
    Open,
}

/// How the desktop shows in a frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Veil {
    /// All black: not yet.
    Black,
    /// Under black of this opacity.
    Fading(f32),
    /// As it is.
    None,
}

impl Opening {
    /// The fade from all black, from `now`.
    pub fn fading(now: Duration) -> Opening {
        let mut black = Motion::new(1.0, UNSEEN);
        black.to(0.0, Spring::SMOOTH, now);
        Opening::Fading(black)
    }

    /// Ends a fade at once: no screen shows it.
    pub fn finish(&mut self) {
        if matches!(self, Opening::Fading(_)) {
            *self = Opening::Open;
        }
    }

    /// What a frame shown at `now` shows; the fade ends once its black is
    /// unseen.
    pub fn veil(&mut self, now: Duration) -> Veil {
        match self {
            Opening::Waiting { .. } => Veil::Black,
            Opening::Fading(black) => {
                let opacity = black.at(now);
                if black.moving() {
                    Veil::Fading(opacity as f32)
                } else {
                    *self = Opening::Open;
                    Veil::None
                }
            }
            Opening::Open => Veil::None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn black_until_fading_then_open() {
        let mut opening = Opening::Waiting { shell: false };
        assert_eq!(opening.veil(ms(5000)), Veil::Black);
        opening = Opening::fading(ms(1000));
        assert_eq!(opening.veil(ms(1000)), Veil::Fading(1.0));
        let Veil::Fading(half) = opening.veil(ms(1100)) else {
            panic!("not fading");
        };
        assert!(half > 0.0 && half < 1.0);
        // Unseen some 0.7 s in: open from then on.
        assert_eq!(opening.veil(ms(1700)), Veil::None);
        assert_eq!(opening, Opening::Open);
        assert_eq!(opening.veil(ms(1000)), Veil::None);
    }

    #[test]
    fn a_fade_no_screen_shows_ends_at_once() {
        let mut opening = Opening::fading(ms(1000));
        opening.finish();
        assert_eq!(opening, Opening::Open);
        // Waiting is not a fade: it stays.
        let mut opening = Opening::Waiting { shell: true };
        opening.finish();
        assert_eq!(opening, Opening::Waiting { shell: true });
    }
}
