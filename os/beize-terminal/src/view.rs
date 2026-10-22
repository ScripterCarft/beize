//! The terminal as Beize UI draws it: the grid in cells of JetBrains Mono
//! on Beize's dark ground (VISION), backgrounds merged along each row, one
//! glyph per character, the cursor as a block (hollow without the
//! keyboard).
//!
//! Colours: the 16 of the ANSI palette from GNOME's Adwaita palette, as
//! GNOME Console has them; 16–255 are xterm's cube and grey ramp, which
//! programs compute themselves; 24-bit colour as sent. Programs may change
//! any of them (OSC 4, 10, 11), alacritty_terminal keeps that.

use alacritty_terminal::Term;
use alacritty_terminal::event::EventListener;
use alacritty_terminal::index::{Column, Line, Point};
use alacritty_terminal::term::TermMode;
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::color::Colors;
use alacritty_terminal::vte::ansi::{Color as TermColor, CursorShape, NamedColor, Rgb};
use beize_ui::{Cells, Color, Rect, Scene};

use crate::boxes;

/// Room between the window's edge and the grid.
pub const PADDING: f32 = 8.0;
pub const BACKGROUND: Rgb = rgb(0x1e1e1e);
pub const FOREGROUND: Rgb = rgb(0xf4f4f4);
const ANSI: [Rgb; 16] = [
    rgb(0x241f31),
    rgb(0xc01c28),
    rgb(0x2ec27e),
    rgb(0xf5c211),
    rgb(0x1e78e4),
    rgb(0x9841bb),
    rgb(0x0ab9dc),
    rgb(0xc0bfbc),
    rgb(0x5e5c64),
    rgb(0xed333b),
    rgb(0x57e389),
    rgb(0xf8e45c),
    rgb(0x51a1ff),
    rgb(0xc061cb),
    rgb(0x4fd2fd),
    rgb(0xf6f5f4),
];
/// Faint text (SGR 2): two thirds of its colour, as Alacritty's default.
const DIM: f32 = 2.0 / 3.0;

const fn rgb(v: u32) -> Rgb {
    Rgb {
        r: (v >> 16) as u8,
        g: (v >> 8) as u8,
        b: v as u8,
    }
}

/// The colour of palette entry `index` (0–255, or one of the named ones
/// above: foreground, background, cursor …) unless a program changed it.
pub fn palette(colors: &Colors, index: usize) -> Rgb {
    if let Some(set) = colors[index] {
        return set;
    }
    match index {
        0..16 => ANSI[index],
        // xterm's 6×6×6 cube.
        16..232 => {
            let i = index - 16;
            let level = |v: usize| if v == 0 { 0 } else { (55 + v * 40) as u8 };
            Rgb {
                r: level(i / 36),
                g: level(i / 6 % 6),
                b: level(i % 6),
            }
        }
        // And its 24 greys.
        232..256 => {
            let v = (8 + (index - 232) * 10) as u8;
            Rgb { r: v, g: v, b: v }
        }
        _ => match index {
            i if i == NamedColor::Background as usize => BACKGROUND,
            i if i == NamedColor::Cursor as usize => FOREGROUND,
            i if i == NamedColor::BrightForeground as usize => FOREGROUND,
            i if i == NamedColor::DimForeground as usize => dim(FOREGROUND),
            i if (NamedColor::DimBlack as usize..=NamedColor::DimWhite as usize).contains(&i) => {
                dim(palette(colors, i - NamedColor::DimBlack as usize))
            }
            _ => FOREGROUND,
        },
    }
}

fn resolve(colors: &Colors, color: TermColor) -> Rgb {
    match color {
        TermColor::Spec(rgb) => rgb,
        TermColor::Named(named) => palette(colors, named as usize),
        TermColor::Indexed(i) => palette(colors, i as usize),
    }
}

fn dim(c: Rgb) -> Rgb {
    let f = |v: u8| (v as f32 * DIM).round() as u8;
    Rgb {
        r: f(c.r),
        g: f(c.g),
        b: f(c.b),
    }
}

fn color(c: Rgb) -> Color {
    Color {
        r: c.r,
        g: c.g,
        b: c.b,
        a: 0xff,
    }
}

