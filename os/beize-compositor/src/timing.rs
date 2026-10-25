//! When clients' content may apply: once the GPU has drawn it, and as
//! wp_fifo and wp_commit_timing ask, as Mesa's Vulkan FIFO present mode and
//! video players use them. Smithay holds a commit back with a barrier; the
//! compositor lifts it.
//!
//! - drawn: a commit of a dmabuf waits until the client's GPU has written it
//!   (its implicit write fence), as Mutter, KWin, niri and Smithay's anvil
//!   hold it; till then the surface shows what it showed. Applied at once,
//!   the frame's own submission waited for the client's work on the GPU, and
//!   the compositor waited for that frame before its next but one – a slow
//!   or hung client held up every window, the pointer and the input.
//! - fifo: a commit waits until the one before it was shown. Its barrier is
//!   taken with what a frame draws and lifted at that frame's vblank
//!   (`State::present`), for hidden surfaces with their "frame done" – as
//!   Mesa paced FIFO on frame callbacks before wp_fifo.
//! - commit timing: a commit waits for its time; it is let go when the frame
//!   that shows at the first vblank at or after that time is drawn – in the
//!   draw itself, against the vblank it aims at, as niri does: let go a
//!   refresh early, a frame drawn at once (the lead is a whole refresh when
//!   idle) would show it a vblank before its time. A timer wakes the frame
//!   loop for it a refresh before that vblank; a frame that changes nothing
//!   costs no commit to KMS.

use std::time::Duration;

use smithay::reexports::calloop::Interest;
use smithay::reexports::calloop::timer::{TimeoutAction, Timer};
use smithay::reexports::wayland_server::Resource;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{Monotonic, Time};
use smithay::wayland::commit_timing::{
    CommitTimerBarrierStateUserData, CommitTimerStateUserData, Timestamp,
};
use smithay::wayland::compositor::{
    BufferAssignment, SurfaceAttributes, add_blocker, add_pre_commit_hook, with_states,
};
use smithay::wayland::dmabuf::get_dmabuf;
use tracing::warn;

use crate::state::State;

/// Watches a new surface's commits for a time they wait for – this hook
/// comes before Smithay's, which takes the time and holds the commit back –
/// and for a dmabuf the GPU still writes.
pub fn watch(surface: &WlSurface) {
    add_pre_commit_hook::<State, _>(surface, |state, _, surface| {
        let time = with_states(surface, |states| {
            states
                .data_map
                .get::<CommitTimerStateUserData>()
                .and_then(|timer| timer.borrow().timestamp)
        });
        if let Some(time) = time {
            state.wake_for(duration(time));
        }
    });
    add_pre_commit_hook::<State, _>(surface, |state, _, surface| {
        let dmabuf = with_states(surface, |states| {
            match states
                .cached_state
                .get::<SurfaceAttributes>()
                .pending()
                .buffer
                .as_ref()
            {
                Some(BufferAssignment::NewBuffer(buffer)) => get_dmabuf(buffer).ok().cloned(),
                _ => None,
            }
        });
        let (Some(dmabuf), Some(client)) = (dmabuf, surface.client()) else {
            return;
        };
        // Err: written already, nothing to wait for.
        let Ok((blocker, source)) = dmabuf.generate_blocker(Interest::READ) else {
            return;
        };
        let inserted = state.handle.insert_source(source, move |_, _, state| {
            state.unblock(vec![client.clone()]);
            Ok(())
        });
        match inserted {
            Ok(_) => add_blocker(surface, blocker),
            Err(e) => warn!("waiting for a client's buffer: {}", e.error),
        }
    });
}

impl State {
    /// Lets go the commits whose time lies at or before `vblank`, the one
    /// the frame now drawn shows at; their clients' held-back commits apply
    /// at once, in this frame. Then wakes for the next.
    pub fn release_timed(&mut self, vblank: Duration) {
        let mut released = Vec::new();
        let mut next: Option<Duration> = None;
        self.each_surface(|surface, states| {
            let Some(timers) = states.data_map.get::<CommitTimerBarrierStateUserData>() else {
                return;
            };
            let Ok(mut timers) = timers.lock() else {
                return;
            };
            if timers.signal_until(Time::<Monotonic>::from(vblank))
                && let Some(client) = surface.client()
            {
                released.push(client);
            }
            if let Some(time) = timers.next_deadline() {
                let time = duration(time);
                next = Some(next.map_or(time, |n| n.min(time)));
            }
        });
        self.unblock(released);
        if let Some(next) = next {
            self.wake_for(next);
        }
    }

    /// Wakes the frame loop a refresh before the vblank at or after `time`,
    /// unless it wakes earlier already. A later wake is not removed but
    /// left to pass: removed while its event waited in the same dispatch,
    /// calloop warned of an event for a source gone (25.10.).
    pub fn wake_for(&mut self, time: Duration) {
        let now: Duration = self.now();
        let at = self.vblank_from(time).saturating_sub(self.refresh());
        if self.timed_wake.is_some_and(|(_, wakes)| wakes <= at) {
            return;
        }
        self.timed_wakes += 1;
        let wake = self.timed_wakes;
        let token = self.handle.insert_source(
            Timer::from_duration(at.saturating_sub(now)),
            move |_, _, state| {
                if state.timed_wake.is_some_and(|(w, _)| w == wake) {
                    state.timed_wake = None;
                }
                // Drawn at the deadline of the next vblank, where the draw
                // lets go what is due by then (`State::render`).
                state.changed();
                TimeoutAction::Drop
            },
        );
        match token {
            Ok(_) => self.timed_wake = Some((wake, at)),
            Err(e) => warn!("commit timer: {}", e.error),
        }
    }
}

fn duration(time: Timestamp) -> Duration {
    Time::<Monotonic>::from(time).into()
}
