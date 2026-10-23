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
//!
//! The pointer lies on the cursor plane: a move is a commit of that plane
//! alone, without drawing (`CursorPlane`).

use std::cell::RefCell;
use std::os::fd::{AsFd, OwnedFd};
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
use smithay::backend::renderer::element::RenderElement;
use smithay::reexports::drm::buffer::{
    Buffer as _, DrmFourcc, DrmModifier, Handle as BufferHandle, PlanarBuffer,
};
use smithay::reexports::drm::control::{
    Device as _, FbCmd2Flags, Mode, ModeFlags, ModeTypeFlags, connector, dumbbuffer, framebuffer,
    plane,
};
use smithay::reexports::drm::{Device as _, VblankWaitFlags, VblankWaitTarget};
use smithay::utils::{Clock, Monotonic, Physical, Point, Rectangle, Transform};
use tracing::{debug, info, warn};

use crate::cursor;
use crate::pacing::{Pacing, Times};
use crate::schedule::{Plan, Schedule};
use beize_compositor::error::{Context, Result, error};
use beize_compositor::render::{Target, VkRenderer, signalled_at};

/// What the screen is, for clients (`wl_output`).
#[derive(Debug, Clone)]
pub struct Info {
    /// The connector, as the kernel names it ("DP-1").
    pub name: String,
    /// Millimetres, if the screen says.
    pub size_mm: (i32, i32),
    pub mode: Mode,
}

/// What [`best_mode`] weighs of a mode.
struct Candidate {
    size: (u16, u16),
    millihertz: u32,
    preferred: bool,
    interlaced: bool,
}

/// The screen's mode: the size of the preferred one (the panel's own) at
/// the highest refresh rate it has, as KWin chooses. A monitor's preferred
/// mode is often not its fastest (a 240 Hz panel may prefer 60 Hz, or list
/// 240 Hz only in DisplayID). No interlaced modes; without a preferred
/// mode, the first one's size.
fn best_mode(modes: &[Candidate]) -> usize {
    let size = modes
        .iter()
        .find(|m| m.preferred)
        .or(modes.first())
        .map(|m| m.size);
    modes
        .iter()
        .enumerate()
        .filter(|(_, m)| Some(m.size) == size && !m.interlaced)
        // Equally fast: the preferred, then the first listed.
        .max_by_key(|(i, m)| (m.millihertz, m.preferred, std::cmp::Reverse(*i)))
        .map_or(0, |(i, _)| i)
}

