//! Round corners and shadows for windows (ADR 0028 §2), as macOS draws
//! every window: the compositor rounds each window's frame – its title bar
//! and content together – with a mask in the shader, and draws a soft
//! shadow below it, computed per pixel (`render`, `SHADOW`), so no window
//! size needs a texture of its own. The active window's shadow is larger
//! and darker, as on macOS.
//!
//! Windows that draw their own frame – GTK's, whose surface reaches past
//! its window geometry for its own shadow – keep theirs, as on GNOME; a
//! maximized window has neither, its edges are the screen's.

use crate::render::{Blur, Liquid, VkFrame, VkRenderer, VkTexture};
use beize_vulkan::pipeline::CORNER_EXTENT;
use smithay::backend::renderer::element::{Element, Id, Kind, RenderElement, UnderlyingStorage};
use smithay::backend::renderer::utils::{CommitCounter, DamageSet, OpaqueRegions};
use smithay::backend::renderer::{Renderer, RendererSuper};
use smithay::desktop::Window;
use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel::State as XdgState;
use smithay::utils::{Buffer, Logical, Physical, Point, Rectangle, Scale, Transform};
use std::cell::RefCell;

/// The corners' radius: macOS 27's windows, with the continuous corner of
/// `beize_vulkan`'s shader; measured 15.7 pt on a screenshot of macOS 27
/// (2 px per pt).
pub const RADIUS: i32 = 16;

/// The squares the corners' curves lie in, along each edge.
fn corner() -> i32 {
    (RADIUS as f32 * CORNER_EXTENT).ceil() as i32
}

/// The shadow of the active window and of the others: how far it falls
/// down, its blur and its darkness.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Look {
    offset: i32,
    sigma: f32,
    alpha: f32,
}

const ACTIVE: Look = Look {
    offset: 10,
    sigma: 14.0,
    alpha: 0.35,
};
const INACTIVE: Look = Look {
    offset: 4,
    sigma: 8.0,
    alpha: 0.2,
};
/// Below a menu, and below other glass that asks for one
/// (`Shadow::below_glass`).
const MENU: Look = Look {
    offset: 6,
    sigma: 12.0,
    alpha: 0.25,
};
const TILE: Look = Look {
    offset: 3,
    sigma: 8.0,
    alpha: 0.12,
};

/// The renderer's part: round corners, shadows and glass (`backdrop`) in
/// a frame. A trait, as the frame's elements are generic over the renderer
/// (`state::scene`).
pub trait Effects: Renderer<TextureId = VkTexture> {
    fn set_rounding(
        frame: &mut Self::Frame<'_, '_>,
        rounding: Option<(Rectangle<i32, Physical>, f32)>,
    );
    #[allow(clippy::too_many_arguments)]
    fn draw_shadow(
        frame: &mut Self::Frame<'_, '_>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        caster: Rectangle<i32, Physical>,
        cover: Rectangle<i32, Physical>,
        radius: i32,
        sigma: f32,
        color: [f32; 4],
    );
    #[allow(clippy::too_many_arguments)]
    fn draw_glass(
        frame: &mut Self::Frame<'_, '_>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        shape: Rectangle<i32, Physical>,
        radius: f32,
        glass: &VkTexture,
        covers: Rectangle<i32, Physical>,
        grain: f32,
        rim: [f32; 4],
        liquid: Option<Liquid>,
    );
    /// Blurs what the frame drew so far in `region` (live glass,
    /// `VkFrame::blur_behind`); the glass, ready for what is drawn after.
    fn blur_behind(
        frame: &mut Self::Frame<'_, '_>,
        region: Rectangle<i32, Physical>,
        blur: &Blur,
        chain: &[VkTexture],
        copy: &mut Option<VkTexture>,
    ) -> Result<VkTexture, Self::Error>;
}

impl Effects for VkRenderer {
    fn blur_behind(
        frame: &mut VkFrame<'_, '_>,
        region: Rectangle<i32, Physical>,
        blur: &Blur,
        chain: &[VkTexture],
        copy: &mut Option<VkTexture>,
    ) -> Result<VkTexture, Self::Error> {
        frame.blur_behind(region, blur, chain, copy)
    }

    fn set_rounding(
        frame: &mut VkFrame<'_, '_>,
        rounding: Option<(Rectangle<i32, Physical>, f32)>,
    ) {
        frame.set_rounding(rounding);
    }

