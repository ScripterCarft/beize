//! One screen: its mode, its images and Beize's own frame loop (ADR 0027
//! §6) – atomic KMS through Smithay's `DrmSurface`, without GBM. Images
//! come from a swapchain (Vulkan's allocator on a GPU with a Vulkan
//! driver, dumb buffers with lavapipe) and reach KMS as framebuffers; a
//! frame is drawn only after the previous one is on the screen (vblank).

use std::time::Duration;

use smithay::backend::allocator::dmabuf::{AsDmabuf, Dmabuf};
use smithay::backend::allocator::dumb::DumbBuffer;
use smithay::backend::allocator::vulkan::VulkanImage;
use smithay::backend::allocator::{Allocator, Buffer, Fourcc, Modifier, Slot, Swapchain};
use smithay::backend::drm::exporter::{ExportBuffer, ExportFramebuffer};
use smithay::backend::drm::{
    DrmDevice, DrmDeviceFd, DrmEventMetadata, DrmEventTime, DrmSurface, Framebuffer, PlaneConfig,
    PlaneState,
};
use smithay::reexports::drm::buffer::{
    Buffer as _, DrmFourcc, DrmModifier, Handle as BufferHandle, PlanarBuffer,
};
use smithay::reexports::drm::control::{Device as _, FbCmd2Flags, connector, framebuffer};
use smithay::utils::{Rectangle, Transform};
use tracing::{info, warn};

use crate::error::{Context, Result, error};
use crate::render::{self, Target, Vk};

/// A connected connector, its preferred mode and a CRTC that can drive it.
pub fn surface(drm: &mut DrmDevice) -> Result<DrmSurface> {
    let fd = drm.device_fd().clone();
    let resources = fd.resource_handles().context("DRM resources")?;
    for &handle in resources.connectors() {
        let connector = fd.get_connector(handle, false).context("DRM connector")?;
        if connector.state() != connector::State::Connected || connector.modes().is_empty() {
            continue;
        }
        let mode = connector
            .modes()
            .iter()
            .find(|m| {
                m.mode_type()
                    .contains(smithay::reexports::drm::control::ModeTypeFlags::PREFERRED)
            })
            .copied()
            .unwrap_or(connector.modes()[0]);
        let crtc = connector
            .encoders()
            .iter()
            .filter_map(|&e| fd.get_encoder(e).ok())
            .flat_map(|e| resources.filter_crtcs(e.possible_crtcs()))
            .next()
            .context("no CRTC for the connector")?;
        let (w, h) = mode.size();
        info!(
            "screen {:?}-{}: {w}x{h} at {} Hz",
            connector.interface(),
            connector.interface_id(),
            mode.vrefresh()
        );
        return drm
            .create_surface(crtc, mode, &[handle])
            .context("taking the screen");
    }
    Err(error("no screen connected"))
}

/// A framebuffer for a dmabuf from Vulkan's allocator: each plane imported
/// into KMS (PRIME), added with its modifier. Smithay has this only through
/// GBM (ADR 0027 §6).
#[derive(Debug)]
pub struct DmabufExporter;

#[derive(Debug)]
pub struct DmabufFramebuffer {
    fd: DrmDeviceFd,
    handle: framebuffer::Handle,
    format: smithay::backend::allocator::Format,
}

impl AsRef<framebuffer::Handle> for DmabufFramebuffer {
    fn as_ref(&self) -> &framebuffer::Handle {
        &self.handle
    }
}

impl Framebuffer for DmabufFramebuffer {
    fn format(&self) -> smithay::backend::allocator::Format {
        self.format
    }
}

impl Drop for DmabufFramebuffer {
    fn drop(&mut self) {
        if let Err(e) = self.fd.destroy_framebuffer(self.handle) {
            warn!("removing a framebuffer: {e}");
        }
    }
}

struct Planes {
    size: (u32, u32),
    format: DrmFourcc,
    modifier: Option<DrmModifier>,
    pitches: [u32; 4],
    handles: [Option<BufferHandle>; 4],
    offsets: [u32; 4],
}

impl PlanarBuffer for Planes {
    fn size(&self) -> (u32, u32) {
        self.size
    }
    fn format(&self) -> DrmFourcc {
        self.format
    }
    fn modifier(&self) -> Option<DrmModifier> {
        self.modifier
    }
    fn pitches(&self) -> [u32; 4] {
        self.pitches
    }
    fn handles(&self) -> [Option<BufferHandle>; 4] {
        self.handles
    }
    fn offsets(&self) -> [u32; 4] {
        self.offsets
    }
}

