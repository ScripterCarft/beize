//! Beize's background and the glass below the shell's panels (ADR 0028
//! §6). Both are drawn once per change – the screen's size or the
//! appearance – by the backdrop's shader (`beize_vulkan`, `backdrop.frag`),
//! then only read: the background is computed, no image file; base glass
//! is the background blurred, its colours lifted and tinted for the
//! appearance (Microsoft's Mica works so), made when a panel first asks
//! for it. A frame adds to the glass only grain and a rim of light, per
//! pixel (`GLASS`), where the shell asked for it (`glass`). Live glass
//! shows what lies behind it, windows too (`LiveGlass`).
//!
//! The menu bar has no background, as macOS' since 26: a faint darkening
//! scrim within it, from its top edge to nothing at its bottom – barely
//! seen, but there, as Apple lays a subtle gradient over the wallpaper –
//! helps its white ink; only where the top is very light is the ink dark
//! (`Tone`, measured once per background with the scrim). Over a
//! fullscreen window it lies on live glass instead (`plate`), its ink for
//! the appearance: no desktop is behind it there, and the glass takes on
//! the window's colours, as the dock's does (the owner, 24.10.; macOS
//! shows the bar opaque there).

use crate::error::{Context, Result, error};
use crate::render::{Material, VkRenderer, VkTexture};
use beize_draw::measures;
use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::element::texture::{TextureBuffer, TextureRenderElement};
use smithay::backend::renderer::element::{Element, Id, Kind, RenderElement};
use smithay::backend::renderer::utils::{CommitCounter, OpaqueRegions};
use smithay::backend::renderer::{ExportMem, ImportMem, RendererSuper};
use smithay::reexports::wayland_server::Resource;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{Buffer, Logical, Physical, Point, Rectangle, Scale, Size, Transform};
use smithay::wayland::compositor::with_states;
use std::sync::{Arc, Mutex};

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

/// How light the background behind the menu bar is: light wants dark ink,
/// dark wants light ink. The shell hears it over beize-shell-v1.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    Light,
    Dark,
}

/// The strip measured for the menu bar's tone: the bar.
const BAR_STRIP: i32 = measures::MENU_BAR as i32;
/// White ink down to a contrast of 2:1: 1.05 / (L + 0.05) >= 2, so a
/// relative luminance L of at most 0.475 behind it. macOS keeps its bar
/// white on all but very light backgrounds (the owner's Mac; Apple names
/// no rule), below WCAG's 3:1 for controls – dark ink only where it is
/// really light.
const WHITE_UP_TO: f64 = 1.05 / 2.0 - 0.05;
/// The scrim: black, only within the bar, from `SCRIM_TOP` at the screen's
/// edge eased out (smoothstep) to nothing at the bar's bottom – no edge
/// below it. Barely seen (the owner: "du siehst ihn kaum, aber er ist
/// da"); Apple publishes no values: estimated, judged on screenshots.
const SCRIM_HEIGHT: i32 = measures::MENU_BAR as i32;
const SCRIM_TOP: f32 = 0.15;

/// The scrim's opacity in row `y`.
fn scrim_alpha(y: i32) -> f32 {
    let t = ((y as f32 + 0.5) / SCRIM_HEIGHT as f32).min(1.0);
    SCRIM_TOP * (1.0 - t * t * (3.0 - 2.0 * t))
}

fn decoded(encoded: f32) -> f32 {
    if encoded <= 0.04045 {
        encoded / 12.92
    } else {
        ((encoded + 0.055) / 1.055).powf(2.4)
    }
}

