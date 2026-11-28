//! Beize's renderer: Vulkan only (ADR 0027 §2), behind Smithay's `Renderer`
//! so its damage tracking and render elements work on it.
//!
//! Built for doing little: a frame draws only its damage, each damaged
//! piece of a surface one quad, all quads of a frame in one buffer and few
//! draws (bindless textures, `pipeline`). Two submissions are in flight; the
//! CPU waits for the GPU only when it is that far behind (`current`, the
//! `wait` of a frame's parts). A frame's end goes to KMS and into the
//! dmabufs it touched as a sync_file (`sync`). lavapipe's copy to a dumb
//! buffer waits, as the CPU has to copy.

mod device;
mod frame;
pub mod geometry;
mod glass;
mod memory;
mod pictures;
mod submit;
mod sync;
mod target;
mod texture;
mod timing;
mod traits;

use std::collections::HashMap;
use std::os::fd::AsFd;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use smithay::backend::allocator::dmabuf::{Dmabuf, WeakDmabuf};
use smithay::backend::allocator::{Buffer as _, Format, Fourcc, Modifier};
use smithay::backend::renderer::sync::SyncPoint;
use smithay::backend::renderer::{
    Bind, ContextId, DebugFlags, ExportMem, ImportDma, ImportDmaWl, ImportMem, ImportMemWl,
    Offscreen, Renderer, RendererSuper, Texture, TextureFilter, TextureMapping,
};
use smithay::backend::vulkan::PhysicalDevice;
use smithay::reexports::ash::vk;
use smithay::reexports::wayland_server::protocol::wl_buffer;
use smithay::utils::{Buffer, Physical, Point, Rectangle, Size, Transform};
use smithay::wayland::compositor::SurfaceData;
use tracing::{debug, info, warn};

use crate::error::{Context, Error, Result, error};
use beize_vulkan::buffer::HostBuffer;
use beize_vulkan::pipeline::{Pipelines, Quad};
use device::{Caps, Ext, Garbage, Shared};
pub use frame::VkFrame;
pub use sync::{pending, signalled_at};
pub use target::{Target, VkFramebuffer};
pub use texture::VkTexture;

/// Frames recorded or on the GPU at once.
const IN_FLIGHT: usize = 2;

/// What one frame in flight owns.
struct InFlight {
    commands: vk::CommandBuffer,
    /// Exported as the frame's sync_file (and reset by that).
    fence: vk::Fence,
    quads: Option<HostBuffer>,
    staging: Option<HostBuffer>,
    /// Bytes of `staging` taken by uploads for this frame.
    staged: u64,
    /// GPU time: start and end, then those of each live glass blurred.
    queries: Option<vk::QueryPool>,
    /// How many live glasses this frame timed.
    blurs: usize,
    /// The submission that last used this frame's objects.
    submission: u64,
    /// Ready for this frame: its last submission waited for.
    ready: bool,
}

/// A copy from staging into a texture, recorded with the next submission.
struct Upload {
    texture: VkTexture,
    regions: Vec<vk::BufferImageCopy>,
}

/// A picture the compositor draws for itself (`draw_picture`, the
/// backdrop), recorded with the next submission ahead of its frame, in
/// order: a pass may read what one before it drew.
struct Pass {
    texture: VkTexture,
    quads: Vec<Quad>,
    blend: Vec<bool>,
    /// Drawn with the backdrop's shader (`beize_vulkan::pipeline`).
    backdrop: bool,
    /// What it reads, kept until the GPU is done with it.
    reads: Vec<VkTexture>,
}

/// How the base glass is made of the backdrop (ADR 0028 §6): blurred by
/// Dual Kawase `down` halvings and `up` doublings back (the glass keeps
/// 1/2^(down - up) of the size: it is soft, and the frame's filter scales
/// it up), the taps `offset` texels apart; then its saturation times
/// `saturation`, and `tint` (linear, premultiplied) over it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Material {
    pub down: u32,
    pub up: u32,
    pub offset: f32,
    pub saturation: f32,
    pub tint: [f32; 4],
}

/// How glass blurs what lies behind it: base, live and menu glass by Dual
/// Kawase (`Material`), clear glass and the smoke around it by a Gaussian
/// (`Gauss`).
#[derive(Clone)]
pub enum Blur {
    Kawase(Material),
    Gauss(Gauss),
}

