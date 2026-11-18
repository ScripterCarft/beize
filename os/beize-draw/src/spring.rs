//! Springs, the one way Beize animates (ADR 0032 §1): SwiftUI's
//! `Spring(duration:bounce:)` with mass 1, stiffness (2π/duration)² and
//! damping 4π(1 − bounce)/duration (WWDC23 "Animate with springs"; Apple's
//! example, duration 0.5 and bounce 0.3, gives 157.9 and 17.6).
//!
//! A [`Motion`] is one value moving on a spring: its value and velocity at a
//! time. It is solved, not stepped, so a frame that comes late lands where
//! the spring is at that time, however long the step (ADR 0032 §3). A new
//! target keeps value and velocity: an animation is taken over where it is,
//! never restarted or waited for. Times are the caller's clock – the
//! compositor's is the vblank a frame is shown at.

use std::f64::consts::{E, TAU};
use std::time::Duration;

/// A spring: how long it takes, roughly, and how much it overshoots (0 none,
/// as SwiftUI's `.smooth`; up to below 1).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Spring {
    pub duration: f64,
    pub bounce: f64,
}

impl Spring {
    /// SwiftUI's `.smooth`: no bounce, half a second.
    pub const SMOOTH: Spring = Spring::new(0.5, 0.0);

    pub const fn new(duration: f64, bounce: f64) -> Spring {
        Spring { duration, bounce }
    }

    /// The undamped angular frequency, 2π/duration.
    fn omega(self) -> f64 {
        TAU / self.duration.max(1e-3)
    }

    /// The damping ratio, 1 − bounce: 1 is critically damped.
    fn zeta(self) -> f64 {
        (1.0 - self.bounce).clamp(0.0, 1.0)
    }

    pub fn stiffness(self) -> f64 {
        self.omega().powi(2)
    }

    pub fn damping(self) -> f64 {
        2.0 * self.zeta() * self.omega()
    }

    /// The velocity of a push from rest whose first swing reaches `reach`
    /// (the shake's 10 px, ADR 0032 §5): the peak of `v·e^(−ζωt)·sin(ω_d t)/ω_d`,
    /// or with critical damping of `v·t·e^(−ωt)`.
    pub fn kick_for(self, reach: f64) -> f64 {
        let (w, z) = (self.omega(), self.zeta());
        let peak = if z < 1.0 {
            let wd = w * (1.0 - z * z).sqrt();
            let t = (wd / (z * w)).atan() / wd;
            (-z * w * t).exp() * (wd * t).sin() / wd
        } else {
            1.0 / (E * w)
        };
        reach / peak
    }

    /// Offset `y` from the target and velocity `v`, `t` seconds on.
    fn solve(self, y: f64, v: f64, t: f64) -> (f64, f64) {
        let (w, z) = (self.omega(), self.zeta());
        if z < 1.0 {
            let wd = w * (1.0 - z * z).sqrt();
            let (s, c) = (wd * t).sin_cos();
            let e = (-z * w * t).exp();
            let b = (v + z * w * y) / wd;
            (
                e * (y * c + b * s),
                e * (v * c - (w * y + z * v) * w / wd * s),
            )
        } else {
            let e = (-w * t).exp();
            let c = v + w * y;
            ((y + c * t) * e, (v - w * c * t) * e)
        }
    }

    /// The most the offset can still be, from offset `y` and velocity `v`
    /// on: the envelope of the oscillation, or with critical damping
    /// `|y| + |v + ωy|/(eω)` (the peak of `c·t·e^(−ωt)`).
    fn reach(self, y: f64, v: f64) -> f64 {
        let (w, z) = (self.omega(), self.zeta());
        if z < 1.0 {
            let wd = w * (1.0 - z * z).sqrt();
            y.hypot((v + z * w * y) / wd)
        } else {
            y.abs() + (v + w * y).abs() / (E * w)
        }
    }
}

