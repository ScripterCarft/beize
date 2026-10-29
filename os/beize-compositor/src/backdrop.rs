//! Beize's background and the glass below the shell's panels (ADR 0028
//! §6). Both are drawn once per change – the screen's size or the
//! appearance – by the backdrop's shader (`beize_vulkan`, `backdrop.frag`),
//! then only read: the background is computed, no image file; base glass
//! is the background blurred, its colours lifted and tinted for the
//! appearance (Microsoft's Mica works so), made when a panel first asks
//! for it. A frame adds to the glass only grain and a rim of light, per
//! pixel (`GLASS`), where the shell asked for it (`glass`). Live glass
//! shows what lies behind it, windows too (`LiveGlass`); liquid glass, the
//! control center's and the dock's, is live glass frosted by a Gaussian
//! (`frost`), its tone, colour and light the frame's (`quad.frag`). Around a glass's shapes a smoke
//! may lie, live glass of its own behind the panel: what lies there blurred
//! less and less away from them, through a mask (`Backdrop::smoke`).
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
use crate::render::{Blur, Gauss, Liquid, Material, VkRenderer, VkTexture};
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

use crate::corners::{Effects, Shadow};
use crate::glass::{self, GlassState, Shape, Smoke};
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

    /// A menu's glass (`glass::Material::Menu`): as the appearance's, but
    /// tinted thickly with macOS' menu colours – #F2F2F2 and #2D2D2D,
    /// measured in Apple's menus (HIG, 2 px per pt) – at 85 %: calm, what
    /// lies behind barely shows (the owner, 28.10., on mock-ups; macOS 26
    /// lets about half through).
    fn menu_material(self) -> Material {
        let tint = match self {
            Appearance::Light => [0xf2, 0xf2, 0xf2],
            Appearance::Dark => [0x2d, 0x2d, 0x2d],
        };
        let alpha = 0.85;
        let linear = |c: u8| decoded(f32::from(c) / 255.0) * alpha;
        Material {
            tint: [linear(tint[0]), linear(tint[1]), linear(tint[2]), alpha],
            ..self.material()
        }
    }

    /// How a glass showing `shows` blurs what lies behind it.
    fn blur_for(self, shows: glass::Material) -> Blur {
        match shows {
            glass::Material::Menu => Blur::Kawase(self.menu_material()),
            glass::Material::Base | glass::Material::Live => Blur::Kawase(self.material()),
            glass::Material::Clear | glass::Material::Liquid => frost(FROST),
        }
    }

    /// The rim of light along the glass (premultiplied): softer on dark,
    /// faint on a menu's (the owner, 28.10.: light, but not white), on clear
    /// glass as approved with it (29.10.).
    fn rim(self, shows: glass::Material) -> [f32; 4] {
        let a = match (self, shows) {
            // Liquid glass's line of light is its own (`quad.frag`).
            (_, glass::Material::Clear | glass::Material::Liquid) => 0.0,
            (Appearance::Light, glass::Material::Menu) => 0.3,
            (Appearance::Dark, glass::Material::Menu) => 0.12,
            (Appearance::Light, _) => 0.6,
            (Appearance::Dark, _) => 0.22,
        };
        [a, a, a, a]
    }
}

/// Liquid glass's blur, in px: clear, what lies behind still shows in
/// shapes (the owner, 29.10.: "klar, ganz leicht milchig"; 0.5.119 had 14
/// below the tiles and 16 below the dock).
const FROST: f32 = 10.0;

