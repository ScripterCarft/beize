//! `beize-ui-test` – checks Beize UI in the VM (bz's test medium; never in
//! the image). It draws a fixed sample: a dark half and a light one, each
//! with text in Inter and JetBrains Mono, and the colour squares with a
//! translucent bar across them, whose pixels are known exactly (storing in
//! sRGB, blending in linear light).
//!
//! Without arguments it opens a window with the sample and redraws it on
//! every configure, printing one line each: the GPU, the window's size,
//! and every input (keys with what they type, the pointer); `--seconds S`
//! ends it after S seconds (default 10). `--offscreen FILE
//! [WxH]` draws the sample without a compositor into FILE (PPM, 640x400 by
//! default): at the screen's size, exactly what the window shows – the
//! smoke test's reference (os/beize-ui/tests/sample.png).

use std::process::ExitCode;
use std::time::{Duration, Instant};

use beize_ui::{Color, Family, Offscreen, Rect, Scene, Style, Text, Window};

const DARK: Color = Color::rgb(0x1e1e1e);
const LIGHT: Color = Color::rgb(0xf4f4f4);
const SQUARES: [Color; 4] = [
    Color::rgb(0xd03030),
    Color::rgb(0x30b040),
    Color::rgb(0x3050d0),
    Color::rgb(0xffffff),
];
/// Half-covering white: in linear light 50 % of the light, not of the
/// sRGB value.
const BAR: Color = Color {
    r: 0xff,
    g: 0xff,
    b: 0xff,
    a: 0x80,
};
const OFFSCREEN: (u32, u32) = (640, 400);

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("beize-ui-test: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut text = Text::new().map_err(|e| e.to_string())?;
    match args.as_slice() {
        [flag, file] if flag == "--offscreen" => offscreen(&mut text, file, OFFSCREEN),
        [flag, file, size] if flag == "--offscreen" => {
            let size = size
                .split_once('x')
                .and_then(|(w, h)| Some((w.parse().ok()?, h.parse().ok()?)))
                .ok_or("the size is WIDTHxHEIGHT")?;
            offscreen(&mut text, file, size)
        }
        [] => window(&mut text, 10),
        [flag, n] if flag == "--seconds" => window(
            &mut text,
            n.parse().map_err(|_| "--seconds needs a number")?,
        ),
        _ => Err("usage: beize-ui-test [--seconds S | --offscreen FILE [WxH]]".into()),
    }
}

fn window(text: &mut Text, seconds: u64) -> Result<(), String> {
    let mut window = Window::open("beize-ui-test", "beize-ui-test").map_err(|e| e.to_string())?;
    println!("gpu {}", window.gpu_name());
    let deadline = Instant::now() + Duration::from_secs(seconds);
    while let Some(left) = deadline.checked_duration_since(Instant::now()) {
        let events = window.dispatch(Some(left)).map_err(|e| e.to_string())?;
        if events.closed {
            break;
        }
        for input in &events.input {
            println!("{input:?}");
        }
        if events.configured {
            let (w, h) = window.size();
            println!("configure {w}x{h}");
            window
                .draw(&sample(text, w, h))
                .map_err(|e| e.to_string())?;
        }
    }
    println!("done");
    Ok(())
}

fn offscreen(text: &mut Text, file: &str, (w, h): (u32, u32)) -> Result<(), String> {
    let mut offscreen = Offscreen::new((w, h)).map_err(|e| e.to_string())?;
    println!("gpu {}", offscreen.gpu_name());
    let start = Instant::now();
    let pixels = offscreen
        .draw(&sample(text, w, h))
        .map_err(|e| e.to_string())?;
    println!("drawn in {:.1} ms", start.elapsed().as_secs_f64() * 1000.0);
    // PPM: RGB, from the image's BGRA.
    let mut ppm = format!("P6\n{w} {h}\n255\n").into_bytes();
    for [b, g, r, _] in pixels.as_chunks::<4>().0 {
        ppm.extend_from_slice(&[*r, *g, *b]);
    }
    std::fs::write(file, ppm).map_err(|e| format!("writing {file}: {e}"))?;
    println!("written {file}");
    Ok(())
}

/// The sample: the upper half dark, the lower light, text on both; the
/// squares and the bar along the bottom of the dark half.
fn sample(text: &mut Text, w: u32, h: u32) -> Scene {
    let (w, h) = (w as f32, h as f32);
    let half = (h / 2.0).floor();
    let mut scene = Scene::new();
    scene.fill(rect(0.0, 0.0, w, half), DARK);
    scene.fill(rect(0.0, half, w, h - half), LIGHT);
    let lines = [
        (
            Family::Sans,
            15.0,
            400.0,
            "Beize – sauber, schnell, bequem. Größe 15 px",
        ),
        (Family::Sans, 22.0, 600.0, "Überschrift in Inter, halbfett"),
        (
            Family::Mono,
            14.0,
            400.0,
            "$ ls -la ~/beize  # JetBrains Mono 14 px {}[]()",
        ),
    ];
    for (top, color) in [(16.0, LIGHT), (half + 16.0, DARK)] {
        let mut y = top;
        for (family, size, weight, line) in lines {
            let style = Style {
                family,
                size,
                weight,
                color,
            };
            let layout = text.layout(line, &style, Some(w - 32.0));
            scene.text(&layout, 16.0, y);
            y += layout.height().ceil() + 6.0;
        }
    }
    // The squares: each 48 px, in a row at the bottom of the dark half,
    // the bar over their upper half.
    let side = 48.0;
    let top = half - side - 16.0;
    for (i, color) in SQUARES.into_iter().enumerate() {
        scene.fill(rect(16.0 + i as f32 * side, top, side, side), color);
    }
    scene.fill(rect(0.0, top, w, side / 2.0), BAR);
    scene
}

fn rect(x: f32, y: f32, w: f32, h: f32) -> Rect {
    Rect { x, y, w, h }
}
