//! Explicit synchronisation (linux-drm-syncobj-v1, ADR 0027 §6), as KWin,
//! Mutter and wlroots offer it and Mesa's Vulkan WSI uses it on a real GPU
//! (not on lavapipe): a client names for each commit the timeline point at
//! which its buffer is written (acquire) and one the compositor signals
//! once it is done reading it (release). Smithay serves the protocol and
//! signals a release point when the last `Buffer` of its commit drops;
//! what it leaves to the compositor is here:
//!
//! - the protocol errors it does not raise – a buffer without points, a
//!   point without a buffer, an shm buffer –, in wlroots' order;
//! - the commit waits for its acquire point instead of the dmabuf's
//!   implicit fence (anvil), and its dmabuf is marked for the renderer,
//!   which then neither waits for the dmabuf's pending writes nor adds its
//!   read to it: nouveau marks every submission a writer of every shared
//!   buffer (`nouveau_exec.c`), so implicit sync waited for the client's
//!   later work (hardware test 0.5.112);
//! - the release: a frame keeps the buffers it read until the GPU finished
//!   it (`Holds`), as KWin ties the release point to its render fence –
//!   replaced while the frame was on the GPU, a buffer was released too
//!   early; a buffer attached again keeps its new release point (Smithay
//!   keeps the `Buffer` of the first commit and leaves the new points; wlroots
//!   ties the release to each commit).

use std::cell::RefCell;
use std::sync::Arc;

use smithay::backend::renderer::utils::{Buffer, RendererSurfaceStateUserData};
use smithay::reexports::wayland_protocols::wp::linux_drm_syncobj::v1::server::wp_linux_drm_syncobj_surface_v1::{
    Error as SyncobjError, WpLinuxDrmSyncobjSurfaceV1,
};
use smithay::reexports::calloop::RegistrationToken;
use smithay::reexports::wayland_server::{DisplayHandle, Resource};
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::wayland::compositor::{
    BufferAssignment, Cacheable, SurfaceAttributes, SurfaceData, TraversalAction,
    add_blocker, add_destruction_hook, add_pre_commit_hook, is_sync_subsurface, with_states,
    with_surface_tree_upward,
};
use smithay::wayland::dmabuf::get_dmabuf;
use smithay::wayland::drm_syncobj::{DrmSyncPoint, DrmSyncobjCachedState};
use tracing::warn;

use crate::render::VkRenderer;
use crate::state::State;

/// A commit that attached a dmabuf with sync points: whether its acquire
/// point was waited for (if not, its dmabuf's implicit fence was). Moves
/// with the commit through Smithay's cached states, so a commit held back
/// (or one of a synchronised subsurface, merged with later ones) keeps its
/// own.
#[derive(Debug, Default, Clone, Copy)]
struct Awaited(Option<bool>);

impl Cacheable for Awaited {
    fn commit(&mut self, _: &DisplayHandle) -> Self {
        std::mem::take(self)
    }

    fn merge_into(self, into: &mut Self, _: &DisplayHandle) {
        if self.0.is_some() {
            *into = self;
        }
    }
}

/// A release point the compositor signals itself: once the last frame
/// that read its buffer is done and the buffer is replaced.
#[derive(Debug)]
struct Release(DrmSyncPoint);

impl Drop for Release {
    fn drop(&mut self) {
        if let Err(e) = self.0.signal() {
            warn!("signalling a release point: {e}");
        }
    }
}

/// The release points of commits that attached the shown buffer again.
#[derive(Debug, Default)]
struct Reattached(RefCell<Vec<Arc<Release>>>);

/// The event sources of a surface's acquire points still awaited: removed
/// with the surface – a point a client never signals would keep its
/// eventfd and source as long as the compositor runs.
#[derive(Debug, Default)]
struct Waits(RefCell<(u64, Vec<(u64, RegistrationToken)>)>);

/// Watches a new surface's commits for their sync points. Comes before
/// Smithay's own hook, which a surface gets with its syncobj object.
pub fn watch(surface: &WlSurface) {
    add_pre_commit_hook::<State, _>(surface, |state, _, surface| {
        let checked = with_states(surface, check);
        match checked {
            Checked::Implicit => crate::timing::wait_written(state, surface),
            Checked::Rejected | Checked::Unattached => {}
            Checked::Acquire(point) => {
                let awaited = wait_for(state, surface, &point);
                if !awaited {
                    crate::timing::wait_written(state, surface);
                }
                with_states(surface, |states| {
                    states.cached_state.get::<Awaited>().pending().0 = Some(awaited);
                });
            }
        }
    });
    add_destruction_hook::<State, _>(surface, |state, surface| {
        let waits = with_states(surface, |states| {
            states
                .data_map
                .get::<Waits>()
                .map(|w| std::mem::take(&mut w.0.borrow_mut().1))
                .unwrap_or_default()
        });
        for (_, token) in waits {
            state.handle.remove(token);
        }
    });
}