/// The background, its scrim and base glass for one screen size and
/// appearance.
pub struct Backdrop {
    size: Size<i32, Physical>,
    appearance: Appearance,
    wallpaper: TextureBuffer<VkTexture>,
    /// The same, as the glass reads it.
    picture: VkTexture,
    tone: Tone,
    scrim: TextureBuffer<VkTexture>,
    /// Base glass, made when a panel first asks for it.
    glass: Option<VkTexture>,
    /// The glass below the menu bar over a fullscreen window (`plate`)
    /// until `LiveGlass` first gives it what lies behind: drawn again when
    /// its place changes.
    plate: (Id, CommitCounter, Option<Rectangle<i32, Logical>>),
    /// Counts the backdrops drawn: a glass element knows a new one by it.
    generation: u64,
}

impl Backdrop {
    /// Draws the background with the next frame and measures it behind the
    /// menu bar (waiting for the GPU, once per background); `generation`
    /// numbers it.
    pub fn new(
        renderer: &mut VkRenderer,
        size: Size<i32, Physical>,
        appearance: Appearance,
        generation: u64,
    ) -> Result<Backdrop> {
        let buffer: Size<i32, Buffer> = (size.w, size.h).into();
        let wallpaper = renderer.draw_wallpaper(buffer, appearance.index())?;
        let tone = tone(renderer, &wallpaper)?;
        let scrim = scrim(renderer)?;
        // Opaque: nothing below it is drawn.
        let opaque = Some(vec![Rectangle::from_size(buffer)]);
        let picture = wallpaper.clone();
        let wallpaper =
            TextureBuffer::from_texture(renderer, wallpaper, 1, Transform::Normal, opaque);
        Ok(Backdrop {
            size,
            appearance,
            wallpaper,
            picture,
            tone,
            scrim,
            glass: None,
            plate: (Id::new(), CommitCounter::default(), None),
            generation,
        })
    }

    /// Base glass, made now if nothing asked for it before.
    fn base(&mut self, renderer: &mut VkRenderer) -> Option<VkTexture> {
        if let Some(glass) = &self.glass {
            return Some(glass.clone());
        }
        match renderer.draw_glass(&self.picture, self.appearance.material(), &mut Vec::new()) {
            Ok(glass) => Some(self.glass.insert(glass).clone()),
            Err(e) => {
                tracing::warn!("base glass: {e}");
                None
            }
        }
    }

    /// Live glass over `rect`, square and without a rim: below the menu bar
    /// where a fullscreen window lies behind it. It reads the background
    /// until `LiveGlass` gives it what lies behind it, in the same frame.
    pub fn plate(&mut self, rect: Rectangle<i32, Logical>) -> Glass {
        let (id, commit, last) = &mut self.plate;
        if *last != Some(rect) {
            *last = Some(rect);
            commit.increment();
        }
        Glass {
            id: id.clone(),
            commit: *commit,
            shape: rect.to_physical(1),
            radius: 0.0,
            rim: [0.0; 4],
            texture: self.picture.clone(),
            covers: Rectangle::from_size(self.size),
            live: Some(Behind::Plate),
            region: None,
            blur: None,
        }
    }

    /// The menu bar's ink: on glass over a fullscreen window (`plate`) as
    /// the appearance has it, else as measured on the background.
    pub fn bar_tone(&self, on_plate: bool) -> Tone {
        match (on_plate, self.appearance) {
            (false, _) => self.tone,
            (true, Appearance::Light) => Tone::Light,
            (true, Appearance::Dark) => Tone::Dark,
        }
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

    /// The scrim along the top of the background, below the menu bar.
    pub fn scrim(&self) -> TextureRenderElement<VkTexture> {
        TextureRenderElement::from_texture_buffer(
            Point::from((0.0, 0.0)),
            &self.scrim,
            None,
            // Its one column, stretched: without a source Smithay would read
            // a texture as wide as the screen.
            Some(Rectangle::from_size((1.0, f64::from(SCRIM_HEIGHT)).into())),
            Some((self.size.w, SCRIM_HEIGHT).into()),
            Kind::Unspecified,
        )
    }

    /// The glass below `surface`, a panel at `at`, if it asked for one:
    /// base glass (made now if no panel asked before); live glass gets what
    /// lies behind it from `LiveGlass`.
    pub fn glass(
        &mut self,
        renderer: &mut VkRenderer,
        surface: &WlSurface,
        at: Point<i32, Logical>,
    ) -> Option<Glass> {
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
        // Live glass reads the background until `LiveGlass` gives it what
        // lies behind it, in the same frame.
        let texture = if state.live {
            self.picture.clone()
        } else {
            self.base(renderer)?
        };
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
            texture,
            covers: Rectangle::from_size(self.size),
            live: state.live.then(|| Behind::Panel(surface.clone())),
            region: None,
            blur: None,
        })
    }
}