/// Liquid glass's frost (`glass::Material::Clear` and `Liquid`; ADR 0028
/// §6): a Gaussian of `sigma` px at half the size: wide and cheap. Its tone,
/// colour and light are the frame's (`quad.frag`, `clear_glass`).
fn frost(sigma: f32) -> Blur {
    Blur::Gauss(Gauss {
        sigma,
        mask: None,
        half: true,
    })
}

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
            shows: glass::Material::Live,
            dark: self.appearance == Appearance::Dark,
            texture: Some(self.picture.clone()),
            covers: Rectangle::from_size(self.size),
            live: Some(Behind::Plate),
            region: None,
            blur: None,
            smoke: None,
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
    /// asked for one, and the shadows of its shapes if it wants them: base
    /// glass (made now if no panel asked before); live, menu and clear glass
    /// get what lies behind them from `LiveGlass`.
    pub fn glass(
        &mut self,
        renderer: &mut VkRenderer,
        surface: &WlSurface,
        at: Point<i32, Logical>,
    ) -> Option<(Glass, Vec<Shadow>)> {
        let (state, drawn) = self.drawn(surface, at);
        let drawn = drawn?;
        let (id, commit, shadow_ids) = (drawn.id, drawn.commit, drawn.shadows);
        let pieces: Vec<Piece> = state
            .shapes
            .iter()
            .map(|shape| {
                let mut rect = shape.rect;
                rect.loc += at;
                Piece {
                    rect: rect.to_physical(1),
                    radius: shape.radius,
                    rim: if shape.rim {
                        self.appearance.rim(state.material)
                    } else {
                        [0.0; 4]
                    },
                }
            })
            .collect();
        let bounds = pieces.iter().map(|p| p.rect).reduce(|a, b| a.merge(b))?;
        let live = state.material != glass::Material::Base;
        // Live and menu glass read the background until `LiveGlass` gives
        // them what lies behind them, in the same frame. Liquid glass reads
        // nothing until then: the background, sharp and untinted, would
        // show through it as something it is not.
        let texture = match state.material {
            glass::Material::Base => Some(self.base(renderer)?),
            glass::Material::Live | glass::Material::Menu => Some(self.picture.clone()),
            glass::Material::Clear | glass::Material::Liquid => None,
        };
        let shadows = if state.shadow {
            pieces
                .iter()
                .zip(shadow_ids)
                .map(|(p, id)| Shadow::below_glass(id, commit, p.rect, p.radius, state.material))
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
            dark: self.appearance == Appearance::Dark,
            texture,
            covers: Rectangle::from_size(self.size),
            live: live.then(|| Behind::Panel(surface.clone())),
            region: None,
            blur: None,
            smoke: None,
        };
        Some((glass, shadows))
    }

    /// The smoke around the glass below `surface` at `at`, if it asked for
    /// one: what lies behind its shapes and `falloff` around them (on the
    /// screen), blurred through its mask by `LiveGlass` – live glass of its
    /// own, square, without rim or grain, to lie behind the panel. Until it
    /// is blurred it shows nothing: never the sharp background over what
    /// lies behind it.
    pub fn smoke(&mut self, surface: &WlSurface, at: Point<i32, Logical>) -> Option<Glass> {
        let (state, drawn) = self.drawn(surface, at);
        let (drawn, smoke) = (drawn?, state.smoke?);
        let shapes: Vec<_> = state
            .shapes
            .iter()
            .map(|shape| {
                let mut rect = shape.rect;
                rect.loc += at;
                (rect.to_physical(1), shape.radius)
            })
            .collect();
        let bounds = shapes.iter().map(|s| s.0).reduce(|a, b| a.merge(b))?;
        let reach = smoke.falloff.ceil() as i32;
        let region = Rectangle::new(
            (bounds.loc.x - reach, bounds.loc.y - reach).into(),
            (bounds.size.w + 2 * reach, bounds.size.h + 2 * reach).into(),
        )
        .intersection(Rectangle::from_size(self.size))?;
        Some(Glass {
            id: drawn.smoke,
            commit: drawn.commit,
            pieces: vec![Piece {
                rect: region,
                radius: 0.0,
                rim: [0.0; 4],
            }],
            bounds: region,
            shows: state.material,
            dark: self.appearance == Appearance::Dark,
            texture: None,
            covers: Rectangle::from_size(self.size),
            live: Some(Behind::Smoke(surface.clone())),
            region: None,
            blur: None,
            smoke: Some(Mask { shapes, smoke }),
        })
    }

    /// The glass state of `surface` at `at`, and its ids and commit, which
    /// counts each change of what it is drawn for.
    fn drawn(&self, surface: &WlSurface, at: Point<i32, Logical>) -> (GlassState, Option<Ids>) {
        with_states(surface, |states| {
            let state = states.cached_state.get::<GlassState>().current().clone();
            states
                .data_map
                .insert_if_missing_threadsafe(|| Mutex::new(Drawn::default()));
            let ids = states.data_map.get::<Mutex<Drawn>>().and_then(|drawn| {
                let mut drawn = drawn.lock().ok()?;
                let key = (!state.shapes.is_empty()).then(|| {
                    let shapes = state.shapes.clone();
                    let look = (state.material, state.shadow, state.smoke);
                    (shapes, look, at, self.generation)
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
                    smoke: drawn.smoke.clone(),
                    commit: drawn.commit,
                    shadows: drawn.shadows.clone(),
                })
            });
            (state, ids)
        })
    }
}

