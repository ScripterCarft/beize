//! Beize's background and the base glass below the shell's panels (ADR
//! 0028 §6). Both are drawn once per change – the screen's size or the
//! appearance – by the backdrop's shader (`beize_vulkan`, `backdrop.frag`),
//! then only read: the background is computed, no image file; the glass is
//! the background blurred, its colours lifted and tinted for the
//! appearance (Microsoft's Mica works so). A frame adds to the glass only
//! grain and a rim of light, per pixel (`GLASS`), where the shell asked for
//! it (`glass`). Live glass shows what lies behind it, windows too
//! (`LiveGlass`).

use beize_compositor::error::{Context, Result, error};
use beize_compositor::render::{Material, Target, VkRenderer, VkTexture};
use smithay::backend::renderer::damage::OutputDamageTracker;
use smithay::backend::renderer::element::texture::{TextureBuffer, TextureRenderElement};
use smithay::backend::renderer::element::utils::{Relocate, RelocateRenderElement};
use smithay::backend::renderer::element::{Element, Id, Kind, RenderElement};
use smithay::backend::renderer::utils::{CommitCounter, OpaqueRegions};
use smithay::backend::renderer::{Bind, RendererSuper};
use smithay::reexports::wayland_server::Resource;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{Buffer, Logical, Physical, Point, Rectangle, Scale, Size, Transform};
use smithay::wayland::compositor::with_states;
use std::sync::Mutex;

use crate::corners::Effects;
use crate::glass::{GlassState, Shape};
use beize_vulkan::pipeline::corner_extent;

/// Light or dark: the background's colours, the glass's tint, and the
/// shell's ink (it hears it over beize-shell-v1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Appearance {
    Light,
    Dark,
}

impl Appearance {
    /// Light, as macOS starts. Until Beize's settings hold the choice,
    /// `BEIZE_APPEARANCE=dark` in the desktop's environment shows dark (for
    /// tests and screenshots).
    pub fn from_environment() -> Appearance {
        match std::env::var("BEIZE_APPEARANCE").as_deref() {
            Ok("dark") => Appearance::Dark,
            _ => Appearance::Light,
        }
    }

    /// As the backdrop's shader and beize-shell-v1 number it.
    pub fn index(self) -> u32 {
        match self {
            Appearance::Light => 0,
            Appearance::Dark => 1,
        }
    }

    /// The glass of the appearance: the background blurred (Dual Kawase,
    /// four halvings and two doublings back – the glass is a quarter of
    /// the screen's size, soft enough for the filter to scale it up), its
    /// saturation lifted, then tinted: nearly white or nearly black, as
    /// the materials of macOS and Windows are (Mica: #F3F3F3 at 50 % or
    /// #202020 at 80 %; Beize's lighter, approved on 24.10. from mock-ups,
    /// estimated, not measured).
    fn material(self) -> Material {
        let (tint, alpha) = match self {
            Appearance::Light => ([0xf6, 0xf6, 0xf6], 0.30),
            Appearance::Dark => ([0x1e, 0x1e, 0x20], 0.45),
        };
        let linear = |c: u8| decoded(f32::from(c) / 255.0) * alpha;
        Material {
            down: 4,
            up: 2,
            offset: 2.0,
            saturation: 1.8,
            tint: [linear(tint[0]), linear(tint[1]), linear(tint[2]), alpha],
        }
    }

    /// The rim of light along the glass (premultiplied): softer on dark.
    fn rim(self) -> [f32; 4] {
        let a = match self {
            Appearance::Light => 0.6,
            Appearance::Dark => 0.22,
        };
        [a, a, a, a]
    }
}

/// The glass's grain: about one and a half 8-bit steps either way, enough
/// to break banding in the soft gradient and give the glass a surface.
const GRAIN: f32 = 1.5 / 255.0;

fn decoded(encoded: f32) -> f32 {
    if encoded <= 0.04045 {
        encoded / 12.92
    } else {
        ((encoded + 0.055) / 1.055).powf(2.4)
    }
}

/// The background and its glass for one screen size and appearance.
pub struct Backdrop {
    size: Size<i32, Physical>,
    appearance: Appearance,
    wallpaper: TextureBuffer<VkTexture>,
    glass: VkTexture,
    /// Counts the backdrops drawn: a glass element knows a new one by it.
    generation: u64,
}

