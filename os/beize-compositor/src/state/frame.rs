//! The frame loop: scheduling a frame, drawing it (`render`), the flip,
//! pacing, the pointer's image, and telling clients when their content
//! was shown (presentation, frame callbacks, fifo, explicit sync).

use super::*;

/// The fifo barrier of a surface's content (`timing`), with its client.
pub(super) fn take_fifo(
    surface: &WlSurface,
    states: &SurfaceData,
) -> Option<(Barrier, Option<Client>)> {
    let barrier = states
        .cached_state
        .get::<FifoBarrierCachedState>()
        .current()
        .barrier
        .take()?;
    Some((barrier, surface.client()))
}

/// Lifts fifo barriers; the clients whose commits waited for them.
pub(super) fn lift(fifo: Vec<(Barrier, Option<Client>)>) -> Vec<Client> {
    fifo.into_iter()
        .filter_map(|(barrier, client)| {
            barrier.signal();
            client
        })
        .collect()
}

impl State {
    /// Sets the timer that draws, unless one is set or a commit waits for
    /// its vblank (whose event calls again). Not while another session
    /// has the screen: `activate` calls again. Without a screen, clients
    /// hear "frame done" as hidden ones do.
    pub(super) fn schedule_render(&mut self) {
        if self.draw_timer.is_some() || !self.session.is_active() {
            return;
        }
        let Some(gpu) = &mut self.gpu else {
            return;
        };
        if !gpu.shows() {
            self.frame_done_later();
            return;
        }
        let Some(at) = gpu.draw_at() else {
            return;
        };
        let now: Duration = self.clock.now().into();
        let token = self.handle.insert_source(
            Timer::from_duration(at.saturating_sub(now)),
            |_, _, state| {
                // Commits due by the vblank this frame shows at apply first
                // (`timing`); while this timer is set, they set no other.
                if let Some(vblank) = state.gpu.as_ref().and_then(Gpu::aim) {
                    state.release_timed(vblank);
                }
                state.draw_timer = None;
                if state.redraw {
                    state.render();
                }
                TimeoutAction::Drop
            },
        );
        match token {
            Ok(token) => self.draw_timer = Some(token),
            Err(e) => self.fail(error(format!("draw timer: {}", e.error))),
        }
    }

    /// Draws again after a failed frame: one refresh later, each further
    /// failure in a row twice as late, at most `FAILED_AT_MOST`. At once would
    /// turn a lasting failure (a lost device, a refused commit) into a loop
    /// on one CPU that fills the journal.
    pub(super) fn retry_frame(&mut self) {
        self.redraw = true;
        if self.draw_timer.is_some() {
            return;
        }
        let refresh = self
            .gpu
            .as_ref()
            .and_then(Gpu::refresh)
            .unwrap_or(FAILED_AT_MOST);
        let wait = refresh
            .saturating_mul(1 << self.frames_failed.min(16))
            .min(FAILED_AT_MOST);
        self.frames_failed = self.frames_failed.saturating_add(1);
        let token = self
            .handle
            .insert_source(Timer::from_duration(wait), |_, _, state| {
                state.draw_timer = None;
                if state.redraw {
                    state.schedule_render();
                }
                TimeoutAction::Drop
            });
        match token {
            Ok(token) => self.draw_timer = Some(token),
            Err(e) => self.fail(error(format!("draw timer: {}", e.error))),
        }
    }

