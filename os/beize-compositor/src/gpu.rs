//! The GPU and its screens (ADR 0029): the GPU udev names primary for the
//! seat, opened through logind, the Vulkan device behind it and the screen
//! it shows. A GPU without a Vulkan driver in Beize (a VM, for now Intel and
//! AMD) gets lavapipe, which draws into the KMS driver's dumb buffers (ADR
//! 0027 §6).
//!
//! Screens come and go (`scan`, at each udev change of the GPU, as Mutter
//! and KWin read the connectors anew): one that goes rests its frame loop,
//! one that comes – again or another – gets its best mode. Beize shows one
//! screen until it shows several (ADR 0029 §6): a laptop's own (eDP) first,
//! else the first one found.

use std::path::Path;
use std::time::Duration;

use smithay::backend::allocator::Modifier;
use smithay::backend::allocator::dumb::DumbAllocator;
use smithay::backend::allocator::vulkan::{ImageUsageFlags, VulkanAllocator};
use smithay::backend::drm::{DrmDevice, DrmDeviceFd, DrmDeviceNotifier, DrmEventMetadata, DrmNode};
use smithay::backend::renderer::element::{RenderElement, RenderElementStates};
use smithay::backend::session::Session;
use smithay::backend::session::libseat::LibSeatSession;
use smithay::backend::udev;
use smithay::backend::vulkan::{AppInfo, Instance, PhysicalDevice, version::Version};
use smithay::reexports::ash::vk;
use smithay::reexports::drm::control::atomic::AtomicModeReq;
use smithay::reexports::drm::control::{
    AtomicCommitFlags, Device as _, Mode, connector, crtc, property,
};
use smithay::reexports::drm::{Device as _, DriverCapability};
use smithay::reexports::rustix::fs::OFlags;
use smithay::utils::{DeviceFd, Physical, Point};
use tracing::{info, warn};

use crate::cursor;
use crate::error::{Context, Result, error};
use crate::output::{self, DmabufExporter, DumbExporter, Frame, Info, Output};
use crate::pacing::Parts;
use crate::render::VkRenderer;

/// A screen, by the way its images are made.
enum Screen {
    /// The GPU's own Vulkan driver allocates them (NVK).
    Gpu(Box<Output<VulkanAllocator, DmabufExporter>>),
    /// Dumb buffers of the KMS driver, drawn by lavapipe.
    Cpu(Box<Output<DumbAllocator, DumbExporter>>),
}

/// `$body` with `$o`, the screen's `Output` of either kind.
macro_rules! each {
    ($screen:expr, $o:ident => $body:expr) => {
        match $screen {
            Screen::Gpu($o) => $body,
            Screen::Cpu($o) => $body,
        }
    };
}

/// Where a screen's images come from.
enum Images {
    /// Vulkan's allocator on the GPU's own driver.
    Vulkan(Box<PhysicalDevice>),
    /// The KMS driver's dumb buffers; lavapipe draws straight into them
    /// where they are fast to read (`direct`), else into its own image,
    /// copied over.
    Dumb { direct: bool },
}

/// What a new look at the connectors found (`Gpu::scan`).
#[derive(Debug)]
pub enum Change {
    /// The screen shown is as it was.
    None,
    /// It went, and no other is connected.
    Gone,
    /// Its link was trained anew; it shows what it showed.
    Retrained,
    /// A screen to show: the one before in a new mode, or another.
    New(Info),
}

