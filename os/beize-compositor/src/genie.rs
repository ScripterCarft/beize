//! The genie (ADR 0032 §5): a window goes into its app's symbol in the dock
//! and comes back out of it, bent like a funnel – macOS' "Genie", KWin's
//! "Magic Lamp" (`magiclamp.cpp`: the window as a grid whose points bend
//! towards the icon). One number drives it, `p`, from 0 (the window where
//! it is) to 1 (in its symbol), in two overlapping parts:
//!
//! - **Bending**, till p = 0.5: from the window's top edge down to the
//!   symbol's top edge its sides follow the smooth curve 3n² − 2n³ (no
//!   kink in place or slope) towards the symbol's sides; below, the funnel
//!   is as wide as the symbol.
//! - **Sliding**, from p = 0.4 to 0.9: the content slides down through the
//!   funnel. Its bottom edge reaches the symbol's bottom after 1/1.6 of the
//!   slide (p ≈ 0.71) and stays there, so the content is squeezed in the
//!   neck rather than reaching past the symbol; its top edge reaches the
//!   symbol's top at p = 0.9.
//!
//! At p = 0.9 the window lies wholly in its symbol: nothing changes above
//! it, and a window going ends there (`END`). The funnel is a shape fixed
//! in space; the content moves through it.
//!
//! The window is its picture (`animation::Picture`), drawn as a mesh
//! (`render::geometry::Mesh`): a line across it about every 4 texels of
//! its height, each line's ends on the funnel's sides, its texels spread
//! evenly between them. Between two lines lies a row, a strip of columns:
//! as many as keep the strip within a quarter pixel of the rows' true
//! shape (a strip's two triangles bend a trapezoid by a quarter of its
//! change in width). The mesh reaches a pixel past the picture on every
//! side, where the shader fades its edge out (`STRIP`).
//!
//! Pure arithmetic, tested without a GPU.

use smithay::desktop::layer_map_for_output;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::{DisplayHandle, Resource};
use smithay::utils::{Logical, Physical, Rectangle};
use smithay::wayland::compositor::{Cacheable, with_states};

use crate::render::geometry::{Line, Mesh};
use crate::state::State;

/// Where the apps' symbols lie on a surface of the shell's (the dock), by
/// app id (`beize_shell_v1.dock_icon`). Double-buffered, applied with the
/// surface's commit, so the symbols and their rectangles move together.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DockIcons(pub Vec<(String, Rectangle<i32, Logical>)>);

impl Cacheable for DockIcons {
    fn commit(&mut self, _dh: &DisplayHandle) -> DockIcons {
        self.clone()
    }

    fn merge_into(self, into: &mut DockIcons, _dh: &DisplayHandle) {
        *into = self;
    }
}

/// Changes the symbols `surface` tells of with its next commit.
pub fn set_pending(surface: &WlSurface, change: impl FnOnce(&mut DockIcons)) {
    if surface.is_alive() {
        with_states(surface, |states| {
            change(states.cached_state.get::<DockIcons>().pending());
        });
    }
}

impl State {
    /// Where the symbol of `app` lies on the screen, if a panel of the
    /// shell's shows one.
    pub fn dock_icon(&self, app: &str) -> Option<Rectangle<i32, Physical>> {
        let map = layer_map_for_output(self.output.as_ref()?);
        map.layers().find_map(|panel| {
            let place = map.layer_geometry(panel)?;
            let icons = with_states(panel.wl_surface(), |states| {
                states.cached_state.get::<DockIcons>().current().0.clone()
            });
            icons
                .into_iter()
                .find(|(id, rect)| id == app && !rect.is_empty())
                .map(|(_, rect)| Rectangle::new(place.loc + rect.loc, rect.size).to_physical(1))
        })
    }
}

