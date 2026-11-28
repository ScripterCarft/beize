//! The session and its screens: activating and pausing (VT switch),
//! the first screen, rescans, the shell becoming ready or ending, and the
//! globals offered per screen and GPU.

use super::*;

impl State {
    /// The session became active: the screen, taken the first time.
    pub fn activate(&mut self) -> Result<()> {
        if self.libinput.resume().is_err() {
            warn!("libinput could not take the input devices");
        }
        match &mut self.gpu {
            Some(gpu) => {
                gpu.resume()?;
                // The screens may have changed meanwhile.
                self.rescan();
            }
            None => self.start()?,
        }
        // Coming back to the session is use: a screen gone off comes on.
        self.input_came();
        self.redraw = true;
        self.schedule_render();
        Ok(())
    }

    /// Another session took the screen and the input devices.
    pub fn pause(&mut self) {
        self.switcher_end();
        self.libinput.suspend();
        if let Some(gpu) = &mut self.gpu {
            gpu.pause();
        }
    }

    /// Takes the GPU and the screen, and offers the screen to clients.
    pub(super) fn start(&mut self) -> Result<()> {
        let (gpu, info, events) = gpu::open(&mut self.session)?;
        self.handle
            .insert_source(events, |event, metadata, state| match event {
                DrmEvent::VBlank(crtc) => {
                    state.flip_came(crtc);
                    let sequence = metadata.as_ref().map_or(0, |m| m.sequence);
                    let time = state
                        .gpu
                        .as_mut()
                        .and_then(|gpu| gpu.vblank(crtc, metadata.take()));
                    state.presented(time, sequence);
                    // `presented` took the frame that showed.
                    state.release_read();
                    state.frame_done(time);
                    if state.redraw {
                        state.schedule_render();
                    }
                    state.watch_pacing();
                }
                DrmEvent::Error(e) => error!("DRM: {e}"),
            })
            .map_err(|e| error(format!("DRM events: {}", e.error)))?;
        // Screens that come and go, and links that break, are changes of
        // the GPU's udev device (ADR 0029 §2).
        let device = gpu.device_id();
        let udev = UdevBackend::new(self.session.seat()).context("udev")?;
        self.handle
            .insert_source(udev, move |event, _, state| match event {
                UdevEvent::Changed { device_id } if device_id == device => state.rescan(),
                UdevEvent::Removed { device_id } if device_id == device => {
                    warn!("the GPU is gone");
                }
                _ => {}
            })
            .map_err(|e| error(format!("udev events: {}", e.error)))?;
        let node = gpu.node();
        self.gpu = Some(gpu);
        self.offer_dmabuf(node);
        self.offer_syncobj();
        match info {
            Some(info) => self.first_screen(&info),
            None => {
                info!("no screen connected: the desktop starts once one is");
                Ok(())
            }
        }
    }

    /// The first screen: offered to clients, the desktop's start begins
    /// (`opening`) and the compositor tells systemd it is ready – only now,
    /// so that the shell and apps start when there is a screen to show
    /// them on; without one, nothing is drawn or started, as there is
    /// nothing to see.
    pub(super) fn first_screen(&mut self, info: &Info) -> Result<()> {
        self.offer_output(info);
        // The desktop shows once the shell is ready, at most SHELL_WAIT
        // from now (`opening`). Black at once sets the mode: the monitor
        // locks on to it while the shell starts and the next frames draw
        // the background behind the black.
        let now: Duration = self.clock.now().into();
        self.screen_at = Some(now);
        if self.opening == (Opening::Waiting { shell: true }) {
            self.show_desktop(now, true);
        } else {
            let token = self
                .handle
                .insert_source(Timer::from_duration(SHELL_WAIT), |_, _, state| {
                    state.wait_timer = None;
                    if matches!(state.opening, Opening::Waiting { .. }) {
                        warn!(
                            "desktop shown without the shell: none was ready within {SHELL_WAIT:?}"
                        );
                        let now = state.clock.now().into();
                        state.show_desktop(now, false);
                    }
                    TimeoutAction::Drop
                })
                .map_err(|e| error(format!("waiting for the shell: {}", e.error)))?;
            self.wait_timer = Some(token);
        }
        let black = scene::veil_over(&self.veil_id, self.veil_commit, self.screen_area(), 1.0);
        if let Some(gpu) = &mut self.gpu {
            gpu.render(&[black], None, now, now, Parts::default())?;
        }
        self.watch_flip();
        crate::notify_ready();
        Ok(())
    }

