//! How frames meet their vblank, measured by the compositor itself (ADR
//! 0027 §6 "Bildtakt messen"). The compositor's task is to show a change at
//! the first vblank after it arrived. Each drawn frame keeps when its change
//! arrived, when drawing began and when it went to KMS; the vblank that
//! shows it says whether it made it.
//!
//! A frame shown later than that vblank is late, and the time it passed
//! says who held it up: still drawing (the compositor's CPU work) or
//! committed in time but not shown (the GPU or the display). How often
//! clients draw is theirs; a client that is late shows in its own frame
//! times, not here.
//!
//! Frames in a row (an animation, a client drawing each frame) make one
//! summary in the journal, when they end or every 30 s, at info level from
//! a second on:
//!
//! `pacing: 4800 frames in 20.00 s at 240 Hz, 0 late (…); change to screen
//! median 2.31 ms, …; busy 4 % of the 4.17 ms per frame`
//!
//! It costs a few numbers per frame, kept until the summary.
//!
//! The vblank times must be real: a display engine's (nouveau, i915,
//! amdgpu). virtio-gpu has no vblank, its "vblank" is when the host took
//! the frame, so in a VM the late frames and the change-to-screen times say
//! little; the frame and drawing times still hold.

use std::time::Duration;

use tracing::{debug, info};

/// A frame longer than this after the last one begins a new run: the
/// screen was idle in between.
const IDLE: Duration = Duration::from_millis(250);
/// A long run is summed up in pieces of this length.
const PIECE: Duration = Duration::from_secs(30);
/// Runs from this long on are summed up at info level, shorter ones (a key,
/// a click) at debug level.
const NOTABLE: Duration = Duration::from_secs(1);

/// When a frame's change arrived, its drawing began and it was committed;
/// on CLOCK_MONOTONIC, as KMS's vblank times.
#[derive(Clone, Copy, Debug)]
pub struct Times {
    pub requested: Duration,
    pub began: Duration,
    pub committed: Duration,
}

/// The frames of one run.
#[derive(Default)]
struct Run {
    /// The vblank of its first frame.
    first: Duration,
    frames: u32,
    /// Shown after the first vblank following their change: committed
    /// after that vblank (drawing), or before it (GPU, display).
    late_drawing: u32,
    late_display: u32,
    /// Change to screen, drawing (CPU) and GPU time, in milliseconds.
    latency: Vec<f64>,
    cpu: Vec<f64>,
    gpu: Vec<f64>,
}

pub struct Pacing {
    refresh: Duration,
    /// A recent vblank: with `refresh`, where every vblank lies.
    phase: Option<Duration>,
    /// The vblank of the last frame shown.
    last: Option<Duration>,
    run: Run,
}

impl Pacing {
    pub fn new(refresh: Duration) -> Pacing {
        Pacing {
            refresh,
            phase: None,
            last: None,
            run: Run::default(),
        }
    }

    /// A vblank passed, drawn frame or not.
    pub fn vblank(&mut self, time: Duration) {
        self.phase = Some(time);
    }

    /// A drawn frame is on the screen since `vblank`; `gpu_ms` if its GPU
    /// time (or a recent frame's) was read since the last call.
    pub fn shown(&mut self, times: Times, vblank: Duration, gpu_ms: Option<f64>) {
        self.vblank(vblank);
        if self
            .last
            .is_none_or(|last| vblank.saturating_sub(last) > IDLE)
        {
            self.report();
            self.run.first = vblank;
        }
        self.last = Some(vblank);
        let run = &mut self.run;
        run.frames += 1;
        let due = self.next_vblank(times.requested);
        // Half a refresh of tolerance: vblank times jitter by microseconds.
        if vblank > due + self.refresh / 2 {
            if times.committed > due {
                self.run.late_drawing += 1;
            } else {
                self.run.late_display += 1;
            }
        }
        let run = &mut self.run;
        run.latency.push(ms(vblank.saturating_sub(times.requested)));
        run.cpu
            .push(ms(times.committed.saturating_sub(times.began)));
        run.gpu.extend(gpu_ms);
        if vblank.saturating_sub(run.first) >= PIECE {
            self.report();
            self.run.first = vblank;
        }
    }

    /// Ends the run once no frame came for a while; false when there is no
    /// run (left) to watch.
    pub fn idle(&mut self, now: Duration) -> bool {
        match self.last {
            Some(last) if now.saturating_sub(last) > IDLE => {
                self.report();
                self.last = None;
                false
            }
            Some(_) => true,
            None => false,
        }
    }

    /// The session paused: the run ends here.
    pub fn pause(&mut self) {
        self.report();
        self.last = None;
        self.phase = None;
    }

    /// The first vblank after `t`.
    fn next_vblank(&self, t: Duration) -> Duration {
        let Some(phase) = self.phase else {
            return t + self.refresh;
        };
        let (p, r) = (
            phase.as_nanos() as i128,
            self.refresh.as_nanos().max(1) as i128,
        );
        let next = p + ((t.as_nanos() as i128 - p).div_euclid(r) + 1) * r;
        Duration::from_nanos(u64::try_from(next.max(0)).unwrap_or(u64::MAX))
    }

