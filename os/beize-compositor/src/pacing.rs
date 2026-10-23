//! How frames meet their vblank, measured by the compositor itself (ADR
//! 0027 §6 "Bildtakt messen"). The compositor's task is to show a change at
//! the first vblank whose deadline it met (`schedule`). Each drawn frame
//! keeps when its change arrived, that vblank, when drawing began and when
//! it went to KMS; the vblank that shows it says whether it made it.
//!
//! A frame shown later than that vblank is late, and its times say who
//! held it up, measured against where the vblank begins (the display takes
//! a frame there; KMS stamps the vblank's end): committed after it
//! (drawing, the compositor's CPU work or a late wakeup), done on the GPU
//! after it (the kernel's time of the frame's fence), or ready before it
//! and still not shown (display: the kernel and the driver). How often
//! clients draw is theirs; a client that is late shows in its own frame
//! times, not here.
//!
//! Frames in a row (an animation, a client drawing each frame) make one
//! summary in the journal, when they end or every 30 s, at info level from
//! a second on:
//!
//! `pacing: 4800 frames in 20.00 s at 240 Hz, 0 late (…); change to screen
//! median 3.71 ms, …; ready median 1.21 ms before the vblank begins, …;
//! busy 4 % of the 4.17 ms per frame`
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

/// When a frame's change arrived, the vblank it is due at (where it ends),
/// when its drawing was planned to begin and began, and when it was
/// committed; on CLOCK_MONOTONIC, as KMS's vblank times. And the schedule's
/// lead it was planned with, if it was planned.
#[derive(Clone, Copy, Debug)]
pub struct Times {
    pub requested: Duration,
    pub due: Duration,
    pub planned: Duration,
    pub began: Duration,
    pub committed: Duration,
    pub lead: Option<Duration>,
}

/// The frames of one run.
#[derive(Default)]
struct Run {
    /// The vblank of its first frame.
    first: Duration,
    frames: u32,
    /// Shown after their vblank: committed after it began (drawing), done
    /// on the GPU after it (GPU), or ready before and still late (display).
    late_drawing: u32,
    late_gpu: u32,
    late_display: u32,
    /// Change to screen, drawing (CPU) and GPU time, and how long before
    /// its vblank began the frame was ready (negative: after), in
    /// milliseconds.
    latency: Vec<f64>,
    cpu: Vec<f64>,
    gpu: Vec<f64>,
    ahead: Vec<f64>,
    /// The schedule's lead, in milliseconds.
    lead: Vec<f64>,
}

pub struct Pacing {
    refresh: Duration,
    /// How long the vblank lasts.
    blank: Duration,
    /// The vblank of the last frame shown.
    last: Option<Duration>,
    run: Run,
}

impl Pacing {
    pub fn new(refresh: Duration, blank: Duration) -> Pacing {
        Pacing {
            refresh,
            blank,
            last: None,
            run: Run::default(),
        }
    }