    /// Draws what changed and places the cursor.
    pub(super) fn render(&mut self) {
        if !self.session.is_active() {
            return;
        }
        // An inhibitor shown until now takes a fade back before it draws.
        self.look_at_inhibitors();
        // A window came to the front or left it: the panels over fullscreen,
        // and what the pointer is over then.
        if self.reveal() {
            crate::input::refresh_pointer(self);
        }
        let hide_upper = self.fullscreen_in_front();
        let revealed = self.revealed;
        // Over a fullscreen window, in front or behind, the menu bar lies on
        // live glass (`backdrop`), in place of any glass it asked for.
        let plate = self
            .space
            .elements()
            .any(crate::windows::fullscreen)
            .then(|| self.band(Side::Top))
            .flatten()
            .filter(|_| !hide_upper || revealed == Some(Side::Top));
        // The windows the switcher's overlay may show pictures of.
        let thumbnailed = if self.switcher.as_ref().is_some_and(|s| s.shown) {
            self.numbered_windows()
        } else {
            Vec::new()
        };
        let refresh = self.refresh();
        let (Some(gpu), Some(output)) = (&mut self.gpu, &self.output) else {
            return;
        };
        self.redraw = false;
        let began: Duration = self.clock.now().into();
        let requested = self.changed_at.unwrap_or(began);
        let Some(area) = self.space.output_geometry(output) else {
            return;
        };
        // The background and the glass, once per size and appearance: drawn
        // with this frame, ahead of it.
        let want = (area.size.to_physical(1), self.appearance);
        if self.backdrop_for != Some(want) && self.backdrop_retry.is_none_or(|retry| began >= retry)
        {
            self.backdrops += 1;
            self.backdrop = match Backdrop::new(gpu.renderer(), want.0, want.1, self.backdrops) {
                Ok(backdrop) => {
                    self.backdrop_for = Some(want);
                    self.backdrop_retry = None;
                    Some(backdrop)
                }
                // With it would go the glass and the snap preview, until the
                // size or appearance changed. Not each frame: a second later.
                Err(e) => {
                    warn!("drawing the background: {e}; a plain colour until it works");
                    self.backdrop_for = None;
                    self.backdrop_retry = Some(began + Duration::from_secs(1));
                    None
                }
            };
        }
        let at = self.pointer.to_physical(1.0).to_i32_round();
        let on_plane = self.cursor.image(began).is_some_and(|i| gpu.cursor_fits(i));
        // In front of all, the black the desktop fades in from, or the
        // black it fades to before the screen goes off (`idle`), the
        // pointer still over it.
        // Animations as they are at the vblank this frame shows at, as
        // Core Animation's `targetTimestamp` (ADR 0032 §3): a frame that
        // comes late shows them where they are then, not behind. Before
        // 17.11. they were taken at when drawing began, a little early.
        let shown_at = gpu.aim().unwrap_or(began);
        self.animations.start(shown_at, refresh);
        // What the windows' pictures drew: shown with them (`animation`).
        let mut offscreen = RenderElementStates::default();
        let veil = self.opening.veil(shown_at);
        let shade = match (veil, self.idle.black(shown_at)) {
            (Veil::None, Some(black)) if black > 0.0 => Veil::Fading(black),
            _ => veil,
        };
        let mut elements: Vec<Element> = Vec::new();
        if shade != Veil::None {
            let alpha = match shade {
                Veil::Fading(alpha) => alpha,
                Veil::Black | Veil::None => 1.0,
            };
            if alpha != self.veil_alpha {
                self.veil_alpha = alpha;
                self.veil_commit.increment();
            }
            elements.push(scene::veil_over(
                &self.veil_id,
                self.veil_commit,
                area,
                alpha,
            ));
        }
        if !on_plane && let Some((buffer, hotspot)) = self.cursor.drawn(began) {
            let location = (at - Point::from(hotspot)).to_f64();
            match MemoryRenderBufferRenderElement::from_buffer(
                gpu.renderer(),
                location,
                &buffer,
                None,
                None,
                None,
                Kind::Cursor,
            ) {
                Ok(element) => elements.push(Element::Cursor(element)),
                Err(e) => warn!("drawing the pointer: {e}"),
            }
        }
        // The panels as the shell drew them, or those kept of a shell that
        // failed; lent to this frame and taken back after it.
        if self.kept.is_none() {
            self.panels_last = scene::panels(
                gpu.renderer(),
                output,
                self.backdrop.as_mut(),
                plate,
                &thumbnailed,
                &mut self.decorations,
                &mut self.animations,
                shown_at,
            );
        }
        let source = match &mut self.kept {
            Some(kept) => &mut kept.panels,
            None => &mut self.panels_last,
        };
        let shown = |panel: &scene::PanelFrame| {
            panel.overlay
                || !panel.upper
                || !hide_upper
                || revealed == Some(side(area, panel.place))
        };
        let upper = scene::lend(source, true, shown, &mut elements);
        let mut plated = false;
        if let Some(backdrop) = &mut self.backdrop {
            let glass = plate.map(|band| backdrop.plate(band));
            plated = glass.is_some();
            self.shell_windows.set_tone(backdrop.bar_tone(plated));
            elements.extend(glass.map(Element::Glass));
        }
        // Each window with its bar above it, front to back.
        let preview_shown = self.animations.preview_shown(began);
        let preview = self.snap_preview.and_then(|place| {
            let backdrop = self.backdrop.as_ref()?;
            let glass = backdrop
                .snap_preview(&self.snap_preview_id, self.snap_preview_commit, place)
                .faded(preview_shown);
            Some(Element::Glass(glass))
        });
        scene::windows(
            gpu.renderer(),
            &self.space,
            &mut self.decorations,
            area,
            preview,
            self.backdrop.as_mut(),
            &mut self.animations,
            shown_at,
            &mut offscreen,
            &mut elements,
        );
        let lower = scene::lend(source, false, shown, &mut elements);
        if let Some(backdrop) = &self.backdrop {
            elements.push(Element::Wallpaper(backdrop.scrim()));
            elements.push(Element::Wallpaper(backdrop.wallpaper()));
        }
        let scene_done: Duration = self.clock.now().into();
        // An app's glass goes with the frame that no longer shows it.
        let shown_glass: Vec<_> = elements
            .iter()
            .filter_map(|e| match e {
                Element::Glass(glass) => glass.live().cloned(),
                _ => None,
            })
            .collect();
        self.live_glass.retain(|l| l.wanted(plated, &shown_glass));
        scene::live_glass(
            gpu.renderer(),
            &mut self.live_glass,
            &mut elements,
            area.size.to_physical(1),
            self.appearance,
        );
        // The pointer shows with the desktop.
        let cursor = self
            .cursor
            .image(began)
            .filter(|_| on_plane && veil == Veil::None)
            .map(|image| (image, at));
        // Until the desktop shows, the black alone: it sets the mode.
        let drawn = if veil == Veil::Black {
            &elements[..1]
        } else {
            &elements[..]
        };
        let now: Duration = self.clock.now().into();
        let parts = Parts {
            scene: scene_done.saturating_sub(began),
            glass: now.saturating_sub(scene_done),
            ..Parts::default()
        };
        let frame = gpu.render(drawn, cursor, requested, began, parts);
        scene::take_back(source, &mut elements, lower);
        scene::take_back(source, &mut elements, upper);
        if !matches!(frame, Ok(Frame::Deferred)) {
            self.changed_at = None;
        }
        if frame.is_ok() {
            self.frames_failed = 0;
        }
        match frame {
            Ok(Frame::Queued(mut shown)) => {
                shown.states.extend(offscreen.states);
                self.hold_read(Some(&shown));
                // The closed windows' pictures are drawn with it: their
                // buffers are kept as long as what it read.
                let kept = self.animations.drawn();
                if let Some(gpu) = &self.gpu {
                    self.holds.add(gpu.renderer_ref().submitted(), kept);
                }
                let (mut taken, hidden) = self.take_shown(&shown);
                // Drawn: their clients may draw the next frame now, a
                // refresh ahead of the next drawing (`take_shown`).
                let msec = Duration::from(self.clock.now()).as_millis() as u32;
                for callback in taken.callbacks.drain(..) {
                    callback.done(msec);
                }
                if let Some(old) = self.presenting.replace(taken) {
                    self.never_shown(old);
                }
                self.drawn = shown;
                self.unblock(lift(hidden));
            }
            Ok(Frame::Deferred) => self.redraw = true,
            Ok(Frame::Unchanged(mut shown)) => {
                shown.states.extend(offscreen.states);
                let (taken, hidden) = self.take_shown(&shown);
                self.unchanged.push(taken);
                self.drawn = shown;
                self.unblock(lift(hidden));
                self.frame_done_later();
            }
            // One frame lost, not the session with its windows: said, and
            // drawn again, as Mutter, KWin and niri go on after a frame that
            // failed. What it would have shown waits for that frame, its
            // clients' callbacks with it.
            Err(e) => {
                warn!("a frame failed: {e:#}");
                // It may have reached the GPU: all it could have read.
                self.hold_read(None);
                self.retry_frame();
            }
        }
        self.watch_flip();
        self.animate_cursor(began);
        self.animations.end(shown_at);
    }

