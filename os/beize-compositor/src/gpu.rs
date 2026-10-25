//! The GPU and its screen: the GPU udev names primary for the seat, opened
//! through logind, and the Vulkan device behind it. A GPU without a Vulkan
//! driver in Beize (a VM, for now Intel and AMD) gets lavapipe, which draws
//! into the KMS driver's dumb buffers (ADR 0027 §6).

use std::path::Path;
use std::time::Duration;

use smithay::backend::allocator::Modifier;
use smithay::backend::allocator::dumb::DumbAllocator;
use smithay::backend::allocator::vulkan::{ImageUsageFlags, VulkanAllocator};
use smithay::backend::drm::{DrmDevice, DrmDeviceFd, DrmDeviceNotifier, DrmEventMetadata, DrmNode};
use smithay::backend::renderer::element::RenderElement;
use smithay::backend::session::Session;
use smithay::backend::session::libseat::LibSeatSession;
use smithay::backend::udev;
use smithay::backend::vulkan::{AppInfo, Instance, PhysicalDevice, version::Version};
use smithay::reexports::ash::vk;
use smithay::reexports::drm::control::Mode;
use smithay::reexports::drm::{Device as _, DriverCapability};
use smithay::reexports::rustix::fs::OFlags;
use smithay::utils::{DeviceFd, Physical, Point};
use tracing::{info, warn};

use crate::cursor;
use crate::output::{DmabufExporter, DumbExporter, Frame, Info, Output};
use crate::pacing::Parts;
use beize_compositor::error::{Context, Result, error};
use beize_compositor::render::VkRenderer;

/// The screen, by the way its images are made.
pub enum Screen {
    /// The GPU's own Vulkan driver allocates them (NVK).
    Gpu(Box<Output<VulkanAllocator, DmabufExporter>>),
    /// Dumb buffers of the KMS driver, drawn by lavapipe.
    Cpu(Box<Output<DumbAllocator, DumbExporter>>),
}

impl Screen {
    pub fn render<E: RenderElement<VkRenderer>>(
        &mut self,
        elements: &[E],
        cursor: Option<(&cursor::Image, Point<i32, Physical>)>,
        requested: Duration,
        began: Duration,
        parts: Parts,
    ) -> Result<Frame> {
        match self {
            Screen::Gpu(o) => o.render(elements, cursor, requested, began, parts),
            Screen::Cpu(o) => o.render(elements, cursor, requested, began, parts),
        }
    }
    pub fn draw_at(&mut self) -> Option<Duration> {
        match self {
            Screen::Gpu(o) => o.draw_at(),
            Screen::Cpu(o) => o.draw_at(),
        }
    }
    pub fn pacing_idle(&mut self) -> bool {
        match self {
            Screen::Gpu(o) => o.pacing_idle(),
            Screen::Cpu(o) => o.pacing_idle(),
        }
    }
    pub fn screenshot(&mut self) -> Result<((u32, u32), Vec<u8>)> {
        match self {
            Screen::Gpu(o) => o.screenshot(),
            Screen::Cpu(o) => o.screenshot(),
        }
    }
    pub fn vblank(&mut self, metadata: Option<DrmEventMetadata>) -> Option<Duration> {
        match self {
            Screen::Gpu(o) => o.vblank(metadata),
            Screen::Cpu(o) => o.vblank(metadata),
        }
    }
    pub fn refresh(&self) -> Duration {
        match self {
            Screen::Gpu(o) => o.refresh(),
            Screen::Cpu(o) => o.refresh(),
        }
    }
    pub fn renderer(&mut self) -> &mut VkRenderer {
        match self {
            Screen::Gpu(o) => o.renderer(),
            Screen::Cpu(o) => o.renderer(),
        }
    }
    pub fn cursor_fits(&self, image: &cursor::Image) -> bool {
        match self {
            Screen::Gpu(o) => o.cursor_fits(image),
            Screen::Cpu(o) => o.cursor_fits(image),
        }
    }
    pub fn pause(&mut self) {
        match self {
            Screen::Gpu(o) => o.pause(),
            Screen::Cpu(o) => o.pause(),
        }
    }
    pub fn resume(&mut self) -> Result<()> {
        match self {
            Screen::Gpu(o) => o.resume(),
            Screen::Cpu(o) => o.resume(),
        }
    }
}

/// The refresh rate in millihertz, from the mode's timings: `vrefresh` is
/// rounded (143.9 Hz reads 144).
pub fn millihertz(mode: &Mode) -> u32 {
    let (htotal, vtotal) = (u64::from(mode.hsync().2), u64::from(mode.vsync().2));
    let total = htotal * vtotal;
    if total == 0 {
        return mode.vrefresh() * 1000;
    }
    u32::try_from(u64::from(mode.clock()) * 1_000_000 / total).unwrap_or(u32::MAX)
}

