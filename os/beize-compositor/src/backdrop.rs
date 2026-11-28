//! Beize's background and Beize glass below the shell's panels and the
//! apps' popups (ADR 0033 §6). The background is drawn once per change –
//! the screen's size or the appearance – by the backdrop's shader
//! (`beize_vulkan`, `backdrop.frag`), then only read: computed, no image
//! file. The glass is one material at three densities: what lies behind
//! it, windows too, blurred by the frame itself (`LiveGlass`), its colours
//! lifted and tinted for the appearance (NSVisualEffectView's materials,
//! Windows' Acrylic and Mica work so); a frame adds grain and a rim of
//! light, per pixel (`GLASS`), where a client asked for it (`glass`).
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
use beize_draw::{measures, tokens};
use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::element::texture::{TextureBuffer, TextureRenderElement};
use smithay::backend::renderer::element::{Element, Id, Kind, RenderElement};
use smithay::backend::renderer::utils::{
    CommitCounter, OpaqueRegions, with_renderer_surface_state,
};
use smithay::backend::renderer::{ExportMem, ImportMem, RendererSuper};
use smithay::reexports::wayland_server::Resource;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{Buffer, Logical, Physical, Point, Rectangle, Scale, Size, Transform};
use smithay::wayland::compositor::with_states;
use std::sync::{Arc, Mutex};

use crate::animation::Pose;
use crate::corners::{Effects, Shadow};
use crate::glass::{self, GlassState, Shape};
use beize_vulkan::pipeline::corner_extent;

/// Light or dark: the background's colours, the glass's tint, and the
/// shell's ink (it hears it over beize-shell-v1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Appearance {
    Light,
    Dark,
}

impl Appearance {
    /// As the user chose it last (`keep`), else light, as macOS starts.
    /// `BEIZE_APPEARANCE=dark` or `light` in the desktop's environment wins
    /// (tests and screenshots).
    pub fn chosen() -> Appearance {
        let named = |name: &str| match name.trim() {
            "dark" => Some(Appearance::Dark),
            "light" => Some(Appearance::Light),
            _ => None,
        };
        std::env::var("BEIZE_APPEARANCE")
            .ok()
            .and_then(|v| named(&v))
            .or_else(|| {
                let file = stored()?;
                named(&std::fs::read_to_string(file).ok()?)
            })
            .unwrap_or(Appearance::Light)
    }

    /// Keeps the user's choice (the control center) for the next login: in
    /// the compositor's configuration directory (its unit's
    /// `ConfigurationDirectory`, ~/.config/beize) until Beize's settings
    /// hold it, as GNOME keeps its colour scheme in dconf.
    pub fn keep(self) {
        let Some(file) = stored() else {
            return;
        };
        let name = match self {
            Appearance::Light => "light\n",
            Appearance::Dark => "dark\n",
        };
        // Written beside and renamed over it: a crash leaves the old choice,
        // never an empty file.
        let next = file.with_extension("new");
        if let Err(e) = std::fs::write(&next, name).and_then(|()| std::fs::rename(&next, &file)) {
            tracing::warn!("keeping the appearance in {}: {e}", file.display());
        }
    }

    /// As the backdrop's shader and beize-shell-v1 number it.
    pub fn index(self) -> u32 {
        match self {
            Appearance::Light => 0,
            Appearance::Dark => 1,
        }
    }

    /// Beize glass of the appearance showing `shows` (ADR 0033 §6): what lies
    /// behind blurred by Dual Kawase – four halvings and two doublings back,
    /// the glass a quarter of the screen's size, soft enough for the filter
    /// to scale it up –, its saturation lifted, then the appearance's neutral
    /// tint at the density's alpha (`tokens`): the recipe of the menu glass
    /// the owner approved (31.10., 80 % of #F2F2F2), for every density.
    fn material(self, shows: glass::Material) -> Material {
        let tint = self.tint();
        let alpha = match shows {
            glass::Material::Thin => tint.thin,
            glass::Material::Medium => tint.medium,
            glass::Material::Dense => tint.dense,
            glass::Material::Preview => return self.preview_material(),
        };
        let linear = |c: u8| decoded(f32::from(c) / 255.0) * alpha;
        let c = tint.color;
        Material {
            down: 4,
            up: 2,
            offset: 2.0,
            saturation: 1.8,
            tint: [linear(c.r), linear(c.g), linear(c.b), alpha],
        }
    }