/// What live glass lies below: a panel that asked for it, the menu bar
/// over a fullscreen window (`plate`), or the smoke around a panel's glass.
#[derive(Clone, PartialEq)]
pub enum Behind {
    Panel(WlSurface),
    Plate,
    Smoke(WlSurface),
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
/// shadows and its smoke, the surface's place and the backdrop's
/// generation.
type DrawnFor = (
    Vec<Shape>,
    (glass::Material, bool, Option<Smoke>),
    Point<i32, Logical>,
    u64,
);

/// What a surface's glass was last drawn for, in its data, and the ids of
/// its smoke and its shapes' shadows.
struct Drawn {
    id: Id,
    smoke: Id,
    commit: CommitCounter,
    last: Option<DrawnFor>,
    shadows: Vec<Id>,
}

impl Default for Drawn {
    fn default() -> Drawn {
        Drawn {
            id: Id::new(),
            smoke: Id::new(),
            commit: CommitCounter::default(),
            last: None,
            shadows: Vec::new(),
        }
    }
}

/// A surface's glass's ids and commit (`Drawn`).
struct Ids {
    id: Id,
    smoke: Id,
    commit: CommitCounter,
    shadows: Vec<Id>,
}

/// What a smoke's mask is made of: the glass's shapes on the screen with
/// their corners' radius, and how the smoke falls off around them.
#[derive(Clone, Debug, PartialEq)]
struct Mask {
    shapes: Vec<(Rectangle<i32, Physical>, f32)>,
    smoke: Smoke,
}

/// A smoke's mask holds a value for each square of this many pixels: it
/// changes slowly (its reach is 130 px), the filter stretches it smoothly.
const MASK_STEP: i32 = 4;
/// How the smoke thins from a shape's edge: t = (1 - d / falloff)^1.6 –
/// at once, no plateau around the shapes (approved on mock-ups, 29.10.).
const SMOKE_FALL: f32 = 1.6;

/// The smoke's mask over `region` (`backdrop.frag`, `smoke_sigma`): t for
/// each `MASK_STEP` square, at its centre.
fn smoke_mask(
    renderer: &mut VkRenderer,
    mask: &Mask,
    region: Rectangle<i32, Physical>,
) -> Result<VkTexture> {
    let w = (region.size.w + MASK_STEP - 1) / MASK_STEP;
    let h = (region.size.h + MASK_STEP - 1) / MASK_STEP;
    let (sx, sy) = (
        region.size.w as f32 / w as f32,
        region.size.h as f32 / h as f32,
    );
    let mut values = Vec::with_capacity((w * h).max(0) as usize);
    for j in 0..h {
        let y = region.loc.y as f32 + (j as f32 + 0.5) * sy;
        for i in 0..w {
            let x = region.loc.x as f32 + (i as f32 + 0.5) * sx;
            values.push((smoke_at(mask, x, y) * 255.0).round() as u8);
        }
    }
    renderer.mask_texture(&values, (w, h).into())
}

/// The smoke's t at `(x, y)` on the screen: 1 on the shapes, thinning to 0
/// at `falloff` from the nearest.
fn smoke_at(mask: &Mask, x: f32, y: f32) -> f32 {
    let d = mask
        .shapes
        .iter()
        .map(|&(rect, radius)| rounded_distance(x, y, rect, radius))
        .fold(f32::INFINITY, f32::min)
        .max(0.0);
    (1.0 - d / mask.smoke.falloff)
        .clamp(0.0, 1.0)
        .powf(SMOKE_FALL)
}

/// The signed distance of `(x, y)` to `rect` with circular corners of
/// `radius`, negative inside: the continuous corner the shader draws lies
/// within a pixel of it, far below the smoke's reach.
fn rounded_distance(x: f32, y: f32, rect: Rectangle<i32, Physical>, radius: f32) -> f32 {
    let (hw, hh) = (rect.size.w as f32 / 2.0, rect.size.h as f32 / 2.0);
    let r = radius.min(hw).min(hh).max(0.0);
    let qx = (x - (rect.loc.x as f32 + hw)).abs() - (hw - r);
    let qy = (y - (rect.loc.y as f32 + hh)).abs() - (hh - r);
    qx.max(0.0).hypot(qy.max(0.0)) + qx.max(qy).min(0.0) - r
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
    /// Its material, tinted for the appearance; of a smoke, the glass's it
    /// lies around.
    shows: glass::Material,
    /// Drawn for the dark appearance.
    dark: bool,
    /// A blurred copy of what lies in `covers` (the screen's coordinates);
    /// none for clear glass and a smoke until first blurred.
    texture: Option<VkTexture>,
    covers: Rectangle<i32, Physical>,
    /// Live glass: what it lies below (`LiveGlass`).
    live: Option<Behind>,
    /// Live glass: the glass and `LIVE_MARGIN` around (`LiveGlass::update`),
    /// its place in the frame – what lies there is drawn again when it is
    /// blurred.
    region: Option<Rectangle<i32, Physical>>,
    /// Live glass blurred again in this frame: by the frame itself, of what
    /// it drew behind it (`Effects::blur_behind`).
    blur: Option<Arc<Mutex<LiveBlur>>>,
    /// A smoke (`Backdrop::smoke`): what its mask is made of.
    smoke: Option<Mask>,
}

impl Glass {
    /// What live glass lies below; it shows what lies behind it.
    pub fn live(&self) -> Option<&Behind> {
        self.live.as_ref()
    }