/// One value on a spring.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Motion {
    value: f64,
    velocity: f64,
    target: f64,
    spring: Spring,
    /// When `value` and `velocity` were.
    at: Duration,
    /// Below this the rest is not seen: the motion ends there, at its
    /// target (half a step of 8 bits for an opacity, a fraction of a pixel
    /// for a place).
    unseen: f64,
}

impl Motion {
    /// A value at rest.
    pub fn new(value: f64, unseen: f64) -> Motion {
        Motion {
            value,
            velocity: 0.0,
            target: value,
            spring: Spring::SMOOTH,
            at: Duration::ZERO,
            unseen,
        }
    }

    /// Moves to `target` on `spring` from `now`, from where it is then and
    /// as fast as it moves then.
    pub fn to(&mut self, target: f64, spring: Spring, now: Duration) {
        self.advance(now);
        self.target = target;
        self.spring = spring;
        self.settle();
    }

    /// Adds `velocity` (per second) at `now`: a push the spring brings back.
    pub fn kick(&mut self, velocity: f64, spring: Spring, now: Duration) {
        self.advance(now);
        self.velocity += velocity;
        self.spring = spring;
    }

    /// Is `value` at once, at rest.
    pub fn jump(&mut self, value: f64) {
        self.value = value;
        self.target = value;
        self.velocity = 0.0;
    }

    /// The value at `now`; a time before the last one gives the last value.
    pub fn at(&mut self, now: Duration) -> f64 {
        self.advance(now);
        self.value
    }

    pub fn target(&self) -> f64 {
        self.target
    }

    /// Whether it still moves: its next frame shows something new.
    pub fn moving(&self) -> bool {
        self.value != self.target || self.velocity != 0.0
    }

    fn advance(&mut self, now: Duration) {
        let Some(t) = now.checked_sub(self.at) else {
            return;
        };
        self.at = now;
        if !self.moving() || t.is_zero() {
            return;
        }
        let (y, v) = self
            .spring
            .solve(self.value - self.target, self.velocity, t.as_secs_f64());
        self.value = self.target + y;
        self.velocity = v;
        self.settle();
    }