/// What a commit is for explicit sync.
enum Checked {
    /// The surface has no syncobj object: implicit sync.
    Implicit,
    /// A protocol error, raised.
    Rejected,
    /// No buffer attached, no points.
    Unattached,
    /// A dmabuf with both points: its acquire point.
    Acquire(DrmSyncPoint),
}

/// The protocol's errors for a commit of a surface with a syncobj object,
/// as wlroots checks them (`wlr_linux_drm_syncobj_v1.c`, surface commit);
/// Smithay raises only some, and conflicting points itself.
fn check(states: &SurfaceData) -> Checked {
    let Some(resource) = states
        .data_map
        .get::<RefCell<Option<WpLinuxDrmSyncobjSurfaceV1>>>()
        .and_then(|r| r.borrow().clone())
    else {
        return Checked::Implicit;
    };
    let mut attributes = states.cached_state.get::<SurfaceAttributes>();
    let buffer = match &attributes.pending().buffer {
        Some(BufferAssignment::NewBuffer(buffer)) => Some(buffer.clone()),
        _ => None,
    };
    let mut points = states.cached_state.get::<DrmSyncobjCachedState>();
    let points = points.pending();
    let error = match (&buffer, &points.acquire_point, &points.release_point) {
        (None, Some(_), _) => Some((
            SyncobjError::NoBuffer,
            "acquire point set but no buffer attached",
        )),
        (None, _, Some(_)) => Some((
            SyncobjError::NoBuffer,
            "release point set but no buffer attached",
        )),
        (Some(_), None, _) => Some((
            SyncobjError::NoAcquirePoint,
            "buffer attached but no acquire point set",
        )),
        (Some(_), _, None) => Some((
            SyncobjError::NoReleasePoint,
            "buffer attached but no release point set",
        )),
        (Some(buffer), Some(_), Some(_)) if get_dmabuf(buffer).is_err() => Some((
            SyncobjError::UnsupportedBuffer,
            "sync points with a buffer that is no dmabuf",
        )),
        _ => None,
    };
    if let Some((code, message)) = error {
        resource.post_error(code as u32, message);
        // Smithay's hook would raise its own error again, and each error
        // ends the client once more (`ClientData::disconnected`); the
        // client is gone, its points need no signal.
        points.acquire_point = None;
        points.release_point = None;
        return Checked::Rejected;
    }
    match &points.acquire_point {
        Some(point) => Checked::Acquire(point.clone()),
        None => Checked::Unattached,
    }
}

/// Holds the commit back until `point` signals (anvil: the point's
/// blocker instead of the dmabuf's); false if its eventfd cannot be made.
fn wait_for(state: &mut State, surface: &WlSurface, point: &DrmSyncPoint) -> bool {
    let (blocker, source) = match point.generate_blocker() {
        Ok(made) => made,
        Err(e) => {
            warn!("waiting for an acquire point: {e}; for the dmabuf's fence instead");
            return false;
        }
    };
    let Some(client) = surface.client() else {
        return false;
    };
    let id = with_states(surface, |states| {
        states.data_map.insert_if_missing(Waits::default);
        let mut waits = states.data_map.get::<Waits>().map(|w| w.0.borrow_mut());
        waits.as_mut().map_or(0, |w| {
            w.0 += 1;
            w.0
        })
    });
    let weak = surface.downgrade();
    let inserted = state.handle.insert_source(source, move |_, _, state| {
        if let Ok(surface) = weak.upgrade() {
            with_states(&surface, |states| {
                if let Some(waits) = states.data_map.get::<Waits>() {
                    waits.0.borrow_mut().1.retain(|(i, _)| *i != id);
                }
            });
        }
        state.unblock(vec![client.clone()]);
        Ok(())
    });
    match inserted {
        Ok(token) => {
            with_states(surface, |states| {
                if let Some(waits) = states.data_map.get::<Waits>() {
                    waits.0.borrow_mut().1.push((id, token));
                }
            });
            add_blocker(surface, blocker);
            true
        }
        Err(e) => {
            warn!("waiting for an acquire point: {}", e.error);
            false
        }
    }
}