    /// Logs the run's summary and starts a new one.
    fn report(&mut self) {
        let run = std::mem::take(&mut self.run);
        let Some(line) = self.summary(&run) else {
            return;
        };
        let length = self
            .last
            .map_or(Duration::ZERO, |l| l.saturating_sub(run.first));
        if length >= NOTABLE {
            info!("{line}");
        } else {
            debug!("{line}");
        }
    }

    fn summary(&self, run: &Run) -> Option<String> {
        if run.frames < 2 {
            return None;
        }
        let length = self.last?.saturating_sub(run.first);
        let budget = ms(self.refresh);
        let (cpu, gpu, latency) = (sorted(&run.cpu), sorted(&run.gpu), sorted(&run.latency));
        let busy = (median(&cpu) + median(&gpu)) / budget * 100.0;
        Some(format!(
            "pacing: {} frames in {:.2} s at {:.0} Hz, {} late (drawing {}, GPU/display {}); \
             change to screen median {:.2} ms, max {:.2}; drawing (CPU) median {:.2} ms, \
             99 % {:.2}, max {:.2}; GPU median {:.2} ms, max {:.2}; busy {:.0} % of the {:.2} ms \
             per frame",
            run.frames,
            length.as_secs_f64(),
            1000.0 / budget,
            run.late_drawing + run.late_display,
            run.late_drawing,
            run.late_display,
            median(&latency),
            max(&latency),
            median(&cpu),
            percentile(&cpu, 0.99),
            max(&cpu),
            median(&gpu),
            max(&gpu),
            busy,
            budget,
        ))
    }
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

fn sorted(values: &[f64]) -> Vec<f64> {
    let mut v = values.to_vec();
    v.sort_by(f64::total_cmp);
    v
}

/// The value below which `p` of the sorted `values` lie; 0 without any.
fn percentile(values: &[f64], p: f64) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    let i = ((values.len() - 1) as f64 * p).round() as usize;
    values[i.min(values.len() - 1)]
}

fn median(values: &[f64]) -> f64 {
    percentile(values, 0.5)
}

fn max(values: &[f64]) -> f64 {
    values.last().copied().unwrap_or(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    const REFRESH: Duration = Duration::from_micros(4167);

    fn at(us: u64) -> Duration {
        Duration::from_micros(us)
    }

    /// A frame asked for `before` µs ahead of vblank `n`, drawn in `cpu`
    /// µs, shown `shown` vblanks after `n`.
    fn frame(p: &mut Pacing, n: u64, before: u64, cpu: u64, shown: u64) {
        let vblank = REFRESH.as_micros() as u64;
        let requested = at(n * vblank - before);
        let times = Times {
            requested,
            began: requested,
            committed: requested + at(cpu),
        };
        p.shown(times, at((n + shown) * vblank), Some(0.1));
    }

    #[test]
    fn on_time_frames_are_not_late() {
        let mut p = Pacing::new(REFRESH);
        p.vblank(at(0));
        for n in 1..=240 {
            frame(&mut p, n, 3000, 200, 0);
        }
        assert_eq!((p.run.late_drawing, p.run.late_display), (0, 0));
        assert_eq!(p.run.frames, 240);
        let line = p.summary(&p.run).unwrap();
        assert!(line.contains("240 frames"), "{line}");
        assert!(line.contains("0 late"), "{line}");
    }

    #[test]
    fn late_frames_are_blamed_on_who_held_them() {
        let mut p = Pacing::new(REFRESH);
        p.vblank(at(0));
        frame(&mut p, 1, 3000, 200, 0);
        // Still drawing when vblank 2 passed.
        frame(&mut p, 2, 3000, 3500, 1);
        // Committed in time, shown a vblank later: GPU or display.
        frame(&mut p, 4, 3000, 200, 1);
        assert_eq!((p.run.late_drawing, p.run.late_display), (1, 1));
    }

    #[test]
    fn a_pause_in_frames_is_not_lateness() {
        let mut p = Pacing::new(REFRESH);
        p.vblank(at(0));
        frame(&mut p, 1, 3000, 200, 0);
        // The client draws again 10 vblanks later: no frame was late.
        frame(&mut p, 11, 3000, 200, 0);
        assert_eq!((p.run.late_drawing, p.run.late_display), (0, 0));
        assert_eq!(p.run.frames, 2);
    }

    #[test]
    fn idle_ends_the_run() {
        let mut p = Pacing::new(REFRESH);
        p.vblank(at(0));
        frame(&mut p, 1, 3000, 200, 0);
        frame(&mut p, 2, 3000, 200, 0);
        assert!(p.idle(at(20_000)));
        assert!(!p.idle(at(400_000)));
        assert_eq!(p.run.frames, 0);
        assert!(!p.idle(at(800_000)));
    }

    #[test]
    fn next_vblank_follows_the_phase() {
        let mut p = Pacing::new(Duration::from_millis(4));
        p.vblank(at(100_000));
        assert_eq!(p.next_vblank(at(100_000)), at(104_000));
        assert_eq!(p.next_vblank(at(101_000)), at(104_000));
        assert_eq!(p.next_vblank(at(97_000)), at(100_000));
        assert_eq!(p.next_vblank(at(109_000)), at(112_000));
    }
}