/// Opens the seat's GPU and takes its screen. Also returns the main
/// device for clients' dmabufs.
pub fn open(session: &mut LibSeatSession) -> Result<(Screen, Info, DrmDeviceNotifier, DrmNode)> {
    let seat = session.seat();
    let path = match udev::primary_gpu(&seat).context("udev")? {
        Some(path) => path,
        None => udev::all_gpus(&seat)
            .context("udev")?
            .into_iter()
            .next()
            .context("no GPU")?,
    };
    let (drm, events) = device(session, &path)?;
    let node = DrmNode::from_dev_id(drm.device_id()).context("DRM node")?;
    let (screen, info) = screen(drm, node)?;
    Ok((screen, info, events, node))
}

fn device(session: &mut LibSeatSession, path: &Path) -> Result<(DrmDevice, DrmDeviceNotifier)> {
    let fd = session
        .open(
            path,
            OFlags::RDWR | OFlags::CLOEXEC | OFlags::NOCTTY | OFlags::NONBLOCK,
        )
        .context(format!("opening {}", path.display()))?;
    let fd = DrmDeviceFd::new(DeviceFd::from(fd));
    let driver = fd.get_driver().context("DRM driver")?;
    info!(
        "GPU {} ({})",
        path.display(),
        driver.name().to_string_lossy()
    );
    // The console's mode stays until the first frame replaces it: no black
    // in between, as from the boot to the desktop on a Mac.
    DrmDevice::new(fd, false).context("DRM device")
}

/// The Vulkan device for the GPU: its own driver, or lavapipe.
fn screen(mut drm: DrmDevice, node: DrmNode) -> Result<(Screen, Info)> {
    if !drm.is_atomic() {
        return Err(error("the GPU has no atomic KMS (ADR 0027 §6)"));
    }
    let (surface, info) = crate::output::surface(&mut drm)?;
    let fd = drm.device_fd().clone();

    let instance = Instance::new(
        Version::VERSION_1_3,
        Some(AppInfo {
            name: "beize-compositor".into(),
            version: Version::from_raw(vk::make_api_version(0, 0, 1, 0)),
        }),
    )
    .context("Vulkan")?;
    let devices: Vec<PhysicalDevice> = PhysicalDevice::enumerate(&instance)
        .context("Vulkan devices")?
        .collect();
    let own = devices
        .iter()
        .find(|d| d.primary_node().ok().flatten() == Some(node));
    if let Some(phd) = own {
        return Ok((gpu_screen(drm, surface, phd)?, info));
    }

    let lavapipe = devices
        .iter()
        .find(|d| d.ty() == vk::PhysicalDeviceType::CPU)
        .context("no Vulkan driver for the GPU, and no lavapipe")?;
    let vk = VkRenderer::new(lavapipe)?;
    // Where the KMS driver's dumb buffers are fast to read (virtio-gpu),
    // lavapipe draws into them; otherwise (i915, nouveau) into its own
    // image, copied over – the rule Mutter keeps for its shadow buffer.
    let shadow = fd
        .get_driver_capability(DriverCapability::DumbPreferShadow)
        .unwrap_or(1);
    let direct = shadow == 0;
    info!(
        "no Vulkan driver for this GPU: lavapipe, {}",
        if direct {
            "drawing into the screen buffers"
        } else {
            "the driver prefers a shadow buffer"
        }
    );
    let output = Output::new(
        drm,
        surface,
        DumbAllocator::new(fd.clone()),
        vec![Modifier::Linear],
        DumbExporter,
        vk,
        direct,
    );
    Ok((Screen::Cpu(Box::new(output)), info))
}

fn gpu_screen(
    drm: DrmDevice,
    surface: smithay::backend::drm::DrmSurface,
    phd: &PhysicalDevice,
) -> Result<Screen> {
    let vk = VkRenderer::new(phd)?;
    let allocator =
        VulkanAllocator::new(phd, ImageUsageFlags::COLOR_ATTACHMENT).context("Vulkan allocator")?;
    // The modifiers the screen's plane takes for XRGB8888, that Vulkan can
    // allocate.
    let modifiers: Vec<Modifier> = surface
        .plane_info()
        .formats
        .iter()
        .filter(|f| f.code == smithay::backend::allocator::Fourcc::Xrgb8888)
        .map(|f| f.modifier)
        .filter(|m| *m != Modifier::Invalid)
        .collect();
    if modifiers.is_empty() {
        warn!("the screen's plane names no modifiers for XRGB8888");
    }
    Ok(Screen::Gpu(Box::new(Output::new(
        drm,
        surface,
        allocator,
        modifiers,
        DmabufExporter,
        vk,
        // The GPU draws into the screen's buffers: they are its own images,
        // often tiled, which no row copy could fill.
        true,
    ))))
}