/// Where a window going stops: wholly in its symbol (ADR 0032 §5).
pub const END: f64 = 0.9;
/// The bending, from 0 till this.
const BENT_AT: f64 = 0.5;
/// The sliding, from and till.
const SLIDE_FROM: f64 = 0.4;
const SLIDE_TILL: f64 = 0.9;
/// How much faster the bottom edge slides than the top: it reaches the
/// symbol after 1/1.6 of the slide.
const BOTTOM_FASTER: f64 = 1.6;
/// The shadow belongs to a rectangle: it fades out by here.
pub const SHADOW_GONE: f64 = 0.15;
/// Texels of the picture's height per row.
const ROW: f64 = 4.0;
/// A row's strip stays within this of its true shape, in pixels.
const TRUE_WITHIN: f64 = 0.25;
/// The most columns a row is split into.
const MOST_COLUMNS: u32 = 64;
/// How far the mesh reaches past the picture, in pixels: where its edge
/// fades out.
const FRINGE: f64 = 1.0;

/// A rectangle in pixels of the screen, in floating point.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Rect {
    x: f64,
    y: f64,
    w: f64,
    h: f64,
}

impl Rect {
    fn of(r: Rectangle<i32, Physical>) -> Rect {
        Rect {
            x: f64::from(r.loc.x),
            y: f64::from(r.loc.y),
            w: f64::from(r.size.w),
            h: f64::from(r.size.h),
        }
    }

    fn right(&self) -> f64 {
        self.x + self.w
    }

    fn bottom(&self) -> f64 {
        self.y + self.h
    }
}

/// A window's picture at `place` going into the symbol at `icon`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Genie {
    place: Rect,
    icon: Rect,
}

/// 3n² − 2n³: from 0 to 1 with no slope at either end.
fn smooth(n: f64) -> f64 {
    let n = n.clamp(0.0, 1.0);
    n * n * (3.0 - 2.0 * n)
}

/// How far `p` has come from `from` to `till`, 0 to 1.
fn part(p: f64, from: f64, till: f64) -> f64 {
    ((p - from) / (till - from)).clamp(0.0, 1.0)
}

impl Genie {
    /// The genie of a picture at `place` into `icon`; none where it cannot
    /// run – an empty picture or symbol, or a symbol not below the window's
    /// top edge (a dock at the side comes later, ADR 0032 §5).
    pub fn new(place: Rectangle<i32, Physical>, icon: Rectangle<i32, Physical>) -> Option<Genie> {
        let (place, icon) = (Rect::of(place), Rect::of(icon));
        (place.w > 0.0 && place.h > 0.0 && icon.w > 0.0 && icon.h > 0.0 && icon.y > place.y)
            .then_some(Genie { place, icon })
    }

    /// The farthest a point of the window travels, in pixels: a quarter
    /// pixel of the way is a quarter pixel divided by it (ADR 0032 §1).
    pub fn longest_way(&self) -> f64 {
        let (place, icon) = (&self.place, &self.icon);
        [
            icon.y - place.y,
            (icon.bottom() - place.bottom()).abs(),
            (icon.x - place.x).abs(),
            (icon.right() - place.right()).abs(),
        ]
        .into_iter()
        .fold(1.0, f64::max)
    }

    /// The funnel's sides at `y`, bent by `bent` (0 to 1).
    fn sides(&self, bent: f64, y: f64) -> (f64, f64) {
        let (place, icon) = (&self.place, &self.icon);
        let curve = bent * smooth((y - place.y) / (icon.y - place.y));
        (
            place.x + curve * (icon.x - place.x),
            place.right() + curve * (icon.right() - place.right()),
        )
    }

    /// Where the content's top and bottom edge lie, slid by `slid` (0 to 1).
    fn content(&self, slid: f64) -> (f64, f64) {
        let (place, icon) = (&self.place, &self.icon);
        let top = place.y + slid * (icon.y - place.y);
        let low = (slid * BOTTOM_FASTER).min(1.0);
        let bottom = place.bottom() + low * (icon.bottom() - place.bottom());
        (top, bottom)
    }