/// What live glass lies below: a panel that asked for it, or the menu bar
/// over a fullscreen window (`plate`).
#[derive(Clone, PartialEq)]
pub enum Behind {
    Panel(WlSurface),
    Plate,
}

/// How light `wallpaper` is behind the menu bar, the scrim over it as a
/// frame blends it (on the colours as encoded): its strip read back
/// (waiting for the GPU), the mean relative luminance against white ink's
/// least contrast.
fn tone(renderer: &mut VkRenderer, wallpaper: &VkTexture) -> Result<Tone> {
    let size = smithay::backend::renderer::Texture::size(wallpaper);
    let strip = Rectangle::from_size((size.w, BAR_STRIP.min(size.h)).into());
    let mapping = renderer.copy_texture(wallpaper, strip, Fourcc::Abgr8888)?;
    let pixels = renderer.map_texture(&mapping)?;
    let width = size.w.max(1) as usize;
    let mut sum = 0.0;
    let mut n = 0.0;
    for (i, p) in pixels.as_chunks::<4>().0.iter().enumerate() {
        let keep = 1.0 - scrim_alpha((i / width) as i32);
        let linear = |c: u8| f64::from(decoded(f32::from(c) / 255.0 * keep));
        sum += 0.2126 * linear(p[0]) + 0.7152 * linear(p[1]) + 0.0722 * linear(p[2]);
        n += 1.0;
    }
    let luminance = sum / f64::max(n, 1.0);
    let tone = if luminance > WHITE_UP_TO {
        Tone::Light
    } else {
        Tone::Dark
    };
    tracing::info!("background behind the menu bar: luminance {luminance:.3}, {tone:?}");
    Ok(tone)
}

/// The scrim: one column, stretched across the screen; black,
/// premultiplied.
fn scrim(renderer: &mut VkRenderer) -> Result<TextureBuffer<VkTexture>> {
    let mut data = Vec::with_capacity(SCRIM_HEIGHT as usize * 4);
    for y in 0..SCRIM_HEIGHT {
        data.extend_from_slice(&[0, 0, 0, (scrim_alpha(y) * 255.0).round() as u8]);
    }
    let texture =
        renderer.import_memory(&data, Fourcc::Abgr8888, (1, SCRIM_HEIGHT).into(), false)?;
    Ok(TextureBuffer::from_texture(
        renderer,
        texture,
        1,
        Transform::Normal,
        None,
    ))
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
    /// Live glass: what it lies below (`LiveGlass`).
    live: Option<Behind>,
    /// Live glass: the glass and `LIVE_MARGIN` around (`LiveGlass::update`),
    /// its place in the frame – what lies there is drawn again when it is
    /// blurred.
    region: Option<Rectangle<i32, Physical>>,
    /// Live glass blurred again in this frame: by the frame itself, of what
    /// it drew behind it (`Effects::blur_behind`).
    blur: Option<Arc<Mutex<Blur>>>,
}

impl Glass {
    /// What live glass lies below; it shows what lies behind it.
    pub fn live(&self) -> Option<&Behind> {
        self.live.as_ref()
    }