impl Backdrop {
    /// Draws the background and its glass with the next frame;
    /// `generation` numbers it.
    pub fn new(
        renderer: &mut VkRenderer,
        size: Size<i32, Physical>,
        appearance: Appearance,
        generation: u64,
    ) -> Result<Backdrop> {
        let buffer: Size<i32, Buffer> = (size.w, size.h).into();
        let wallpaper = renderer.draw_wallpaper(buffer, appearance.index())?;
        let glass = renderer.draw_glass(&wallpaper, appearance.material())?;
        // Opaque: nothing below it is drawn.
        let opaque = Some(vec![Rectangle::from_size(buffer)]);
        let wallpaper =
            TextureBuffer::from_texture(renderer, wallpaper, 1, Transform::Normal, opaque);
        Ok(Backdrop {
            size,
            appearance,
            wallpaper,
            glass,
            generation,
        })
    }

    /// The background, behind everything.
    pub fn wallpaper(&self) -> TextureRenderElement<VkTexture> {
        TextureRenderElement::from_texture_buffer(
            Point::from((0.0, 0.0)),
            &self.wallpaper,
            None,
            None,
            None,
            Kind::Unspecified,
        )
    }

    /// The glass below `surface`, a panel at `at`, if it asked for one:
    /// base glass; live glass gets what lies behind it from `LiveGlass`.
    pub fn glass(&self, surface: &WlSurface, at: Point<i32, Logical>) -> Option<Glass> {
        let (state, drawn) = with_states(surface, |states| {
            let state = *states.cached_state.get::<GlassState>().current();
            states
                .data_map
                .insert_if_missing_threadsafe(|| Mutex::new(Drawn::default()));
            let drawn = states.data_map.get::<Mutex<Drawn>>().and_then(|drawn| {
                let mut drawn = drawn.lock().ok()?;
                let key = state.shape.map(|s| (s, at, self.generation));
                if drawn.last != key {
                    drawn.last = key;
                    drawn.commit.increment();
                }
                Some((drawn.id.clone(), drawn.commit))
            });
            (state, drawn)
        });
        let shape = state.shape?;
        let (id, commit) = drawn?;
        let mut rect = shape.rect;
        rect.loc += at;
        Some(Glass {
            id,
            commit,
            shape: rect.to_physical(1),
            radius: shape.radius,
            rim: if shape.rim {
                self.appearance.rim()
            } else {
                [0.0; 4]
            },
            texture: self.glass.clone(),
            covers: Rectangle::from_size(self.size),
            live: state.live.then(|| surface.clone()),
        })
    }
}

/// What a surface's glass was last drawn for, in its data.
struct Drawn {
    id: Id,
    commit: CommitCounter,
    last: Option<(Shape, Point<i32, Logical>, u64)>,
}

impl Default for Drawn {
    fn default() -> Drawn {
        Drawn {
            id: Id::new(),
            commit: CommitCounter::default(),
            last: None,
        }
    }
}

/// The glass below a panel: drawn again when its shape, its place or what
/// it shows changes.
pub struct Glass {
    id: Id,
    commit: CommitCounter,
    shape: Rectangle<i32, Physical>,
    radius: f32,
    rim: [f32; 4],
    /// A blurred copy of what lies in `covers` (the screen's coordinates).
    texture: VkTexture,
    covers: Rectangle<i32, Physical>,
    /// Live glass: the surface it lies below (`LiveGlass`).
    live: Option<WlSurface>,
}

impl Glass {
    /// The surface of live glass, which shows what lies behind it.
    pub fn live(&self) -> Option<&WlSurface> {
        self.live.as_ref()
    }
}

impl Element for Glass {
    fn id(&self) -> &Id {
        &self.id
    }

    fn current_commit(&self) -> CommitCounter {
        self.commit
    }

    fn src(&self) -> Rectangle<f64, Buffer> {
        let size = self.shape.size;
        Rectangle::from_size((f64::from(size.w), f64::from(size.h)).into())
    }

    fn geometry(&self, _scale: Scale<f64>) -> Rectangle<i32, Physical> {
        self.shape
    }

    /// All of it but the corners: what lies below is not drawn.
    fn opaque_regions(&self, _scale: Scale<f64>) -> OpaqueRegions<i32, Physical> {
        let (w, h) = (self.shape.size.w, self.shape.size.h);
        let c = corner_extent(self.radius, w as f32, h as f32).ceil() as i32;
        if 2 * c >= w || 2 * c >= h {
            return OpaqueRegions::default();
        }
        OpaqueRegions::from_slice(&[
            Rectangle::new((c, 0).into(), (w - 2 * c, h).into()),
            Rectangle::new((0, c).into(), (c, h - 2 * c).into()),
            Rectangle::new((w - c, c).into(), (c, h - 2 * c).into()),
        ])
    }
}