/// A Gaussian blur in two passes – across, then down –, on the colours as
/// encoded: by `sigma` px, or per pixel by a `mask` (8 bits, stretched over
/// the source; `backdrop.frag`, `smoke_sigma`) up to it. A Gaussian, not
/// Kawase stages mixed by the mask: those leave blocks and ghost edges, as
/// stacked CSS backdrop filters do. `half`: at half the size, the first
/// pass halving the source as it reads it (liquid glass's wide frost: a
/// quarter of the pixels, half the taps; its result is soft enough for the
/// frame's filter to scale it up). `keeps`: of a half-size blur, the source's
/// part anything reads; the passes draw only what that needs.
#[derive(Clone)]
pub struct Gauss {
    pub sigma: f32,
    pub mask: Option<VkTexture>,
    pub half: bool,
    pub keeps: Option<Rectangle<i32, Physical>>,
}

/// How liquid glass is drawn (`beize_vulkan::pipeline::CLEAR`): with white
/// ink on it – its tone held for the ink – or none (the dock), in the dark
/// appearance or not.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Liquid {
    pub ink: bool,
    pub dark: bool,
}

/// Where the CPU's time went in the submissions since it was last taken
/// (`VkRenderer::take_spent`): waiting for the GPU to give back a frame's
/// objects (or for lavapipe's copy), recording the commands – beginning
/// them (uploads, the quads into their buffer), the barriers and draws, and
/// ending them –, the fences of the clients' dmabufs and of the frame
/// (sync_file in and out), and the submission to the queue. And how many
/// quads the frames drew, in how many draw calls.
#[derive(Clone, Copy, Debug, Default)]
pub struct Spent {
    /// In the frame's draw calls: the quads of each element.
    pub quads: Duration,
    pub wait: Duration,
    pub begin: Duration,
    pub record: Duration,
    pub end: Duration,
    pub quads_drawn: usize,
    pub draws: usize,
    /// Clients' buffers whose writes were still pending at submission
    /// (`frame::implicit_waits`): the GPU waited for them.
    pub pending_writes: usize,
    pub sync: Duration,
    pub queue: Duration,
}

/// How long the GPU worked on a frame, and, with calibrated timestamps,
/// from when to when on CLOCK_MONOTONIC: after the commit it waited that
/// long to begin (for the fences it waits on, for the queue), and its fence
/// signalled that long after it ended (the driver's interrupt and work).
#[derive(Clone, Copy, Debug)]
pub struct GpuTime {
    pub ms: f64,
    pub span: Option<(Duration, Duration)>,
    /// Of `ms`, the live glass blurred in the frame: copies and passes.
    pub blur: f64,
}

/// How many live glasses of a frame get timestamps of their own.
const BLURS_TIMED: usize = 4;

/// What liquid glass's frost keeps around its shape, in texels at half size:
/// what the refraction and the filter read of it (some 4), with some to spare.
const FROST_KEEPS: i32 = 5;

/// How often the GPU's clock is set against CLOCK_MONOTONIC: they drift by
/// microseconds a second.
const CALIBRATE: Duration = Duration::from_secs(1);

/// A picture's format: read as UNORM, drawn through an sRGB view.
const PICTURE: vk::Format = vk::Format::R8G8B8A8_UNORM;
const PICTURE_DRAWN: vk::Format = vk::Format::R8G8B8A8_SRGB;
/// Where each list of quads starts in a frame's buffer, at least.
const QUADS_ALIGN: usize = 256;

pub struct VkRenderer {
    shared: Arc<Shared>,
    queue: vk::Queue,
    family: u32,
    pool: vk::CommandPool,
    caps: Caps,
    ext: Ext,
    pipelines: Pipelines,
    in_flight: Vec<InFlight>,
    current: usize,
    submitted: u64,
    uploads: Vec<Upload>,
    passes: Vec<Pass>,
    /// Pictures drawn, to be recorded with the next frame (`frame::Canvas`).
    canvases: Vec<frame::Canvas>,
    dmabufs: HashMap<WeakDmabuf, VkTexture>,
    dmabuf_formats: smithay::backend::allocator::format::FormatSet,
    context: ContextId<VkTexture>,
    debug: DebugFlags,
    name: String,
    spent: Spent,
    /// When the GPU's clock was last set against CLOCK_MONOTONIC, and the
    /// nanoseconds to add to a GPU time for it.
    calibration: std::cell::Cell<Option<(std::time::Instant, i128)>>,
}