    /// Whether it may stay as it is when its panel goes (`keep_panels`):
    /// not clear glass or a smoke, whose last blur would be stale over what
    /// lies behind it by then.
    pub fn keeps(&self) -> bool {
        self.smoke.is_none() && self.shows != glass::Material::Clear
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

    /// Drawn as liquid glass: with white ink on it (the control center's
    /// tiles) or none (the dock), no grain.
    fn liquid(&self) -> Option<Liquid> {
        let ink = match self.shows {
            _ if self.smoke.is_some() => return None,
            glass::Material::Clear => true,
            glass::Material::Liquid => false,
            _ => return None,
        };
        Some(Liquid {
            ink,
            dark: self.dark,
        })
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

    /// All of each piece but its corners: what lies below is not drawn – but
    /// when the frame blurs it again, which reads what lies below, or when
    /// it has nothing to show.
    fn opaque_regions(&self, scale: Scale<f64>) -> OpaqueRegions<i32, Physical> {
        if self.blur.is_some() || self.texture.is_none() {
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
        // Liquid glass and a smoke show nothing rather than a stand-in.
        let Some(texture) = texture else {
            return Ok(());
        };
        let liquid = self.liquid();
        let grain = if liquid.is_some() || self.smoke.is_some() {
            0.0
        } else {
            GRAIN
        };
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
                liquid,
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
    how: Blur,
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
    /// Where it reads, on the screen: the glass and `LIVE_MARGIN` around –
    /// a smoke, only itself.
    region: Option<Rectangle<i32, Physical>>,
    /// The appearance and material it was blurred for last.
    look: Option<(Appearance, glass::Material)>,
    /// What lay behind it in the region at the last frame, back to front.
    seen: Vec<Seen>,
    blur: Arc<Mutex<LiveBlur>>,
    /// A smoke's mask, what it was made of and over which region: made
    /// again only when one of them changes.
    mask: Option<(Mask, Rectangle<i32, Physical>, VkTexture)>,
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
                how: Blur::Kawase(Appearance::Light.material()),
                chain: Vec::new(),
                copy: None,
                drawn: false,
            })),
            mask: None,
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
            Behind::Panel(surface) | Behind::Smoke(surface) => surface.is_alive(),
            Behind::Plate => plated,
        }
    }