    /// An animated pointer (`cursor`): draws again when its next frame is
    /// due; a still one sets no timer.
    pub(super) fn animate_cursor(&mut self, now: Duration) {
        if self.cursor_timer.is_some() {
            return;
        }
        let Some(next) = self.cursor.next_frame(now) else {
            return;
        };
        let token = self.handle.insert_source(
            Timer::from_duration(next.saturating_sub(now)),
            |_, _, state| {
                state.cursor_timer = None;
                state.changed();
                TimeoutAction::Drop
            },
        );
        match token {
            Ok(token) => self.cursor_timer = Some(token),
            Err(e) => warn!("cursor timer: {}", e.error),
        }
    }

    /// The pointer is on no client's surface: the compositor names its
    /// image – a resize arrow on a window's edge, as macOS shows it, else
    /// the arrow. Not during a grab: a resize keeps its arrow.
    pub fn own_cursor(&mut self) {
        if self.seat.get_pointer().is_some_and(|p| p.is_grabbed()) {
            return;
        }
        let icon = match self.edge_under(self.pointer).map(|(_, edges)| edges) {
            Some(ResizeEdge::Top | ResizeEdge::Bottom) => CursorIcon::NsResize,
            Some(ResizeEdge::Left | ResizeEdge::Right) => CursorIcon::EwResize,
            Some(ResizeEdge::TopLeft | ResizeEdge::BottomRight) => CursorIcon::NwseResize,
            Some(ResizeEdge::TopRight | ResizeEdge::BottomLeft) => CursorIcon::NeswResize,
            _ => CursorIcon::Default,
        };
        let now = self.clock.now().into();
        self.cursor.set(CursorImageStatus::Named(icon), now);
    }