    /// The GPU changed (udev): its screens are looked at anew (ADR 0029).
    /// A screen that went leaves the windows as they are and clients hear
    /// "frame done" as hidden ones do; one that came is drawn in full.
    pub(super) fn rescan(&mut self) {
        if !self.session.is_active() {
            // `activate` looks.
            return;
        }
        let Some(gpu) = &mut self.gpu else {
            return;
        };
        match gpu.scan(true) {
            Ok(Change::None) => {}
            Ok(Change::Gone) => {
                info!("no screen: windows keep their places until one comes");
                // Its last frame will not show.
                if let Some(shown) = self.presenting.take() {
                    self.never_shown(shown);
                }
                self.release_read();
                self.frame_done_later();
            }
            Ok(Change::Retrained) => self.changed(),
            Ok(Change::New(info)) if self.output.is_none() => {
                if let Err(e) = self.first_screen(&info) {
                    self.fail(e);
                }
            }
            Ok(Change::New(info)) => {
                self.offer_output(&info);
                // Without a screen left the pointer stays where it was.
                if let Some(pointer) =
                    crate::input::on_screen(self.screen_area().to_f64(), self.pointer)
                {
                    self.pointer = pointer;
                }
                self.arrange_windows();
                self.changed();
            }
            Err(e) => warn!("looking at the screens: {e}"),
        }
    }

    /// The shell's panels are drawn (`beize_shell_v1.ready`): the desktop
    /// shows, or a new shell's panels replace those kept of the old.
    pub fn shell_ready(&mut self) {
        let now: Duration = self.clock.now().into();
        if let Some(kept) = self.kept.take() {
            self.handle.remove(kept.timer);
            info!(
                "a new shell is ready {} ms after the old one failed: its panels replace the kept ones",
                now.saturating_sub(kept.since).as_millis()
            );
            self.arrange_windows();
            self.changed();
        }
        if matches!(self.opening, Opening::Waiting { .. }) {
            if self.screen_at.is_some() {
                self.show_desktop(now, true);
            } else {
                self.opening = Opening::Waiting { shell: true };
            }
        }
    }

    /// The desktop fades in from black, with the shell's panels or – it
    /// failed or was not ready in time – without them.
    pub(super) fn show_desktop(&mut self, now: Duration, with_shell: bool) {
        if let Some(timer) = self.wait_timer.take() {
            self.handle.remove(timer);
        }
        if with_shell {
            let after = now.saturating_sub(self.screen_at.unwrap_or(now));
            info!(
                "desktop shown with the shell, {} ms after the screen",
                after.as_millis()
            );
        }
        self.opening = Opening::fading(now);
        self.changed();
    }

    /// The shell's connection ended, after it was `ready` or before. A
    /// shell that ends before it is ready failed to start: the desktop
    /// shows without it, and the panels kept of the shell before go – a
    /// new one is not coming to replace them. Waiting out SHELL_WAIT is
    /// left to a shell that hangs.
    pub(super) fn shell_ended(&mut self, ready: bool) {
        if ready {
            return;
        }
        if let Some(kept) = self.kept.take() {
            self.handle.remove(kept.timer);
            warn!("a new shell ended before it was ready: the kept panels go");
            self.arrange_windows();
            self.changed();
        }
        if matches!(self.opening, Opening::Waiting { .. }) && self.screen_at.is_some() {
            warn!("desktop shown without the shell: it ended before it was ready");
            let now = self.clock.now().into();
            self.show_desktop(now, false);
        }
    }

    /// The shell failed (its connection is gone): its panels stay as the
    /// last frame drew them, with the room they took from the windows,
    /// until a new shell is ready – at most SHELL_WAIT –, as macOS keeps
    /// the desktop while its Dock starts again. Kept, they no longer
    /// change: their glass stays as it was.
    pub fn keep_panels(&mut self) {
        if self.kept.is_some() || self.panels_last.is_empty() {
            return;
        }
        let area = self.window_area();
        let mut panels = std::mem::take(&mut self.panels_last);
        // A dialog or the switcher goes with the shell that asked for it.
        panels.retain(|panel| !panel.overlay);
        for panel in &mut panels {
            panel
                .elements
                .retain(|e| !matches!(e, Element::Glass(glass) if !glass.keeps()));
            for element in &mut panel.elements {
                if let Element::Glass(glass) = element {
                    glass.freeze();
                }
            }
        }
        let timer = self
            .handle
            .insert_source(Timer::from_duration(SHELL_WAIT), |_, _, state| {
                if state.kept.take().is_some() {
                    warn!("no new shell was ready within {SHELL_WAIT:?}: its panels go");
                    state.arrange_windows();
                    state.changed();
                }
                TimeoutAction::Drop
            });
        let timer = match timer {
            Ok(timer) => timer,
            Err(e) => {
                warn!("kept panels: {}", e.error);
                return;
            }
        };
        info!(
            "the shell failed: its {} panels stay until a new one is ready",
            panels.len()
        );
        self.kept = Some(Kept {
            panels,
            area,
            since: self.clock.now().into(),
            timer,
        });
    }

