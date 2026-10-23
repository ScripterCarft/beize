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

use beize_compositor::render::{VkFrame, VkRenderer};
use smithay::backend::renderer::element::{Element, Id, Kind, RenderElement, UnderlyingStorage};
use smithay::backend::renderer::utils::{CommitCounter, DamageSet, OpaqueRegions};
use smithay::backend::renderer::{Renderer, RendererSuper};
use smithay::desktop::Window;
use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel::State as XdgState;
use smithay::utils::{Buffer, Logical, Physical, Point, Rectangle, Scale, Transform};
use std::cell::RefCell;

/// The corners' radius: macOS' windows.
pub const RADIUS: i32 = 10;

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

/// The renderer's part: round corners and shadows in a frame. A trait, as
/// the frame's elements are generic over the renderer (`state::scene`).
pub trait Rounding: Renderer {
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
        radius: i32,
        sigma: f32,
        color: [f32; 4],
    );
}

impl Rounding for VkRenderer {
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
        radius: i32,
        sigma: f32,
        color: [f32; 4],
    ) {
        frame.draw_shadow(dst, damage, caster, radius, sigma, color);
    }
}

/// Whether the compositor rounds `window` and gives it a shadow: not
/// maximized, and not drawing its own frame. `frame` is its title bar and
/// content.
pub fn rounded(window: &Window, frame: Rectangle<i32, Logical>) -> bool {
    let maximized = window
        .toplevel()
        .is_some_and(|t| t.current_state().states.contains(XdgState::Maximized));
    // Its surfaces reach past its geometry: a frame and shadow of its own.
    let geometry = window.geometry();
    let own_frame = !geometry.contains_rect(window.bbox());
    !maximized && !own_frame && frame.size.w > 2 * RADIUS && frame.size.h > 2 * RADIUS
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
        let r = RADIUS;
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

impl<R: Rounding, E: RenderElement<R>> RenderElement<R> for Rounded<E> {
    fn draw(
        &self,
        frame: &mut R::Frame<'_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        opaque_regions: &[Rectangle<i32, Physical>],
    ) -> Result<(), <R as RendererSuper>::Error> {
        let Some(rect) = self.frame else {
            return self.inner.draw(frame, src, dst, damage, opaque_regions);
        };
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
/// size or becomes active or not.
pub struct Shadow {
    id: Id,
    commit: CommitCounter,
    caster: Rectangle<i32, Physical>,
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
            look: if active { ACTIVE } else { INACTIVE },
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

impl<R: Rounding> RenderElement<R> for Shadow {
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
        let a = self.look.alpha;
        R::draw_shadow(
            frame,
            dst,
            damage,
            caster,
            RADIUS,
            self.look.sigma,
            [0.0, 0.0, 0.0, a],
        );
        Ok(())
    }
}