impl std::fmt::Debug for VkRenderer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VkRenderer")
            .field("device", &self.name)
            .finish()
    }
}

impl VkRenderer {
    pub fn new(phd: &PhysicalDevice) -> Result<VkRenderer> {
        let created = device::create(phd)?;
        let shared = created.shared;
        let d = &shared.device;
        let pipelines = Pipelines::new(d, shared.max_textures())?;
        // SAFETY: plain creation; destroyed in Drop.
        let pool = unsafe {
            d.create_command_pool(
                &vk::CommandPoolCreateInfo::default()
                    .queue_family_index(created.family)
                    .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER),
                None,
            )
        }
        .context("command pool")?;
        let mut renderer = VkRenderer {
            shared: shared.clone(),
            queue: created.queue,
            family: created.family,
            pool,
            caps: created.caps,
            ext: created.ext,
            pipelines,
            in_flight: Vec::new(),
            current: 0,
            submitted: 0,
            uploads: Vec::new(),
            passes: Vec::new(),
            canvases: Vec::new(),
            dmabufs: HashMap::new(),
            dmabuf_formats: Default::default(),
            context: ContextId::new(),
            debug: DebugFlags::empty(),
            name: phd.name().to_owned(),
            spent: Spent::default(),
            calibration: std::cell::Cell::new(None),
        };
        for _ in 0..IN_FLIGHT {
            let frame = renderer.in_flight_new()?;
            renderer.in_flight.push(frame);
        }
        renderer.dmabuf_formats = renderer.query_dmabuf_formats(phd);
        info!(
            "renderer on {}: {} textures, dmabufs {}",
            renderer.name,
            shared.max_textures(),
            if renderer.caps.modifiers {
                "with format modifiers"
            } else {
                "linear only"
            }
        );
        Ok(renderer)
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// The clients' dmabufs it holds, of them those synchronised
    /// explicitly, and the textures in use: they stay flat while windows
    /// come, go and change size (a leak shows here).
    pub fn held(&self) -> (usize, usize, u32) {
        let explicit = self.dmabufs.values().filter(|t| t.explicit()).count();
        (self.dmabufs.len(), explicit, self.shared.textures_in_use())
    }

    /// Whether `dmabuf`'s last applied commit is synchronised explicitly –
    /// its acquire point waited for –, and whether it has a release point
    /// (`syncobj`); its imported texture keeps both. Without one (never
    /// imported) nothing is drawn from it.
    pub fn synced_explicitly(&self, dmabuf: &Dmabuf, explicit: bool, released: bool) {
        if let Some(texture) = self.dmabufs.get(&dmabuf.weak()) {
            texture.0.explicit.store(explicit, Ordering::Relaxed);
            texture.0.released.store(released, Ordering::Relaxed);
        }
    }

    /// Whether frames that read `dmabuf` keep it for its release point.
    pub fn released_explicitly(&self, dmabuf: &Dmabuf) -> bool {
        self.dmabufs
            .get(&dmabuf.weak())
            .is_some_and(VkTexture::released)
    }

    /// The number of the last submission the GPU finished.
    pub fn completed(&self) -> u64 {
        self.shared.completed()
    }

    /// The number of the last submission: a frame's, right after it.
    pub fn submitted(&self) -> u64 {
        self.submitted
    }
}

impl Drop for VkRenderer {
    fn drop(&mut self) {
        let d = &self.shared.device;
        // SAFETY: after the wait nothing of the renderer is in use; the
        // textures that outlive it keep only their own objects.
        unsafe {
            if let Err(e) = d.device_wait_idle() {
                warn!("waiting for the GPU: {e}");
            }
            for f in self.in_flight.drain(..) {
                d.destroy_fence(f.fence, None);
                if let Some(q) = f.queries {
                    d.destroy_query_pool(q, None);
                }
                for b in [f.quads, f.staging].into_iter().flatten() {
                    d.destroy_buffer(b.buffer, None);
                    self.shared.free(b.memory);
                }
            }
            d.destroy_command_pool(self.pool, None);
        }
        self.pipelines.destroy(d);
        self.dmabufs.clear();
        self.uploads.clear();
        self.shared.collect();
    }
}

/// Textures of a surface per renderer, as Smithay's GLES renderer keeps
/// them: a new buffer of the same size and format reuses the texture and
/// uploads only its damage.
type SurfaceTextures = Mutex<HashMap<ContextId<VkTexture>, VkTexture>>;