    /// The screen for clients: the one they have in its new mode, or – a
    /// different connector – a new wl_output that takes over the panels.
    pub(super) fn offer_output(&mut self, info: &Info) {
        let (w, h) = info.mode.size();
        let mode = OutputMode {
            size: (i32::from(w), i32::from(h)).into(),
            refresh: i32::try_from(gpu::millihertz(&info.mode)).unwrap_or(i32::MAX),
        };
        if let Some(output) = &self.output
            && output.name() == info.name
        {
            let before = output.current_mode();
            output.change_current_state(Some(mode), None, None, None);
            output.set_preferred(mode);
            if let Some(before) = before.filter(|b| *b != mode) {
                output.delete_mode(before);
            }
            // The panels hear the new size.
            layer_map_for_output(output).arrange();
            return;
        }
        let output = WlOutput::new(
            info.name.clone(),
            PhysicalProperties {
                size: info.size_mm.into(),
                subpixel: Subpixel::Unknown,
                make: "Unknown".into(),
                model: info.name.clone(),
            },
        );
        let global = output.create_global::<State>(&self.display);
        output.change_current_state(
            Some(mode),
            Some(Transform::Normal),
            None,
            Some((0, 0).into()),
        );
        output.set_preferred(mode);
        self.space.map_output(&output, (0, 0));
        let before = self.output.replace(output.clone());
        let before_global = self.output_global.replace(global);
        let (Some(before), Some(before_global)) = (before, before_global) else {
            return;
        };
        info!("{} replaces {} for clients", info.name, before.name());
        // One screen: the panels go where they went, whichever they named.
        let panels: Vec<LayerSurface> = layer_map_for_output(&before).layers().cloned().collect();
        for panel in panels {
            layer_map_for_output(&before).unmap_layer(&panel);
            if let Err(e) = layer_map_for_output(&output).map_layer(&panel) {
                warn!("a panel: {e}");
            }
        }
        layer_map_for_output(&output).arrange();
        self.space.unmap_output(&before);
        self.display.disable_global::<State>(before_global.clone());
        let removed =
            self.handle
                .insert_source(Timer::from_duration(GLOBAL_GRACE), move |_, _, state| {
                    state.display.remove_global::<State>(before_global.clone());
                    TimeoutAction::Drop
                });
        if let Err(e) = removed {
            warn!("the replaced screen's global stays: {}", e.error);
        }
    }

    /// linux-dmabuf with the formats the renderer imports, the GPU as the
    /// main device (feedback, version 4).
    pub(super) fn offer_dmabuf(&mut self, node: DrmNode) {
        let Some(gpu) = &mut self.gpu else {
            return;
        };
        let formats = gpu.renderer().dmabuf_formats();
        match DmabufFeedbackBuilder::new(node.dev_id(), formats).build() {
            Ok(feedback) => {
                self.dmabuf
                    .create_global_with_default_feedback::<State>(&self.display, &feedback);
            }
            Err(e) => warn!("no linux-dmabuf for clients: {e}"),
        }
    }

    /// linux-drm-syncobj, with the GPU importing the clients' timelines (as
    /// anvil): only if its driver signals an eventfd from a syncobj point,
    /// or no commit could wait for its acquire point.
    pub(super) fn offer_syncobj(&mut self) {
        let Some(gpu) = &self.gpu else {
            return;
        };
        let device = gpu.device_fd().clone();
        if supports_syncobj_eventfd(&device) {
            self.syncobj = Some(DrmSyncobjState::new::<State>(&self.display, device));
            info!("explicit sync for clients (linux-drm-syncobj)");
        } else {
            info!("no explicit sync: the GPU's driver has no syncobj eventfd");
        }
    }

    /// localed changed the layout.
    /// Only a layout other than the last: localed's one write is two
    /// events (its file linked over the old one, then closed), and it too
    /// writes only what differs.
    pub fn reload_layout(&mut self) {
        let layout = Layout::current();
        if layout == self.layout {
            return;
        }
        let keyboard = self.keyboard.clone();
        match keyboard.set_xkb_config(self, layout.xkb()) {
            Ok(()) => info!("keyboard layout {layout}"),
            Err(e) => warn!("keyboard layout {layout}: {e:?}; keeping the previous one"),
        }
        self.layout = layout;
    }
}