/// The fonts of the grid: regular and bold, in the regular's cells.
pub struct Fonts {
    pub regular: Cells,
    pub bold: Cells,
}

/// The whole window: `size` in pixels.
pub fn draw<T: EventListener>(
    term: &Term<T>,
    fonts: &Fonts,
    focused: bool,
    size: (u32, u32),
) -> Scene {
    let content = term.renderable_content();
    let colors = content.colors;
    let cells = &fonts.regular;
    let (cw, ch) = (cells.width, cells.height);
    let ground = palette(colors, NamedColor::Background as usize);
    let mut scene = Scene::new();
    scene.fill(
        Rect {
            x: 0.0,
            y: 0.0,
            w: size.0 as f32,
            h: size.1 as f32,
        },
        color(ground),
    );
    let offset = content.display_offset as i32;
    let selection = content.selection;
    let cursor = content.cursor;
    let show_cursor =
        content.mode.contains(TermMode::SHOW_CURSOR) && cursor.shape != CursorShape::Hidden;
    // Glyphs after all backgrounds: a wide glyph may reach into the next
    // cell's.
    let mut glyphs = Vec::new();
    // Underlines and strike-through, over the glyphs.
    let mut lines = Vec::new();
    // Box drawing and blocks, which the terminal draws itself.
    let mut boxed = Vec::new();
    // The background run being merged: its row, first column, end, colour.
    let mut run: Option<(i32, usize, usize, Rgb)> = None;
    let flush = |scene: &mut Scene, run: &mut Option<(i32, usize, usize, Rgb)>| {
        if let Some((row, from, to, bg)) = run.take() {
            scene.fill(
                Rect {
                    x: PADDING + from as f32 * cw,
                    y: PADDING + row as f32 * ch,
                    w: (to - from) as f32 * cw,
                    h: ch,
                },
                color(bg),
            );
        }
    };
    for indexed in content.display_iter {
        let cell = indexed.cell;
        let point = indexed.point;
        let row = point.line.0 + offset;
        let column = point.column.0;
        if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
            continue;
        }
        let mut fg = resolve(colors, cell.fg);
        let mut bg = resolve(colors, cell.bg);
        if cell.flags.contains(Flags::DIM) {
            fg = dim(fg);
        }
        if cell.flags.contains(Flags::INVERSE) {
            std::mem::swap(&mut fg, &mut bg);
        }
        if selection.is_some_and(|s| s.contains(point)) {
            std::mem::swap(&mut fg, &mut bg);
        }
        let on_cursor =
            show_cursor && focused && cursor.point == point && cursor.shape == CursorShape::Block;
        if on_cursor {
            (fg, bg) = (bg, palette(colors, NamedColor::Cursor as usize));
        }
        let width = if cell.flags.contains(Flags::WIDE_CHAR) {
            2
        } else {
            1
        };
        match &mut run {
            Some((r, _, to, c)) if *r == row && *to == column && *c == bg => *to += width,
            _ => {
                flush(&mut scene, &mut run);
                if bg != ground {
                    run = Some((row, column, column + width, bg));
                }
            }
        }
        let hidden = cell.flags.contains(Flags::HIDDEN);
        if !hidden && cell.c != ' ' && cell.c != '\0' {
            let bold = cell.flags.contains(Flags::BOLD);
            let font = if bold { &fonts.bold } else { &fonts.regular };
            let x = PADDING + column as f32 * cw;
            let top = PADDING + row as f32 * ch;
            if boxes::own(cell.c) {
                boxed.push((cell.c, x, top, width as f32 * cw, fg));
            } else {
                let y = top + cells.baseline;
                glyphs.push((font.glyph(cell.c), bold, x, y, fg));
                // Combining marks attach behind their base, where its
                // advance ends: the font places them from there.
                let after = x + width as f32 * cw;
                for &mark in cell.zerowidth().unwrap_or_default() {
                    glyphs.push((font.glyph(mark), bold, after, y, fg));
                }
            }
        }
        if !hidden
            && cell
                .flags
                .intersects(Flags::ALL_UNDERLINES | Flags::STRIKEOUT)
        {
            let y = PADDING
                + row as f32 * ch
                + if cell.flags.contains(Flags::STRIKEOUT) {
                    (cells.baseline * 0.65).round()
                } else {
                    cells.baseline + 2.0
                };
            let line = Rect {
                x: PADDING + column as f32 * cw,
                y: y.min(PADDING + (row + 1) as f32 * ch - 1.0),
                w: width as f32 * cw,
                h: 1.0,
            };
            lines.push((line, fg));
        }
    }
    flush(&mut scene, &mut run);
    for (id, bold, x, y, fg) in glyphs {
        let font = if bold { &fonts.bold } else { &fonts.regular };
        scene.cell_glyph(font, id, x, y, color(fg));
    }
    for (line, fg) in lines {
        scene.fill(line, color(fg));
    }
    for (c, x, y, w, fg) in boxed {
        boxes::draw(&mut scene, c, x, y, w, ch, color(fg));
    }
    if show_cursor {
        cursor_shape(
            &mut scene,
            cursor.shape,
            cursor.point,
            offset,
            focused,
            cells,
            colors,
        );
    }
    scene
}

