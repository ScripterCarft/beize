//! Box drawing (U+2500–U+257F) and block elements (U+2580–U+259F) drawn
//! by the terminal itself, as VTE, kitty, foot and Alacritty do: a font's
//! lines end where its glyph ends, the terminal's reach the cell's edges,
//! so frames close across rows whatever the line height.
//!
//! Lines are light, heavy or double, from the cell's centre to each edge
//! they name. Rounded corners (╭╮╯╰) are corners without their corner
//! pixel, as in bitmap fonts; true arcs need anti-aliased curves, which
//! come with vector icons (shell, step 4). The diagonals stay with the
//! font.

use beize_ui::{Color, Rect, Scene};

/// An arm from the centre: none, light, heavy, double.
const N: u8 = 0;
const L: u8 = 1;
const H: u8 = 2;
const D: u8 = 3;

/// Arms of U+2500–U+257F: up, right, down, left. U+2571–U+2573 (the
/// diagonals) are none: the font draws them.
#[rustfmt::skip]
const ARMS: [[u8; 4]; 128] = [
    [N,L,N,L], [N,H,N,H], [L,N,L,N], [H,N,H,N], // ─━│┃
    [N,L,N,L], [N,H,N,H], [L,N,L,N], [H,N,H,N], // ┄┅┆┇ (dashed)
    [N,L,N,L], [N,H,N,H], [L,N,L,N], [H,N,H,N], // ┈┉┊┋ (dashed)
    [N,L,L,N], [N,H,L,N], [N,L,H,N], [N,H,H,N], // ┌┍┎┏
    [N,N,L,L], [N,N,L,H], [N,N,H,L], [N,N,H,H], // ┐┑┒┓
    [L,L,N,N], [L,H,N,N], [H,L,N,N], [H,H,N,N], // └┕┖┗
    [L,N,N,L], [L,N,N,H], [H,N,N,L], [H,N,N,H], // ┘┙┚┛
    [L,L,L,N], [L,H,L,N], [H,L,L,N], [L,L,H,N], // ├┝┞┟
    [H,L,H,N], [H,H,L,N], [L,H,H,N], [H,H,H,N], // ┠┡┢┣
    [L,N,L,L], [L,N,L,H], [H,N,L,L], [L,N,H,L], // ┤┥┦┧
    [H,N,H,L], [H,N,L,H], [L,N,H,H], [H,N,H,H], // ┨┩┪┫
    [N,L,L,L], [N,L,L,H], [N,H,L,L], [N,H,L,H], // ┬┭┮┯
    [N,L,H,L], [N,L,H,H], [N,H,H,L], [N,H,H,H], // ┰┱┲┳
    [L,L,N,L], [L,L,N,H], [L,H,N,L], [L,H,N,H], // ┴┵┶┷
    [H,L,N,L], [H,L,N,H], [H,H,N,L], [H,H,N,H], // ┸┹┺┻
    [L,L,L,L], [L,L,L,H], [L,H,L,L], [L,H,L,H], // ┼┽┾┿
    [H,L,L,L], [L,L,H,L], [H,L,H,L], [H,L,L,H], // ╀╁╂╃
    [H,H,L,L], [L,L,H,H], [L,H,H,L], [H,H,L,H], // ╄╅╆╇
    [L,H,H,H], [H,L,H,H], [H,H,H,L], [H,H,H,H], // ╈╉╊╋
    [N,L,N,L], [N,H,N,H], [L,N,L,N], [H,N,H,N], // ╌╍╎╏ (dashed)
    [N,D,N,D], [D,N,D,N], [N,D,L,N], [N,L,D,N], // ═║╒╓
    [N,D,D,N], [N,N,L,D], [N,N,D,L], [N,N,D,D], // ╔╕╖╗
    [L,D,N,N], [D,L,N,N], [D,D,N,N], [L,N,N,D], // ╘╙╚╛
    [D,N,N,L], [D,N,N,D], [L,D,L,N], [D,L,D,N], // ╜╝╞╟
    [D,D,D,N], [L,N,L,D], [D,N,D,L], [D,N,D,D], // ╠╡╢╣
    [N,D,L,D], [N,L,D,L], [N,D,D,D], [L,D,N,D], // ╤╥╦╧
    [D,L,N,L], [D,D,N,D], [L,D,L,D], [D,L,D,L], // ╨╩╪╫
    [D,D,D,D], [N,L,L,N], [N,N,L,L], [L,N,N,L], // ╬╭╮╯
    [L,L,N,N], [N,N,N,N], [N,N,N,N], [N,N,N,N], // ╰╱╲╳
    [N,N,N,L], [L,N,N,N], [N,L,N,N], [N,N,L,N], // ╴╵╶╷
    [N,N,N,H], [H,N,N,N], [N,H,N,N], [N,N,H,N], // ╸╹╺╻
    [N,H,N,L], [L,N,H,N], [N,L,N,H], [H,N,L,N], // ╼╽╾╿
];