pub struct Gpu {
    /// First, so that it goes first: its images are the renderer's and the
    /// device's.
    screen: Option<Screen>,
    renderer: VkRenderer,
    images: Images,
    drm: DrmDevice,
    node: DrmNode,
    /// What the renderer held when last logged (`pacing_idle`).
    held: Option<(usize, u32)>,
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

/// Opens the seat's GPU and takes its screen, if one is connected; also
/// returns what the screen is and the GPU's events.
pub fn open(session: &mut LibSeatSession) -> Result<(Gpu, Option<Info>, DrmDeviceNotifier)> {
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
    if !drm.is_atomic() {
        return Err(error("the GPU has no atomic KMS (ADR 0027 §6)"));
    }
    let node = DrmNode::from_dev_id(drm.device_id()).context("DRM node")?;
    let (renderer, images) = vulkan(&drm, node)?;
    let mut gpu = Gpu {
        screen: None,
        renderer,
        images,
        drm,
        node,
        held: None,
    };
    // The console probed the connectors already.
    let info = match gpu.scan(false)? {
        Change::New(info) => Some(info),
        _ => None,
    };
    Ok((gpu, info, events))
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
fn vulkan(drm: &DrmDevice, node: DrmNode) -> Result<(VkRenderer, Images)> {
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
    if let Some(phd) = devices
        .iter()
        .find(|d| d.primary_node().ok().flatten() == Some(node))
    {
        return Ok((VkRenderer::new(phd)?, Images::Vulkan(Box::new(phd.clone()))));
    }

    let lavapipe = devices
        .iter()
        .find(|d| d.ty() == vk::PhysicalDeviceType::CPU)
        .context("no Vulkan driver for the GPU, and no lavapipe")?;
    // Where the KMS driver's dumb buffers are fast to read (virtio-gpu),
    // lavapipe draws into them; otherwise (i915, nouveau) into its own
    // image, copied over – the rule Mutter keeps for its shadow buffer.
    let shadow = drm
        .device_fd()
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
    Ok((VkRenderer::new(lavapipe)?, Images::Dumb { direct }))
}

/// The connector's `link-status` property, if it is BAD: the driver could
/// not keep the link (DisplayPort after the monitor slept, a KVM switch)
/// and waits for a modeset that sets it GOOD (kernel, *Standard Connector
/// Properties*).
fn link_bad(fd: &DrmDeviceFd, connector: connector::Handle) -> Option<property::Handle> {
    // DRM_MODE_LINK_STATUS_BAD in the uapi's drm_mode.h.
    const BAD: u64 = 1;
    let props = fd.get_properties(connector).ok()?;
    let (handles, values) = props.as_props_and_values();
    handles.iter().zip(values).find_map(|(&handle, &value)| {
        let info = fd.get_property(handle).ok()?;
        (info.name().to_bytes() == b"link-status" && value == BAD).then_some(handle)
    })
}

/// Trains the link anew: a modeset with `link-status` GOOD, the screen's
/// state otherwise as it is – as Mutter and KWin add GOOD to their modeset.
fn retrain(
    fd: &DrmDeviceFd,
    connector: connector::Handle,
    property: property::Handle,
) -> Result<()> {
    // DRM_MODE_LINK_STATUS_GOOD.
    const GOOD: u64 = 0;
    let mut request = AtomicModeReq::new();
    request.add_raw_property(connector.into(), property, GOOD);
    fd.atomic_commit(AtomicCommitFlags::ALLOW_MODESET, request)
        .context("training the link anew")
}

impl Gpu {
    /// The main device for clients' dmabufs.
    pub fn node(&self) -> DrmNode {
        self.node
    }

    pub fn device_id(&self) -> smithay::reexports::rustix::fs::Dev {
        self.drm.device_id()
    }

    pub fn renderer(&mut self) -> &mut VkRenderer {
        &mut self.renderer
    }

    /// A screen is shown.
    pub fn shows(&self) -> bool {
        self.screen.is_some()
    }

    /// Looks at the connectors anew (`probe`: asks the screens, as at a
    /// hotplug; else what the kernel knows): keeps the screen shown while
    /// it is connected in its mode, trains its link anew if it broke, and
    /// otherwise takes the one to show.
    pub fn scan(&mut self, probe: bool) -> Result<Change> {
        let fd = self.drm.device_fd().clone();
        let connectors: Vec<connector::Info> = fd
            .resource_handles()
            .context("DRM resources")?
            .connectors()
            .iter()
            .filter_map(|&handle| {
                fd.get_connector(handle, probe)
                    .inspect_err(|e| warn!("a DRM connector: {e}"))
                    .ok()
            })
            .collect();
        let mut gone = false;
        if let Some(screen) = &mut self.screen {
            let (handle, mode, name) =
                each!(screen, o => (o.connector(), o.mode(), o.name().to_string()));
            match connectors
                .iter()
                .find(|c| c.handle() == handle)
                .filter(|c| output::connected(c))
            {
                Some(c) if c.modes().contains(&mode) => {
                    let Some(property) = link_bad(&fd, handle) else {
                        return Ok(Change::None);
                    };
                    info!("screen {name}: its link broke, training it anew");
                    retrain(&fd, handle, property)?;
                    each!(screen, o => o.retrained());
                    return Ok(Change::Retrained);
                }
                Some(_) => info!("screen {name}: its mode is gone"),
                None => {
                    info!("screen {name} is gone");
                    gone = true;
                }
            }
            self.screen = None;
        }
        let shown: Vec<&connector::Info> =
            connectors.iter().filter(|c| output::connected(c)).collect();
        let Some(connector) = shown
            .iter()
            .find(|c| c.interface() == connector::Interface::EmbeddedDisplayPort)
            .or(shown.first())
        else {
            return Ok(if gone { Change::Gone } else { Change::None });
        };
        // A link that broke while no one showed it.
        if let Some(property) = link_bad(&fd, connector.handle()) {
            retrain(&fd, connector.handle(), property)?;
        }
        self.show(connector).map(Change::New)
    }

    /// Takes `connector`'s screen.
    fn show(&mut self, connector: &connector::Info) -> Result<Info> {
        let (surface, info) = output::surface(&mut self.drm, connector)?;
        let id = (connector.handle(), info.name.clone());
        self.screen = Some(match &self.images {
            Images::Vulkan(phd) => {
                // Drawn into, and read from by live glass (`blur_behind`), as
                // the renderer imports it: only modifiers that allow both.
                let allocator = VulkanAllocator::new(
                    phd,
                    ImageUsageFlags::COLOR_ATTACHMENT | ImageUsageFlags::TRANSFER_SRC,
                )
                .context("Vulkan allocator")?;
                // The modifiers the screen's plane takes for XRGB8888, that
                // Vulkan can allocate.
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
                // The GPU draws into the screen's buffers: they are its own
                // images, often tiled, which no row copy could fill.
                Screen::Gpu(Box::new(Output::new(
                    &self.drm,
                    surface,
                    id,
                    allocator,
                    modifiers,
                    DmabufExporter,
                    true,
                )))
            }
            Images::Dumb { direct } => Screen::Cpu(Box::new(Output::new(
                &self.drm,
                surface,
                id,
                DumbAllocator::new(self.drm.device_fd().clone()),
                vec![Modifier::Linear],
                DumbExporter,
                *direct,
            ))),
        });
        Ok(info)
    }

    /// Draws on the screen (`Output::render`); without one, nothing.
    pub fn render<E: RenderElement<VkRenderer>>(
        &mut self,
        elements: &[E],
        cursor: Option<(&cursor::Image, Point<i32, Physical>)>,
        requested: Duration,
        began: Duration,
        parts: Parts,
    ) -> Result<Frame> {
        let renderer = &mut self.renderer;
        match &mut self.screen {
            Some(screen) => {
                each!(screen, o => o.render(renderer, elements, cursor, requested, began, parts))
            }
            None => Ok(Frame::Unchanged(RenderElementStates::default())),
        }
    }

    pub fn draw_at(&mut self) -> Option<Duration> {
        each!(self.screen.as_mut()?, o => o.draw_at())
    }

    /// The vblank the frame about to be drawn shows at (`Output::aim`).
    pub fn aim(&self) -> Option<Duration> {
        each!(self.screen.as_ref()?, o => Some(o.aim()))
    }

    /// The screen's first vblank at or after `time`.
    pub fn vblank_from(&self, time: Duration) -> Option<Duration> {
        each!(self.screen.as_ref()?, o => Some(o.vblank_from(time)))
    }

    /// A run of frames is being measured (`Output::pacing_idle`); once
    /// things are still, what the renderer holds, if it changed.
    pub fn pacing_idle(&mut self) -> bool {
        let running = self
            .screen
            .as_mut()
            .is_some_and(|screen| each!(screen, o => o.pacing_idle()));
        let held = self.renderer.held();
        if !running && self.held != Some(held) {
            self.held = Some(held);
            info!("held: {} dmabufs of clients, {} textures", held.0, held.1);
        }
        running
    }

    pub fn screenshot(&mut self) -> Result<((u32, u32), Vec<u8>)> {
        let renderer = &mut self.renderer;
        let screen = self.screen.as_mut().context("no screen")?;
        each!(screen, o => o.screenshot(renderer))
    }

    /// A commit on `crtc` is on the screen; the time of its vblank. One
    /// for a screen that went since is dropped.
    pub fn vblank(
        &mut self,
        crtc: crtc::Handle,
        metadata: Option<DrmEventMetadata>,
    ) -> Option<Duration> {
        let renderer = &self.renderer;
        let screen = self.screen.as_mut()?;
        each!(screen, o => if o.crtc() == crtc { o.vblank(renderer, metadata) } else { None })
    }

    /// The time between two vblanks of the screen.
    pub fn refresh(&self) -> Option<Duration> {
        self.screen
            .as_ref()
            .map(|screen| each!(screen, o => o.refresh()))
    }

    pub fn cursor_fits(&self, image: &cursor::Image) -> bool {
        self.screen
            .as_ref()
            .is_some_and(|screen| each!(screen, o => o.cursor_fits(image)))
    }

    /// Another session took the GPU (logind).
    pub fn pause(&mut self) {
        if let Some(screen) = &mut self.screen {
            each!(screen, o => o.pause());
        }
        self.drm.pause();
    }

    /// The GPU is ours again: the screen is set anew; the caller draws and
    /// looks whether it is still there (`scan`).
    pub fn resume(&mut self) -> Result<()> {
        self.drm.activate(false).context("taking the screen back")?;
        match &mut self.screen {
            Some(screen) => each!(screen, o => o.resume()),
            None => Ok(()),
        }
    }
}
