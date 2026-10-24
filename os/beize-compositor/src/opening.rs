//! The desktop appears in one piece (ADR 0028 §6), as macOS' does: the
//! mode is set with a black frame at once – the monitor locks on to it
//! while the shell starts –, the desktop shows once the shell's panels are
//! drawn (`beize_shell_v1.ready`) and fades in from that black. No frame
//! shows half a desktop. Without the shell only if it ends before it is
//! ready, or after `SHELL_WAIT` if it hangs.
//!
//! The fade is a spring without bounce, as SwiftUI's `.smooth` (duration
//! 0.5 s, bounce 0): a critically damped one, the black's opacity
//! `(1 + ωt)·e^(−ωt)` with ω = 2π / duration.
//!
//! A shell that fails leaves its panels' last frame on the screen until
//! the new one is ready (`State::keep_panels`).

use std::f64::consts::TAU;
use std::time::Duration;

/// How long the desktop waits for the shell: at the start, and for a new
/// shell after one failed. A shell that ends before it is ready is not
/// waited for (`State::shell_ended`): this is for one that hangs. A working
/// shell is ready in 0.17 s, on lavapipe with Mesa's shader cache still
/// empty (the first sign-in on a PC without a Vulkan driver) in 0.9–1.2 s,
/// once 2.6 s with the smoke test's other machines beside it (24.10.):
/// well over that (owner, 24.10.).
pub const SHELL_WAIT: Duration = Duration::from_secs(10);

/// The fade's spring: SwiftUI's `.smooth`.
const DURATION: f64 = 0.5;

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
    /// The desktop fades in, since then.
    Fading {
        since: Duration,
    },
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
    /// What a frame at `now` shows; the fade ends once its black is unseen.
    pub fn veil(&mut self, now: Duration) -> Veil {
        match *self {
            Opening::Waiting { .. } => Veil::Black,
            Opening::Fading { since } => {
                let black = black(now.saturating_sub(since));
                if black < UNSEEN {
                    *self = Opening::Open;
                    Veil::None
                } else {
                    Veil::Fading(black as f32)
                }
            }
            Opening::Open => Veil::None,
        }
    }
}

/// The black's opacity `t` into the fade.
fn black(t: Duration) -> f64 {
    let x = TAU / DURATION * t.as_secs_f64();
    (1.0 + x) * (-x).exp()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn the_fade_is_a_spring_without_bounce() {
        assert!((black(Duration::ZERO) - 1.0).abs() < 1e-12);
        // Falling all the way, never below none (no bounce).
        let mut last = 1.0;
        for n in 1..1000 {
            let b = black(ms(n));
            assert!(b < last && b > 0.0, "at {n} ms: {b}");
            last = b;
        }
        // At its duration a little over 1 % is left, as SwiftUI's spring.
        let at_end = black(Duration::from_secs_f64(DURATION));
        assert!((at_end - 0.0136).abs() < 0.0005, "{at_end}");
    }

    #[test]
    fn black_until_fading_then_open() {
        let mut opening = Opening::Waiting { shell: false };
        assert_eq!(opening.veil(ms(5000)), Veil::Black);
        opening = Opening::Fading { since: ms(1000) };
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
}
