//! A window of an app (`app`): an xdg-shell toplevel, or a panel of Beize's
//! shell along an edge of the screen (wlr-layer-shell, ADR 0028 §1), shown
//! through a Vulkan swapchain on the app's libwayland display (Mesa's WSI:
//! dmabufs on a GPU, shm on lavapipe; FIFO paces it to the compositor's
//! frames). Opaque, in the size the compositor asks for. The app's renderer
//! draws into the swapchain's images.
//!
//! An app's window has Beize UI's title bar along its top (`titlebar`):
//! the app draws and hears its content only, below the bar, in its own
//! coordinates, as with AppKit's content view. The bar redraws itself
//! (pointer over a button, the window active or not) with the content the
//! app drew last.

use std::time::Instant;

use ash::vk;
use beize_vulkan::error::{Context, Result, error};
use beize_vulkan::image;
use tracing::debug;
use wayland_client::QueueHandle;
use wayland_client::protocol::{wl_compositor::WlCompositor, wl_region::WlRegion, wl_seat::WlSeat};
use wayland_protocols::xdg::shell::client::{
    xdg_surface::XdgSurface, xdg_toplevel::XdgToplevel, xdg_wm_base::XdgWmBase,
};
use wayland_protocols_wlr::layer_shell::v1::client::{
    zwlr_layer_shell_v1::ZwlrLayerShellV1, zwlr_layer_surface_v1::ZwlrLayerSurfaceV1,
};

use crate::app::{Events, State, Surface, WindowId};
use crate::gpu::Gpu;
use crate::input::{Button, Input, Pointer};
use crate::renderer::{After, Renderer, Target};
use beize_draw::Scene;
use beize_draw::Text;
use beize_draw::titlebar::{self, Appearance, Press, TitleBar};

/// sRGB: the GPU blends in linear light and stores sRGB (`draw`).
const FORMAT: vk::Format = vk::Format::B8G8R8A8_SRGB;
/// Until the app prefers another (`prefer_size`), the size it opens in when
/// the compositor leaves the choice to it.
const DEFAULT_SIZE: (u32, u32) = (800, 600);

struct Swapchain {
    handle: vk::SwapchainKHR,
    size: (u32, u32),
    images: Vec<vk::Image>,
    views: Vec<vk::ImageView>,
    /// Per image: signalled when its frame is drawn, for presenting.
    drawn: Vec<vk::Semaphore>,
}

/// What the window is to the compositor.
pub(crate) enum Role {
    /// An app's window.
    Toplevel {
        _wm_base: XdgWmBase,
        _xdg_surface: XdgSurface,
        toplevel: XdgToplevel,
    },
    /// A panel of the shell: the compositor keeps windows out of its strip.
    Panel {
        _layer_shell: ZwlrLayerShellV1,
        _layer: ZwlrLayerSurfaceV1,
    },
}

pub(crate) struct Window {
    pub(crate) id: WindowId,
    role: Role,
    /// An app's window: its title bar, the content the app drew last (below
    /// the bar), and whether the bar changed since it was drawn.
    bar: Option<TitleBar>,
    content: Option<Scene>,
    bar_changed: bool,
    /// Where the pointer is on the surface; whether the app has it (it is
    /// below the bar), and the buttons held on the content, which keep it
    /// there until released, as Wayland's own implicit grab.
    pointer_at: Option<(f64, f64)>,
    in_content: bool,
    held_in_content: usize,
    vk_surface: vk::SurfaceKHR,
    swapchain: Option<Swapchain>,
    /// A present said the swapchain no longer fits the surface.
    stale: bool,
    /// The content's size where the compositor leaves the choice to the
    /// app.
    pub(crate) preferred: (u32, u32),
}

