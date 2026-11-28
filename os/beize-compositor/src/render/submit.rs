//! Recording and submitting: a frame's commands (uploads, passes, the
//! frame, pictures), its fence for KMS and the dmabufs it touched, and
//! freeing what the GPU is done with once a frame is committed.

use super::*;

impl VkRenderer {
    pub(super) fn in_flight_new(&self) -> Result<InFlight> {
        let d = &self.shared.device;
        let mut export = vk::ExportFenceCreateInfo::default()
            .handle_types(vk::ExternalFenceHandleTypeFlags::SYNC_FD);
        // SAFETY: plain creation; destroyed in Drop.
        unsafe {
            let commands = d
                .allocate_command_buffers(
                    &vk::CommandBufferAllocateInfo::default()
                        .command_pool(self.pool)
                        .command_buffer_count(1),
                )
                .context("command buffer")?[0];
            let fence = d
                .create_fence(&vk::FenceCreateInfo::default().push_next(&mut export), None)
                .context("fence")?;
            let queries = match self.caps.timestamp_period {
                Some(_) => Some(
                    d.create_query_pool(
                        &vk::QueryPoolCreateInfo::default()
                            .query_type(vk::QueryType::TIMESTAMP)
                            .query_count(2 + 2 * BLURS_TIMED as u32),
                        None,
                    )
                    .context("query pool")?,
                ),
                None => None,
            };
            Ok(InFlight {
                commands,
                fence,
                quads: None,
                staging: None,
                staged: 0,
                queries,
                blurs: 0,
                submission: 0,
                ready: true,
            })
        }
    }

    /// The frame being recorded, ready: its last submission is done (as a
    /// rule long since – vblank paces frames).
    pub(super) fn current(&mut self) -> Result<usize> {
        let i = self.current;
        if !self.in_flight[i].ready {
            let submission = self.in_flight[i].submission;
            self.shared.wait(submission)?;
            self.in_flight[i].staged = 0;
            self.in_flight[i].ready = true;
        }
        Ok(i)
    }

    /// Destroys what the GPU is done with: the pictures, glass and clients'
    /// buffers that went away. Called once a frame is committed to KMS,
    /// never between a frame's begin and its commit – each memory freed
    /// waits in the kernel (`Shared::collect`). 0.5.132 freed them before
    /// recording, and the frames of closing menus and dialogs came late.
    /// wgpu frees in `poll` after `submit` in the same way.
    pub fn free_finished(&mut self) {
        let began = Instant::now();
        let count = self.shared.collect();
        let took = began.elapsed();
        if took >= Duration::from_millis(1) {
            info!(
                "freed {count} GPU objects in {:.2} ms",
                took.as_secs_f64() * 1000.0
            );
        }
    }

