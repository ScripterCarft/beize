//! `beize-compositor` – Beize's compositor (ADR 0027). Runs as the logged-in
//! user, never as root: logind opens the GPU and input devices for it
//! (libseat). It picks the GPU udev names primary for the seat and the
//! Vulkan device behind it; a GPU without a Vulkan driver in Beize (a VM,
//! for now Intel and AMD) gets lavapipe, which draws into the KMS driver's
//! dumb buffers (ADR 0027 §6).
//!
//! Step 2 of the ADR: the screen in its preferred mode, Beize's own frame
//! loop and Vulkan renderer (`render`, §7); it draws only what changed.

mod output;

use std::path::Path;
use std::process::ExitCode;

use smithay::backend::allocator::Modifier;
use smithay::backend::allocator::dumb::DumbAllocator;
use smithay::backend::allocator::vulkan::{ImageUsageFlags, VulkanAllocator};
use smithay::backend::drm::{DrmDevice, DrmDeviceFd, DrmEvent, DrmEventMetadata, DrmNode};
use smithay::backend::session::libseat::LibSeatSession;
use smithay::backend::session::{Event as SessionEvent, Session};
use smithay::backend::udev;
use smithay::backend::vulkan::{AppInfo, Instance, PhysicalDevice, version::Version};
use smithay::reexports::ash::vk;
use smithay::reexports::calloop::{EventLoop, LoopHandle, LoopSignal};
use smithay::reexports::drm::{Device as _, DriverCapability};
use smithay::reexports::rustix::fs::OFlags;
use smithay::utils::DeviceFd;
use tracing::{error, info, warn};

use crate::output::{DmabufExporter, DumbExporter, Output};
use beize_compositor::error::{Context, Error, Result, error};
use beize_compositor::log;
use beize_compositor::render::VkRenderer;

/// The screen, by the way its images are made.
enum Screen {
    /// The GPU's own Vulkan driver allocates them (NVK).
    Gpu(Box<Output<VulkanAllocator, DmabufExporter>>),
    /// Dumb buffers of the KMS driver, drawn by lavapipe.
    Cpu(Box<Output<DumbAllocator, DumbExporter>>),
}

impl Screen {
    fn render(&mut self) -> Result<()> {
        match self {
            Screen::Gpu(o) => o.render(),
            Screen::Cpu(o) => o.render(),
        }
    }
    fn vblank(&mut self, metadata: Option<DrmEventMetadata>) -> Result<()> {
        match self {
            Screen::Gpu(o) => o.vblank(metadata),
            Screen::Cpu(o) => o.vblank(metadata),
        }
    }
    fn pause(&mut self) {
        match self {
            Screen::Gpu(o) => o.pause(),
            Screen::Cpu(o) => o.pause(),
        }
    }
    fn resume(&mut self) -> Result<()> {
        match self {
            Screen::Gpu(o) => o.resume(),
            Screen::Cpu(o) => o.resume(),
        }
    }
}

struct State {
    session: LibSeatSession,
    handle: LoopHandle<'static, State>,
    /// None until the session is first active: only then does logind hand
    /// out the GPU for modesetting (a session started on a background VT
    /// waits).
    screen: Option<Screen>,
    signal: LoopSignal,
    failure: Option<Error>,
}

impl State {
    /// Ends the compositor with `e`.
    fn fail(&mut self, e: Error) {
        self.failure = Some(e);
        self.signal.stop();
    }
}

fn main() -> ExitCode {
    log::init();
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            error!("{e}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<()> {
    let mut event_loop: EventLoop<State> = EventLoop::try_new().context("event loop")?;
    info!("starting");
    let (session, session_events) =
        LibSeatSession::new().context("the logind session (libseat)")?;
    let handle = event_loop.handle();
    handle
        .insert_source(session_events, |event, _, state| match event {
            SessionEvent::PauseSession => {
                info!("session paused");
                if let Some(screen) = &mut state.screen {
                    screen.pause();
                }
            }
            SessionEvent::ActivateSession => {
                info!("session active");
                let result = match &mut state.screen {
                    Some(screen) => screen.resume(),
                    None => start(state),
                };
                if let Err(e) = result {
                    state.fail(e);
                }
            }
        })
        .map_err(|e| error(format!("session events: {}", e.error)))?;

    let mut state = State {
        session,
        handle,
        screen: None,
        signal: event_loop.get_signal(),
        failure: None,
    };
    if state.session.is_active() {
        start(&mut state)?;
    } else {
        info!("waiting for the session to become active");
    }
    event_loop
        .run(None, &mut state, |_| {})
        .context("event loop")?;
    state.failure.map_or(Ok(()), Err)
}

/// Takes the GPU and the screen, and draws the first frame.
fn start(state: &mut State) -> Result<()> {
    let seat = state.session.seat();
    let path = match udev::primary_gpu(&seat).context("udev")? {
        Some(path) => path,
        None => udev::all_gpus(&seat)
            .context("udev")?
            .into_iter()
            .next()
            .context("no GPU")?,
    };
    let (drm, drm_events) = open(&mut state.session, &path)?;
    let mut screen = screen(drm)?;
    state
        .handle
        .insert_source(drm_events, |event, metadata, state| match event {
            DrmEvent::VBlank(_) => {
                if let Some(screen) = &mut state.screen
                    && let Err(e) = screen.vblank(metadata.take())
                {
                    error!("{e}");
                }
            }
            DrmEvent::Error(e) => error!("DRM: {e}"),
        })
        .map_err(|e| error(format!("DRM events: {}", e.error)))?;
    screen.render()?;
    state.screen = Some(screen);
    Ok(())
}

fn open(
    session: &mut LibSeatSession,
    path: &Path,
) -> Result<(DrmDevice, smithay::backend::drm::DrmDeviceNotifier)> {
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
fn screen(mut drm: DrmDevice) -> Result<Screen> {
    if !drm.is_atomic() {
        return Err(error("the GPU has no atomic KMS (ADR 0027 §6)"));
    }
    let surface = output::surface(&mut drm)?;
    let fd = drm.device_fd().clone();
    let node = DrmNode::from_dev_id(drm.device_id()).context("DRM node")?;

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
        return gpu_screen(drm, surface, phd);
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
    Ok(Screen::Cpu(Box::new(Output::new(
        drm,
        surface,
        DumbAllocator::new(fd.clone()),
        vec![Modifier::Linear],
        DumbExporter,
        vk,
        direct,
    ))))
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
        false,
    ))))
}