    /// Stays as it is: no longer live, its panel gone (`keep_panels`).
    pub fn freeze(&mut self) {
        self.live = None;
        self.blur = None;
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
        let size = self.geometry(Scale::from(1.0)).size;
        Rectangle::from_size((f64::from(size.w), f64::from(size.h)).into())
    }

    fn geometry(&self, _scale: Scale<f64>) -> Rectangle<i32, Physical> {
        self.region.unwrap_or(self.shape)
    }

    /// All of the glass but its corners: what lies below is not drawn – but
    /// when the frame blurs it again, which reads what lies below.
    fn opaque_regions(&self, scale: Scale<f64>) -> OpaqueRegions<i32, Physical> {
        if self.blur.is_some() {
            return OpaqueRegions::default();
        }
        let (w, h) = (self.shape.size.w, self.shape.size.h);
        let c = corner_extent(self.radius, w as f32, h as f32).ceil() as i32;
        if 2 * c >= w || 2 * c >= h {
            return OpaqueRegions::default();
        }
        let at = self.shape.loc - self.geometry(scale).loc;
        OpaqueRegions::from_slice(&[
            Rectangle::new((at.x + c, at.y).into(), (w - 2 * c, h).into()),
            Rectangle::new((at.x, at.y + c).into(), (c, h - 2 * c).into()),
            Rectangle::new((at.x + w - c, at.y + c).into(), (c, h - 2 * c).into()),
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
        let origin = dst.loc - self.geometry(Scale::from(1.0)).loc;
        let shape = Rectangle::new(self.shape.loc + origin, self.shape.size);
        let mut texture = self.texture.clone();
        let mut covers = Rectangle::new(self.covers.loc + origin, self.covers.size);
        if let Some(blur) = &self.blur
            && let Ok(mut blur) = blur.lock()
        {
            let blur = &mut *blur;
            match R::blur_behind(frame, dst, blur.material, &blur.chain, &mut blur.copy) {
                Ok(glass) => {
                    texture = glass;
                    covers = dst;
                    blur.drawn = true;
                }
                Err(e) => tracing::warn!("live glass: {e:?}"),
            }
        }
        // The damage, given within `dst`, within the glass.
        let damage: Vec<_> = damage
            .iter()
            .filter_map(|d| Rectangle::new(d.loc + dst.loc, d.size).intersection(shape))
            .map(|d| Rectangle::new(d.loc - shape.loc, d.size))
            .collect();
        R::draw_glass(
            frame,
            shape,
            &damage,
            shape,
            self.radius,
            &texture,
            covers,
            GRAIN,
            self.rim,
        );
        Ok(())
    }
}

/// Around live glass, what lies behind it is read this much further, so
/// the blur at its edge takes in what is beside it.
const LIVE_MARGIN: i32 = 64;

/// What live glass blurs through, kept from one change to the next: its
/// `LiveGlass` prepares it, the frame that blurs draws it (`Glass::draw`).
pub struct Blur {
    material: Material,
    /// The pictures of the blur; the last is the glass.
    chain: Vec<VkTexture>,
    /// What lies behind, copied from the frame (in the screen's format).
    copy: Option<VkTexture>,
    /// A frame blurred into `chain` as it is now: its last picture shows.
    drawn: bool,
}

/// An element behind live glass, as a frame showed it: what is compared
/// with the next to know whether the glass needs blurring again.
#[derive(Clone, PartialEq)]
struct Seen {
    id: Id,
    commit: CommitCounter,
    geometry: Rectangle<i32, Physical>,
}

/// Live glass (ADR 0028 §6): what lies behind a panel or the menu bar over
/// a fullscreen window – windows, their shadows, the background – blurred
/// by the frame itself, in its submission, as KWin's blur does: drawing
/// back to front, the frame stops at the glass, copies what it drew in the
/// glass's region and blurs it (`VkFrame::blur_behind`). That region is
/// drawn whole then – the screen held the glass there – and only when
/// something behind changed in it: otherwise the glass shows the last blur
/// and costs nothing. Lifted and tinted as base glass is, so the two look
/// the same over the background alone.
pub struct LiveGlass {
    behind: Behind,
    /// Where it reads, on the screen: the glass and `LIVE_MARGIN` around.
    region: Option<Rectangle<i32, Physical>>,
    appearance: Option<Appearance>,
    /// What lay behind it in the region at the last frame, back to front.
    seen: Vec<Seen>,
    blur: Arc<Mutex<Blur>>,
    id: Id,
    commit: CommitCounter,
}

impl LiveGlass {
    /// Live glass below `behind`.
    pub fn new(behind: Behind) -> LiveGlass {
        LiveGlass {
            behind,
            region: None,
            appearance: None,
            seen: Vec::new(),
            blur: Arc::new(Mutex::new(Blur {
                material: Appearance::Light.material(),
                chain: Vec::new(),
                copy: None,
                drawn: false,
            })),
            id: Id::new(),
            commit: CommitCounter::default(),
        }
    }

    pub fn is_for(&self, behind: &Behind) -> bool {
        &self.behind == behind
    }

    /// Whether it is still wanted: its panel alive, the menu bar's while a
    /// fullscreen window lies below it (`plated`).
    pub fn wanted(&self, plated: bool) -> bool {
        match &self.behind {
            Behind::Panel(surface) => surface.is_alive(),
            Behind::Plate => plated,
        }
    }

    /// Readies the panel's glass `glass` on a `screen` for this frame, with
    /// what lies `behind` it (front to back): blurred again by the frame if
    /// something there changed since the last, else the last blur.
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
        let scale = Scale::from(1.0);
        let seen: Vec<Seen> = behind
            .iter()
            .rev()
            .filter(|e| e.geometry(scale).overlaps(region))
            .map(|e| Seen {
                id: e.id().clone(),
                commit: e.current_commit(),
                geometry: e.geometry(scale),
            })
            .collect();
        let changed = changed_in(&self.seen, &seen, behind, region);
        self.seen = seen;
        let mut blur = self
            .blur
            .lock()
            .map_err(|_| error("live glass's blur poisoned"))?;
        let again = changed
            || !blur.drawn
            || self.region != Some(region)
            || self.appearance != Some(appearance);
        self.region = Some(region);
        self.appearance = Some(appearance);
        if again {
            blur.material = appearance.material();
            let size = (region.size.w, region.size.h).into();
            renderer.prepare_glass(size, blur.material, &mut blur.chain)?;
            blur.drawn = false;
            self.commit.increment();
            // The frame gives it the new blur (`Glass::draw`); should that
            // fail, it shows the background it came with.
            glass.blur = Some(self.blur.clone());
        } else if let Some(last) = blur.chain.last() {
            glass.texture = last.clone();
            glass.covers = region;
        }
        glass.region = Some(region);
        glass.id = self.id.clone();
        glass.commit = self.commit;
        Ok(())
    }
}

/// Whether what lies behind live glass in `region` changed from `before`
/// to `now` (back to front, of `behind`, front to back): an element came,
/// went, moved or changed order, or one changed within the region.
fn changed_in<E: Element>(
    before: &[Seen],
    now: &[Seen],
    behind: &[E],
    region: Rectangle<i32, Physical>,
) -> bool {
    if before.len() != now.len()
        || before
            .iter()
            .zip(now)
            .any(|(b, n)| b.id != n.id || b.geometry != n.geometry)
    {
        return true;
    }
    let scale = Scale::from(1.0);
    before.iter().zip(now).any(|(b, n)| {
        b.commit != n.commit
            && behind.iter().find(|e| *e.id() == n.id).is_none_or(|e| {
                e.damage_since(scale, Some(b.commit))
                    .iter()
                    .any(|d| Rectangle::new(d.loc + n.geometry.loc, d.size).overlaps(region))
            })
    })
}