    fn draw_shadow(
        frame: &mut VkFrame<'_, '_>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        caster: Rectangle<i32, Physical>,
        cover: Rectangle<i32, Physical>,
        radius: i32,
        sigma: f32,
        color: [f32; 4],
    ) {
        frame.draw_shadow(dst, damage, caster, cover, radius, sigma, color);
    }

    fn draw_glass(
        frame: &mut VkFrame<'_, '_>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        shape: Rectangle<i32, Physical>,
        radius: f32,
        glass: &VkTexture,
        covers: Rectangle<i32, Physical>,
        grain: f32,
        rim: [f32; 4],
        liquid: Option<Liquid>,
    ) {
        frame.draw_glass(
            dst, damage, shape, radius, glass, covers, grain, rim, liquid,
        );
    }
}

/// Whether the compositor rounds `window` and gives it a shadow: not
/// maximized or fullscreen, and not drawing its own frame. `frame` is its
/// title bar and content.
pub fn rounded(window: &Window, frame: Rectangle<i32, Logical>) -> bool {
    let maximized = window.toplevel().is_some_and(|t| {
        let states = t.current_state().states;
        states.contains(XdgState::Maximized) || states.contains(XdgState::Fullscreen)
    });
    // Its surfaces reach past its geometry: a frame and shadow of its own.
    let geometry = window.geometry();
    let own_frame = !geometry.contains_rect(window.bbox());
    !maximized && !own_frame && frame.size.w > 2 * corner() && frame.size.h > 2 * corner()
}

/// An element of a window, drawn inside its rounded frame (`frame`,
/// physical) if it has one.
pub struct Rounded<E> {
    inner: E,
    frame: Option<Rectangle<i32, Physical>>,
}

impl<E> Rounded<E> {
    pub fn new(inner: E, frame: Option<Rectangle<i32, Logical>>) -> Rounded<E> {
        Rounded {
            inner,
            frame: frame.map(|f| f.to_physical(1)),
        }
    }
}

impl<E: Element> Element for Rounded<E> {
    fn id(&self) -> &Id {
        self.inner.id()
    }

    fn current_commit(&self) -> CommitCounter {
        self.inner.current_commit()
    }

    fn location(&self, scale: Scale<f64>) -> Point<i32, Physical> {
        self.inner.location(scale)
    }

    fn src(&self) -> Rectangle<f64, Buffer> {
        self.inner.src()
    }

    fn transform(&self) -> Transform {
        self.inner.transform()
    }

    fn geometry(&self, scale: Scale<f64>) -> Rectangle<i32, Physical> {
        self.inner.geometry(scale)
    }

    fn damage_since(
        &self,
        scale: Scale<f64>,
        commit: Option<CommitCounter>,
    ) -> DamageSet<i32, Physical> {
        self.inner.damage_since(scale, commit)
    }

    /// The inner element's, but the frame's corners.
    fn opaque_regions(&self, scale: Scale<f64>) -> OpaqueRegions<i32, Physical> {
        let opaque = self.inner.opaque_regions(scale);
        let Some(f) = self.frame else {
            return opaque;
        };
        let origin = self.inner.geometry(scale).loc;
        let r = corner();
        let corners = [
            (f.loc.x, f.loc.y),
            (f.loc.x + f.size.w - r, f.loc.y),
            (f.loc.x, f.loc.y + f.size.h - r),
            (f.loc.x + f.size.w - r, f.loc.y + f.size.h - r),
        ]
        .map(|(x, y)| Rectangle::new(Point::from((x, y)) - origin, (r, r).into()));
        let regions = Rectangle::subtract_rects_many(opaque.iter().copied(), corners);
        OpaqueRegions::from_slice(&regions)
    }

    fn alpha(&self) -> f32 {
        self.inner.alpha()
    }

    fn kind(&self) -> Kind {
        self.inner.kind()
    }
}

impl<R: Effects, E: RenderElement<R>> RenderElement<R> for Rounded<E> {
    fn draw(
        &self,
        frame: &mut R::Frame<'_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        opaque_regions: &[Rectangle<i32, Physical>],
    ) -> Result<(), <R as RendererSuper>::Error> {
        let Some(mut rect) = self.frame else {
            return self.inner.draw(frame, src, dst, damage, opaque_regions);
        };
        // Where it is drawn: elsewhere when relocated (behind live glass).
        rect.loc += dst.loc - self.inner.geometry(Scale::from(1.0)).loc;
        R::set_rounding(frame, Some((rect, RADIUS as f32)));
        let drawn = self.inner.draw(frame, src, dst, damage, opaque_regions);
        R::set_rounding(frame, None);
        drawn
    }