impl Window {
    pub(crate) fn new(
        id: WindowId,
        role: Role,
        vk_surface: vk::SurfaceKHR,
        bar: Option<TitleBar>,
    ) -> Window {
        Window {
            id,
            role,
            bar,
            content: None,
            bar_changed: false,
            pointer_at: None,
            in_content: false,
            held_in_content: 0,
            vk_surface,
            swapchain: None,
            stale: false,
            preferred: DEFAULT_SIZE,
        }
    }

    /// The window's title, in its title bar and the shell (a panel has
    /// none).
    pub(crate) fn set_title(&mut self, title: &str) {
        if let Role::Toplevel { toplevel, .. } = &self.role {
            toplevel.set_title(title.into());
        }
        if let Some(bar) = &mut self.bar
            && bar.title != title
        {
            bar.title = title.to_owned();
            self.bar_changed = true;
        }
    }

    pub(crate) fn set_appearance(&mut self, appearance: Appearance) {
        if let Some(bar) = &mut self.bar
            && bar.appearance != appearance
        {
            bar.appearance = appearance;
            self.bar_changed = true;
        }
    }

    /// The size the app's next frame is drawn in: the content's, below the
    /// title bar.
    pub(crate) fn size(&self, surface: &Surface) -> (u32, u32) {
        let (w, h) = self.surface_size(surface);
        (w, h.saturating_sub(self.bar_height()).max(1))
    }

    /// The whole surface's size, title bar and content.
    fn surface_size(&self, surface: &Surface) -> (u32, u32) {
        match surface.asked {
            (w, h) if w > 0 && h > 0 => (w as u32, h as u32),
            _ => (self.preferred.0, self.preferred.1 + self.bar_height()),
        }
    }

    fn bar_height(&self) -> u32 {
        if self.bar.is_some() {
            titlebar::HEIGHT
        } else {
            0
        }
    }

    /// The title bar's part of what happened: the window's state, and the
    /// pointer over the bar; the rest of the pointer's events go to the app
    /// in its content's coordinates. Whether the bar is to be shown anew
    /// over the content drawn last.
    pub(crate) fn title_bar(
        &mut self,
        events: &mut Events,
        surface: &Surface,
        seat: Option<&WlSeat>,
        serial: u32,
    ) -> bool {
        let width = self.surface_size(surface).0 as f32;
        let (activated, maximized) = (surface.activated, surface.maximized);
        let Some(bar) = &mut self.bar else {
            return false;
        };
        if bar.active != activated || bar.maximized != maximized {
            bar.active = activated;
            bar.maximized = maximized;
            self.bar_changed = true;
        }
        let height = f64::from(titlebar::HEIGHT);
        let mut routed = Vec::with_capacity(events.input.len());
        for input in events.input.drain(..) {
            let Input::Pointer(pointer) = input else {
                routed.push(input);
                continue;
            };
            match pointer {
                Pointer::Enter(x, y) | Pointer::Motion(x, y) => {
                    self.pointer_at = Some((x, y));
                    let to_content = self.held_in_content > 0 || (y >= height && !bar.holding());
                    if to_content {
                        self.bar_changed |= bar.hover(width, None);
                        let event = if self.in_content {
                            Pointer::Motion(x, y - height)
                        } else {
                            Pointer::Enter(x, y - height)
                        };
                        self.in_content = true;
                        routed.push(Input::Pointer(event));
                    } else {
                        self.bar_changed |= bar.hover(width, Some((x, y)));
                        if std::mem::take(&mut self.in_content) {
                            routed.push(Input::Pointer(Pointer::Leave));
                        }
                    }
                }
                Pointer::Leave => {
                    self.pointer_at = None;
                    self.held_in_content = 0;
                    self.bar_changed |= bar.hover(width, None);
                    if std::mem::take(&mut self.in_content) {
                        routed.push(Input::Pointer(Pointer::Leave));
                    }
                }
                Pointer::Button { button, pressed } if self.in_content => {
                    if pressed {
                        self.held_in_content += 1;
                    } else {
                        self.held_in_content = self.held_in_content.saturating_sub(1);
                    }
                    routed.push(Input::Pointer(Pointer::Button { button, pressed }));
                }
                Pointer::Button {
                    button: Button::Left,
                    pressed: true,
                } => {
                    let Some((x, y)) = self.pointer_at else {
                        continue;
                    };
                    let Role::Toplevel { toplevel, .. } = &self.role else {
                        continue;
                    };
                    match bar.press(width, x, y, Instant::now()) {
                        Press::Button => self.bar_changed = true,
                        Press::Move => {
                            if let Some(seat) = seat {
                                toplevel._move(seat, serial);
                            }
                        }
                        Press::ToggleMaximized => toggle_maximized(toplevel, maximized),
                    }
                }
                Pointer::Button {
                    button: Button::Left,
                    pressed: false,
                } => {
                    let Role::Toplevel { toplevel, .. } = &self.role else {
                        continue;
                    };
                    match bar.release() {
                        Some(titlebar::Button::Close) => events.closed = true,
                        Some(titlebar::Button::Maximize) => {
                            toggle_maximized(toplevel, maximized);
                        }
                        // Not available until the dock (`titlebar`).
                        Some(titlebar::Button::Minimize) | None => {}
                    }
                    self.bar_changed = true;
                }
                // Other buttons and scrolling on the bar do nothing.
                Pointer::Button { .. } => {}
                Pointer::Scroll { .. } if self.in_content => {
                    routed.push(Input::Pointer(pointer));
                }
                Pointer::Scroll { .. } => {}
            }
        }
        events.input = routed;
        self.bar_changed && self.content.is_some()
    }