/// Whether the terminal draws `c` itself.
pub fn own(c: char) -> bool {
    match c as u32 {
        i @ 0x2500..=0x257f => ARMS[(i - 0x2500) as usize] != [N; 4],
        0x2580..=0x259f => true,
        _ => false,
    }
}

/// Draws `c` into the cell at `x`, `y` of `w`×`h` pixels if it is one of
/// the terminal's own; false if the font draws it.
pub fn draw(scene: &mut Scene, c: char, x: f32, y: f32, w: f32, h: f32, color: Color) -> bool {
    let mut fill = |rx: f32, ry: f32, rw: f32, rh: f32, color: Color| {
        if rw > 0.0 && rh > 0.0 {
            scene.fill(
                Rect {
                    x: x + rx,
                    y: y + ry,
                    w: rw,
                    h: rh,
                },
                color,
            );
        }
    };
    match c as u32 {
        0x2500..=0x257f => lines(c as u32 - 0x2500, w, h, color, &mut fill),
        0x2580..=0x259f => blocks(c as u32, w, h, color, &mut fill),
        _ => false,
    }
}

fn lines(
    i: u32,
    w: f32,
    h: f32,
    color: Color,
    fill: &mut impl FnMut(f32, f32, f32, f32, Color),
) -> bool {
    let [up, right, down, left] = ARMS[i as usize];
    if [up, right, down, left] == [N; 4] {
        return false;
    }
    let light = (w / 8.0).round().max(1.0);
    let thickness = |arm: u8| if arm == H { 2.0 * light } else { light };
    // Double lines: two light strokes this far either side of the centre.
    let gap = light;
    let (cx, cy) = ((w / 2.0).floor(), (h / 2.0).floor());
    let start = |t: f32| -> f32 { (t / 2.0).floor() };
    let dashes = match i {
        0x04..=0x07 => 3,
        0x08..=0x0b => 4,
        0x4c..=0x4f => 2,
        _ => 1,
    };
    // Rounded corners (╭╮╯╰): their arms stop short of the centre, the
    // corner's pixel stays empty.
    let arc = (0x6d..=0x70).contains(&i);
    // Where the strokes across lie around the centre `c`: an arm runs to
    // their far edge, so corners and crossings close without overhang.
    let core = |c: f32, a: u8, b: u8| -> (f32, f32) {
        if a == D || b == D {
            (c - gap - start(light), c + gap - start(light) + light)
        } else {
            let t = thickness(a.max(b));
            (c - start(t), c - start(t) + t)
        }
    };
    // From the centre to an edge: a rightward (downward) arm starts at the
    // core's near edge, a leftward (upward) one ends at its far edge; an
    // arc's arms leave the centre's pixel out.
    let span = |c: f32, a: u8, b: u8, forwards: bool, edge: f32| -> (f32, f32) {
        let (lo, hi) = core(c, a, b);
        match (forwards, arc) {
            (true, false) => (lo, edge),
            (true, true) => (hi, edge),
            (false, false) => (0.0, hi),
            (false, true) => (0.0, lo),
        }
    };

    // Horizontal arms.
    for (arm, rightwards) in [(right, true), (left, false)] {
        if arm == N {
            continue;
        }
        if arm == D {
            for (offset, side) in [(-gap, up), (gap, down)] {
                let other = if offset < 0.0 { down } else { up };
                // Stop at the line on its own side, or round the corner.
                let reach = if side != N {
                    if side == D { gap } else { 0.0 }
                } else if other == D {
                    -gap
                } else {
                    0.0
                };
                let yy = cy + offset - start(light);
                let (from, to) = if rightwards {
                    (cx + reach, w)
                } else {
                    (0.0, cx - reach + light)
                };
                fill(from, yy, to - from, light, color);
            }
            continue;
        }
        let t = thickness(arm);
        let yy = cy - start(t);
        let (from, to) = span(cx, up, down, rightwards, w);
        dashed(fill, from, yy, to - from, t, dashes, true, color);
    }
    // Vertical arms.
    for (arm, downwards) in [(down, true), (up, false)] {
        if arm == N {
            continue;
        }
        if arm == D {
            for (offset, side) in [(-gap, left), (gap, right)] {
                let other = if offset < 0.0 { right } else { left };
                let reach = if side != N {
                    if side == D { gap } else { 0.0 }
                } else if other == D {
                    -gap
                } else {
                    0.0
                };
                let xx = cx + offset - start(light);
                let (from, to) = if downwards {
                    (cy + reach, h)
                } else {
                    (0.0, cy - reach + light)
                };
                fill(xx, from, light, to - from, color);
            }
            continue;
        }
        let t = thickness(arm);
        let xx = cx - start(t);
        let (from, to) = span(cy, left, right, downwards, h);
        dashed(fill, xx, from, t, to - from, dashes, false, color);
    }
    true
}