/// A commit of `surface` applies (`CompositorHandler::commit`, before
/// Smithay's buffer handler takes its points): for each surface of the
/// tree whose new buffer is a dmabuf, the renderer learns whether it is
/// synchronised explicitly; a buffer attached again keeps the commit's
/// release point. Returns the release points of buffers attached again and
/// now replaced: frames on the GPU may still read them (`Holds`).
pub fn applied(renderer: &VkRenderer, surface: &WlSurface) -> Vec<Held> {
    let mut replaced = Vec::new();
    if is_sync_subsurface(surface) {
        return replaced;
    }
    with_surface_tree_upward(
        surface,
        (),
        |_, _, _| TraversalAction::DoChildren(()),
        |_, states, _| replaced.extend(applied_one(renderer, states)),
        |_, _, _| true,
    );
    replaced
}

fn applied_one(renderer: &VkRenderer, states: &SurfaceData) -> Option<Held> {
    let mut attributes = states.cached_state.get::<SurfaceAttributes>();
    let synced = states.cached_state.get::<Awaited>().current().0.take();
    let buffer = match &attributes.current().buffer {
        Some(BufferAssignment::NewBuffer(buffer)) => buffer.clone(),
        Some(BufferAssignment::Removed) => return forget_reattached(states),
        None => return None,
    };
    if let Ok(dmabuf) = get_dmabuf(&buffer) {
        // Without its acquire point waited for (no eventfd), the implicit
        // fence was, and stays what the GPU waits for; its release point
        // still waits for the frames that read it.
        renderer.synced_explicitly(dmabuf, synced == Some(true), synced.is_some());
    }
    let again = states
        .data_map
        .get::<RendererSurfaceStateUserData>()
        .and_then(|s| {
            s.lock()
                .ok()
                .map(|s| s.buffer().is_some_and(|b| b == buffer))
        })
        .unwrap_or(false);
    if !again {
        return forget_reattached(states);
    }
    // Smithay keeps the `Buffer` of the first commit and these points in
    // the current state, where the next commit with points would signal
    // the release at once (`DrmSyncobjCachedState::merge_into`).
    let mut points = states.cached_state.get::<DrmSyncobjCachedState>();
    let points = points.current();
    points.acquire_point = None;
    if let Some(release) = points.release_point.take() {
        states.data_map.insert_if_missing(Reattached::default);
        if let Some(reattached) = states.data_map.get::<Reattached>() {
            reattached.0.borrow_mut().push(Arc::new(Release(release)));
        }
    }
    None
}

/// The buffer is replaced: its extra release points, to be kept until the
/// frames on the GPU are done (wlroots signals them with the buffer's
/// release).
fn forget_reattached(states: &SurfaceData) -> Option<Held> {
    let reattached = std::mem::take(&mut *states.data_map.get::<Reattached>()?.0.borrow_mut());
    (!reattached.is_empty()).then_some(Held {
        _buffer: None,
        _reattached: reattached,
    })
}

/// What a frame read of a buffer synchronised explicitly: kept until the
/// GPU finished the frame, its release points signal only then.
#[derive(Debug)]
pub struct Held {
    _buffer: Option<Buffer>,
    _reattached: Vec<Arc<Release>>,
}

/// The buffer a surface shows, if it is synchronised explicitly.
pub fn held(renderer: &VkRenderer, states: &SurfaceData) -> Option<Held> {
    let buffer = states
        .data_map
        .get::<RendererSurfaceStateUserData>()?
        .lock()
        .ok()?
        .buffer()?
        .clone();
    if !get_dmabuf(&buffer).is_ok_and(|d| renderer.released_explicitly(d)) {
        return None;
    }
    let reattached = states
        .data_map
        .get::<Reattached>()
        .map(|r| r.0.borrow().clone())
        .unwrap_or_default();
    Some(Held {
        _buffer: Some(buffer),
        _reattached: reattached,
    })
}

/// The buffers frames on the GPU read, by submission.
#[derive(Debug, Default)]
pub struct Holds(Vec<(u64, Vec<Held>)>);

impl Holds {
    pub fn add(&mut self, submission: u64, held: Vec<Held>) {
        if !held.is_empty() {
            self.0.push((submission, held));
        }
    }

    /// Lets go of what the submissions up to `completed` read.
    pub fn done(&mut self, completed: u64) {
        self.0.retain(|(submission, _)| *submission > completed);
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}