    /// Draws `scene` (the content, or the whole of a panel) and shows it
    /// with the compositor's next frame.
    pub(crate) fn draw(
        &mut self,
        scene: &Scene,
        renderer: &mut Renderer,
        text: &mut Text,
        surface: &Surface,
        compositor: &WlCompositor,
        qh: &QueueHandle<State>,
    ) -> Result<()> {
        if self.bar.is_none() {
            return self.show(scene, renderer, surface, compositor, qh);
        }
        let mut content = Scene::new();
        content.append(scene, titlebar::HEIGHT as f32);
        self.content = Some(content);
        self.present(renderer, text, surface, compositor, qh)
    }

    /// The title bar over the content the app drew last.
    pub(crate) fn present(
        &mut self,
        renderer: &mut Renderer,
        text: &mut Text,
        surface: &Surface,
        compositor: &WlCompositor,
        qh: &QueueHandle<State>,
    ) -> Result<()> {
        let width = self.surface_size(surface).0 as f32;
        let (Some(bar), Some(content)) = (&self.bar, &self.content) else {
            return Ok(());
        };
        let mut frame = bar.draw(text, width);
        frame.append(content, 0.0);
        self.bar_changed = false;
        self.show(&frame, renderer, surface, compositor, qh)
    }

    /// Shows `scene` on the whole surface.
    fn show(
        &mut self,
        scene: &Scene,
        renderer: &mut Renderer,
        surface: &Surface,
        compositor: &WlCompositor,
        qh: &QueueHandle<State>,
    ) -> Result<()> {
        let size = self.surface_size(surface);
        for _ in 0..2 {
            if self.stale || self.swapchain.as_ref().is_none_or(|s| s.size != size) {
                self.recreate(size, &renderer.gpu)?;
                self.set_opaque(surface, compositor, qh);
            }
            if self.draw_once(scene, renderer)? {
                return Ok(());
            }
            // Out of date before drawing: once more with a new one.
            self.stale = true;
        }
        Err(error("the swapchain stays out of date"))
    }