fn add_dmabuf(fd: &DrmDeviceFd, dmabuf: &Dmabuf) -> Result<DmabufFramebuffer> {
    let format = dmabuf.format();
    let mut planes = Planes {
        size: (dmabuf.width(), dmabuf.height()),
        // Opaque: the screen shows no alpha (as Smithay's use_opaque).
        format: match format.code {
            Fourcc::Argb8888 => Fourcc::Xrgb8888,
            code => code,
        },
        modifier: explicit(format.modifier),
        pitches: [0; 4],
        handles: [None; 4],
        offsets: [0; 4],
    };
    for (i, ((plane, stride), offset)) in dmabuf
        .handles()
        .zip(dmabuf.strides())
        .zip(dmabuf.offsets())
        .enumerate()
    {
        planes.handles[i] = Some(fd.prime_fd_to_buffer(plane).context("PRIME import")?);
        planes.pitches[i] = stride;
        planes.offsets[i] = offset;
    }
    let flags = if planes.modifier.is_some() {
        FbCmd2Flags::MODIFIERS
    } else {
        FbCmd2Flags::empty()
    };
    let added = fd.add_planar_framebuffer(&planes, flags);
    // The framebuffer holds the buffer now; our GEM handles go (once each).
    let mut closed = Vec::new();
    for handle in planes.handles.into_iter().flatten() {
        if !closed.contains(&handle) {
            if let Err(e) = fd.close_buffer(handle) {
                warn!("closing a GEM handle: {e}");
            }
            closed.push(handle);
        }
    }
    Ok(DmabufFramebuffer {
        fd: fd.clone(),
        handle: added.context("adding a framebuffer")?,
        format,
    })
}

/// A modifier for AddFB2, or none for linear: KMS takes "no modifier" as
/// linear, and drivers without modifier support (virtio-gpu) refuse even
/// an explicit LINEAR.
fn explicit(modifier: Modifier) -> Option<DrmModifier> {
    match modifier {
        Modifier::Linear | Modifier::Invalid => None,
        m => Some(m),
    }
}

/// Dumb buffers through AddFB2 with their own GEM handle, implicitly
/// linear. Smithay's exporter names LINEAR and, refused, falls back to the
/// legacy AddFB with a warning.
#[derive(Debug)]
pub struct DumbExporter;

impl ExportFramebuffer<DumbBuffer> for DumbExporter {
    type Framebuffer = DmabufFramebuffer;
    type Error = crate::error::Error;

    fn add_framebuffer(
        &self,
        drm: &DrmDeviceFd,
        buffer: ExportBuffer<'_, DumbBuffer>,
        _use_opaque: bool,
    ) -> Result<Option<DmabufFramebuffer>> {
        let ExportBuffer::Allocator(dumb) = buffer else {
            return Ok(None);
        };
        let handle = dumb.handle();
        let planes = Planes {
            size: handle.size(),
            format: Fourcc::Xrgb8888,
            modifier: None,
            pitches: [handle.pitch(), 0, 0, 0],
            handles: [Some(handle.handle()), None, None, None],
            offsets: [0; 4],
        };
        let framebuffer = drm
            .add_planar_framebuffer(&planes, FbCmd2Flags::empty())
            .context("adding a framebuffer")?;
        Ok(Some(DmabufFramebuffer {
            fd: drm.clone(),
            handle: framebuffer,
            format: dumb.format(),
        }))
    }

    fn can_add_framebuffer(&self, buffer: &ExportBuffer<'_, DumbBuffer>) -> bool {
        matches!(buffer, ExportBuffer::Allocator(_))
    }
}

impl ExportFramebuffer<VulkanImage> for DmabufExporter {
    type Framebuffer = DmabufFramebuffer;
    type Error = crate::error::Error;

    fn add_framebuffer(
        &self,
        drm: &DrmDeviceFd,
        buffer: ExportBuffer<'_, VulkanImage>,
        _use_opaque: bool,
    ) -> Result<Option<DmabufFramebuffer>> {
        match buffer {
            ExportBuffer::Allocator(image) => {
                let dmabuf = image.export().context("exporting a Vulkan image")?;
                add_dmabuf(drm, &dmabuf).map(Some)
            }
            ExportBuffer::Wayland(_) => Ok(None),
        }
    }

    fn can_add_framebuffer(&self, buffer: &ExportBuffer<'_, VulkanImage>) -> bool {
        matches!(buffer, ExportBuffer::Allocator(_))
    }
}

/// What a swapchain image keeps: its framebuffer and Vulkan's target.
struct Image<F> {
    framebuffer: F,
    target: Target,
}

/// Frames drawn for the start, to see the loop run and measure it.
const START_FRAMES: u32 = 120;

pub struct Output<A: Allocator, F: ExportFramebuffer<A::Buffer>> {
    drm: DrmDevice,
    surface: DrmSurface,
    swapchain: Swapchain<A>,
    exporter: F,
    vk: Vk,
    /// lavapipe may draw straight into the screen's buffers.
    direct: bool,
    /// Submitted, waiting for its vblank.
    pending: Option<Slot<A::Buffer>>,
    /// On the screen.
    shown: Option<Slot<A::Buffer>>,
    modeset: bool,
    frames: u32,
    missed: u32,
    last: Option<Duration>,
    period: Duration,
}