    /// A drawn frame is on the screen since `vblank`; `gpu_ms` if its GPU
    /// time (or a recent frame's) was read since the last call, `gpu_done`
    /// when its fence signalled if it had one.
    pub fn shown(
        &mut self,
        times: Times,
        vblank: Duration,
        gpu_ms: Option<f64>,
        gpu_done: Option<Duration>,
    ) {
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
        let due = times.due;
        let begins = due.saturating_sub(self.blank);
        let ready = gpu_done.map_or(times.committed, |g| g.max(times.committed));
        // Half a refresh of tolerance: vblank times jitter by microseconds.
        if vblank > due + self.refresh / 2 {
            if times.committed > begins {
                self.run.late_drawing += 1;
            } else if ready > begins {
                self.run.late_gpu += 1;
            } else {
                self.run.late_display += 1;
            }
        }
        let run = &mut self.run;
        run.latency.push(ms(vblank.saturating_sub(times.requested)));
        run.cpu
            .push(ms(times.committed.saturating_sub(times.began)));
        run.gpu.extend(gpu_ms);
        run.ahead.push(ms(begins) - ms(ready));
        run.lead.extend(times.lead.map(ms));
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
        let (ahead, lead) = (sorted(&run.ahead), sorted(&run.lead));
        let busy = (median(&cpu) + median(&gpu)) / budget * 100.0;
        Some(format!(
            "pacing: {} frames in {:.2} s at {:.0} Hz, {} late (drawing {}, GPU {}, display {}); \
             change to screen median {:.2} ms, max {:.2}; ready median {:.2} ms before the \
             vblank begins, least {:.2}; lead median {:.2} ms, max {:.2}; drawing (CPU) median \
             {:.2} ms, 99 % {:.2}, max {:.2}; \
             GPU median {:.2} ms, max {:.2}; busy {:.0} % of the {:.2} ms per frame",
            run.frames,
            length.as_secs_f64(),
            1000.0 / budget,
            run.late_drawing + run.late_gpu + run.late_display,
            run.late_drawing,
            run.late_gpu,
            run.late_display,
            median(&latency),
            max(&latency),
            median(&ahead),
            ahead.first().copied().unwrap_or(0.0),
            median(&lead),
            max(&lead),
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
    const BLANK: Duration = Duration::from_micros(461);

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
            due: at(n * vblank),
            planned: requested,
            began: requested,
            committed: requested + at(cpu),
            lead: Some(at(3000)),
        };
        p.shown(times, at((n + shown) * vblank), Some(0.1), None);
    }

    #[test]
    fn on_time_frames_are_not_late() {
        let mut p = Pacing::new(REFRESH, BLANK);
        for n in 1..=240 {
            frame(&mut p, n, 3000, 200, 0);
        }
        assert_eq!((p.run.late_drawing, p.run.late_display), (0, 0));
        assert_eq!(p.run.frames, 240);
        let line = p.summary(&p.run).unwrap();
        assert!(line.contains("240 frames"), "{line}");
        assert!(line.contains("0 late"), "{line}");
        // Committed 2.8 ms before the vblank's end, 0.461 ms of blank.
        assert!(line.contains("ready median 2.34 ms before"), "{line}");
        assert!(line.contains("lead median 3.00 ms, max 3.00"), "{line}");
    }

    #[test]
    fn late_frames_are_blamed_on_who_held_them() {
        let mut p = Pacing::new(REFRESH, BLANK);
        frame(&mut p, 1, 3000, 200, 0);
        // Committed inside vblank 2: after it began.
        frame(&mut p, 2, 3000, 2800, 1);
        // Committed in time, shown a vblank later: the display.
        frame(&mut p, 4, 3000, 200, 1);
        assert_eq!(
            (p.run.late_drawing, p.run.late_gpu, p.run.late_display),
            (1, 0, 1)
        );
        // Committed in time, the GPU done after the vblank began.
        let vblank = REFRESH.as_micros() as u64;
        let times = Times {
            requested: at(6 * vblank - 3000),
            due: at(6 * vblank),
            planned: at(6 * vblank - 3000),
            began: at(6 * vblank - 3000),
            committed: at(6 * vblank - 2800),
            lead: None,
        };
        p.shown(times, at(7 * vblank), None, Some(at(6 * vblank - 100)));
        assert_eq!(p.run.late_gpu, 1);
    }

    #[test]
    fn a_pause_in_frames_is_not_lateness() {
        let mut p = Pacing::new(REFRESH, BLANK);
        frame(&mut p, 1, 3000, 200, 0);
        // The client draws again 10 vblanks later: no frame was late.
        frame(&mut p, 11, 3000, 200, 0);
        assert_eq!((p.run.late_drawing, p.run.late_display), (0, 0));
        assert_eq!(p.run.frames, 2);
    }

    #[test]
    fn idle_ends_the_run() {
        let mut p = Pacing::new(REFRESH, BLANK);
        frame(&mut p, 1, 3000, 200, 0);
        frame(&mut p, 2, 3000, 200, 0);
        assert!(p.idle(at(20_000)));
        assert!(!p.idle(at(400_000)));
        assert_eq!(p.run.frames, 0);
        assert!(!p.idle(at(800_000)));
    }
}