    /// The arrow, until a client names another image.
    pub fn default_cursor(&mut self) {
        let now = self.clock.now().into();
        self.cursor
            .set(CursorImageStatus::Named(CursorIcon::Default), now);
    }

    /// A commit went to KMS: if its vblank has not come after
    /// `FLIP_TIMEOUT`, that is said each time, as KWin's commit thread says
    /// it, and waited for further – nothing is reset; the vblank, when it
    /// comes, says how late (`Output::vblank`). An event that lay unread
    /// meanwhile means the compositor was held up, not the driver (KWin
    /// asks its main thread). Only while a commit waits: idle, no timer.
    pub(super) fn watch_flip(&mut self) {
        if self.flip_watch.is_some() {
            return;
        }
        // Counted from when it could show (`Output::flip_since`); while the
        // GPU still draws, from now, looked at again then.
        let Some(since) = self.gpu.as_ref().and_then(Gpu::flip_since) else {
            return;
        };
        let now = self.now();
        let at = since.unwrap_or(now) + FLIP_TIMEOUT;
        let token = self.handle.insert_source(
            Timer::from_duration(at.saturating_sub(now)),
            |_, _, state| {
                let now = state.now();
                // Another session's screen is not ours to watch.
                let waiting = state
                    .gpu
                    .as_ref()
                    .filter(|_| state.session.is_active())
                    .and_then(|gpu| Some((gpu, gpu.flip_since()?)));
                let Some((gpu, since)) = waiting else {
                    state.flip_watch = None;
                    return TimeoutAction::Drop;
                };
                let next = match since {
                    // The GPU still draws the frame (a client's long work it
                    // waits for): not KMS's to show yet.
                    None => FLIP_TIMEOUT,
                    // A later commit than the one this was set for.
                    Some(since) if now.saturating_sub(since) < FLIP_TIMEOUT => {
                        FLIP_TIMEOUT - now.saturating_sub(since)
                    }
                    Some(since) => {
                        let waited = now.saturating_sub(since).as_millis();
                        // Held up (a VM's CPU stopped, the system swapping) is
                        // no fault of the compositor or the driver: said, not
                        // warned of.
                        if gpu.events_waiting() {
                            info!(
                                "the compositor was held up: a vblank waited {waited} ms to be read"
                            );
                        } else {
                            error!(
                                "page flip timed out, no vblank {waited} ms after the frame \
                                 was committed and drawn: a bug in the kernel driver"
                            );
                        }
                        FLIP_TIMEOUT
                    }
                };
                if let Some((_, at)) = &mut state.flip_watch {
                    *at = now + next;
                }
                TimeoutAction::ToDuration(next)
            },
        );
        match token {
            Ok(token) => self.flip_watch = Some((token, at)),
            Err(e) => warn!("page flip timer: {}", e.error),
        }
    }