    fn tint(self) -> tokens::Tint {
        match self {
            Appearance::Light => tokens::TINT_LIGHT,
            Appearance::Dark => tokens::TINT_DARK,
        }
    }

    /// The snap preview's glass, in either appearance: minimal, only to see
    /// the place (the owner, 31.10.). One halving less than `material`: a
    /// sigma of 20 px instead of 44 (backdrop.frag's kernels computed), the
    /// colours as they are, a breath of white: 18 % as encoded on the mock-up
    /// he chose, about 13 % in linear light.
    fn preview_material(self) -> Material {
        let linear = decoded(f32::from(0xf6_u8) / 255.0) * PREVIEW_WHITE;
        Material {
            down: 3,
            up: 1,
            offset: 2.0,
            saturation: 1.0,
            tint: [linear, linear, linear, PREVIEW_WHITE],
        }
    }

    /// The rim of light along the glass (premultiplied): one for every
    /// density, softer on dark (the menu's, the owner, 28.10.: light, but not
    /// white).
    fn rim(self) -> [f32; 4] {
        let a = self.tint().rim;
        [a, a, a, a]
    }
}

/// The white over the snap preview (`preview_material`). Its line is a
/// menu's light one since 0.5.131: the dark one of 31.10. (black 22 %
/// linear) set it apart from its glass (the owner on hardware).
const PREVIEW_WHITE: f32 = 0.13;

