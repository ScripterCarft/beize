//! The pointer's image. The screen shows it on the cursor plane (`output`),
//! so moving the pointer draws nothing: the display engine places the
//! image, as on every Mac and in KWin and Mutter. Images are premultiplied
//! ARGB8888, the format of the cursor plane – and of Xcursor files.
//!
//! A client sets its own image (a surface with an shm buffer) or names one
//! (cursor-shape-v1, as GTK 4, Qt 6 and Chromium do); where no client is
//! under the pointer, the compositor names one: the arrow, or a resize
//! arrow on a window's edge (`input`). Named images come from the cursor
//! theme "default" – Bibata Modern Classic (recipes/bibata-cursor.toml,
//! ADR 0028 §8) – read once each at `SIZE`; one the theme lacks is the
//! arrow. An animated one (waiting) shows each frame for its delay: the
//! state draws again when the next is due (`next_frame`).
//!
//! Without a cursor plane that fits the image, the pointer is drawn into
//! the frame instead (`Cursor::drawn`): then a move redraws the two small
//! rectangles it left and entered. That is the case in VMs: virtio-gpu
//! shows its cursor plane only to compositors that set the cursor's
//! hotspot (DRM_CLIENT_CAP_CURSOR_PLANE_HOTSPOT, Linux 6.8), which
//! Smithay's KMS layer cannot yet.

use std::collections::HashMap;
use std::time::Duration;

use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::element::memory::MemoryRenderBuffer;
use smithay::backend::renderer::utils::with_renderer_surface_state;
use smithay::input::pointer::{CursorIcon, CursorImageAttributes, CursorImageStatus};
use smithay::reexports::wayland_server::protocol::wl_shm;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::Transform;
use smithay::wayland::compositor::with_states;
use smithay::wayland::shm::with_buffer_contents;
use tracing::{debug, info, warn};
use xcursor::CursorTheme;

/// The theme's name: "default" inherits Beize's (recipes/bibata-cursor.toml),
/// as libXcursor and wayland-cursor look it up.
const THEME: &str = "default";
/// The pointer's size in pixels at scale 1: GNOME's and KDE's default.
const SIZE: u32 = 24;

/// A cursor image: premultiplied ARGB8888, rows without padding. `id`
/// changes with the pixels, so the screen copies them only then.
#[derive(Debug)]
pub struct Image {
    pub id: u64,
    pub width: u32,
    pub height: u32,
    pub hotspot: (i32, i32),
    pub pixels: Vec<u8>,
}

/// A named cursor's frames, each with how long it shows; one for a still
/// one.
#[derive(Debug, Default)]
struct Frames(Vec<(Image, Duration)>);

impl Frames {
    /// The frame shown `elapsed` after it started, and when the next is
    /// due (from the start); none for a still one.
    fn at(&self, elapsed: Duration) -> Option<(&Image, Option<Duration>)> {
        let (first, _) = self.0.first()?;
        let cycle: Duration = self.0.iter().map(|(_, delay)| *delay).sum();
        if self.0.len() == 1 || cycle.is_zero() {
            return Some((first, None));
        }
        let cycles = elapsed.as_nanos() / cycle.as_nanos();
        let mut end = cycle * u32::try_from(cycles).unwrap_or(u32::MAX);
        let into = elapsed.saturating_sub(end);
        let mut shown = Duration::ZERO;
        for (image, delay) in &self.0 {
            shown += *delay;
            if into < shown {
                end += shown;
                return Some((image, Some(end)));
            }
        }
        Some((first, None))
    }
}

pub struct Cursor {
    pub status: CursorImageStatus,
    theme: CursorTheme,
    /// Named images, read on first use; empty where the theme has none.
    named: HashMap<CursorIcon, Frames>,
    /// When the named image was set: its animation counts from there.
    since: Duration,
    /// The client's image, from its cursor surface's last commit.
    surface: Option<Image>,
    next_id: u64,
    /// The image drawn into the frame (`drawn`), by its id.
    drawn: Option<(u64, MemoryRenderBuffer)>,
}

impl Cursor {
    pub fn new() -> Cursor {
        let mut cursor = Cursor {
            status: CursorImageStatus::Hidden,
            theme: CursorTheme::load(THEME),
            named: HashMap::new(),
            since: Duration::ZERO,
            surface: None,
            next_id: 1,
            drawn: None,
        };
        cursor.set(CursorImageStatus::default_named(), Duration::ZERO);
        cursor
    }

