//! `beize-load-test` – the load test's scene (ADR 0028 §9, "der Compositor
//! soll sich langweilen"), on bz's test medium, never in the image. It
//! opens many windows of Beize UI, as an app's – its swapchain's buffers,
//! dmabufs on a GPU –, and redraws every one of them each frame: a moving
//! gradient, translucent rounded blocks sliding across it and a line of
//! text with the frame's number, so the compositor composes all of them,
//! their round corners and shadows, and blurs anew the live glass of a
//! panel over any of them, every frame. The compositor measures itself
//! (`pacing:` in its journal); this program prints how often each window
//! got to draw.
//!
//! `--windows N` (default 12), `--size WxH` the content of each (default
//! 640x400; the compositor cascades them), `--seconds S` (default 20).
//! `--rotate` begins each round of drawing at the next window rather than
//! the first: drawn in a fixed order, the later windows got fewer frames
//! (0.5.111: 9380 to 9061), likely as their commits came after the frame's
//! drawing began; if so, rotated, all get about the same.
//! At the end one line per window, `window I: F frames, between frames
//! median M ms, max X ms`, then `load: N windows, F frames in S s, at most
//! A on their way` – a window's frames drawn and not yet shown at once.

use std::process::ExitCode;
use std::time::{Duration, Instant};

use beize_ui::{App, Color, Family, Rect, Scene, Style, Text, WindowId};

/// The blocks sliding across each window, and their size.
const BLOCKS: usize = 4;
const BLOCK: (f32, f32) = (120.0, 72.0);
/// One turn of the gradient's hues, and one way across for a block.
const HUE_TURN: f32 = 6.0;
const SLIDE: f32 = 2.5;

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("beize-load-test: {e}");
            ExitCode::FAILURE
        }
    }
}

struct Options {
    windows: u32,
    size: (u32, u32),
    seconds: u64,
    rotate: bool,
}

fn options() -> Result<Options, String> {
    let mut options = Options {
        windows: 12,
        size: (640, 400),
        seconds: 20,
        rotate: false,
    };
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--rotate" {
            options.rotate = true;
            continue;
        }
        let value = args.next().ok_or(format!("{arg} needs a value"))?;
        let number = || {
            value
                .parse::<u32>()
                .map_err(|_| format!("{arg} needs a number"))
        };
        match arg.as_str() {
            "--windows" => options.windows = number()?,
            "--seconds" => options.seconds = number()?.into(),
            "--size" => {
                options.size = value
                    .split_once('x')
                    .and_then(|(w, h)| Some((w.parse().ok()?, h.parse().ok()?)))
                    .ok_or("the size is WIDTHxHEIGHT")?;
            }
            other => return Err(format!("unknown argument {other}")),
        }
    }
    Ok(options)
}

/// One window of the scene and when it drew.
struct Load {
    id: WindowId,
    index: u32,
    configured: bool,
    drawn: Vec<Instant>,
}