    /// The vblank of a commit on `crtc` came: the watch goes – not for an
    /// event of a screen gone, nor once its time is reached: then its event
    /// may wait in this same dispatch (calloop warns of an event for a
    /// source gone); it finds no commit then and goes itself.
    pub(super) fn flip_came(&mut self, crtc: smithay::reexports::drm::control::crtc::Handle) {
        if !self.gpu.as_ref().is_some_and(|gpu| gpu.flipping_on(crtc)) {
            return;
        }
        let now = self.now();
        if let Some((token, at)) = self.flip_watch
            && now < at
        {
            self.flip_watch = None;
            self.handle.remove(token);
        }
    }

    /// While frames come in a row, checks twice a second whether they
    /// stopped, so their summary reaches the journal; idle, no timer runs.
    pub(super) fn watch_pacing(&mut self) {
        if self.pacing_timer.is_some() {
            return;
        }
        let token = self
            .handle
            .insert_source(Timer::from_duration(PACING_CHECK), |_, _, state| {
                let running = state.gpu.as_mut().is_some_and(Gpu::pacing_idle);
                if running {
                    TimeoutAction::ToDuration(PACING_CHECK)
                } else {
                    state.pacing_timer = None;
                    TimeoutAction::Drop
                }
            });
        match token {
            Ok(token) => self.pacing_timer = Some(token),
            Err(e) => warn!("pacing timer: {}", e.error),
        }
    }

    /// "Frame done" one refresh from now, for clients that committed
    /// without changing what the screen shows; without a screen, as for
    /// hidden ones.
    /// "Frame done" for the hidden windows in `after`.
    pub(super) fn hidden_tick(&mut self, after: Duration) {
        if self.hidden_timer.is_some() {
            return;
        }
        let token = self
            .handle
            .insert_source(Timer::from_duration(after), |_, _, state| {
                state.hidden_timer = None;
                state.frame_done(None);
                TimeoutAction::Drop
            });
        match token {
            Ok(token) => self.hidden_timer = Some(token),
            Err(e) => warn!("hidden windows' timer: {}", e.error),
        }
    }

    pub(super) fn frame_done_later(&mut self) {
        if self.frame_timer.is_some() {
            return;
        }
        let refresh = self.gpu.as_ref().map_or(Duration::from_millis(16), |gpu| {
            gpu.refresh().unwrap_or(HIDDEN_FRAMES)
        });
        let token = self
            .handle
            .insert_source(Timer::from_duration(refresh), |_, _, state| {
                state.frame_timer = None;
                // Without a screen no frame is drawn: what is due applies
                // now (`timing`).
                if !state.gpu.as_ref().is_some_and(Gpu::shows) {
                    let now = state.now();
                    state.release_timed(now);
                }
                state.presented_unchanged();
                state.release_read();
                state.frame_done(None);
                TimeoutAction::Drop
            });
        match token {
            Ok(token) => self.frame_timer = Some(token),
            Err(e) => warn!("frame timer: {}", e.error),
        }
    }