    /// The picture as shown at `p`: lines across it, top to bottom.
    pub fn mesh(&self, p: f64) -> Mesh {
        let (w, h) = (self.place.w, self.place.h);
        let bent = part(p, 0.0, BENT_AT);
        let (top, bottom) = self.content(part(p, SLIDE_FROM, SLIDE_TILL));
        let tall = bottom - top;
        let content = [0.0, 0.0, w as f32, h as f32];
        if tall <= f64::EPSILON {
            return Mesh {
                content,
                ..Mesh::default()
            };
        }
        // A pixel past the top and the bottom, in texels.
        let past = FRINGE * h / tall;
        let rows = (h / ROW).ceil().max(1.0) as usize;
        let mut lines = Vec::with_capacity(rows + 1);
        let mut widest_change: f64 = 0.0;
        for k in 0..=rows {
            let v = -past + (h + 2.0 * past) * k as f64 / rows as f64;
            let y = top + v / h * tall;
            let (left, right) = self.sides(bent, y);
            let wide = right - left;
            // A pixel past each side, in texels.
            let aside = if wide > f64::EPSILON {
                FRINGE * w / wide
            } else {
                0.0
            };
            if let Some(last) = lines.last() {
                let last: &Line = last;
                widest_change =
                    widest_change.max(((right - left) - (last.right - last.left)).abs());
            }
            lines.push(Line {
                y,
                left: left - FRINGE,
                right: right + FRINGE,
                u: [-aside, w + aside],
                v,
            });
        }
        let columns = (widest_change / (4.0 * TRUE_WITHIN)).ceil() as u32;
        Mesh {
            lines,
            columns: columns.clamp(1, MOST_COLUMNS),
            content,
        }
    }

    /// How much of its shadow shows at `p`: it fades out first.
    pub fn shadow(p: f64) -> f32 {
        (1.0 - part(p, 0.0, SHADOW_GONE)) as f32
    }
}