    /// Ends the motion once nothing more of it would be seen.
    fn settle(&mut self) {
        if self.spring.reach(self.value - self.target, self.velocity) < self.unseen {
            self.jump(self.target);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(secs: f64) -> Duration {
        Duration::from_secs_f64(secs)
    }

    #[test]
    fn apples_example() {
        let spring = Spring::new(0.5, 0.3);
        assert!((spring.stiffness() - 157.9).abs() < 0.05);
        assert!((spring.damping() - 17.6).abs() < 0.05);
    }

    #[test]
    fn smooth_is_critically_damped_without_overshoot() {
        let mut m = Motion::new(0.0, 1e-9);
        m.to(1.0, Spring::SMOOTH, Duration::ZERO);
        let mut last = 0.0;
        for n in 1..2000 {
            let x = m.at(Duration::from_millis(n));
            assert!(x >= last && x <= 1.0, "at {n} ms: {x}");
            last = x;
        }
        // At its duration a little over 1 % is left, as SwiftUI's.
        let mut m = Motion::new(0.0, 1e-9);
        m.to(1.0, Spring::SMOOTH, Duration::ZERO);
        let left = 1.0 - m.at(s(0.5));
        assert!((left - 0.0136).abs() < 0.0005, "{left}");
    }

    #[test]
    fn bounce_overshoots() {
        let mut m = Motion::new(0.0, 1e-9);
        m.to(1.0, Spring::new(0.5, 0.3), Duration::ZERO);
        let peak = (1..1000)
            .map(|n| m.at(Duration::from_millis(n)))
            .fold(0.0, f64::max);
        // e^(−πζ/√(1−ζ²)) with ζ = 0.7: 4.6 %.
        assert!((peak - 1.046).abs() < 0.002, "{peak}");
    }

    #[test]
    fn a_late_frame_lands_where_the_spring_is() {
        let spring = Spring::new(0.4, 0.15);
        let (mut one, mut many) = (Motion::new(0.0, 1e-12), Motion::new(0.0, 1e-12));
        one.to(100.0, spring, Duration::ZERO);
        many.to(100.0, spring, Duration::ZERO);
        for n in 1..=250 {
            many.at(Duration::from_micros(n * 1000));
        }
        let (a, b) = (one.at(s(0.25)), many.at(s(0.25)));
        assert!((a - b).abs() < 1e-9, "{a} {b}");
    }

    #[test]
    fn a_new_target_keeps_value_and_velocity() {
        let spring = Spring::new(0.45, 0.0);
        let mut m = Motion::new(0.0, 1e-9);
        m.to(1.0, spring, Duration::ZERO);
        let before = m.at(s(0.1));
        let v = m.velocity;
        assert!(v > 0.0);
        m.to(0.0, Spring::new(0.25, 0.0), s(0.1));
        assert_eq!(m.value, before);
        assert_eq!(m.velocity, v);
        // It goes on a little the old way before it turns: no jump back.
        assert!(m.at(s(0.101)) > before);
    }

    #[test]
    fn it_ends_at_its_target_once_the_rest_is_unseen() {
        let mut m = Motion::new(0.0, 0.5 / 255.0);
        m.to(1.0, Spring::new(0.3, 0.5), Duration::ZERO);
        let mut end = None;
        for n in 1..3000 {
            let now = Duration::from_millis(n);
            let x = m.at(now);
            if !m.moving() {
                assert_eq!(x, 1.0);
                end = Some(n);
                break;
            }
            assert!((x - 1.0).abs() < 1.0);
        }
        // Ended, and not long after the spring's duration.
        let end = end.unwrap_or(u64::MAX);
        assert!((300..1200).contains(&end), "{end} ms");
    }

    #[test]
    fn what_is_left_when_it_ends_is_never_seen() {
        // Whenever it ends, the spring would never have gone further from
        // its target than `unseen`.
        let spring = Spring::new(0.3, 0.6);
        let mut m = Motion::new(0.0, 0.01);
        m.to(1.0, spring, Duration::ZERO);
        let mut free = Motion::new(0.0, 0.0);
        free.to(1.0, spring, Duration::ZERO);
        let mut n = 0;
        while m.moving() {
            n += 1;
            m.at(Duration::from_millis(n));
        }
        for later in n..n + 3000 {
            let x = free.at(Duration::from_millis(later));
            assert!((x - 1.0).abs() < 0.01, "{later} ms: {x}");
        }
    }

    #[test]
    fn a_kick_swings_back_to_rest() {
        let spring = Spring::new(0.15, 0.7);
        let mut m = Motion::new(0.0, 0.25);
        m.kick(-1000.0, spring, Duration::ZERO);
        let first = m.at(s(0.02));
        assert!(first < 0.0);
        let mut crossed = false;
        let mut n = 20;
        while m.moving() {
            n += 1;
            crossed |= m.at(Duration::from_millis(n)) > 0.0;
        }
        assert!(crossed && m.at(Duration::from_millis(n)) == 0.0);
        assert!(n < 1000, "{n} ms");
    }

    #[test]
    fn a_kick_for_a_reach_swings_that_far_first() {
        for spring in [Spring::new(0.15, 0.7), Spring::new(0.3, 0.0)] {
            let mut m = Motion::new(0.0, 1e-9);
            m.kick(spring.kick_for(10.0), spring, Duration::ZERO);
            let first = (1..400)
                .map(|n| m.at(Duration::from_micros(n * 250)))
                .fold(0.0, f64::max);
            assert!((first - 10.0).abs() < 0.01, "{spring:?}: {first}");
        }
    }

    #[test]
    fn time_does_not_go_back() {
        let mut m = Motion::new(0.0, 1e-9);
        m.to(1.0, Spring::SMOOTH, s(1.0));
        let x = m.at(s(1.2));
        assert_eq!(m.at(s(1.1)), x);
        // Before it began it is where it was.
        let mut m = Motion::new(0.0, 1e-9);
        m.to(1.0, Spring::SMOOTH, s(1.0));
        assert_eq!(m.at(s(0.5)), 0.0);
    }
}