fn run() -> Result<(), String> {
    let options = options()?;
    let mut app = App::connect().map_err(|e| e.to_string())?;
    println!("gpu {}", app.gpu_name());
    let mut windows = Vec::new();
    for index in 1..=options.windows {
        let id = app
            .open(&format!("Last {index}"), "beize-load-test")
            .map_err(|e| e.to_string())?;
        app.prefer_size(id, options.size);
        windows.push(Load {
            id,
            index,
            configured: false,
            drawn: Vec::new(),
        });
    }
    let start = Instant::now();
    let deadline = start + Duration::from_secs(options.seconds);
    // The most frames of a window on their way at once, drawn and not yet
    // shown: one, as Beize UI paces its windows.
    let mut ahead = 0;
    let mut first = 0;
    while let Some(left) = deadline.checked_duration_since(Instant::now()) {
        // Each window draws as soon as its last frame is shown (`ready`):
        // the compositor's frames pace the loop.
        let mut dispatched = app.dispatch(Some(left), &[]).map_err(|e| e.to_string())?;
        for window in &mut windows {
            let events = dispatched.take(window.id);
            if events.closed {
                return Err(format!("window {} was closed", window.index));
            }
            window.configured |= events.configured;
        }
        let t = start.elapsed().as_secs_f32();
        let count = windows.len();
        let mut drew = false;
        for n in 0..count {
            let window = &mut windows[(first + n) % count];
            if !(window.configured && app.ready(window.id)) {
                continue;
            }
            drew = true;
            let (w, h) = app.size(window.id);
            let frame = window.drawn.len() + 1;
            let scene = scene(app.text(), w, h, window.index, frame, t);
            app.draw(window.id, &scene).map_err(|e| e.to_string())?;
            window.drawn.push(Instant::now());
            let (drawn, shown) = app.frames(window.id);
            ahead = ahead.max(drawn.wrapping_sub(shown));
        }
        // A round is a wake that drew.
        if options.rotate && drew {
            first = (first + 1) % count.max(1);
        }
    }
    let mut total = 0;
    for window in &windows {
        total += window.drawn.len();
        let mut between: Vec<f64> = window
            .drawn
            .windows(2)
            .map(|p| (p[1] - p[0]).as_secs_f64() * 1000.0)
            .collect();
        between.sort_by(f64::total_cmp);
        println!(
            "window {}: {} frames, between frames median {:.2} ms, max {:.2} ms",
            window.index,
            window.drawn.len(),
            between.get(between.len() / 2).copied().unwrap_or(0.0),
            between.last().copied().unwrap_or(0.0),
        );
    }
    println!(
        "load: {} windows, {total} frames in {:.2} s, at most {ahead} on their way",
        windows.len(),
        start.elapsed().as_secs_f64()
    );
    Ok(())
}

/// Window `index`'s content at `t` seconds, its `frame`th.
fn scene(text: &mut Text, w: u32, h: u32, index: u32, frame: usize, t: f32) -> Scene {
    let (w, h) = (w as f32, h as f32);
    let phase = index as f32 * 0.13;
    let mut scene = Scene::new();
    let hue = t / HUE_TURN + phase;
    scene.gradient(
        rect(0.0, 0.0, w, h),
        0.0,
        hsv(hue, 0.55, 0.85),
        hsv(hue + 0.3, 0.7, 0.45),
    );
    for i in 0..BLOCKS {
        // Back and forth across the window, each block on its own beat.
        let beat = (t / SLIDE + phase + i as f32 / BLOCKS as f32) * std::f32::consts::PI;
        let x = (w - BLOCK.0) * 0.5 * (1.0 - beat.cos());
        let y = (h - BLOCK.1) * (i as f32 + 0.5) / BLOCKS as f32;
        let mut color = hsv(hue + 0.5 + i as f32 * 0.1, 0.4, 1.0);
        color.a = 0x99;
        scene.rounded(rect(x, y, BLOCK.0, BLOCK.1), 14.0, color);
    }
    let style = Style {
        family: Family::Sans,
        size: 15.0,
        weight: 600.0,
        color: Color::rgb(0xffffff),
    };
    let layout = text.layout(
        &format!("Fenster {index} · Bild {frame}"),
        &style,
        Some(w - 32.0),
    );
    scene.text(&layout, 16.0, 16.0);
    scene
}

/// A colour from hue (turns, any number), saturation and value (0 to 1).
fn hsv(hue: f32, s: f32, v: f32) -> Color {
    let h = hue.rem_euclid(1.0) * 6.0;
    let c = v * s;
    let x = c * (1.0 - (h % 2.0 - 1.0).abs());
    let (r, g, b) = match h as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = v - c;
    let byte = |f: f32| ((f + m) * 255.0).round() as u8;
    Color {
        r: byte(r),
        g: byte(g),
        b: byte(b),
        a: 0xff,
    }
}

fn rect(x: f32, y: f32, w: f32, h: f32) -> Rect {
    Rect { x, y, w, h }
}
