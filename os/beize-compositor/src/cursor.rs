//! The pointer's image. The screen shows it on the cursor plane (`output`),
//! so moving the pointer draws nothing: the display engine places the
//! image, as on every Mac and in KWin and Mutter. Images are premultiplied
//! ARGB8888, the format of the cursor plane.
//!
//! A client may set its own image (a surface with an shm buffer); otherwise
//! Beize shows its arrow. Beize has no cursor theme yet – its icons come
//! with the shell (ADR 0027 §5) – so every named cursor is the arrow for now.
//!
//! Without a cursor plane that fits the image, the pointer is drawn into
//! the frame instead (`Cursor::drawn`): then a move redraws the two small
//! rectangles it left and entered. That is the case in VMs: virtio-gpu
//! shows its cursor plane only to compositors that set the cursor's
//! hotspot (DRM_CLIENT_CAP_CURSOR_PLANE_HOTSPOT, Linux 6.8), which
//! Smithay's KMS layer cannot yet.

use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::element::memory::MemoryRenderBuffer;
use smithay::backend::renderer::utils::with_renderer_surface_state;
use smithay::input::pointer::{CursorImageAttributes, CursorImageStatus};
use smithay::reexports::wayland_server::protocol::wl_shm;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::Transform;
use smithay::wayland::compositor::with_states;
use smithay::wayland::shm::with_buffer_contents;
use tracing::debug;

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

pub struct Cursor {
    pub status: CursorImageStatus,
    arrow: Image,
    /// The client's image, from its cursor surface's last commit.
    surface: Option<Image>,
    next_id: u64,
    /// The image drawn into the frame (`drawn`), by its id.
    drawn: Option<(u64, MemoryRenderBuffer)>,
}

impl Cursor {
    pub fn new() -> Cursor {
        Cursor {
            status: CursorImageStatus::default_named(),
            arrow: arrow(0),
            surface: None,
            next_id: 1,
            drawn: None,
        }
    }

    /// The image to show, or none (hidden, or a client's surface without
    /// a usable buffer yet).
    pub fn image(&self) -> Option<&Image> {
        match &self.status {
            CursorImageStatus::Hidden => None,
            CursorImageStatus::Named(_) => Some(&self.arrow),
            CursorImageStatus::Surface(_) => self.surface.as_ref(),
        }
    }

    pub fn set(&mut self, status: CursorImageStatus) {
        let surface = match &status {
            CursorImageStatus::Surface(surface) => Some(surface.clone()),
            _ => None,
        };
        self.status = status;
        self.surface = None;
        if let Some(surface) = surface {
            self.committed(&surface);
        }
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

    /// The image as a buffer to draw into the frame, where the screen has
    /// no cursor plane for it; kept while the image stays, so the renderer
    /// uploads it once. With its hotspot.
    pub fn drawn(&mut self) -> Option<(MemoryRenderBuffer, (i32, i32))> {
        let (id, hotspot) = self.image().map(|i| (i.id, i.hotspot))?;
        if self.drawn.as_ref().is_none_or(|(drawn, _)| *drawn != id) {
            let image = self.image()?;
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

/// Beize's arrow, until its icon set brings one: black with a white rim,
/// edges antialiased (4×4 samples per pixel).
fn arrow(id: u64) -> Image {
    // The outline, tip at the origin, in pixels.
    const SHAPE: [(f32, f32); 7] = [
        (0.0, 0.0),
        (0.0, 16.5),
        (4.0, 12.8),
        (6.8, 19.3),
        (9.3, 18.2),
        (6.6, 11.8),
        (11.6, 11.8),
    ];
    const RIM: f32 = 1.0;
    const MARGIN: f32 = 1.0;
    const SIZE: u32 = 24;
    const SAMPLES: u32 = 4;
    let mut pixels = Vec::with_capacity((SIZE * SIZE * 4) as usize);
    for py in 0..SIZE {
        for px in 0..SIZE {
            let (mut cover, mut white) = (0.0f32, 0.0f32);
            for sy in 0..SAMPLES {
                for sx in 0..SAMPLES {
                    let x = px as f32 + (sx as f32 + 0.5) / SAMPLES as f32 - MARGIN;
                    let y = py as f32 + (sy as f32 + 0.5) / SAMPLES as f32 - MARGIN;
                    if inside(&SHAPE, x, y) {
                        cover += 1.0;
                        if distance(&SHAPE, x, y) < RIM {
                            white += 1.0;
                        }
                    }
                }
            }
            let n = (SAMPLES * SAMPLES) as f32;
            // Premultiplied: white contributes its coverage, black nothing.
            let value = (white / n * 255.0).round() as u8;
            let alpha = (cover / n * 255.0).round() as u8;
            pixels.extend_from_slice(&[value, value, value, alpha]);
        }
    }
    Image {
        id,
        width: SIZE,
        height: SIZE,
        hotspot: (MARGIN as i32, MARGIN as i32),
        pixels,
    }
}

/// Even-odd rule.
fn inside(shape: &[(f32, f32)], x: f32, y: f32) -> bool {
    let mut inside = false;
    let mut j = shape.len() - 1;
    for i in 0..shape.len() {
        let ((xi, yi), (xj, yj)) = (shape[i], shape[j]);
        if (yi > y) != (yj > y) && x < (xj - xi) * (y - yi) / (yj - yi) + xi {
            inside = !inside;
        }
        j = i;
    }
    inside
}

/// Distance to the outline.
fn distance(shape: &[(f32, f32)], x: f32, y: f32) -> f32 {
    let mut nearest = f32::MAX;
    for i in 0..shape.len() {
        let (ax, ay) = shape[i];
        let (bx, by) = shape[(i + 1) % shape.len()];
        let (dx, dy) = (bx - ax, by - ay);
        let t = (((x - ax) * dx + (y - ay) * dy) / (dx * dx + dy * dy)).clamp(0.0, 1.0);
        let (cx, cy) = (ax + t * dx - x, ay + t * dy - y);
        nearest = nearest.min((cx * cx + cy * cy).sqrt());
    }
    nearest
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pixel(image: &Image, x: u32, y: u32) -> [u8; 4] {
        let i = ((y * image.width + x) * 4) as usize;
        image.pixels[i..i + 4].try_into().unwrap_or_default()
    }

    #[test]
    fn the_arrow_is_black_with_a_white_rim() {
        let arrow = arrow(0);
        assert_eq!(
            arrow.pixels.len(),
            (arrow.width * arrow.height * 4) as usize
        );
        // Inside the shaft: opaque black.
        assert_eq!(pixel(&arrow, 3, 8), [0, 0, 0, 255]);
        // On the left edge: the rim, opaque white.
        assert_eq!(pixel(&arrow, 1, 8), [255, 255, 255, 255]);
        // Right of the tip and below the tail: nothing.
        assert_eq!(pixel(&arrow, 10, 2), [0, 0, 0, 0]);
        assert_eq!(pixel(&arrow, 2, 23), [0, 0, 0, 0]);
        // Premultiplied: no channel above alpha.
        assert!(arrow.pixels.as_chunks::<4>().0.iter().all(|p| p[0] <= p[3]));
    }
}