    /// Each surface of the windows – with their popups, minimized ones too –
    /// and of the panels.
    pub fn each_surface(&self, mut f: impl FnMut(&WlSurface, &SurfaceData)) {
        let mut tree = |surface: &WlSurface| {
            with_surface_tree_downward(
                surface,
                (),
                |_, _, &()| TraversalAction::DoChildren(()),
                |surface, states, &()| f(surface, states),
                |_, _, &()| true,
            );
        };
        // With windows not shown yet: a first commit held back for its time
        // (wp_commit_timing) has to be let go, or the window never maps.
        for window in self
            .space
            .elements()
            .chain(&self.minimized)
            .chain(&self.unmapped)
        {
            let Some(toplevel) = window.toplevel() else {
                continue;
            };
            let surface = toplevel.wl_surface();
            tree(surface);
            for (popup, _) in PopupManager::popups_for_surface(surface) {
                tree(popup.wl_surface());
            }
        }
        if let Some(output) = &self.output {
            for panel in layer_map_for_output(output).layers() {
                let surface = panel.wl_surface();
                tree(surface);
                // As Smithay draws them with their panel.
                for (popup, _) in PopupManager::popups_for_surface(surface) {
                    tree(popup.wl_surface());
                }
            }
        }
    }

    /// Takes from the surfaces a frame shows (`shown`, the damage tracker's
    /// states, which name each element in view, changed or not) what is
    /// told of them: their "frame done" callbacks, answered once the frame
    /// is drawn, and their presentation feedback, told when it is on the
    /// screen. Only those of commits the frame draws: one that arrives
    /// after drawing began keeps its own for the next frame, as Weston
    /// takes them at repaint and answers them right after it
    /// (`weston_output_repaint`; anvil's `post_repaint` too) – told at the
    /// vblank before (until 0.5.104), its client drew the next frame at
    /// once and the one not shown was discarded; told at the frame's own
    /// vblank (until 0.5.107), a client had only what was left of the
    /// refresh before the next drawing, ~1 ms at 240 Hz (hardware test
    /// 26.10.: 9 of the load test's 12 windows at 165 frames a second,
    /// not 240). The surfaces not
    /// shown keep theirs until their next commit discards the feedback, as
    /// the protocol asks for content never seen. Their fifo barriers too
    /// (`timing`): the next commit applies once this one showed. Those of
    /// the surfaces not shown are returned, to be lifted now: wp_fifo lets
    /// a compositor ignore the barrier of a surface it does not update, its
    /// client throttles itself (Mesa by commit times); held, Mesa's FIFO
    /// blocked a covered app in its present, events and all (25.10.).
    pub(super) fn take_shown(
        &self,
        shown: &RenderElementStates,
    ) -> (Shown, Vec<(Barrier, Option<Client>)>) {
        let mut callbacks = Vec::new();
        let mut fifo = Vec::new();
        let mut hidden = Vec::new();
        self.each_surface(|surface, states| {
            if shown.element_was_presented(surface) {
                let mut attributes = states.cached_state.get::<SurfaceAttributes>();
                callbacks.append(&mut attributes.current().frame_callbacks);
                fifo.extend(take_fifo(surface, states));
            } else {
                hidden.extend(take_fifo(surface, states));
            }
        });
        let feedback = self.output.as_ref().map(|output| {
            let mut feedback = OutputPresentationFeedback::new(output);
            let on = |surface: &WlSurface, _: &SurfaceData| {
                shown.element_was_presented(surface).then(|| output.clone())
            };
            let flags = |surface: &WlSurface, _: &SurfaceData| {
                surface_presentation_feedback_flags_from_states(surface, shown)
            };
            for window in self.space.elements() {
                window.take_presentation_feedback(&mut feedback, on, flags);
            }
            for panel in layer_map_for_output(output).layers() {
                panel.take_presentation_feedback(&mut feedback, on, flags);
            }
            feedback
        });
        let shown = Shown {
            feedback,
            callbacks,
            fifo,
        };
        (shown, hidden)
    }

    /// Keeps the buffers synchronised explicitly that the frame just
    /// drawn read – of the surfaces it shows (`shown`), or of all – until
    /// the GPU finished it (`release_read`).
    pub(super) fn hold_read(&mut self, shown: Option<&RenderElementStates>) {
        let Some(gpu) = &self.gpu else {
            return;
        };
        let renderer = gpu.renderer_ref();
        let mut held = Vec::new();
        self.each_surface(|surface, states| {
            if shown.is_none_or(|shown| shown.element_was_presented(surface)) {
                held.extend(syncobj::held(renderer, states));
            }
        });
        let submission = renderer.submitted();
        self.holds.add(submission, held);
    }