impl<R: Effects> RenderElement<R> for Glass {
    fn draw(
        &self,
        frame: &mut R::Frame<'_, '_>,
        _src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        _opaque_regions: &[Rectangle<i32, Physical>],
    ) -> std::result::Result<(), <R as RendererSuper>::Error> {
        // Where it is drawn: elsewhere when relocated (behind live glass).
        let mut covers = self.covers;
        covers.loc += dst.loc - self.shape.loc;
        R::draw_glass(
            frame,
            dst,
            damage,
            dst,
            self.radius,
            &self.texture,
            covers,
            GRAIN,
            self.rim,
        );
        Ok(())
    }
}

/// Around live glass, what lies behind it is drawn this much further, so
/// the blur at its edge takes in what is beside it.
const LIVE_MARGIN: i32 = 64;

/// Live glass (ADR 0028 §6): what lies behind a panel – windows, their
/// shadows, the background – drawn into an image of its own with a damage
/// tracker of its own, never read back from the screen (which holds the
/// glass itself), and blurred only when something there changed. Lifted
/// and tinted as base glass is, so the two look the same over the
/// background alone.
pub struct LiveGlass {
    surface: WlSurface,
    /// Where it draws, on the screen: the glass and `LIVE_MARGIN` around.
    region: Rectangle<i32, Physical>,
    target: Target,
    damage: OutputDamageTracker,
    drawn: bool,
    glass: Option<(VkTexture, Appearance)>,
    id: Id,
    commit: CommitCounter,
}

impl LiveGlass {
    /// Live glass below `surface`.
    pub fn new(renderer: &mut VkRenderer, surface: &WlSurface) -> Result<LiveGlass> {
        let size = (1, 1);
        Ok(LiveGlass {
            surface: surface.clone(),
            region: Rectangle::from_size(size.into()),
            target: renderer.sampled_target(size.into())?,
            damage: OutputDamageTracker::new(size, 1.0, Transform::Normal),
            drawn: false,
            glass: None,
            id: Id::new(),
            commit: CommitCounter::default(),
        })
    }

    pub fn is_for(&self, surface: &WlSurface) -> bool {
        &self.surface == surface
    }

    pub fn alive(&self) -> bool {
        self.surface.is_alive()
    }

    /// Draws what lies `behind` (front to back) the panel's glass `glass`
    /// on a `screen`, blurs it if it changed, and gives `glass` the result.
    pub fn update<E: RenderElement<VkRenderer>>(
        &mut self,
        renderer: &mut VkRenderer,
        behind: &[E],
        glass: &mut Glass,
        screen: Size<i32, Physical>,
        appearance: Appearance,
    ) -> Result<()> {
        let s = glass.shape;
        let around = Rectangle::new(
            (s.loc.x - LIVE_MARGIN, s.loc.y - LIVE_MARGIN).into(),
            (s.size.w + 2 * LIVE_MARGIN, s.size.h + 2 * LIVE_MARGIN).into(),
        );
        let region = around
            .intersection(Rectangle::from_size(screen))
            .context("live glass off the screen")?;
        if region.size != self.region.size {
            let size = (region.size.w, region.size.h);
            self.target = renderer.sampled_target(size.into())?;
            self.damage = OutputDamageTracker::new(size, 1.0, Transform::Normal);
            self.drawn = false;
        }
        self.region = region;
        let origin = Point::from((-region.loc.x, -region.loc.y));
        let relocated: Vec<_> = behind
            .iter()
            .map(|e| RelocateRenderElement::from_element(e, origin, Relocate::Relative))
            .collect();
        let age = usize::from(self.drawn);
        let changed = {
            let mut framebuffer = renderer.bind(&mut self.target)?;
            self.damage
                .render_output(renderer, &mut framebuffer, age, &relocated, CLEAR)
                .map_err(|e| error(format!("drawing behind live glass: {e:?}")))?
                .damage
                .is_some()
        };
        self.drawn = true;
        let stale = self.glass.as_ref().is_none_or(|(_, a)| *a != appearance);
        if changed || stale {
            let behind = self
                .target
                .texture()
                .context("live glass without a texture")?;
            let blurred = renderer.draw_glass(behind, appearance.material())?;
            self.glass = Some((blurred, appearance));
            self.commit.increment();
        }
        if let Some((texture, _)) = &self.glass {
            glass.texture = texture.clone();
            glass.covers = region;
            glass.id = self.id.clone();
            glass.commit = self.commit;
        }
        Ok(())
    }
}

/// What nothing covers behind live glass (never on the screen: the
/// background covers all).
const CLEAR: [f32; 4] = [0.0, 0.0, 0.0, 1.0];