    /// The smoke's mask for `mask` over `region`, made again if either
    /// changed; whether it was.
    fn mask_for(
        &mut self,
        renderer: &mut VkRenderer,
        mask: &Mask,
        region: Rectangle<i32, Physical>,
    ) -> Result<bool> {
        if self
            .mask
            .as_ref()
            .is_some_and(|(m, r, _)| m == mask && *r == region)
        {
            return Ok(false);
        }
        self.mask = None;
        let texture = smoke_mask(renderer, mask, region)?;
        self.mask = Some((mask.clone(), region, texture));
        Ok(true)
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
        let s = glass.bounds;
        // A smoke blurs nothing where it ends: it reads no further.
        let margin = if glass.smoke.is_some() {
            0
        } else {
            LIVE_MARGIN
        };
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
        let masked = match &glass.smoke {
            Some(mask) => self.mask_for(renderer, mask, region)?,
            None => false,
        };
        let blur = self.blur.clone();
        let mut blur = blur
            .lock()
            .map_err(|_| error("live glass's blur poisoned"))?;
        let look = (appearance, glass.shows);
        let again = changed
            || masked
            || !blur.drawn
            || self.region != Some(region)
            || self.look != Some(look);
        self.region = Some(region);
        self.look = Some(look);
        if again {
            blur.how = match (&glass.smoke, &self.mask) {
                (Some(smoke), Some((_, _, mask))) => Blur::Gauss(Gauss {
                    sigma: smoke.smoke.strength,
                    mask: Some(mask.clone()),
                    half: false,
                }),
                _ => appearance.blur_for(glass.shows),
            };
            let size = (region.size.w, region.size.h).into();
            let blur = &mut *blur;
            renderer.prepare_glass(size, &blur.how, &mut blur.chain)?;
            blur.drawn = false;
            self.commit.increment();
            // The frame gives it the new blur (`Glass::draw`); should that
            // fail, it shows the background it came with – liquid glass and
            // a smoke nothing.
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The control center's two tiles (0.5.117): a capsule and a circle
    /// below it, the smoke as the shell asks for it.
    fn tiles() -> Mask {
        Mask {
            shapes: vec![
                (Rectangle::new((2376, 46).into(), (164, 75).into()), 37.5),
                (Rectangle::new((2376, 135).into(), (75, 75).into()), 37.5),
            ],
            smoke: Smoke {
                falloff: 130.0,
                strength: 4.0,
            },
        }
    }

    #[test]
    fn smoke_is_whole_on_the_tiles_and_thins_to_nothing_at_its_falloff() {
        let mask = tiles();
        // On a tile, and in the gap below its left end, 7 px from each.
        assert_eq!(smoke_at(&mask, 2400.0, 80.0), 1.0);
        let gap = smoke_at(&mask, 2413.5, 128.0);
        assert!(
            (gap - (1.0 - 7.0 / 130.0_f32).powf(1.6)).abs() < 1e-4,
            "{gap}"
        );
        // Left of the capsule's straight part: the mock-up's formula.
        let at = |d: f32| smoke_at(&mask, 2376.0 - d, 83.5);
        assert!((at(65.0) - 0.5_f32.powf(1.6)).abs() < 1e-4);
        assert_eq!(at(130.0), 0.0);
        assert_eq!(at(200.0), 0.0);
        // Thinner the further away, never back.
        let ts: Vec<f32> = (0..=130).map(|d| at(d as f32)).collect();
        assert!(ts.windows(2).all(|w| w[1] <= w[0]));
    }

    #[test]
    fn a_round_corner_is_measured_from_its_arc() {
        let r = Rectangle::new((0, 0).into(), (100, 100).into());
        assert_eq!(rounded_distance(50.0, 50.0, r, 0.0), -50.0);
        assert!((rounded_distance(150.0, 50.0, r, 20.0) - 50.0).abs() < 1e-4);
        // Diagonally off the corner: from the arc's centre (20, 20).
        let d = rounded_distance(-10.0, -10.0, r, 20.0);
        assert!((d - (30.0 * 2.0_f32.sqrt() - 20.0)).abs() < 1e-4, "{d}");
        // A radius beyond half the side: a circle.
        let c = Rectangle::new((0, 0).into(), (40, 40).into());
        assert!((rounded_distance(20.0, -10.0, c, 99.0) - 10.0).abs() < 1e-4);
    }
}