    /// Lets go of the buffers synchronised explicitly that finished frames
    /// read (`syncobj`): at each vblank – KMS waited for the frame's fence,
    /// which signals after the submission's timeline –, and while no frame
    /// waits for its vblank when clients hear "frame done" without one (no
    /// screen, nothing drawn). So a buffer is let go no earlier than the
    /// vblank of the last frame that showed it, as a buffer on a plane of
    /// its own will need (direct scanout). With some left and no vblank to
    /// come, it looks again a refresh later.
    pub(super) fn release_read(&mut self) {
        if self.presenting.is_some() {
            return;
        }
        if let Some(gpu) = &mut self.gpu {
            let completed = gpu.renderer().completed();
            self.holds.done(completed);
        }
        if !self.holds.is_empty() {
            self.frame_done_later();
        }
    }

    /// Tells clients their content showed: that of the frame KMS just
    /// showed, and of commits that changed nothing since, at the vblank
    /// `time` with its `sequence`, timed by the hardware (as Smithay's
    /// DrmCompositor reports it). Without a vblank time, none is exact.
    pub(super) fn presented(&mut self, time: Option<Duration>, sequence: u32) {
        let kind = wp_presentation_feedback::Kind::Vsync
            | wp_presentation_feedback::Kind::HwClock
            | wp_presentation_feedback::Kind::HwCompletion;
        let (at, kind) = match time {
            Some(time) => (time, kind),
            None => (
                self.clock.now().into(),
                wp_presentation_feedback::Kind::empty(),
            ),
        };
        let mut shown: Vec<_> = self.presenting.take().into_iter().collect();
        shown.append(&mut self.unchanged);
        self.present(shown, at, u64::from(sequence), kind);
    }

    /// Commits that changed nothing on the screen, when no vblank came
    /// since: shown now, a refresh after them, without the hardware's
    /// flags (as niri reports them).
    pub(super) fn presented_unchanged(&mut self) {
        let shown = std::mem::take(&mut self.unchanged);
        let now = self.clock.now().into();
        self.present(shown, now, 0, wp_presentation_feedback::Kind::empty());
    }

    pub(super) fn present(
        &mut self,
        shown: Vec<Shown>,
        at: Duration,
        sequence: u64,
        kind: wp_presentation_feedback::Kind,
    ) {
        let refresh = self
            .gpu
            .as_ref()
            .and_then(Gpu::refresh)
            .map_or(Refresh::Unknown, Refresh::fixed);
        let time = smithay::utils::Time::<Monotonic>::from(at);
        // In milliseconds, as wl_callback.done carries it.
        let msec = at.as_millis() as u32;
        let mut released = Vec::new();
        for shown in shown {
            if let Some(mut feedback) = shown.feedback {
                feedback.presented(time, refresh, sequence, kind);
            }
            for callback in shown.callbacks {
                callback.done(msec);
            }
            released.extend(lift(shown.fifo));
        }
        self.unblock(released);
    }

    /// A frame that will not show (its screen went, or a new frame replaced
    /// it while the session was away): its content counts as discarded, and
    /// its clients hear "frame done" now, so none waits for it.
    pub(super) fn never_shown(&mut self, shown: Shown) {
        if let Some(mut feedback) = shown.feedback {
            feedback.discarded();
        }
        let now: Duration = self.clock.now().into();
        for callback in shown.callbacks {
            callback.done(now.as_millis() as u32);
        }
        let released = lift(shown.fifo);
        self.unblock(released);
    }

    /// Clients whose held-back commits may apply now, a barrier lifted
    /// (`timing`): Smithay applies them, each through `commit`.
    pub fn unblock(&mut self, released: Vec<Client>) {
        let mut clients: Vec<Client> = Vec::new();
        for client in released {
            if !clients.iter().any(|c| c.id() == client.id()) {
                clients.push(client);
            }
        }
        let display = self.display.clone();
        for client in clients {
            if let Some(data) = client.get_data::<ClientState>() {
                data.compositor.blocker_cleared(self, &display);
            }
        }
    }