    /// One frame; false if the swapchain was out of date to begin with.
    fn draw_once(&mut self, scene: &Scene, renderer: &mut Renderer) -> Result<bool> {
        let acquired = renderer.next()?;
        let swapchain = self.swapchain.as_ref().context("no swapchain")?;
        let gpu = &renderer.gpu;
        // SAFETY: the swapchain is this device's; the semaphore is
        // unsignalled (its frame's last submission waited on it and is done).
        let acquire = unsafe {
            gpu.swapchain.acquire_next_image(
                swapchain.handle,
                u64::MAX,
                acquired,
                vk::Fence::null(),
            )
        };
        let index = match acquire {
            Ok((index, _suboptimal)) => index as usize,
            Err(vk::Result::ERROR_OUT_OF_DATE_KHR) => return Ok(false),
            Err(e) => return Err(error(format!("acquiring a swapchain image: {e}"))),
        };
        let target = Target {
            image: swapchain.images[index],
            view: swapchain.views[index],
            format: FORMAT,
            size: swapchain.size,
            after: After::Present,
        };
        let (handle, drawn) = (swapchain.handle, swapchain.drawn[index]);
        renderer.draw(scene, &target, Some(acquired), Some(drawn))?;

        let swapchains = [handle];
        let indices = [index as u32];
        let wait = [drawn];
        let present = vk::PresentInfoKHR::default()
            .wait_semaphores(&wait)
            .swapchains(&swapchains)
            .image_indices(&indices);
        let gpu = &renderer.gpu;
        // SAFETY: the image was acquired above and is drawn when `drawn`
        // is signalled.
        match unsafe { gpu.swapchain.queue_present(gpu.queue, &present) } {
            Ok(false) => {}
            // Suboptimal: shown, but the next frame gets a new swapchain.
            Ok(true) | Err(vk::Result::ERROR_OUT_OF_DATE_KHR) => self.stale = true,
            Err(e) => return Err(error(format!("presenting: {e}"))),
        }
        Ok(true)
    }

