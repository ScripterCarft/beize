//! `beize-ui-test` – checks Beize UI's window in the VM (bz's test medium;
//! never in the image). It opens one window with a fixed scene – a dark
//! ground, four coloured squares and a translucent bar across them – and
//! redraws it on every configure, printing one line each: the GPU, the
//! window's size. The scene's pixels are known exactly (`scene`), so a
//! screenshot shows whether storing in sRGB and blending in linear light
//! work. `--seconds S` ends it after S seconds (default 10).

use std::process::ExitCode;
use std::time::{Duration, Instant};

use beize_ui::{Color, Rect, Scene, Window};

const GROUND: Color = Color::rgb(0x1e1e1e);
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
    let seconds = match std::env::args().skip(1).collect::<Vec<_>>().as_slice() {
        [] => 10,
        [flag, n] if flag == "--seconds" => n.parse().map_err(|_| "--seconds needs a number")?,
        _ => return Err("usage: beize-ui-test [--seconds S]".into()),
    };
    let mut window = Window::open("beize-ui-test", "beize-ui-test").map_err(|e| e.to_string())?;
    println!("gpu {}", window.gpu_name());
    let deadline = Instant::now() + Duration::from_secs(seconds);
    while let Some(left) = deadline.checked_duration_since(Instant::now()) {
        let events = window.dispatch(Some(left)).map_err(|e| e.to_string())?;
        if events.closed {
            break;
        }
        if events.configured {
            let (w, h) = window.size();
            println!("configure {w}x{h}");
            window.draw(&scene(w, h)).map_err(|e| e.to_string())?;
        }
    }
    println!("done");
    Ok(())
}

/// The ground, the squares in a row through the middle (each a quarter
/// of the width, at most a third of the height), the bar over their upper
/// half.
fn scene(w: u32, h: u32) -> Scene {
    let (w, h) = (w as f32, h as f32);
    let side = (w / 4.0).min(h / 3.0).floor();
    let top = ((h - side) / 2.0).floor();
    let mut scene = Scene::new();
    scene.fill(
        Rect {
            x: 0.0,
            y: 0.0,
            w,
            h,
        },
        GROUND,
    );
    for (i, color) in SQUARES.into_iter().enumerate() {
        scene.fill(
            Rect {
                x: i as f32 * side + ((w - 4.0 * side) / 2.0).floor(),
                y: top,
                w: side,
                h: side,
            },
            color,
        );
    }
    scene.fill(
        Rect {
            x: 0.0,
            y: top,
            w,
            h: (side / 2.0).floor(),
        },
        BAR,
    );
    scene
}