    /// The first vblank at or after `time`; without a screen, `time`.
    pub fn vblank_from(&self, time: Duration) -> Duration {
        self.gpu
            .as_ref()
            .and_then(|gpu| gpu.vblank_from(time))
            .unwrap_or(time)
    }

    /// "Frame done" for the surfaces the last frame did not show – off the
    /// screen, covered, minimized, or all without a screen – at most every
    /// HIDDEN_FRAMES, at `time` (the vblank) or now; and for the pointer's.
    /// Those shown hear it once their frame is drawn (`render`). Their fifo
    /// barriers are lifted by each draw (`take_shown`), which knows what it
    /// shows; here only without a screen. Lifted here against the last
    /// frame, a window's first commit counted as hidden – not in that frame
    /// yet – and its next replaced it unseen (25.10.).
    pub(super) fn frame_done(&mut self, time: Option<Duration>) {
        let Some(output) = self.output.clone() else {
            return;
        };
        let time = time.unwrap_or_else(|| self.clock.now().into());
        let shows = self.gpu.as_ref().is_some_and(Gpu::shows);
        let callbacks = self
            .hidden_done
            .is_none_or(|last| time.saturating_sub(last) >= HIDDEN_FRAMES);
        if callbacks {
            self.hidden_done = Some(time);
        }
        let drawn = shows.then_some(&self.drawn);
        let msec = time.as_millis() as u32;
        let mut fifo = Vec::new();
        let mut withheld = false;
        self.each_surface(|surface, states| {
            if drawn.is_some_and(|d| d.element_was_presented(surface)) {
                return;
            }
            let mut attributes = states.cached_state.get::<SurfaceAttributes>();
            let waiting = &mut attributes.current().frame_callbacks;
            if callbacks {
                for callback in waiting.drain(..) {
                    callback.done(msec);
                }
            } else if !waiting.is_empty() {
                withheld = true;
            }
            if !shows {
                fifo.extend(take_fifo(surface, states));
            }
        });
        self.unblock(lift(fifo));
        // A hidden window waits for its second: someone must wake then. Only a
        // frame of a window still shown did (0.5.127: with all of them covered,
        // 3 s in the VM), as Mutter throttles hidden windows with a timer.
        if withheld {
            let at = self
                .hidden_done
                .map_or(time, |last| last + HIDDEN_FRAMES)
                .saturating_sub(time);
            self.hidden_tick(at);
        }
        let shown = shows.then(|| output.clone());
        if let Some(surface) = self.cursor.surface() {
            send_frames_surface_tree(surface, &output, time, Some(HIDDEN_FRAMES), |_, _| {
                shown.clone()
            });
        }
        self.space.refresh();
        self.popups.cleanup();
        // The compositor's own animation (the fade) asks for its next frame
        // here, as a client does on "frame done": the change dates from
        // this vblank and is drawn at the next one's deadline. Asked for
        // while drawing (until 0.5.102), its frame was dated before the
        // last one was shown, could only be drawn at that one's vblank, at
        // once – every frame of the fade late (hardware test 24.10.).
        // Without a screen nothing shows the fade, and nothing would end it:
        // it ends at once (ADR 0032 §3) instead of asking for frames each
        // refresh.
        if let Some(gpu) = self.gpu.as_mut().filter(|g| !g.shows()) {
            self.opening.finish();
            self.animations.finish();
            gpu.renderer().drop_canvases();
        }
        if matches!(self.opening, Opening::Fading(_)) || self.animations.moving() {
            self.changed();
        }
        // The fade to black (`idle`) asks for its next step once one is
        // shown; once all black is on the screen, it goes off.
        let now = self.now();
        if self.idle.faded(now) {
            self.screen_off();
        } else {
            self.fade_step();
        }
    }

    /// Saves what the screen shows (`screenshot`).
    pub fn screenshot(&mut self) {
        let Some(gpu) = &mut self.gpu else {
            return;
        };
        match gpu.screenshot() {
            Ok((size, rgb)) => crate::screenshot::save(size, rgb),
            Err(e) => warn!("screenshot: {e}"),
        }
    }
}
