//! One screen: its mode, its images and Beize's own frame loop (ADR 0027
//! §6) – atomic KMS through Smithay's `DrmSurface`, without GBM. Images
//! come from a swapchain (Vulkan's allocator on a GPU with a Vulkan
//! driver, dumb buffers with lavapipe) and reach KMS as framebuffers.
//!
//! A frame is drawn only when something changed (Smithay's damage tracker,
//! with each image's age), at most one waits for its vblank, and KMS shows
//! it once the GPU's fence signals (IN_FENCE_FD): the CPU never waits for
//! the GPU. With nothing changing, nothing is drawn or committed – the
//! display engine keeps scanning out the last image, as on a Mac.

use std::cell::RefCell;
use std::os::fd::AsFd;
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
use smithay::backend::renderer::Bind;
use smithay::backend::renderer::damage::OutputDamageTracker;
use smithay::backend::renderer::element::Kind;
use smithay::backend::renderer::element::solid::{SolidColorBuffer, SolidColorRenderElement};
use smithay::reexports::drm::buffer::{
    Buffer as _, DrmFourcc, DrmModifier, Handle as BufferHandle, PlanarBuffer,
};
use smithay::reexports::drm::control::{Device as _, FbCmd2Flags, connector, framebuffer};
use smithay::utils::{Rectangle, Transform};
use tracing::{debug, info, warn};

use beize_compositor::error::{Context, Result, error};
use beize_compositor::render::{Target, VkRenderer};

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
    type Error = beize_compositor::error::Error;

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
    type Error = beize_compositor::error::Error;

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

/// What a swapchain image keeps: its framebuffer and the renderer's target.
struct Image<F> {
    framebuffer: F,
    target: RefCell<Target>,
}

/// Beize's background until the shell draws one.
const BACKGROUND: [f32; 4] = [0.08, 0.10, 0.14, 1.0];

pub struct Output<A: Allocator, F: ExportFramebuffer<A::Buffer>> {
    drm: DrmDevice,
    surface: DrmSurface,
    swapchain: Swapchain<A>,
    exporter: F,
    renderer: VkRenderer,
    damage: OutputDamageTracker,
    background: SolidColorBuffer,
    /// lavapipe may draw straight into the screen's buffers.
    direct: bool,
    /// Submitted, waiting for its vblank.
    pending: Option<Slot<A::Buffer>>,
    /// On the screen.
    shown: Option<Slot<A::Buffer>>,
    /// Something changed while a frame waited for its vblank.
    again: bool,
    modeset: bool,
    frames: u64,
    last: Option<Duration>,
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
        renderer: VkRenderer,
        direct: bool,
    ) -> Output<A, F> {
        let mode = surface.pending_mode();
        let (w, h) = mode.size();
        let size = (i32::from(w), i32::from(h));
        Output {
            drm,
            surface,
            swapchain: Swapchain::new(allocator, w.into(), h.into(), Fourcc::Xrgb8888, modifiers),
            exporter,
            renderer,
            damage: OutputDamageTracker::new(size, 1.0, Transform::Normal),
            background: SolidColorBuffer::new(size, BACKGROUND),
            direct,
            pending: None,
            shown: None,
            again: false,
            modeset: true,
            frames: 0,
            last: None,
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
            self.renderer.import_target(&dmabuf)
        } else {
            self.renderer.shadow_target(&dmabuf)
        };
        let target = match target {
            Ok(target) => target,
            // lavapipe without format modifiers: only a matching row pitch.
            Err(e) if self.direct => {
                warn!("drawing into the screen buffer directly: {e}; copying instead");
                self.renderer.shadow_target(&dmabuf)?
            }
            Err(e) => return Err(e),
        };
        Ok(Image {
            framebuffer,
            target: RefCell::new(target),
        })
    }

    /// Draws what changed and hands it to KMS; nothing if nothing changed.
    pub fn render(&mut self) -> Result<()> {
        if self.pending.is_some() {
            // One frame at a time waits for its vblank; the next follows it.
            self.again = true;
            return Ok(());
        }
        let slot = self
            .swapchain
            .acquire()
            .context("screen buffer")?
            .context("no free screen buffer")?;
        if slot.userdata().get::<Image<F::Framebuffer>>().is_none() {
            let image = self.image(&slot)?;
            if self.frames == 0 {
                let way = if image.target.borrow().copies() {
                    "into its own image, copied to the screen buffer"
                } else {
                    "straight into the screen buffer"
                };
                info!("rendering with {} {way}", self.renderer.name());
            }
            slot.userdata().insert_if_missing(|| image);
        }
        let image = slot
            .userdata()
            .get::<Image<F::Framebuffer>>()
            .context("screen buffer data")?;
        let elements = [SolidColorRenderElement::from_buffer(
            &self.background,
            (0, 0),
            1.0,
            1.0,
            Kind::Unspecified,
        )];
        let (damaged, fence) = {
            let mut target = image.target.borrow_mut();
            let mut framebuffer = self.renderer.bind(&mut *target)?;
            let result = self
                .damage
                .render_output(
                    &mut self.renderer,
                    &mut framebuffer,
                    slot.age().into(),
                    &elements,
                    BACKGROUND,
                )
                .map_err(|e| error(format!("rendering: {e:?}")))?;
            (result.damage.is_some(), result.sync.export())
        };
        if !damaged {
            // Nothing changed: no frame, no commit; the slot goes back.
            return Ok(());
        }

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
                // KMS shows the frame once the GPU is done with it.
                fence: fence.as_ref().map(|f| f.as_fd()),
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
        let time = match metadata.map(|m| m.time) {
            Some(DrmEventTime::Monotonic(time)) => Some(time),
            _ => None,
        };
        let since = time.zip(self.last).map(|(t, l)| t.saturating_sub(l));
        self.last = time.or(self.last);
        match self.frames {
            1 => info!("first frame on the screen; idle until something changes"),
            n => debug!(
                "frame {n}: {since:?} after the last, GPU {:?} ms",
                self.renderer.gpu_ms()
            ),
        }
        if std::mem::take(&mut self.again) {
            self.render()
        } else {
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
        self.again = false;
        self.modeset = true;
        self.last = None;
        // Whatever the other session left is drawn over in full.
        let (w, h) = self.surface.pending_mode().size();
        self.damage =
            OutputDamageTracker::new((i32::from(w), i32::from(h)), 1.0, Transform::Normal);
        self.render()
    }
}