/// A stroke, in `n` dashes with gaps between (n = 1: solid).
#[allow(clippy::too_many_arguments)]
fn dashed(
    fill: &mut impl FnMut(f32, f32, f32, f32, Color),
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    n: u32,
    horizontal: bool,
    color: Color,
) {
    if n == 1 {
        fill(x, y, w, h, color);
        return;
    }
    let length = if horizontal { w } else { h };
    let part = length / n as f32;
    let gap = (part / 3.0).round().max(1.0);
    for k in 0..n {
        let from = (k as f32 * part).round();
        let to = ((k + 1) as f32 * part).round() - gap;
        if horizontal {
            fill(x + from, y, to - from, h, color);
        } else {
            fill(x, y + from, w, to - from, color);
        }
    }
}

fn blocks(
    c: u32,
    w: f32,
    h: f32,
    color: Color,
    fill: &mut impl FnMut(f32, f32, f32, f32, Color),
) -> bool {
    let eighth_h = |n: f32| (h * n / 8.0).round();
    let eighth_w = |n: f32| (w * n / 8.0).round();
    let (hw, hh) = ((w / 2.0).round(), (h / 2.0).round());
    let quadrants = |ul: bool,
                     ur: bool,
                     ll: bool,
                     lr: bool,
                     fill: &mut dyn FnMut(f32, f32, f32, f32, Color)| {
        for (on, x, y, qw, qh) in [
            (ul, 0.0, 0.0, hw, hh),
            (ur, hw, 0.0, w - hw, hh),
            (ll, 0.0, hh, hw, h - hh),
            (lr, hw, hh, w - hw, h - hh),
        ] {
            if on {
                fill(x, y, qw, qh, color);
            }
        }
    };
    match c {
        0x2580 => fill(0.0, 0.0, w, hh, color),
        0x2581..=0x2588 => {
            let n = (c - 0x2580) as f32;
            let top = h - eighth_h(n);
            fill(0.0, top, w, h - top, color);
        }
        0x2589..=0x258f => fill(0.0, 0.0, eighth_w((0x2590 - c) as f32), h, color),
        0x2590 => fill(hw, 0.0, w - hw, h, color),
        // Shades: a quarter, half, three quarters of the colour.
        0x2591..=0x2593 => {
            let a = [0x40, 0x80, 0xc0][(c - 0x2591) as usize];
            fill(0.0, 0.0, w, h, Color { a, ..color });
        }
        0x2594 => fill(0.0, 0.0, w, eighth_h(1.0), color),
        0x2595 => {
            let left = w - eighth_w(1.0);
            fill(left, 0.0, w - left, h, color);
        }
        0x2596 => quadrants(false, false, true, false, fill),
        0x2597 => quadrants(false, false, false, true, fill),
        0x2598 => quadrants(true, false, false, false, fill),
        0x2599 => quadrants(true, false, true, true, fill),
        0x259a => quadrants(true, false, false, true, fill),
        0x259b => quadrants(true, true, true, false, fill),
        0x259c => quadrants(true, true, false, true, fill),
        0x259d => quadrants(false, true, false, false, fill),
        0x259e => quadrants(false, true, true, false, fill),
        0x259f => quadrants(false, true, true, true, fill),
        _ => return false,
    }
    true
}