    /// Submits what is recorded: pending uploads, then a frame or a read
    /// back. Returns the submission's number.
    pub(super) fn submit(
        &mut self,
        frame: Option<&mut frame::Recorded<'_>>,
        readout: Option<&frame::Readout>,
    ) -> Result<u64> {
        let began = Instant::now();
        let i = self.current()?;
        let recording = Instant::now();
        self.spent.wait += recording - began;
        // Each submission lets go of the clients' dmabufs that are gone (a
        // window's old size, a closed app), as Smithay's GLES renderer does
        // each frame: Smithay asks for it only in its DrmCompositor, which
        // Beize does not use. Kept, every resize held the buffers of each
        // size until the GPU's memory ran out (0.5.103). That only queues
        // them: they are destroyed after the frame's commit (`free_finished`).
        self.dmabufs.retain(|weak, _| !weak.is_gone());
        let d = self.shared.device.clone();
        let commands = self.in_flight[i].commands;
        let submission = self.submitted + 1;
        // SAFETY: the command buffer is this frame's and free (`current`
        // waited for its last submission).
        unsafe {
            d.reset_command_buffer(commands, vk::CommandBufferResetFlags::empty())
                .context("resetting commands")?;
            d.begin_command_buffer(
                commands,
                &vk::CommandBufferBeginInfo::default()
                    .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
            )
            .context("recording")?;
            self.in_flight[i].blurs = 0;
            if let Some(pool) = self.in_flight[i].queries {
                d.cmd_reset_query_pool(commands, pool, 0, 2 + 2 * BLURS_TIMED as u32);
                d.cmd_write_timestamp2(commands, vk::PipelineStageFlags2::NONE, pool, 0);
            }
        }
        let uploads = std::mem::take(&mut self.uploads);
        if let Some(staging) = &self.in_flight[i].staging {
            frame::record_uploads(&d, commands, staging.buffer, &uploads, submission);
        }
        drop(uploads);
        let passes = std::mem::take(&mut self.passes);
        // The pictures go with a frame for the screen; a read back leaves
        // them for the next.
        let mut canvases = Vec::new();
        let mut frame = frame;
        if let Some(frame) = frame.as_deref_mut() {
            canvases = std::mem::take(&mut self.canvases);
            for canvas in &mut canvases {
                canvas.join(frame);
            }
        }
        // The passes', then the frame's, then those of its live glass, then
        // the pictures'.
        let mut lists: Vec<&[Quad]> = passes.iter().map(|p| p.quads.as_slice()).collect();
        if let Some(frame) = &frame {
            lists.push(&frame.quads);
            for insert in &frame.inserts {
                lists.extend(insert.passes.iter().map(|p| p.quads.as_slice()));
            }
        }
        lists.extend(canvases.iter().map(|c| c.quads.as_slice()));
        let addresses = self.write_quads(i, &lists)?;
        let drawing = Instant::now();
        self.spent.begin += drawing - recording;
        let pass_pipelines = [
            self.pipelines.get(&d, PICTURE_DRAWN)?,
            self.pipelines.get_backdrop(&d, PICTURE_DRAWN)?,
        ];
        if !passes.is_empty() {
            let pipelines = pass_pipelines;
            frame::record_passes(
                &d,
                commands,
                &self.pipelines,
                pipelines,
                &passes,
                &addresses,
                submission,
            );
        }
        let mut waits = Vec::new();
        let mut synced = Duration::ZERO;
        if let Some(frame) = frame {
            let pipelines = self.pipelines.get(&d, frame.target.format)?;
            let quads = addresses.get(passes.len()).copied().unwrap_or_default();
            let mut rest = addresses.iter().skip(passes.len() + 1).copied();
            let insert_addresses: Vec<Vec<u64>> = frame
                .inserts
                .iter()
                .map(|insert| rest.by_ref().take(insert.passes.len()).collect())
                .collect();
            let mut drawn = Vec::with_capacity(canvases.len());
            for (canvas, address) in canvases.into_iter().zip(rest) {
                let pipelines = self.pipelines.get(&d, canvas.format)?;
                self.spent.quads_drawn += canvas.quads.len();
                drawn.push((canvas, pipelines, address));
            }
            let queries = self.in_flight[i].queries;
            self.in_flight[i].blurs = queries.map_or(0, |_| frame.inserts.len().min(BLURS_TIMED));
            let draws = frame::record_frame(
                &d,
                commands,
                &self.pipelines,
                pipelines,
                pass_pipelines,
                quads,
                &insert_addresses,
                &drawn,
                frame,
                submission,
                queries.map(|pool| (pool, BLURS_TIMED)),
            );
            self.spent.quads_drawn += frame.quads.len();
            self.spent.draws += draws;
            let syncing = Instant::now();
            let pending;
            (waits, pending) = frame::implicit_waits(&d, &self.ext, frame, submission);
            self.spent.pending_writes += pending;
            synced = syncing.elapsed();
        }
        if let Some(readout) = readout {
            frame::record_readout(&d, commands, readout);
        }
        let ending = Instant::now();
        // SAFETY: as above.
        unsafe {
            if let Some(pool) = self.in_flight[i].queries {
                d.cmd_write_timestamp2(commands, vk::PipelineStageFlags2::ALL_COMMANDS, pool, 1);
            }
            d.end_command_buffer(commands).context("recording")?;
        }
        let fence = self.in_flight[i].fence;
        let wait_infos: Vec<vk::SemaphoreSubmitInfo> = waits
            .iter()
            .map(|&s| {
                vk::SemaphoreSubmitInfo::default()
                    .semaphore(s)
                    .stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)
            })
            .collect();
        let signal = [vk::SemaphoreSubmitInfo::default()
            .semaphore(self.shared.timeline)
            .value(submission)
            .stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)];
        let buffers = [vk::CommandBufferSubmitInfo::default().command_buffer(commands)];
        let info = vk::SubmitInfo2::default()
            .wait_semaphore_infos(&wait_infos)
            .command_buffer_infos(&buffers)
            .signal_semaphore_infos(&signal);
        let queueing = Instant::now();
        self.spent.record += (ending - drawing).saturating_sub(synced);
        self.spent.sync += synced;
        self.spent.end += queueing - ending;
        // SAFETY: the fence is not pending (its frame is done); every
        // object the commands use lives until this submission completes.
        unsafe {
            d.reset_fences(&[fence]).context("fence")?;
            d.queue_submit2(self.queue, &[info], fence)
                .context("submitting")?;
        }
        self.spent.queue += queueing.elapsed();
        for s in waits {
            self.shared.defer(submission, Garbage::Semaphore(s));
        }
        self.submitted = submission;
        self.in_flight[i].submission = submission;
        self.in_flight[i].ready = false;
        self.current = (i + 1) % IN_FLIGHT;
        Ok(submission)
    }

    /// Ends a frame: submits it and hands out its end.
    /// A picture drawn (`frame::Canvas`): recorded with the next frame for
    /// the screen; one drawn again before replaces it.
    pub(super) fn queue_canvas(&mut self, canvas: frame::Canvas) {
        self.canvases
            .retain(|c| !Arc::ptr_eq(&c.picture.0, &canvas.picture.0));
        self.canvases.push(canvas);
    }

    /// Lets go of the pictures not recorded yet: nothing will show them (no
    /// screen).
    pub fn drop_canvases(&mut self) {
        self.canvases.clear();
    }

    pub(super) fn finish_frame(&mut self, frame: &mut frame::Recorded<'_>) -> Result<SyncPoint> {
        let submission = self.submit(Some(frame), None)?;
        let target = &mut *frame.target;
        target.filled = true;
        target.last_used = submission;
        if let Some(readback) = &target.readback {
            // lavapipe's copy: the CPU copies, so it waits.
            let waiting = Instant::now();
            self.shared.wait(submission)?;
            readback.copy_out(target.size)?;
            self.spent.wait += waiting.elapsed();
            return Ok(SyncPoint::signaled());
        }
        let syncing = Instant::now();
        let point = self.export_frame(frame, submission);
        self.spent.sync += syncing.elapsed();
        point
    }

    /// The frame's fence as a sync_file, for KMS and into the dmabufs it
    /// touched.
    pub(super) fn export_frame(
        &mut self,
        frame: &mut frame::Recorded<'_>,
        submission: u64,
    ) -> Result<SyncPoint> {
        let target = &*frame.target;
        let i = (self.current + IN_FLIGHT - 1) % IN_FLIGHT;
        let fence = self.in_flight[i].fence;
        let info = vk::FenceGetFdInfoKHR::default()
            .fence(fence)
            .handle_type(vk::ExternalFenceHandleTypeFlags::SYNC_FD);
        // SAFETY: the fence has a pending signal (just submitted); export
        // resets it, which `submit` does anyway before its next use.
        let fd = match unsafe { self.ext.fence_fd.get_fence_fd(&info) } {
            Ok(fd) if fd >= 0 => {
                // SAFETY: a new file descriptor we own.
                unsafe { <std::os::fd::OwnedFd as std::os::fd::FromRawFd>::from_raw_fd(fd) }
            }
            // -1: already signalled.
            Ok(_) => return Ok(SyncPoint::signaled()),
            Err(e) => {
                warn!("exporting the frame's fence: {e}; waiting instead");
                self.shared.wait(submission)?;
                return Ok(SyncPoint::signaled());
            }
        };
        // Into the dmabufs the frame touched: readers and writers of them
        // elsewhere see it (implicit sync). Not into those synchronised
        // explicitly: their clients wait for the release point instead
        // (`syncobj`; the protocol leaves implicit sync to the compositor).
        for texture in frame.foreign.iter().filter(|t| !t.explicit()) {
            if let Some(dmabuf) = texture.0.dmabuf.as_ref().and_then(|w| w.upgrade())
                && let Some(handle) = dmabuf.handles().next()
                && let Err(e) = sync::attach(handle, fd.as_fd(), false)
            {
                debug!("adding the frame's read to a dmabuf: {e}");
            }
        }
        if let Some(dmabuf) = &target.dmabuf
            && let Some(handle) = dmabuf.handles().next()
            && let Err(e) = sync::attach(handle, fd.as_fd(), true)
        {
            debug!("adding the frame's write to the screen buffer: {e}");
        }
        Ok(SyncPoint::from(sync::SyncFile(fd)))
    }
}