/// The pixels `mesh` covers.
pub fn bounds(mesh: &Mesh) -> Rectangle<i32, Physical> {
    let mut lines = mesh.lines.iter();
    let Some(first) = lines.next() else {
        return Rectangle::default();
    };
    let (mut x0, mut x1, mut y0, mut y1) = (first.left, first.right, first.y, first.y);
    for line in lines {
        x0 = x0.min(line.left);
        x1 = x1.max(line.right);
        y0 = y0.min(line.y);
        y1 = y1.max(line.y);
    }
    let (x0, y0) = (x0.floor() as i32, y0.floor() as i32);
    let (x1, y1) = (x1.ceil() as i32, y1.ceil() as i32);
    Rectangle::new((x0, y0).into(), (x1 - x0, y1 - y0).into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: i32, y: i32, w: i32, h: i32) -> Rectangle<i32, Physical> {
        Rectangle::new((x, y).into(), (w, h).into())
    }

    /// A window 1200 x 800 at (500, 200) on a 2560 x 1440 screen; its
    /// symbol 56 px in the dock, left of the middle.
    fn genie() -> Genie {
        Genie::new(rect(500, 200, 1200, 800), rect(1200, 1357, 56, 56)).unwrap()
    }

    /// The lines that lie on the content, without the fringe.
    fn inner(mesh: &Mesh) -> Vec<Line> {
        mesh.lines
            .iter()
            .filter(|l| l.v >= 0.0 && l.v <= f64::from(mesh.content[3]))
            .map(|l| Line {
                left: l.left + FRINGE,
                right: l.right - FRINGE,
                ..*l
            })
            .collect()
    }

    fn steps() -> impl Iterator<Item = f64> {
        (0..=100).map(|i| f64::from(i) / 100.0)
    }

    #[test]
    fn at_the_start_it_is_the_window() {
        let mesh = genie().mesh(0.0);
        let lines = inner(&mesh);
        for line in &lines {
            assert!((line.left - 500.0).abs() < 1e-9, "{line:?}");
            assert!((line.right - 1700.0).abs() < 1e-9, "{line:?}");
        }
        let (first, last) = (lines[0], lines[lines.len() - 1]);
        assert!(first.y >= 200.0 && first.y < 204.0, "{first:?}");
        assert!(last.y <= 1000.0 && last.y > 996.0, "{last:?}");
        // The fringe a pixel past every side.
        let outer = bounds(&mesh);
        assert_eq!(outer, rect(499, 199, 1202, 802));
    }

    #[test]
    fn at_its_end_it_lies_in_the_symbol() {
        let mesh = genie().mesh(END);
        for line in inner(&mesh) {
            assert!(
                line.left >= 1200.0 - 1e-9 && line.right <= 1256.0 + 1e-9,
                "{line:?}"
            );
            assert!(
                line.y >= 1357.0 - 1e-9 && line.y <= 1413.0 + 1e-9,
                "{line:?}"
            );
        }
        // Past the end nothing changes.
        assert_eq!(genie().mesh(1.0), mesh);
    }

    #[test]
    fn it_is_never_wider_than_the_window() {
        let genie = genie();
        for p in steps() {
            for line in inner(&genie.mesh(p)) {
                assert!(line.right - line.left <= 1200.0 + 1e-9, "p {p}: {line:?}");
            }
        }
    }

    #[test]
    fn its_sides_bend_without_a_kink() {
        // Down a side the change from one line to the next changes
        // smoothly: no line turns sharply against its neighbours.
        let genie = genie();
        for p in steps() {
            let lines = inner(&genie.mesh(p));
            for three in lines.windows(3) {
                let slope = |a: &Line, b: &Line| (b.left - a.left) / (b.y - a.y);
                let turn = (slope(&three[1], &three[2]) - slope(&three[0], &three[1])).abs();
                assert!(turn < 0.05, "p {p}: turns by {turn} at {:?}", three[1]);
            }
        }
    }

    #[test]
    fn it_moves_on_smoothly() {
        // A small step of p moves no point far: no jump between frames.
        let genie = genie();
        for i in 0..1000 {
            let p = f64::from(i) / 1000.0;
            let (a, b) = (genie.mesh(p), genie.mesh(p + 0.001));
            for (a, b) in a.lines.iter().zip(&b.lines) {
                let moved = (a.left - b.left)
                    .abs()
                    .max((a.right - b.right).abs())
                    .max((a.y - b.y).abs());
                assert!(moved < 6.0, "p {p}: {moved} px");
            }
        }
    }

    #[test]
    fn the_bottom_reaches_the_symbol_before_the_top() {
        let genie = genie();
        let bottom = |p: f64| inner(&genie.mesh(p)).last().map(|l| l.y).unwrap();
        // 1/1.6 of the slide: p = 0.4 + 0.5 / 1.6.
        assert!(bottom(0.70) < 1409.0, "{}", bottom(0.70));
        assert!(bottom(0.72) > 1409.0, "{}", bottom(0.72));
        // And stays there: squeezed in the neck, not past the symbol.
        for p in [0.75, 0.8, 0.85, 0.9] {
            assert!(bottom(p) <= 1413.0 + 1e-9, "p {p}: {}", bottom(p));
        }
    }

    #[test]
    fn rows_bend_less_than_a_quarter_pixel() {
        // A strip's two triangles bend a row by a quarter of its change in
        // width, split into its columns.
        let genie = genie();
        for p in steps() {
            let mesh = genie.mesh(p);
            for pair in mesh.lines.windows(2) {
                let change =
                    ((pair[1].right - pair[1].left) - (pair[0].right - pair[0].left)).abs();
                assert!(
                    change / 4.0 / f64::from(mesh.columns) <= TRUE_WITHIN + 1e-9,
                    "p {p}: {change} px over {} columns",
                    mesh.columns
                );
            }
        }
    }

    #[test]
    fn its_shadow_fades_first() {
        assert_eq!(Genie::shadow(0.0), 1.0);
        assert!(Genie::shadow(0.075) > 0.4 && Genie::shadow(0.075) < 0.6);
        assert_eq!(Genie::shadow(SHADOW_GONE), 0.0);
        assert_eq!(Genie::shadow(END), 0.0);
    }

    #[test]
    fn a_symbol_above_the_window_has_no_genie() {
        assert!(Genie::new(rect(0, 500, 400, 300), rect(10, 400, 56, 56)).is_none());
        assert!(Genie::new(rect(0, 0, 0, 300), rect(10, 400, 56, 56)).is_none());
    }

    #[test]
    fn its_longest_way_is_the_top_edge_to_the_symbol() {
        assert_eq!(genie().longest_way(), 1157.0);
    }
}