    /// Never on a plane of its own: the corners are drawn.
    fn underlying_storage(&self, _renderer: &mut R) -> Option<UnderlyingStorage<'_>> {
        None
    }
}

/// A window's shadow: below its frame, drawn again when it moves, changes
/// size or becomes active or not. Or the shadow of glass that asked for one
/// (`backdrop`): a menu's, a tile's of the control center.
pub struct Shadow {
    id: Id,
    commit: CommitCounter,
    caster: Rectangle<i32, Physical>,
    radius: i32,
    look: Look,
}

/// What a window's shadow was last drawn for, in its user data.
#[derive(Default)]
struct Drawn {
    id: Option<Id>,
    commit: CommitCounter,
    last: Option<(Rectangle<i32, Logical>, bool)>,
}

impl Shadow {
    /// The shadow of `window` with its frame `frame`.
    pub fn new(window: &Window, frame: Rectangle<i32, Logical>) -> Shadow {
        let active = window
            .toplevel()
            .is_some_and(|t| t.current_state().states.contains(XdgState::Activated));
        let data = window.user_data();
        data.insert_if_missing(|| RefCell::new(Drawn::default()));
        let (id, commit) = match data.get::<RefCell<Drawn>>() {
            Some(drawn) => {
                let mut drawn = drawn.borrow_mut();
                let id = drawn.id.get_or_insert_with(Id::new).clone();
                if drawn.last != Some((frame, active)) {
                    drawn.last = Some((frame, active));
                    drawn.commit.increment();
                }
                (id, drawn.commit)
            }
            None => (Id::new(), CommitCounter::default()),
        };
        Shadow {
            id,
            commit,
            caster: frame.to_physical(1),
            radius: RADIUS,
            look: if active { ACTIVE } else { INACTIVE },
        }
    }

    /// The shadow below a glass's piece `rect` with corners of `radius`:
    /// a menu's larger, as Apple's menus cast one (black about 15 % at the
    /// edge in the HIG's images, fading over some 8 pt; offset and blur
    /// estimated), other glass's faint (the owner, 28.10., on mock-ups). The
    /// glass's `id` and `commit` tell when it changes.
    pub fn below_glass(
        id: Id,
        commit: CommitCounter,
        rect: Rectangle<i32, Physical>,
        radius: f32,
        shows: crate::glass::Material,
    ) -> Shadow {
        Shadow {
            id,
            commit,
            caster: rect,
            radius: radius.round() as i32,
            look: match shows {
                crate::glass::Material::Menu => MENU,
                _ => TILE,
            },
        }
    }

    /// Where the shadow falls: the frame, moved down, three sigmas around.
    fn area(&self) -> Rectangle<i32, Physical> {
        let reach = (3.0 * self.look.sigma).ceil() as i32;
        let caster = self.cast();
        Rectangle::new(
            (caster.loc.x - reach, caster.loc.y - reach).into(),
            (caster.size.w + 2 * reach, caster.size.h + 2 * reach).into(),
        )
    }

    /// The rectangle that casts it: the frame, moved down by the offset.
    fn cast(&self) -> Rectangle<i32, Physical> {
        Rectangle::new(
            (self.caster.loc.x, self.caster.loc.y + self.look.offset).into(),
            self.caster.size,
        )
    }
}

impl Element for Shadow {
    fn id(&self) -> &Id {
        &self.id
    }

    fn current_commit(&self) -> CommitCounter {
        self.commit
    }

    fn src(&self) -> Rectangle<f64, Buffer> {
        let size = self.area().size;
        Rectangle::from_size((f64::from(size.w), f64::from(size.h)).into())
    }

    fn geometry(&self, _scale: Scale<f64>) -> Rectangle<i32, Physical> {
        self.area()
    }
}

impl<R: Effects> RenderElement<R> for Shadow {
    fn draw(
        &self,
        frame: &mut R::Frame<'_, '_>,
        _src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        _opaque_regions: &[Rectangle<i32, Physical>],
    ) -> Result<(), <R as RendererSuper>::Error> {
        // The caster where `dst` puts the area (they are the same, but for
        // a move between geometry and drawing).
        let offset = dst.loc - self.area().loc;
        let mut caster = self.cast();
        caster.loc += offset;
        // The window itself hides the shadow below it, not the caster,
        // which lies lower by the offset.
        let mut cover = self.caster;
        cover.loc += offset;
        let a = self.look.alpha;
        R::draw_shadow(
            frame,
            dst,
            damage,
            caster,
            cover,
            self.radius,
            self.look.sigma,
            [0.0, 0.0, 0.0, a],
        );
        Ok(())
    }
}