    /// The image to show `now`, or none (hidden, or a client's surface
    /// without a usable buffer yet).
    pub fn image(&self, now: Duration) -> Option<&Image> {
        match &self.status {
            CursorImageStatus::Hidden => None,
            CursorImageStatus::Named(icon) => self
                .frames(*icon)
                .at(now.saturating_sub(self.since))
                .map(|(image, _)| image),
            CursorImageStatus::Surface(_) => self.surface.as_ref(),
        }
    }

    /// When an animated image shows its next frame, after `now`.
    pub fn next_frame(&self, now: Duration) -> Option<Duration> {
        let CursorImageStatus::Named(icon) = &self.status else {
            return None;
        };
        let (_, next) = self.frames(*icon).at(now.saturating_sub(self.since))?;
        next.map(|next| self.since + next)
    }

    /// The named image, else the arrow.
    fn frames(&self, icon: CursorIcon) -> &Frames {
        static NONE: Frames = Frames(Vec::new());
        [icon, CursorIcon::Default]
            .iter()
            .filter_map(|icon| self.named.get(icon))
            .find(|frames| !frames.0.is_empty())
            .unwrap_or(&NONE)
    }

    /// Shows `status` from `now`; the same named image again changes
    /// nothing, so an animation runs on.
    pub fn set(&mut self, status: CursorImageStatus, now: Duration) {
        if let (CursorImageStatus::Named(old), CursorImageStatus::Named(new)) =
            (&self.status, &status)
            && old == new
        {
            return;
        }
        let surface = match &status {
            CursorImageStatus::Surface(surface) => Some(surface.clone()),
            _ => None,
        };
        if let CursorImageStatus::Named(icon) = &status {
            for icon in [*icon, CursorIcon::Default] {
                if !self.named.contains_key(&icon) {
                    let frames = self.load(icon);
                    self.named.insert(icon, frames);
                }
            }
        }
        self.status = status;
        self.since = now;
        self.surface = None;
        if let Some(surface) = surface {
            self.committed(&surface);
        }
    }

    /// The theme's frames for `icon` at `SIZE`, by its name or an older
    /// one (X11's cursor font, which themes link).
    fn load(&mut self, icon: CursorIcon) -> Frames {
        let names = std::iter::once(icon.name()).chain(icon.alt_names().iter().copied());
        let Some(file) = names.clone().find_map(|name| self.theme.load_icon(name)) else {
            let names: Vec<&str> = names.collect();
            warn!("the cursor theme has no {}", names.join(" or "));
            return Frames::default();
        };
        let images = std::fs::read(&file)
            .ok()
            .and_then(|data| xcursor::parser::parse_xcursor(&data))
            .unwrap_or_default();
        // The size nearest `SIZE`; its frames in the file's order.
        let Some(size) = images
            .iter()
            .map(|i| i.size)
            .min_by_key(|size| size.abs_diff(SIZE))
        else {
            warn!("the cursor {} cannot be read", file.display());
            return Frames::default();
        };
        let mut frames = Vec::new();
        for image in images.into_iter().filter(|i| i.size == size) {
            let id = self.next_id;
            self.next_id += 1;
            let (Ok(x), Ok(y)) = (i32::try_from(image.xhot), i32::try_from(image.yhot)) else {
                continue;
            };
            frames.push((
                Image {
                    id,
                    width: image.width,
                    height: image.height,
                    hotspot: (x, y),
                    // The file's bytes: premultiplied ARGB, little-endian.
                    pixels: image.pixels_rgba,
                },
                Duration::from_millis(image.delay.into()),
            ));
        }
        // Once each, for the smoke test (tools/bz, `desktop`).
        let n = frames.len();
        let s = if n == 1 { "" } else { "s" };
        info!("cursor {}: {n} frame{s} at {size} px", icon.name());
        Frames(frames)
    }

    /// The surface committed: if it is the cursor, its new image.
    pub fn committed(&mut self, surface: &WlSurface) -> bool {
        if !matches!(&self.status, CursorImageStatus::Surface(s) if s == surface) {
            return false;
        }
        let id = self.next_id;
        self.next_id += 1;
        self.surface = surface_image(surface, id);
        true
    }