/// A connected connector, its mode ([`best_mode`]) and a CRTC that can drive it.
pub fn surface(drm: &mut DrmDevice) -> Result<(DrmSurface, Info)> {
    let fd = drm.device_fd().clone();
    let resources = fd.resource_handles().context("DRM resources")?;
    for &handle in resources.connectors() {
        let connector = fd.get_connector(handle, false).context("DRM connector")?;
        if connector.state() != connector::State::Connected || connector.modes().is_empty() {
            continue;
        }
        let modes = connector.modes();
        let mode = modes[best_mode(
            &modes
                .iter()
                .map(|m| Candidate {
                    size: m.size(),
                    millihertz: crate::gpu::millihertz(m),
                    preferred: m.mode_type().contains(ModeTypeFlags::PREFERRED),
                    interlaced: m.flags().contains(ModeFlags::INTERLACE),
                })
                .collect::<Vec<_>>(),
        )];
        let crtc = connector
            .encoders()
            .iter()
            .filter_map(|&e| fd.get_encoder(e).ok())
            .flat_map(|e| resources.filter_crtcs(e.possible_crtcs()))
            .next()
            .context("no CRTC for the connector")?;
        let (w, h) = mode.size();
        let name = format!(
            "{}-{}",
            connector.interface().as_str(),
            connector.interface_id()
        );
        info!("screen {name}: {w}x{h} at {} Hz", mode.vrefresh());
        let size_mm = connector.size().unwrap_or_default();
        let info = Info {
            name,
            size_mm: (
                i32::try_from(size_mm.0).unwrap_or(0),
                i32::try_from(size_mm.1).unwrap_or(0),
            ),
            mode,
        };
        let surface = drm
            .create_surface(crtc, mode, &[handle])
            .context("taking the screen")?;
        let planes = surface.planes();
        let handles =
            |p: &[smithay::backend::drm::PlaneInfo]| p.iter().map(|p| p.handle).collect::<Vec<_>>();
        debug!(
            "planes of {crtc:?}: primary {:?}, cursor {:?}, overlay {:?}; all: {:?}",
            handles(&planes.primary),
            handles(&planes.cursor),
            handles(&planes.overlay),
            fd.plane_handles().ok()
        );
        return Ok((surface, info));
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

/// A dumb buffer as a framebuffer, through AddFB2 with its own GEM handle,
/// implicitly linear.
fn add_dumb(
    fd: &DrmDeviceFd,
    handle: &dumbbuffer::DumbBuffer,
    format: DrmFourcc,
) -> Result<framebuffer::Handle> {
    let planes = Planes {
        size: handle.size(),
        format,
        modifier: None,
        pitches: [handle.pitch(), 0, 0, 0],
        handles: [Some(handle.handle()), None, None, None],
        offsets: [0; 4],
    };
    fd.add_planar_framebuffer(&planes, FbCmd2Flags::empty())
        .context("adding a framebuffer")
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
        Ok(Some(DmabufFramebuffer {
            fd: drm.clone(),
            handle: add_dumb(drm, dumb.handle(), Fourcc::Xrgb8888)?,
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

/// One image of the cursor plane: a dumb buffer (linear ARGB8888, which
/// every cursor plane takes) and its framebuffer.
struct CursorBuffer {
    fd: DrmDeviceFd,
    dumb: Option<dumbbuffer::DumbBuffer>,
    framebuffer: framebuffer::Handle,
    /// The `cursor::Image` it holds.
    image: Option<u64>,
}

impl CursorBuffer {
    fn new(fd: &DrmDeviceFd, (w, h): (u32, u32)) -> Result<CursorBuffer> {
        let dumb = fd
            .create_dumb_buffer((w, h), DrmFourcc::Argb8888, 32)
            .context("cursor buffer")?;
        let framebuffer = match add_dumb(fd, &dumb, DrmFourcc::Argb8888) {
            Ok(framebuffer) => framebuffer,
            Err(e) => {
                let _ = fd.destroy_dumb_buffer(dumb);
                return Err(e);
            }
        };
        Ok(CursorBuffer {
            fd: fd.clone(),
            dumb: Some(dumb),
            framebuffer,
            image: None,
        })
    }

    /// Copies `image` in, the rest transparent. Never the buffer on the
    /// screen: the display engine would show it half written.
    fn write(&mut self, image: &cursor::Image) -> Result<()> {
        let dumb = self.dumb.as_mut().context("cursor buffer")?;
        let pitch = dumb.pitch() as usize;
        let mut map = self
            .fd
            .map_dumb_buffer(dumb)
            .context("mapping the cursor buffer")?;
        map.fill(0);
        let row = image.width as usize * 4;
        for (y, line) in image.pixels.chunks_exact(row).enumerate() {
            map[y * pitch..y * pitch + row].copy_from_slice(line);
        }
        self.image = Some(image.id);
        Ok(())
    }
}

impl Drop for CursorBuffer {
    fn drop(&mut self) {
        let _ = self.fd.destroy_framebuffer(self.framebuffer);
        if let Some(dumb) = self.dumb.take()
            && let Err(e) = self.fd.destroy_dumb_buffer(dumb)
        {
            warn!("removing a cursor buffer: {e}");
        }
    }
}

/// Where the cursor plane is and what it shows: top-left corner and image.
type Placement = (Point<i32, Physical>, u64);

/// The cursor plane with two buffers: a new image goes into the one not on
/// the screen, then the two swap.
struct CursorPlane {
    handle: plane::Handle,
    size: (u32, u32),
    buffers: [CursorBuffer; 2],
    front: usize,
    /// On the screen (or committed for the next vblank); None: hidden.
    shown: Option<Placement>,
    /// Commited, not yet confirmed: becomes `front` and `shown`.
    pending: Option<(usize, Option<Placement>)>,
}

impl CursorPlane {
    fn new(drm: &DrmDevice, surface: &DrmSurface) -> Option<CursorPlane> {
        let handle = surface.planes().cursor.first()?.handle;
        let size = drm.cursor_size();
        let size = (size.w, size.h);
        let fd = drm.device_fd();
        match (CursorBuffer::new(fd, size), CursorBuffer::new(fd, size)) {
            (Ok(a), Ok(b)) => Some(CursorPlane {
                handle,
                size,
                buffers: [a, b],
                front: 0,
                shown: None,
                pending: None,
            }),
            (Err(e), _) | (_, Err(e)) => {
                warn!("no cursor plane: {e}");
                None
            }
        }
    }

    fn fits(&self, image: &cursor::Image) -> bool {
        image.width <= self.size.0 && image.height <= self.size.1
    }

    /// The plane's new state, if the cursor changed. An image that does not
    /// fit (`fits`) is drawn into the frame by the caller and hides the plane.
    fn update(
        &mut self,
        cursor: Option<(&cursor::Image, Point<i32, Physical>)>,
    ) -> Result<Option<PlaneState<'static>>> {
        let cursor = cursor.filter(|(image, _)| self.fits(image));
        let wanted = cursor.map(|(image, at)| {
            (
                at - Point::from((image.hotspot.0, image.hotspot.1)),
                image.id,
            )
        });
        if wanted == self.shown {
            return Ok(None);
        }
        let Some(((at, id), image)) = wanted.zip(cursor.map(|(image, _)| image)) else {
            self.pending = Some((self.front, None));
            return Ok(Some(PlaneState {
                handle: self.handle,
                config: None,
            }));
        };
        let mut buffer = self.front;
        if self.buffers[buffer].image != Some(id) {
            buffer = 1 - self.front;
            self.buffers[buffer].write(image)?;
        }
        self.pending = Some((buffer, wanted));
        let (w, h) = (self.size.0 as i32, self.size.1 as i32);
        Ok(Some(PlaneState {
            handle: self.handle,
            config: Some(PlaneConfig {
                src: Rectangle::from_size((w, h).into()).to_f64(),
                dst: Rectangle::new(at, (w, h).into()),
                transform: Transform::Normal,
                alpha: 1.0,
                damage_clips: None,
                fb: self.buffers[buffer].framebuffer,
                fence: None,
            }),
        }))
    }

    fn committed(&mut self) {
        if let Some((front, shown)) = self.pending.take() {
            self.front = front;
            self.shown = shown;
        }
    }
}

/// What a swapchain image keeps: its framebuffer and the renderer's target.
struct Image<F> {
    framebuffer: F,
    target: RefCell<Target>,
}

/// Beize's background until the shell draws one: what the damage tracker
/// clears to where no window is.
const BACKGROUND: [f32; 4] = [0.08, 0.10, 0.14, 1.0];

/// The time between two vblanks of `mode`.
fn period(mode: &Mode) -> Duration {
    let millihertz = crate::gpu::millihertz(mode).max(1);
    Duration::from_nanos(1_000_000_000_000 / u64::from(millihertz))
}

/// How long a vblank of `mode` lasts: its blank lines (total less shown)
/// at the time a line takes, as Mutter and KWin compute it.
fn blank(mode: &Mode) -> Duration {
    blank_lines(mode.size().1, mode.vsync().2, mode.hsync().2, mode.clock())
}

/// `vtotal - vdisplay` lines of `htotal` pixels at `khz`.
fn blank_lines(vdisplay: u16, vtotal: u16, htotal: u16, khz: u32) -> Duration {
    let lines = u64::from(vtotal.saturating_sub(vdisplay));
    Duration::from_nanos(lines * u64::from(htotal) * 1_000_000 / u64::from(khz).max(1))
}

/// A drawn frame waiting for its vblank: its image, its times, its fence
/// (when the GPU is done), whether the schedule learns from it (not from
/// one that also set up a buffer or the mode) and the vblank it was planned
/// for, if one.
struct Pending<B: Buffer> {
    aim: Option<Duration>,
    slot: Slot<B>,
    times: Times,
    fence: Option<OwnedFd>,
    learn: bool,
}

/// What `Output::render` did.
#[derive(Debug, PartialEq, Eq)]
pub enum Frame {
    /// A frame (or a cursor move) waits for its vblank.
    Queued,
    /// Nothing changed: no frame.
    Unchanged,
    /// The last frame is not on the screen yet; the next follows it.
    Deferred,
}

pub struct Output<A: Allocator, F: ExportFramebuffer<A::Buffer>> {
    drm: DrmDevice,
    surface: DrmSurface,
    swapchain: Swapchain<A>,
    exporter: F,
    renderer: VkRenderer,
    damage: OutputDamageTracker,
    cursor: Option<CursorPlane>,
    /// The renderer draws straight into the screen's buffers (always on the
    /// GPU; lavapipe where dumb buffers are fast to read).
    direct: bool,
    /// A commit waits for its vblank.
    flipping: bool,
    /// Its image, if it brought one, and when it was asked for and drawn.
    pending: Option<Pending<A::Buffer>>,
    /// When the next frame is planned to be drawn, and for which vblank
    /// (`draw_at`).
    planned: Option<Plan>,
    /// On the screen.
    shown: Option<Slot<A::Buffer>>,
    modeset: bool,
    frames: u64,
    last: Option<Duration>,
    /// The CRTC's index, for asking KMS its last vblank.
    pipe: u32,
    schedule: Schedule,
    pacing: Pacing,
    clock: Clock<Monotonic>,
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
        let cursor = CursorPlane::new(&drm, &surface);
        if cursor.is_none() {
            info!("no cursor plane: the pointer is drawn into the frame");
        }
        let refresh = period(&mode);
        let pipe = drm
            .resource_handles()
            .ok()
            .and_then(|r| r.crtcs().iter().position(|&c| c == surface.crtc()))
            .and_then(|i| u32::try_from(i).ok())
            .unwrap_or(0);
        Output {
            swapchain: Swapchain::new(allocator, w.into(), h.into(), Fourcc::Xrgb8888, modifiers),
            damage: OutputDamageTracker::new((i32::from(w), i32::from(h)), 1.0, Transform::Normal),
            drm,
            surface,
            exporter,
            renderer,
            cursor,
            direct,
            flipping: false,
            pending: None,
            planned: None,
            shown: None,
            modeset: true,
            frames: 0,
            last: None,
            pipe,
            schedule: Schedule::new(refresh, blank(&mode)),
            pacing: Pacing::new(refresh, blank(&mode)),
            clock: Clock::new(),
        }
    }

    /// When to draw a change there now (`Schedule`); None while a commit
    /// waits for its vblank, which asks again.
    pub fn draw_at(&mut self) -> Option<Duration> {
        if self.flipping {
            return None;
        }
        let now: Duration = self.clock.now().into();
        if self.schedule.stale(now)
            && let Some(time) = self.last_vblank()
        {
            self.schedule.vblank(time);
        }
        let plan = self.schedule.draw_at(now);
        self.planned = Some(plan);
        Some(plan.at)
    }

    /// The last vblank, from KMS: the libdrm query (a wait for none).
    /// Not every driver counts vblanks (virtio-gpu).
    fn last_vblank(&self) -> Option<Duration> {
        match self.drm.wait_vblank(
            VblankWaitTarget::Relative(0),
            VblankWaitFlags::empty(),
            self.pipe,
            0,
        ) {
            Ok(reply) => reply.time(),
            Err(e) => {
                debug!("no vblank time from KMS: {e}");
                None
            }
        }
    }

    pub fn renderer(&mut self) -> &mut VkRenderer {
        &mut self.renderer
    }

    /// The cursor plane can show `image`; otherwise it is drawn.
    pub fn cursor_fits(&self, image: &cursor::Image) -> bool {
        self.cursor.as_ref().is_some_and(|plane| plane.fits(image))
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

    /// Draws what changed of `elements` (front to back), places the
    /// cursor (image and its hotspot's position) and hands both to KMS in
    /// one commit; nothing if nothing changed. The change arrived at
    /// `requested`, the caller began at `began` (for `Pacing`).
    pub fn render<E: RenderElement<VkRenderer>>(
        &mut self,
        elements: &[E],
        cursor: Option<(&cursor::Image, Point<i32, Physical>)>,
        requested: Duration,
        began: Duration,
    ) -> Result<Frame> {
        if self.flipping {
            return Ok(Frame::Deferred);
        }
        let slot = self
            .swapchain
            .acquire()
            .context("screen buffer")?
            .context("no free screen buffer")?;
        // Setting up a screen buffer or the mode is done once, not each
        // frame: the schedule does not learn from it.
        let mut setup = self.modeset;
        if slot.userdata().get::<Image<F::Framebuffer>>().is_none() {
            setup = true;
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
        let (damaged, fence) = {
            let mut target = image.target.borrow_mut();
            let mut framebuffer = self.renderer.bind(&mut *target)?;
            let result = self
                .damage
                .render_output(
                    &mut self.renderer,
                    &mut framebuffer,
                    slot.age().into(),
                    elements,
                    BACKGROUND,
                )
                .map_err(|e| error(format!("rendering: {e:?}")))?;
            (result.damage.is_some(), result.sync.export())
        };
        let cursor = match &mut self.cursor {
            Some(plane) => plane.update(cursor)?,
            None => None,
        };
        if !damaged && cursor.is_none() {
            // Nothing changed: no frame, no commit; the slot goes back.
            return Ok(Frame::Unchanged);
        }

        let size = slot.size();
        let primary = damaged.then(|| PlaneState {
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
        });
        let planes = primary.into_iter().chain(cursor);
        if self.modeset {
            self.surface
                .commit(planes, true)
                .context("setting the mode")?;
            self.modeset = false;
        } else {
            self.surface.page_flip(planes, true).context("page flip")?;
        }
        if let Some(plane) = &mut self.cursor {
            plane.committed();
        }
        self.flipping = true;
        let committed: Duration = self.clock.now().into();
        // Planned by the timer; a frame drawn without it (after a pause)
        // from when it began, for no vblank.
        let plan = self.planned.take();
        if damaged {
            self.swapchain.submitted(&slot);
            let times = Times {
                requested,
                due: self.schedule.target(requested),
                planned: plan.map_or(began, |p| p.at.min(began)),
                began,
                committed,
                lead: plan.map(|p| p.lead),
            };
            self.pending = Some(Pending {
                slot,
                times,
                fence,
                learn: !setup,
                aim: plan.and_then(|p| p.aim),
            });
        }
        Ok(Frame::Queued)
    }

    /// The commit is on the screen; the time of its vblank, if KMS said.
    pub fn vblank(&mut self, metadata: Option<DrmEventMetadata>) -> Option<Duration> {
        self.flipping = false;
        let time = match metadata.map(|m| m.time) {
            Some(DrmEventTime::Monotonic(time)) => Some(time),
            _ => None,
        };
        if let Some(time) = time {
            self.schedule.vblank(time);
        }
        let gpu_ms = self.renderer.take_gpu_ms();
        let Some(Pending {
            slot,
            times,
            fence,
            learn,
            aim,
        }) = self.pending.take()
        else {
            debug!("cursor plane only, nothing drawn");
            return time;
        };
        // KMS showed it, so its fence has signalled: when, the kernel says.
        let gpu_done = fence.as_ref().and_then(|f| signalled_at(f.as_fd()));
        if learn {
            let ready = gpu_done.map_or(times.committed, |g| g.max(times.committed));
            self.schedule.ready(times.planned, ready, aim);
        }
        if let Some(time) = time {
            self.pacing.shown(times, time, gpu_ms, gpu_done);
        }
        self.shown = Some(slot);
        self.frames += 1;
        let since = time.zip(self.last).map(|(t, l)| t.saturating_sub(l));
        self.last = time.or(self.last);
        match self.frames {
            1 => info!("first frame on the screen; idle until something changes"),
            n => debug!(
                "frame {n}: {since:?} after the last, GPU {:?} ms",
                self.renderer.gpu_ms()
            ),
        }
        time
    }

    /// The time between two vblanks.
    pub fn refresh(&self) -> Duration {
        period(&self.surface.pending_mode())
    }

    /// A run of frames is being measured; ends it after a while without
    /// frames (false then).
    pub fn pacing_idle(&mut self) -> bool {
        self.pacing.idle(self.clock.now().into())
    }

    /// Another session took the screen (logind).
    pub fn pause(&mut self) {
        self.pacing.pause();
        self.drm.pause();
    }

    /// The screen is ours again: everything is set anew; the caller draws.
    pub fn resume(&mut self) -> Result<()> {
        self.drm.activate(false).context("taking the screen back")?;
        self.schedule.reset();
        self.surface.reset_state().context("screen state")?;
        self.swapchain.reset_buffers();
        self.flipping = false;
        self.pending = None;
        self.planned = None;
        self.shown = None;
        self.modeset = true;
        self.last = None;
        if let Some(plane) = &mut self.cursor {
            plane.shown = None;
            plane.pending = None;
        }
        // Whatever the other session left is drawn over in full.
        let (w, h) = self.surface.pending_mode().size();
        self.damage =
            OutputDamageTracker::new((i32::from(w), i32::from(h)), 1.0, Transform::Normal);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mode(size: (u16, u16), hz: u32, preferred: bool, interlaced: bool) -> Candidate {
        Candidate {
            size,
            millihertz: hz * 1000,
            preferred,
            interlaced,
        }
    }

    #[test]
    fn takes_the_preferred_size_at_its_fastest() {
        let qhd = (2560, 1440);
        // The AW2723DF on DisplayPort: 1440p preferred at 60 Hz, 144 and
        // 240 Hz further down, a faster mode only at a lower size.
        let modes = [
            mode(qhd, 60, true, false),
            mode((1920, 1080), 360, false, false),
            mode(qhd, 144, false, false),
            mode(qhd, 240, false, false),
            mode(qhd, 300, false, true),
        ];
        assert_eq!(best_mode(&modes), 3);
        // Equally fast: the preferred one.
        let modes = [mode(qhd, 60, false, false), mode(qhd, 60, true, false)];
        assert_eq!(best_mode(&modes), 1);
        // No preferred mode: the first one's size.
        let modes = [
            mode(qhd, 60, false, false),
            mode((800, 600), 75, false, false),
        ];
        assert_eq!(best_mode(&modes), 0);
    }

    #[test]
    fn the_vblank_lasts_its_blank_lines() {
        // The AW2723DF's 2560x1440 at 240 Hz, as KMS listed it on the
        // owner's PC: 179 blank lines of 2720 pixels at 1056.75 MHz.
        assert_eq!(
            blank_lines(1440, 1619, 2720, 1_056_750),
            Duration::from_nanos(460_733)
        );
        assert_eq!(blank_lines(800, 800, 1280, 0), Duration::ZERO);
    }
}