impl<A, F> Output<A, F>
where
    A: Allocator,
    A::Buffer: AsDmabuf + 'static,
    <A::Buffer as AsDmabuf>::Error: std::fmt::Display,
    A::Error: std::fmt::Display,
    F: ExportFramebuffer<A::Buffer>,
    F::Framebuffer: 'static,
    F::Error: std::fmt::Display,
{
    pub fn new(
        drm: DrmDevice,
        surface: DrmSurface,
        allocator: A,
        modifiers: Vec<Modifier>,
        exporter: F,
        vk: Vk,
        direct: bool,
    ) -> Output<A, F> {
        let mode = surface.pending_mode();
        let (w, h) = mode.size();
        let refresh = f64::from(mode.vrefresh().max(1));
        Output {
            drm,
            surface,
            swapchain: Swapchain::new(allocator, w.into(), h.into(), Fourcc::Xrgb8888, modifiers),
            exporter,
            vk,
            direct,
            pending: None,
            shown: None,
            modeset: true,
            frames: 0,
            missed: 0,
            last: None,
            period: Duration::from_secs_f64(1.0 / refresh),
        }
    }

    fn image(&self, slot: &Slot<A::Buffer>) -> Result<Image<F::Framebuffer>> {
        let framebuffer = self
            .exporter
            .add_framebuffer(self.drm.device_fd(), ExportBuffer::Allocator(slot), true)
            .context("framebuffer")?
            .context("the buffer cannot be shown")?;
        let dmabuf = slot.export().context("exporting a screen buffer")?;
        let target = if self.direct {
            self.vk.import(&dmabuf)
        } else {
            self.vk.shadow(&dmabuf)
        };
        let target = match target {
            Ok(target) => target,
            // lavapipe without format modifiers: only a matching row pitch.
            Err(e) if self.direct => {
                warn!("drawing into the screen buffer directly: {e}; copying instead");
                self.vk.shadow(&dmabuf)?
            }
            Err(e) => return Err(e),
        };
        Ok(Image {
            framebuffer,
            target,
        })
    }

    /// Draws the next frame and hands it to KMS.
    pub fn render(&mut self) -> Result<()> {
        let slot = self
            .swapchain
            .acquire()
            .context("screen buffer")?
            .context("no free screen buffer")?;
        if slot.userdata().get::<Image<F::Framebuffer>>().is_none() {
            let image = self.image(&slot)?;
            if self.frames == 0 {
                render::describe(&self.vk, &image.target);
            }
            slot.userdata().insert_if_missing(|| image);
        }
        let image = slot
            .userdata()
            .get::<Image<F::Framebuffer>>()
            .context("screen buffer data")?;
        // Beize's start colour, fading in: the loop at work until the
        // renderer draws windows.
        let t = (self.frames as f32 / START_FRAMES as f32).min(1.0);
        let color = [0.08 * t, 0.10 * t, 0.14 * t, 1.0];
        self.vk.clear(&image.target, color)?;

        let size = slot.size();
        let plane = PlaneState {
            handle: self.surface.plane(),
            config: Some(PlaneConfig {
                src: Rectangle::from_size(size).to_f64(),
                dst: Rectangle::from_size((size.w, size.h).into()),
                transform: Transform::Normal,
                alpha: 1.0,
                damage_clips: None,
                fb: *image.framebuffer.as_ref(),
                fence: None,
            }),
        };
        if self.modeset {
            self.surface
                .commit([plane], true)
                .context("setting the mode")?;
            self.modeset = false;
        } else {
            self.surface.page_flip([plane], true).context("page flip")?;
        }
        self.swapchain.submitted(&slot);
        self.pending = Some(slot);
        Ok(())
    }

    /// The submitted frame is on the screen.
    pub fn vblank(&mut self, metadata: Option<DrmEventMetadata>) -> Result<()> {
        self.shown = self.pending.take();
        self.frames += 1;
        if let Some(DrmEventTime::Monotonic(time)) = metadata.map(|m| m.time) {
            if let Some(last) = self.last
                && time.saturating_sub(last) > self.period.mul_f32(1.5)
            {
                self.missed += 1;
            }
            self.last = Some(time);
        }
        if self.frames < START_FRAMES {
            self.render()
        } else {
            info!(
                "{START_FRAMES} frames, {} missed; idle until something changes",
                self.missed
            );
            Ok(())
        }
    }

    /// Another session took the screen (logind).
    pub fn pause(&mut self) {
        self.drm.pause();
    }

    /// The screen is ours again: everything is set anew.
    pub fn resume(&mut self) -> Result<()> {
        self.drm.activate(false).context("taking the screen back")?;
        self.surface.reset_state().context("screen state")?;
        self.swapchain.reset_buffers();
        self.pending = None;
        self.shown = None;
        self.modeset = true;
        self.frames = 0;
        self.missed = 0;
        self.last = None;
        self.render()
    }
}