/// The cursor where it is not a filled block (drawn with its cell).
fn cursor_shape(
    scene: &mut Scene,
    shape: CursorShape,
    point: Point,
    offset: i32,
    focused: bool,
    cells: &Cells,
    colors: &Colors,
) {
    let (cw, ch) = (cells.width, cells.height);
    let x = PADDING + point.column.0 as f32 * cw;
    let y = PADDING + (point.line.0 + offset) as f32 * ch;
    let c = color(palette(colors, NamedColor::Cursor as usize));
    let mut fill = |x, y, w, h| scene.fill(Rect { x, y, w, h }, c);
    match shape {
        CursorShape::Block if focused => {}
        CursorShape::Block | CursorShape::HollowBlock => {
            fill(x, y, cw, 1.0);
            fill(x, y + ch - 1.0, cw, 1.0);
            fill(x, y, 1.0, ch);
            fill(x + cw - 1.0, y, 1.0, ch);
        }
        CursorShape::Beam => fill(x, y, 2.0, ch),
        CursorShape::Underline => fill(x, y + ch - 2.0, cw, 2.0),
        CursorShape::Hidden => {}
    }
}

/// The grid's size for a window of `size` pixels: at least one cell.
pub fn grid(size: (u32, u32), cells: &Cells) -> (usize, usize) {
    let fit =
        |pixels: u32, cell: f32| ((pixels as f32 - 2.0 * PADDING) / cell).floor().max(1.0) as usize;
    (fit(size.0, cells.width), fit(size.1, cells.height))
}

/// The cell under the window's pixel `x`, `y`, as a point in the grid.
pub fn point(x: f64, y: f64, cells: &Cells, columns: usize, lines: usize, offset: usize) -> Point {
    let column = ((x as f32 - PADDING) / cells.width)
        .floor()
        .clamp(0.0, columns as f32 - 1.0) as usize;
    let line = ((y as f32 - PADDING) / cells.height)
        .floor()
        .clamp(0.0, lines as f32 - 1.0) as i32;
    Point::new(Line(line - offset as i32), Column(column))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_palette_is_xterms_beyond_the_sixteen() {
        let colors = Colors::default();
        assert_eq!(palette(&colors, 1), rgb(0xc01c28));
        assert_eq!(palette(&colors, 16), rgb(0x000000));
        assert_eq!(palette(&colors, 196), rgb(0xff0000));
        assert_eq!(palette(&colors, 110), rgb(0x87afd7));
        assert_eq!(palette(&colors, 232), rgb(0x080808));
        assert_eq!(palette(&colors, 255), rgb(0xeeeeee));
        assert_eq!(
            palette(&colors, NamedColor::Background as usize),
            BACKGROUND
        );
        assert_eq!(
            palette(&colors, NamedColor::Foreground as usize),
            FOREGROUND
        );
        assert_eq!(
            palette(&colors, NamedColor::DimRed as usize),
            dim(rgb(0xc01c28))
        );
    }

    #[test]
    fn a_program_may_change_a_colour() {
        let mut colors = Colors::default();
        colors[NamedColor::Background as usize] = Some(rgb(0x000000));
        assert_eq!(
            palette(&colors, NamedColor::Background as usize),
            rgb(0x000000)
        );
    }
}