/// Where the chosen appearance is kept (`Appearance::keep`).
fn stored() -> Option<std::path::PathBuf> {
    let dir = std::env::var_os("CONFIGURATION_DIRECTORY")?;
    Some(std::path::Path::new(&dir).join("appearance"))
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

/// The background and its scrim for one screen size and appearance.
pub struct Backdrop {
    size: Size<i32, Physical>,
    appearance: Appearance,
    wallpaper: TextureBuffer<VkTexture>,
    /// The same, as the glass reads it.
    picture: VkTexture,
    tone: Tone,
    scrim: TextureBuffer<VkTexture>,
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
            plate: (Id::new(), CommitCounter::default(), None),
            generation,
        })
    }

    /// Thin glass over `rect`, square and without a rim: below the menu bar
    /// where a fullscreen window lies behind it. It reads the background
    /// until `LiveGlass` gives it what lies behind it, in the same frame.
    pub fn plate(&mut self, rect: Rectangle<i32, Logical>) -> Glass {
        let (id, commit, last) = &mut self.plate;
        if *last != Some(rect) {
            *last = Some(rect);
            commit.increment();
        }
        let rect = rect.to_physical(1);
        Glass {
            id: id.clone(),
            commit: *commit,
            pieces: vec![Piece {
                rect,
                radius: 0.0,
                rim: [0.0; 4],
            }],
            bounds: rect,
            shows: glass::Material::Thin,
            texture: Some(self.picture.clone()),
            covers: Rectangle::from_size(self.size),
            live: Some(Behind::Plate),
            region: None,
            reads: None,
            blur: None,
            opacity: 1.0,
        }
    }

    /// The place a dragged window would snap to, as live glass with the
    /// windows' corners and one flat line: what lies behind it, windows too,
    /// stays in view, a little blurred (`preview_material`). `LiveGlass` gives
    /// it what lies behind, in the same frame.
    pub fn snap_preview(
        &self,
        id: &Id,
        commit: CommitCounter,
        place: Rectangle<i32, Logical>,
    ) -> Glass {
        let rect = place.to_physical(1);
        Glass {
            id: id.clone(),
            commit,
            pieces: vec![Piece {
                rect,
                radius: crate::corners::RADIUS as f32,
                // The glass's light line (`PREVIEW_WHITE`).
                rim: self.appearance.rim(),
            }],
            bounds: rect,
            shows: glass::Material::Preview,
            texture: Some(self.picture.clone()),
            covers: Rectangle::from_size(self.size),
            live: Some(Behind::SnapPreview),
            region: None,
            reads: None,
            blur: None,
            opacity: 1.0,
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

    /// The glass below `surface`, a panel or a popup of one at `at`, if it
    /// asked for one, and the shadows of its shapes if it wants them: it
    /// gets what lies behind it from `LiveGlass`.
    pub fn glass(
        &mut self,
        surface: &WlSurface,
        at: Point<i32, Logical>,
        pose: Pose,
    ) -> Option<(Glass, Vec<Shadow>)> {
        let (state, drawn) = self.drawn(surface, at, pose);
        let drawn = drawn?;
        // An app's glass stays on its surface.
        let own = state.confined.then(|| {
            Rectangle::from_size(
                with_renderer_surface_state(surface, |s| s.surface_size())
                    .flatten()
                    .unwrap_or_default(),
            )
        });
        let ids = (drawn.id, drawn.commit, drawn.shadows);
        self.glass_of(surface, &state, own, ids, at, pose)
    }

    /// What is left of `surface`'s glass at `at` once its surface goes (a
    /// dialog closing, `animation`): shown as it was, by its own ids.
    pub fn ghost(&self, surface: &WlSurface, at: Point<i32, Logical>) -> Option<Ghost> {
        let (state, _) = self.drawn(surface, at, Pose::still());
        if state.shapes.is_empty() {
            return None;
        }
        Some(Ghost {
            surface: surface.clone(),
            shadows: state.shapes.iter().map(|_| Id::new()).collect(),
            state,
            id: Id::new(),
            commit: CommitCounter::default(),
        })
    }

    /// A ghost's glass at `at`, as `pose` shows it.
    pub fn ghost_glass(
        &mut self,
        ghost: &mut Ghost,
        at: Point<i32, Logical>,
        pose: Pose,
    ) -> Option<(Glass, Vec<Shadow>)> {
        ghost.commit.increment();
        let ids = (ghost.id.clone(), ghost.commit, ghost.shadows.clone());
        let surface = ghost.surface.clone();
        self.glass_of(&surface, &ghost.state, None, ids, at, pose)
    }

    /// The glass of `state` at `at`, cut to `own` (an app's), by `ids`
    /// (its own, its commit, its shadows'), as `pose` shows it.
    fn glass_of(
        &self,
        surface: &WlSurface,
        state: &GlassState,
        own: Option<Rectangle<i32, Logical>>,
        (id, commit, shadow_ids): (Id, CommitCounter, Vec<Id>),
        at: Point<i32, Logical>,
        pose: Pose,
    ) -> Option<(Glass, Vec<Shadow>)> {
        let pieces: Vec<Piece> = state
            .shapes
            .iter()
            .filter_map(|shape| match own {
                Some(own) => shape.rect.intersection(own).map(|rect| (shape, rect)),
                None => Some((shape, shape.rect)),
            })
            .map(|(shape, mut rect)| {
                rect.loc += at;
                Piece {
                    rect: pose.rect(rect.to_physical(1)),
                    radius: shape.radius * pose.scale as f32,
                    rim: if shape.rim {
                        self.appearance.rim()
                    } else {
                        [0.0; 4]
                    },
                }
            })
            .collect();
        let bounds = pieces.iter().map(|p| p.rect).reduce(|a, b| a.merge(b))?;
        let reads = if pose.is_still() {
            None
        } else {
            state
                .shapes
                .iter()
                .filter_map(|shape| match own {
                    Some(own) => shape.rect.intersection(own),
                    None => Some(shape.rect),
                })
                .map(|mut rect| {
                    rect.loc += at;
                    rect.to_physical(1)
                })
                .reduce(|a, b| a.merge(b))
                .map(|whole| whole.merge(bounds))
        };
        let shadows = if state.shadow {
            pieces
                .iter()
                .zip(shadow_ids)
                .map(|(p, id)| {
                    Shadow::below_glass(id, commit, p.rect, p.radius).faded(pose.opacity)
                })
                .collect()
        } else {
            Vec::new()
        };
        let glass = Glass {
            id,
            commit,
            pieces,
            bounds,
            shows: state.material,
            texture: Some(self.picture.clone()),
            covers: Rectangle::from_size(self.size),
            live: Some(Behind::Panel(surface.clone())),
            region: None,
            reads,
            blur: None,
            opacity: pose.opacity,
        };
        Some((glass, shadows))
    }

    /// The glass state of `surface` at `at`, and its ids and commit, which
    /// counts each change of what it is drawn for.
    fn drawn(
        &self,
        surface: &WlSurface,
        at: Point<i32, Logical>,
        pose: Pose,
    ) -> (GlassState, Option<Ids>) {
        with_states(surface, |states| {
            let state = states.cached_state.get::<GlassState>().current().clone();
            states
                .data_map
                .insert_if_missing_threadsafe(|| Mutex::new(Drawn::default()));
            let ids = states.data_map.get::<Mutex<Drawn>>().and_then(|drawn| {
                let mut drawn = drawn.lock().ok()?;
                let key = (!state.shapes.is_empty()).then(|| {
                    let shapes = state.shapes.clone();
                    let look = (state.material, state.shadow);
                    (shapes, look, at, self.generation, pose)
                });
                if drawn.last != key {
                    drawn.last = key;
                    drawn.commit.increment();
                }
                while drawn.shadows.len() < state.shapes.len() {
                    drawn.shadows.push(Id::new());
                }
                Some(Ids {
                    id: drawn.id.clone(),
                    commit: drawn.commit,
                    shadows: drawn.shadows.clone(),
                })
            });
            (state, ids)
        })
    }
}

/// What live glass lies below: a panel that asked for it, the menu bar
/// over a fullscreen window (`plate`), or the place a dragged window would
/// snap to.
#[derive(Clone, PartialEq)]
pub enum Behind {
    Panel(WlSurface),
    Plate,
    SnapPreview,
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

/// What a glass is drawn for: its shapes, its material, whether they cast
/// shadows, the surface's place and the backdrop's generation.
type DrawnFor = (
    Vec<Shape>,
    (glass::Material, bool),
    Point<i32, Logical>,
    u64,
    Pose,
);

/// What a surface's glass was last drawn for, in its data, and the ids of
/// its shapes' shadows.
struct Drawn {
    id: Id,
    commit: CommitCounter,
    last: Option<DrawnFor>,
    shadows: Vec<Id>,
}

impl Default for Drawn {
    fn default() -> Drawn {
        Drawn {
            id: Id::new(),
            commit: CommitCounter::default(),
            last: None,
            shadows: Vec::new(),
        }
    }
}

/// A surface's glass's ids and commit (`Drawn`).
struct Ids {
    id: Id,
    commit: CommitCounter,
    shadows: Vec<Id>,
}

/// One place of a glass on the screen: a rectangle with round corners of
/// `radius`, and its rim of light (premultiplied; clear for none).
#[derive(Clone, Copy, Debug)]
struct Piece {
    rect: Rectangle<i32, Physical>,
    radius: f32,
    rim: [f32; 4],
}

/// The glass below a panel or a popup of one, in one place or more: drawn
/// again when its shapes, its place or what it shows changes.
pub struct Glass {
    id: Id,
    commit: CommitCounter,
    pieces: Vec<Piece>,
    /// All of them.
    bounds: Rectangle<i32, Physical>,
    /// Its material, tinted for the appearance.
    shows: glass::Material,
    /// A blurred copy of what lies in `covers` (the screen's coordinates).
    texture: Option<VkTexture>,
    covers: Rectangle<i32, Physical>,
    /// Live glass: what it lies below (`LiveGlass`).
    live: Option<Behind>,
    /// Live glass: the glass and `LIVE_MARGIN` around (`LiveGlass::update`),
    /// its place in the frame – what lies there is drawn again when it is
    /// blurred.
    region: Option<Rectangle<i32, Physical>>,
    /// While its dialog grows or shrinks: where it reads, the whole shape
    /// and the shown one, so the pictures it blurs through keep their size
    /// – read where it showed, they were made anew each frame (audit of
    /// 0.5.130), and blurred again.
    reads: Option<Rectangle<i32, Physical>>,
    /// Live glass blurred again in this frame: by the frame itself, of what
    /// it drew behind it (`Effects::blur_behind`).
    blur: Option<Arc<Mutex<LiveBlur>>>,
    /// Fades in and out with its dialog (`animation`), 1 whole.
    opacity: f32,
}

/// The glass of a surface gone (`Backdrop::ghost`), shown while its dialog
/// fades out.
pub struct Ghost {
    surface: WlSurface,
    state: GlassState,
    id: Id,
    commit: CommitCounter,
    shadows: Vec<Id>,
}

impl Glass {
    /// Faded to `opacity` (the snap preview coming and going).
    pub fn faded(mut self, opacity: f32) -> Glass {
        self.opacity = opacity;
        self
    }

    /// The frame blurs what lies behind it again.
    pub fn blurring(&self) -> bool {
        self.blur.is_some()
    }

    /// What live glass lies below; it shows what lies behind it.
    pub fn live(&self) -> Option<&Behind> {
        self.live.as_ref()
    }

    /// Stays as it is: no longer live, its panel gone (`keep_panels`) –
    /// showing its last blur, if a frame drew one.
    pub fn freeze(&mut self) {
        if let Some(blur) = &self.blur
            && let Ok(blur) = blur.lock()
            && blur.drawn
            && let Some(last) = blur.chain.last()
        {
            self.texture = Some(last.clone());
            self.covers = self.region.unwrap_or(self.bounds);
        }
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
        self.region.unwrap_or(self.bounds)
    }

    /// Its opacity, so a change of it alone damages it: the last step of a
    /// fade is drawn too.
    fn alpha(&self) -> f32 {
        self.opacity
    }

    /// All of each piece but its corners: what lies below is not drawn – but
    /// when the frame blurs it again, which reads what lies below, or when
    /// it has nothing to show.
    fn opaque_regions(&self, scale: Scale<f64>) -> OpaqueRegions<i32, Physical> {
        if self.blur.is_some() || self.texture.is_none() || self.opacity < 1.0 {
            return OpaqueRegions::default();
        }
        let origin = self.geometry(scale).loc;
        let mut regions = Vec::new();
        for piece in &self.pieces {
            let (w, h) = (piece.rect.size.w, piece.rect.size.h);
            let c = corner_extent(piece.radius, w as f32, h as f32).ceil() as i32;
            if 2 * c >= w || 2 * c >= h {
                continue;
            }
            let at = piece.rect.loc - origin;
            regions.extend([
                Rectangle::new((at.x + c, at.y).into(), (w - 2 * c, h).into()),
                Rectangle::new((at.x, at.y + c).into(), (c, h - 2 * c).into()),
                Rectangle::new((at.x + w - c, at.y + c).into(), (c, h - 2 * c).into()),
            ]);
        }
        OpaqueRegions::from_slice(&regions)
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
        let mut texture = self.texture.clone();
        let mut covers = Rectangle::new(self.covers.loc + origin, self.covers.size);
        if let Some(blur) = &self.blur
            && let Ok(mut blur) = blur.lock()
        {
            let blur = &mut *blur;
            match R::blur_behind(frame, dst, &blur.how, &blur.chain, &mut blur.copy) {
                Ok(glass) => {
                    texture = Some(glass);
                    covers = dst;
                    blur.drawn = true;
                }
                Err(e) => tracing::warn!("live glass: {e:?}"),
            }
        }
        let Some(texture) = texture else {
            return Ok(());
        };
        let flat = self.shows == glass::Material::Preview;
        let grain = if flat { 0.0 } else { GRAIN };
        for piece in &self.pieces {
            let shape = Rectangle::new(piece.rect.loc + origin, piece.rect.size);
            // The damage, given within `dst`, within the piece.
            let damage: Vec<_> = damage
                .iter()
                .filter_map(|d| Rectangle::new(d.loc + dst.loc, d.size).intersection(shape))
                .map(|d| Rectangle::new(d.loc - shape.loc, d.size))
                .collect();
            if damage.is_empty() {
                continue;
            }
            R::draw_glass(
                frame,
                shape,
                &damage,
                shape,
                piece.radius,
                &texture,
                covers,
                grain,
                piece.rim,
                flat,
                self.opacity,
            );
        }
        Ok(())
    }
}

/// Around live glass, what lies behind it is read this much further, so
/// the blur at its edge takes in what is beside it.
const LIVE_MARGIN: i32 = 64;

/// What live glass blurs through, kept from one change to the next: its
/// `LiveGlass` prepares it, the frame that blurs draws it (`Glass::draw`).
pub struct LiveBlur {
    how: Material,
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
/// drawn whole then, windows in front of the glass hiding nothing of it
/// (`Rounded::hides_nothing`) – the screen held the glass there – and only when
/// something behind changed in it: otherwise the glass shows the last blur
/// and costs nothing. Lifted and tinted as base glass is, so the two look
/// the same over the background alone.
pub struct LiveGlass {
    behind: Behind,
    /// Where it reads, on the screen: the glass and `LIVE_MARGIN` around.
    region: Option<Rectangle<i32, Physical>>,
    /// The appearance and material it was blurred for last.
    look: Option<(Appearance, glass::Material)>,
    /// What lay behind it in the region at the last frame, back to front.
    seen: Vec<Seen>,
    blur: Arc<Mutex<LiveBlur>>,
    id: Id,
    commit: CommitCounter,
}

impl LiveGlass {
    /// Live glass below `behind`.
    pub fn new(behind: Behind) -> LiveGlass {
        LiveGlass {
            behind,
            region: None,
            look: None,
            seen: Vec::new(),
            blur: Arc::new(Mutex::new(LiveBlur {
                how: Appearance::Light.material(glass::Material::Thin),
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
    /// `shown`: what lies behind the glass of this frame. A panel hidden by a
    /// fullscreen window keeps its blur; an app's goes when it stops showing,
    /// so a client cannot pile up blur with surfaces it keeps.
    pub fn wanted(&self, plated: bool, shown: &[Behind]) -> bool {
        match &self.behind {
            Behind::Panel(surface) => {
                surface.is_alive()
                    && (shown.contains(&self.behind)
                        || !with_states(surface, |states| {
                            states.cached_state.get::<GlassState>().current().confined
                        }))
            }
            Behind::Plate => plated,
            // Gone with the drag: its blur costs nothing until the next.
            Behind::SnapPreview => shown.contains(&self.behind),
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
        let s = glass.reads.unwrap_or(glass.bounds);
        let margin = LIVE_MARGIN;
        let around = Rectangle::new(
            (s.loc.x - margin, s.loc.y - margin).into(),
            (s.size.w + 2 * margin, s.size.h + 2 * margin).into(),
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
        let blur = self.blur.clone();
        let mut blur = blur
            .lock()
            .map_err(|_| error("live glass's blur poisoned"))?;
        let look = (appearance, glass.shows);
        let again =
            changed || !blur.drawn || self.region != Some(region) || self.look != Some(look);
        self.region = Some(region);
        self.look = Some(look);
        if again {
            blur.how = appearance.material(glass.shows);
            let size = (region.size.w, region.size.h).into();
            let blur = &mut *blur;
            renderer.prepare_glass(size, &blur.how, &mut blur.chain)?;
            blur.drawn = false;
            self.commit.increment();
            // The frame gives it the new blur (`Glass::draw`); should that
            // fail, it shows the background it came with.
            glass.blur = Some(self.blur.clone());
        } else if let Some(last) = blur.chain.last() {
            glass.texture = Some(last.clone());
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