    /// A swapchain of `size` (the surface's own size where it has one),
    /// replacing the old one.
    fn recreate(&mut self, size: (u32, u32), gpu: &Gpu) -> Result<()> {
        // A new swapchain is rare (configure): the old one goes once nothing
        // uses it any more, presents included.
        let d = &gpu.shared.device;
        // SAFETY: nothing records at the moment.
        unsafe { d.device_wait_idle() }.context("waiting for the GPU")?;
        let surface = &gpu.instance.surface;
        // SAFETY: the surface and physical device belong to this instance.
        let (caps, formats) = unsafe {
            (
                surface.get_physical_device_surface_capabilities(gpu.physical, self.vk_surface),
                surface.get_physical_device_surface_formats(gpu.physical, self.vk_surface),
            )
        };
        let caps = caps.context("the surface's capabilities")?;
        let formats = formats.context("the surface's formats")?;
        if !formats
            .iter()
            .any(|f| f.format == FORMAT && f.color_space == vk::ColorSpaceKHR::SRGB_NONLINEAR)
        {
            return Err(error(format!("the surface does not offer {FORMAT:?}")));
        }
        if !caps
            .supported_composite_alpha
            .contains(vk::CompositeAlphaFlagsKHR::OPAQUE)
        {
            return Err(error("the surface cannot be opaque"));
        }
        // Wayland leaves the size to the window (u32::MAX).
        let extent = if caps.current_extent.width != u32::MAX {
            caps.current_extent
        } else {
            vk::Extent2D {
                width: size
                    .0
                    .clamp(caps.min_image_extent.width, caps.max_image_extent.width),
                height: size
                    .1
                    .clamp(caps.min_image_extent.height, caps.max_image_extent.height),
            }
        };
        let mut count = caps.min_image_count + 1;
        if caps.max_image_count > 0 {
            count = count.min(caps.max_image_count);
        }
        let old = self.swapchain.take();
        let info = vk::SwapchainCreateInfoKHR::default()
            .surface(self.vk_surface)
            .min_image_count(count)
            .image_format(FORMAT)
            .image_color_space(vk::ColorSpaceKHR::SRGB_NONLINEAR)
            .image_extent(extent)
            .image_array_layers(1)
            .image_usage(vk::ImageUsageFlags::COLOR_ATTACHMENT)
            .image_sharing_mode(vk::SharingMode::EXCLUSIVE)
            .pre_transform(caps.current_transform)
            .composite_alpha(vk::CompositeAlphaFlagsKHR::OPAQUE)
            .present_mode(vk::PresentModeKHR::FIFO)
            .clipped(true)
            .old_swapchain(old.as_ref().map_or(vk::SwapchainKHR::null(), |s| s.handle));
        // SAFETY: the info lives until the call returns; the old swapchain
        // is retired by it and destroyed below.
        let created = unsafe { gpu.swapchain.create_swapchain(&info, None) };
        if let Some(old) = old {
            destroy_swapchain(gpu, old);
        }
        let handle = created.context("creating the swapchain")?;
        let mut swapchain = Swapchain {
            handle,
            size: (extent.width, extent.height),
            images: Vec::new(),
            views: Vec::new(),
            drawn: Vec::new(),
        };
        // On failure the half-made swapchain is destroyed like a whole one.
        let filled = (|| -> Result<()> {
            // SAFETY: the swapchain was just made by this device.
            swapchain.images = unsafe { gpu.swapchain.get_swapchain_images(handle) }
                .context("the swapchain's images")?;
            for &image in &swapchain.images {
                swapchain.views.push(image::view(d, image, FORMAT, true)?);
                // SAFETY: plain creation; destroyed with the swapchain.
                swapchain.drawn.push(
                    unsafe { d.create_semaphore(&vk::SemaphoreCreateInfo::default(), None) }
                        .context("semaphore")?,
                );
            }
            Ok(())
        })();
        if let Err(e) = filled {
            destroy_swapchain(gpu, swapchain);
            return Err(e);
        }
        debug!(
            "swapchain {}x{} with {} images",
            extent.width,
            extent.height,
            swapchain.images.len()
        );
        self.swapchain = Some(swapchain);
        self.stale = false;
        Ok(())
    }

    /// The window is opaque: the compositor need not draw what is under it.
    fn set_opaque(&self, surface: &Surface, compositor: &WlCompositor, qh: &QueueHandle<State>) {
        let Some(swapchain) = &self.swapchain else {
            return;
        };
        let (w, h) = swapchain.size;
        let region: WlRegion = compositor.create_region(qh, ());
        region.add(0, 0, w as i32, h as i32);
        surface.wl.set_opaque_region(Some(&region));
        region.destroy();
    }

    /// Its swapchain and Vulkan surface, once the GPU is idle (the app
    /// waits); the Wayland surface goes with the connection.
    pub(crate) fn destroy(mut self, gpu: &Gpu) {
        if let Some(swapchain) = self.swapchain.take() {
            destroy_swapchain(gpu, swapchain);
        }
        // SAFETY: the GPU is idle; the Wayland surface outlives the Vulkan
        // one (the connection goes after the app's windows).
        unsafe { gpu.instance.surface.destroy_surface(self.vk_surface, None) };
    }
}

fn destroy_swapchain(gpu: &Gpu, swapchain: Swapchain) {
    let d = &gpu.shared.device;
    // SAFETY: the device is idle (callers wait); the images belong to the
    // swapchain and go with it.
    unsafe {
        for view in swapchain.views {
            d.destroy_image_view(view, None);
        }
        for semaphore in swapchain.drawn {
            d.destroy_semaphore(semaphore, None);
        }
        gpu.swapchain.destroy_swapchain(swapchain.handle, None);
    }
}

/// Maximizes the window, or back.
fn toggle_maximized(toplevel: &XdgToplevel, maximized: bool) {
    if maximized {
        toplevel.unset_maximized();
    } else {
        toplevel.set_maximized();
    }
}