    pub fn surface(&self) -> Option<&WlSurface> {
        match &self.status {
            CursorImageStatus::Surface(surface) => Some(surface),
            _ => None,
        }
    }

    /// The image `now` as a buffer to draw into the frame, where the
    /// screen has no cursor plane for it; kept while the image stays, so
    /// the renderer uploads it once. With its hotspot.
    pub fn drawn(&mut self, now: Duration) -> Option<(MemoryRenderBuffer, (i32, i32))> {
        let (id, hotspot) = self.image(now).map(|i| (i.id, i.hotspot))?;
        if self.drawn.as_ref().is_none_or(|(drawn, _)| *drawn != id) {
            let image = self.image(now)?;
            let buffer = MemoryRenderBuffer::from_slice(
                &image.pixels,
                Fourcc::Argb8888,
                (image.width as i32, image.height as i32),
                1,
                Transform::Normal,
                None,
            );
            self.drawn = Some((id, buffer));
        }
        self.drawn
            .as_ref()
            .map(|(_, buffer)| (buffer.clone(), hotspot))
    }
}

/// The client's image: its shm buffer, copied (cursors are tiny and change
/// rarely). Other buffers (dmabuf) are not shown.
fn surface_image(surface: &WlSurface, id: u64) -> Option<Image> {
    let hotspot = with_states(surface, |states| {
        states
            .data_map
            .get::<std::sync::Mutex<CursorImageAttributes>>()
            .and_then(|a| a.lock().ok().map(|a| a.hotspot))
    })
    .unwrap_or_default();
    let buffer = with_renderer_surface_state(surface, |s| s.buffer().cloned()).flatten()?;
    let copied = with_buffer_contents(&buffer, |pool, len, data| {
        let opaque = match data.format {
            wl_shm::Format::Argb8888 => false,
            wl_shm::Format::Xrgb8888 => true,
            other => {
                debug!("cursor buffer in {other:?}: not shown");
                return None;
            }
        };
        let (width, height) = (
            u32::try_from(data.width).ok()?,
            u32::try_from(data.height).ok()?,
        );
        let offset = usize::try_from(data.offset).ok()?;
        let stride = usize::try_from(data.stride).ok()?;
        let row = width as usize * 4;
        let end = offset.checked_add(stride.checked_mul(height as usize)?)?;
        if stride < row || end > len {
            return None;
        }
        // SAFETY: Smithay maps the client's pool for `len` bytes for as long
        // as this closure runs; the rows read lie inside it (checked above).
        // The client may write meanwhile – then the image is torn, which
        // only it sees.
        let pool = unsafe { std::slice::from_raw_parts(pool, len) };
        let mut pixels = Vec::with_capacity(row * height as usize);
        for y in 0..height as usize {
            let start = offset + y * stride;
            pixels.extend_from_slice(&pool[start..start + row]);
        }
        if opaque {
            pixels
                .as_chunks_mut::<4>()
                .0
                .iter_mut()
                .for_each(|p| p[3] = 0xff);
        }
        Some(Image {
            id,
            width,
            height,
            hotspot: (hotspot.x, hotspot.y),
            pixels,
        })
    });
    copied.ok().flatten()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image(id: u64) -> Image {
        Image {
            id,
            width: 1,
            height: 1,
            hotspot: (0, 0),
            pixels: vec![0; 4],
        }
    }

    #[test]
    fn a_still_image_has_no_next_frame() {
        let frames = Frames(vec![(image(1), Duration::from_millis(40))]);
        let (shown, next) = frames.at(Duration::from_secs(5)).unwrap();
        assert_eq!((shown.id, next), (1, None));
        assert!(Frames::default().at(Duration::ZERO).is_none());
    }

    #[test]
    fn an_animation_shows_each_frame_for_its_delay_and_runs_round() {
        let ms = Duration::from_millis;
        let frames = Frames(vec![(image(1), ms(40)), (image(2), ms(60))]);
        let at = |t| {
            let (image, next) = frames.at(ms(t)).unwrap();
            (image.id, next)
        };
        assert_eq!(at(0), (1, Some(ms(40))));
        assert_eq!(at(39), (1, Some(ms(40))));
        assert_eq!(at(40), (2, Some(ms(100))));
        // The second round.
        assert_eq!(at(100), (1, Some(ms(140))));
        assert_eq!(at(1050), (2, Some(ms(1100))));
    }
}
