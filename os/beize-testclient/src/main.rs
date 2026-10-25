//! `beize-testclient` – checks beize-compositor from a client's side, in
//! the VM (bz's test medium; never in the image, ADR 0027 §6).
//!
//! It opens one window (xdg-shell) with a fixed pattern in wl_shm – four
//! quadrants, red, green, blue and white, opaque – and prints what the
//! compositor sends, one line each: the globals it offers this client (an
//! app sees no panels, ADR 0028 §1), the window's size, the keyboard's
//! layout (the `xkb_symbols` of its keymap), keys, pointer and buttons.
//! A press in its top `TITLE` rows moves the window, as on a title bar;
//! the M key maximizes it and back (the compositor's side of both, ADR
//! 0028 §2). `--server-side` asks the compositor for its frame instead, as
//! Qt does (xdg-decoration), and prints the mode it is given; its top rows
//! are then pattern only.
//!
//! `--frames N` draws N frames, each on the last one's "frame done", with
//! a bar moving through the top, and prints the time between frames: the
//! compositor should pace them to the screen's refresh. Like a real client,
//! it redraws only what changed: a buffer gets the whole pattern once, then
//! only the bar's strip (and damages only that), so the measure is the
//! compositor's pace, not the client's drawing; its own drawing time is
//! printed too. `--seconds S` ends it after S seconds (default 10).
//! It asks for each of those frames when it showed (wp_presentation) and,
//! once all answered, prints how many showed, on which clock, with which
//! flags, and whether times and sequence rose and each time lay before its
//! answer arrived.

use std::fs::File;
use std::io::Read as _;
use std::os::fd::AsFd;
use std::os::unix::fs::FileExt as _;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use rustix::event::{PollFd, PollFlags, Timespec, poll};
use wayland_client::backend::WaylandError;
use wayland_client::globals::{BindError, GlobalListContents, registry_queue_init};
use wayland_client::protocol::{
    wl_buffer::{self, WlBuffer},
    wl_callback::{self, WlCallback},
    wl_compositor::WlCompositor,
    wl_keyboard::{self, WlKeyboard},
    wl_pointer::{self, WlPointer},
    wl_region::WlRegion,
    wl_registry::WlRegistry,
    wl_seat::{self, WlSeat},
    wl_shm::{self, WlShm},
    wl_shm_pool::WlShmPool,
    wl_surface::WlSurface,
};
use wayland_client::{Connection, Dispatch, EventQueue, QueueHandle, WEnum, delegate_noop};
use wayland_protocols::wp::presentation_time::client::{
    wp_presentation::{self, WpPresentation},
    wp_presentation_feedback::{self, WpPresentationFeedback},
};
use wayland_protocols::xdg::decoration::zv1::client::{
    zxdg_decoration_manager_v1::ZxdgDecorationManagerV1,
    zxdg_toplevel_decoration_v1::{self, ZxdgToplevelDecorationV1},
};
use wayland_protocols::xdg::shell::client::{
    xdg_surface::{self, XdgSurface},
    xdg_toplevel::{self, XdgToplevel},
    xdg_wm_base::{self, XdgWmBase},
};

/// The quadrants, as XRGB8888 (little endian: B, G, R, X).
const RED: u32 = 0xffd0_3030;
const GREEN: u32 = 0xff30_b040;
const BLUE: u32 = 0xff30_50d0;
const WHITE: u32 = 0xffff_ffff;
const BAR: u32 = 0xff20_2020;
/// The moving bar of `--frames`, and how far it moves each frame.
const BAR_SIZE: (i32, i32) = (32, 32);
const BAR_STEP: i32 = 8;
/// The rows at the top that act as a title bar.
const TITLE: f64 = 32.0;
/// The left button and the M key (linux/input-event-codes.h).
const BTN_LEFT: u32 = 0x110;
const KEY_M: u32 = 50;

type Result<T> = std::result::Result<T, String>;

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("beize-testclient: {e}");
            ExitCode::FAILURE
        }
    }
}

struct Options {
    frames: u32,
    seconds: u64,
    server_side: bool,
}

fn options() -> Result<Options> {
    let mut options = Options {
        frames: 0,
        seconds: 10,
        server_side: false,
    };
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let mut value = || -> Result<u64> {
            args.next()
                .and_then(|v| v.parse().ok())
                .ok_or(format!("{arg} needs a number"))
        };
        match arg.as_str() {
            "--frames" => options.frames = u32::try_from(value()?).map_err(|e| e.to_string())?,
            "--seconds" => options.seconds = value()?,
            "--server-side" => options.server_side = true,
            other => return Err(format!("unknown argument {other}")),
        }
    }
    Ok(options)
}

/// One shm buffer: its own memfd and pool, so it can be replaced alone.
struct Buffer {
    file: File,
    buffer: WlBuffer,
    size: (i32, i32),
    /// The compositor still reads it.
    busy: bool,
    /// Holds the whole pattern; from then on only the bar's strip changes.
    filled: bool,
}

struct App {
    compositor: WlCompositor,
    shm: WlShm,
    surface: WlSurface,
    toplevel: XdgToplevel,
    seat: WlSeat,
    /// As the last configure said.
    maximized: bool,
    /// The compositor draws the window's frame (`--server-side`).
    framed: bool,
    /// Where the pointer is on the window, down.
    pointer_y: f64,
    /// The size the compositor asked for; 0 means "choose".
    size: (i32, i32),
    /// The next draw damages everything (after a configure).
    full: bool,
    buffers: Vec<Buffer>,
    frames_left: u32,
    /// A "frame done" is on its way.
    frame_asked: bool,
    /// "Frame done" times, in milliseconds.
    frame_times: Vec<u32>,
    /// How long each frame took to draw.
    draw_times: Vec<Duration>,
    /// When the frames showed, if the compositor tells.
    presentation: Option<WpPresentation>,
    /// The clock its times are on.
    clock: Option<u32>,
    /// Frames asked about, those that showed, and the numbers of those
    /// discarded (the first is 1).
    asked: u32,
    shown: Vec<Shown>,
    discarded: Vec<u32>,
    /// The answers are summed up.
    presented_all: bool,
    bar: i32,
    running: bool,
}

/// A frame on the screen, as wp_presentation_feedback.presented says.
struct Shown {
    time: Duration,
    refresh: u32,
    sequence: u64,
    flags: u32,
    /// When the answer arrived, on the same clock.
    arrived: Duration,
}

fn run() -> Result<()> {
    let options = options()?;
    let connection = Connection::connect_to_env().map_err(|e| format!("connecting: {e}"))?;
    let (globals, mut queue) =
        registry_queue_init::<App>(&connection).map_err(|e| format!("registry: {e}"))?;
    let qh = queue.handle();
    let offered: Vec<String> = globals
        .contents()
        .clone_list()
        .into_iter()
        .map(|g| g.interface)
        .collect();
    println!("globals {}", offered.join(" "));
    let compositor: WlCompositor = globals
        .bind(&qh, 4..=6, ())
        .map_err(missing("wl_compositor"))?;
    let shm: WlShm = globals.bind(&qh, 1..=1, ()).map_err(missing("wl_shm"))?;
    let wm_base: XdgWmBase = globals
        .bind(&qh, 1..=6, ())
        .map_err(missing("xdg_wm_base"))?;
    let seat: WlSeat = globals.bind(&qh, 5..=9, ()).map_err(missing("wl_seat"))?;
    let presentation: Option<WpPresentation> = (options.frames > 0)
        .then(|| globals.bind(&qh, 1..=2, ()).ok())
        .flatten();
    if options.frames > 0 && presentation.is_none() {
        println!("presentation: not offered");
    }

    let surface = compositor.create_surface(&qh, ());
    let xdg_surface = wm_base.get_xdg_surface(&surface, &qh, ());
    let toplevel = xdg_surface.get_toplevel(&qh, ());
    toplevel.set_title("beize-testclient".into());
    toplevel.set_app_id("beize-testclient".into());
    // Kept until the end: dropping it would not end it, but its frame
    // ends with it in the protocol.
    let _decoration = if options.server_side {
        let manager: ZxdgDecorationManagerV1 = globals
            .bind(&qh, 1..=1, ())
            .map_err(missing("zxdg_decoration_manager_v1"))?;
        let decoration = manager.get_toplevel_decoration(&toplevel, &qh, ());
        decoration.set_mode(zxdg_toplevel_decoration_v1::Mode::ServerSide);
        Some(decoration)
    } else {
        None
    };
    surface.commit();

    let mut app = App {
        compositor,
        shm,
        surface,
        toplevel,
        seat,
        maximized: false,
        framed: false,
        pointer_y: 0.0,
        size: (0, 0),
        full: true,
        buffers: Vec::new(),
        frames_left: options.frames,
        frame_asked: false,
        frame_times: Vec::new(),
        draw_times: Vec::new(),
        presentation,
        clock: None,
        asked: 0,
        shown: Vec::new(),
        discarded: Vec::new(),
        presented_all: false,
        bar: 0,
        running: true,
    };
    let deadline = Instant::now() + Duration::from_secs(options.seconds);
    while app.running {
        let Some(left) = deadline.checked_duration_since(Instant::now()) else {
            break;
        };
        dispatch(&mut queue, &mut app, left)?;
    }
    if options.frames > 0 {
        report_frames(&app.frame_times, options.frames);
        report_drawing(&app.draw_times);
    }
    println!("done");
    Ok(())
}

/// The error for a global the compositor does not offer.
fn missing(name: &str) -> impl FnOnce(BindError) -> String + use<'_> {
    move |e| format!("the compositor has no {name}: {e}")
}

/// Handles what arrived, waiting at most `timeout` for more.
fn dispatch(queue: &mut EventQueue<App>, app: &mut App, timeout: Duration) -> Result<()> {
    queue
        .dispatch_pending(app)
        .map_err(|e| format!("dispatching: {e}"))?;
    queue.flush().map_err(|e| format!("sending: {e}"))?;
    let Some(guard) = queue.prepare_read() else {
        return Ok(());
    };
    let fd = guard.connection_fd();
    let mut fds = [PollFd::new(&fd, PollFlags::IN)];
    let timeout = Timespec::try_from(timeout).map_err(|e| e.to_string())?;
    let ready = poll(&mut fds, Some(&timeout)).map_err(|e| format!("poll: {e}"))?;
    if ready > 0 {
        match guard.read() {
            Ok(_) => {}
            // Nothing to read after all: the Rust backend says so as an
            // error (smithay-client-toolkit and calloop ignore it too).
            Err(WaylandError::Io(e)) if e.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(e) => return Err(format!("reading: {e}")),
        }
    }
    queue
        .dispatch_pending(app)
        .map_err(|e| format!("dispatching: {e}"))?;
    Ok(())
}

fn report_frames(times: &[u32], wanted: u32) {
    let gaps: Vec<u32> = times.windows(2).map(|w| w[1].wrapping_sub(w[0])).collect();
    if gaps.is_empty() {
        println!("frames: {} of {wanted}, too few to measure", times.len());
        return;
    }
    let mean = f64::from(gaps.iter().sum::<u32>()) / gaps.len() as f64;
    let max = gaps.iter().max().copied().unwrap_or(0);
    println!(
        "frames: {} of {wanted}, {mean:.2} ms apart on average, at most {max} ms",
        times.len()
    );
}

fn report_drawing(times: &[Duration]) {
    let mut ms: Vec<f64> = times.iter().map(|t| t.as_secs_f64() * 1000.0).collect();
    if ms.is_empty() {
        return;
    }
    ms.sort_by(f64::total_cmp);
    println!(
        "drawing: {:.3} ms per frame (median), at most {:.3} ms",
        ms[ms.len() / 2],
        ms[ms.len() - 1]
    );
}

impl App {
    fn size(&self) -> (i32, i32) {
        match self.size {
            (0, _) | (_, 0) => (640, 480),
            size => size,
        }
    }

    /// Draws the pattern (with the bar while frames are wanted) into a
    /// free buffer and commits it.
    fn draw(&mut self, qh: &QueueHandle<App>) -> Result<()> {
        let start = Instant::now();
        let size = self.size();
        let index = self.free_buffer(qh, size)?;
        let buffer = &mut self.buffers[index];
        let bar = (self.frames_left > 0).then_some(self.bar);
        // The bar's strip holds all that changes; the rows are at the
        // buffer's start.
        let rows = if buffer.filled {
            BAR_SIZE.1.min(size.1)
        } else {
            size.1
        };
        buffer
            .file
            .write_all_at(&pattern(size, bar, rows), 0)
            .map_err(|e| format!("writing the buffer: {e}"))?;
        buffer.filled = true;
        buffer.busy = true;
        self.surface.attach(Some(&buffer.buffer), 0, 0);
        if std::mem::take(&mut self.full) {
            self.surface.damage_buffer(0, 0, size.0, size.1);
        } else {
            // Only the strip the bar moves in changed.
            self.surface.damage_buffer(0, 0, size.0, BAR_SIZE.1);
        }
        // One "frame done" asked for at a time: a configure between frames
        // redraws without starting a second chain.
        if self.frames_left > 0 && !self.frame_asked {
            self.frame_asked = true;
            self.surface.frame(qh, ());
            if let Some(presentation) = &self.presentation {
                presentation.feedback(&self.surface, qh, ());
                self.asked += 1;
            }
            self.bar = (self.bar + BAR_STEP) % (size.0 - BAR_SIZE.0).max(1);
        }
        self.surface.commit();
        if self.frames_left > 0 {
            self.draw_times.push(start.elapsed());
        }
        Ok(())
    }

    /// A buffer of `size` the compositor does not read, made if needed.
    fn free_buffer(&mut self, qh: &QueueHandle<App>, size: (i32, i32)) -> Result<usize> {
        self.buffers.retain(|b| b.busy || b.size == size);
        if let Some(index) = self.buffers.iter().position(|b| !b.busy) {
            return Ok(index);
        }
        let bytes = size.0 * size.1 * 4;
        let fd = rustix::fs::memfd_create("beize-testclient", rustix::fs::MemfdFlags::CLOEXEC)
            .map_err(|e| format!("memfd: {e}"))?;
        let file = File::from(fd);
        file.set_len(u64::try_from(bytes).map_err(|e| e.to_string())?)
            .map_err(|e| format!("sizing the buffer: {e}"))?;
        let pool: WlShmPool = self.shm.create_pool(file.as_fd(), bytes, qh, ());
        let index = self.buffers.len();
        let buffer = pool.create_buffer(
            0,
            size.0,
            size.1,
            size.0 * 4,
            wl_shm::Format::Xrgb8888,
            qh,
            index,
        );
        pool.destroy();
        self.buffers.push(Buffer {
            file,
            buffer,
            size,
            busy: false,
            filled: false,
        });
        Ok(index)
    }

    /// How many frames were answered, shown or discarded; the answers come
    /// in the order of the frames.
    fn answered(&self) -> u32 {
        u32::try_from(self.shown.len() + self.discarded.len()).unwrap_or(u32::MAX)
    }

    /// Once the frames are drawn and each answered whether it showed: the
    /// sum, at once, as the client may be stopped any moment after.
    fn report_presentation(&mut self) {
        let answered = self.answered();
        if self.presented_all
            || self.presentation.is_none()
            || self.frames_left > 0
            || answered < self.asked
        {
            return;
        }
        self.presented_all = true;
        let shown = &self.shown;
        let names = [
            (wp_presentation_feedback::Kind::Vsync, "vsync"),
            (wp_presentation_feedback::Kind::HwClock, "hw_clock"),
            (
                wp_presentation_feedback::Kind::HwCompletion,
                "hw_completion",
            ),
            (wp_presentation_feedback::Kind::ZeroCopy, "zero_copy"),
        ];
        let common = shown.iter().fold(u32::MAX, |all, s| all & s.flags);
        let flags: Vec<&str> = names
            .iter()
            .filter(|(kind, _)| common & kind.bits() != 0)
            .map(|(_, name)| *name)
            .collect();
        let yes = |b: bool| if b { "yes" } else { "no" };
        let rising = shown
            .windows(2)
            .all(|w| w[1].time > w[0].time && w[1].sequence > w[0].sequence);
        let before = shown.iter().all(|s| s.time <= s.arrived);
        let refresh = shown.last().map_or(0, |s| s.refresh);
        println!(
            "presentation: {} of {} shown, {} discarded {:?}, clock {}, refresh {refresh} ns, on \
             all: {}, rising {}, before arrival {}",
            shown.len(),
            self.asked,
            self.discarded.len(),
            self.discarded,
            self.clock.map_or("none".into(), |c| c.to_string()),
            if flags.is_empty() {
                "no flags".into()
            } else {
                flags.join(" ")
            },
            yes(rising),
            yes(before),
        );
    }

    /// The window is opaque: the compositor need not draw what is under it.
    fn set_opaque(&self, qh: &QueueHandle<App>) {
        let (w, h) = self.size();
        let region: WlRegion = self.compositor.create_region(qh, ());
        region.add(0, 0, w, h);
        self.surface.set_opaque_region(Some(&region));
        region.destroy();
    }
}

/// The quadrants, and the bar at `bar` (its left edge) if given: the
/// first `rows` rows.
fn pattern((w, h): (i32, i32), bar: Option<i32>, rows: i32) -> Vec<u8> {
    let mut pixels = Vec::with_capacity((w * rows * 4) as usize);
    for y in 0..rows {
        for x in 0..w {
            let on_bar = bar.is_some_and(|b| (b..b + BAR_SIZE.0).contains(&x) && y < BAR_SIZE.1);
            let color = match (on_bar, x < w / 2, y < h / 2) {
                (true, _, _) => BAR,
                (false, true, true) => RED,
                (false, false, true) => GREEN,
                (false, true, false) => BLUE,
                (false, false, false) => WHITE,
            };
            pixels.extend_from_slice(&color.to_le_bytes());
        }
    }
    pixels
}

/// What names the layout in a keymap as libxkbcommon writes it:
/// `xkb_symbols "pc+de(nodeadkeys)+inet(evdev)" {` gives the quoted part.
fn symbols(keymap: &str) -> Option<&str> {
    let line = keymap
        .lines()
        .map(str::trim)
        .find(|l| l.starts_with("xkb_symbols"))?;
    line.split('"').nth(1)
}

impl Dispatch<WlRegistry, GlobalListContents> for App {
    fn event(
        _: &mut App,
        _: &WlRegistry,
        _: <WlRegistry as wayland_client::Proxy>::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<App>,
    ) {
    }
}

impl Dispatch<XdgWmBase, ()> for App {
    fn event(
        _: &mut App,
        wm_base: &XdgWmBase,
        event: xdg_wm_base::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<App>,
    ) {
        if let xdg_wm_base::Event::Ping { serial } = event {
            wm_base.pong(serial);
        }
    }
}

impl Dispatch<XdgSurface, ()> for App {
    fn event(
        app: &mut App,
        xdg_surface: &XdgSurface,
        event: xdg_surface::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<App>,
    ) {
        if let xdg_surface::Event::Configure { serial } = event {
            xdg_surface.ack_configure(serial);
            let (w, h) = app.size();
            println!("configure {w}x{h}");
            app.full = true;
            app.set_opaque(qh);
            if let Err(e) = app.draw(qh) {
                eprintln!("beize-testclient: {e}");
                app.running = false;
            }
        }
    }
}

impl Dispatch<XdgToplevel, ()> for App {
    fn event(
        app: &mut App,
        _: &XdgToplevel,
        event: xdg_toplevel::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<App>,
    ) {
        match event {
            xdg_toplevel::Event::Configure {
                width,
                height,
                states,
            } => {
                app.size = (width, height);
                let maximized = u32::from(xdg_toplevel::State::Maximized);
                app.maximized = states
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .any(|s| u32::from_ne_bytes(*s) == maximized);
            }
            xdg_toplevel::Event::Close => app.running = false,
            _ => {}
        }
    }
}

impl Dispatch<WlCallback, ()> for App {
    fn event(
        app: &mut App,
        _: &WlCallback,
        event: wl_callback::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<App>,
    ) {
        if let wl_callback::Event::Done { callback_data } = event {
            app.frame_asked = false;
            app.frame_times.push(callback_data);
            app.frames_left = app.frames_left.saturating_sub(1);
            app.report_presentation();
            if app.frames_left > 0
                && let Err(e) = app.draw(qh)
            {
                eprintln!("beize-testclient: {e}");
                app.running = false;
            }
        }
    }
}

impl Dispatch<WlBuffer, usize> for App {
    fn event(
        app: &mut App,
        _: &WlBuffer,
        event: wl_buffer::Event,
        index: &usize,
        _: &Connection,
        _: &QueueHandle<App>,
    ) {
        if let wl_buffer::Event::Release = event
            && let Some(buffer) = app.buffers.get_mut(*index)
        {
            buffer.busy = false;
        }
    }
}

impl Dispatch<WlSeat, ()> for App {
    fn event(
        _: &mut App,
        seat: &WlSeat,
        event: wl_seat::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<App>,
    ) {
        if let wl_seat::Event::Capabilities {
            capabilities: WEnum::Value(capabilities),
        } = event
        {
            if capabilities.contains(wl_seat::Capability::Keyboard) {
                seat.get_keyboard(qh, ());
            }
            if capabilities.contains(wl_seat::Capability::Pointer) {
                seat.get_pointer(qh, ());
            }
        }
    }
}

impl Dispatch<WlKeyboard, ()> for App {
    fn event(
        app: &mut App,
        _: &WlKeyboard,
        event: wl_keyboard::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<App>,
    ) {
        match event {
            wl_keyboard::Event::Keymap { fd, size, .. } => {
                let mut keymap = String::new();
                let read = File::from(fd)
                    .take(u64::from(size))
                    .read_to_string(&mut keymap);
                match read.map(|_| symbols(&keymap)) {
                    Ok(Some(symbols)) => println!("keymap {symbols}"),
                    Ok(None) => println!("keymap without xkb_symbols"),
                    Err(e) => println!("keymap unreadable: {e}"),
                }
            }
            wl_keyboard::Event::Enter { .. } => println!("keyboard focus"),
            wl_keyboard::Event::Leave { .. } => println!("keyboard focus lost"),
            wl_keyboard::Event::Key {
                key,
                state: WEnum::Value(state),
                ..
            } => {
                let pressed = state == wl_keyboard::KeyState::Pressed;
                println!("key {key} {}", if pressed { "pressed" } else { "released" });
                if pressed && key == KEY_M {
                    if app.maximized {
                        app.toplevel.unset_maximized();
                    } else {
                        app.toplevel.set_maximized();
                    }
                }
            }
            _ => {}
        }
    }
}

impl Dispatch<WlPointer, ()> for App {
    fn event(
        app: &mut App,
        _: &WlPointer,
        event: wl_pointer::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<App>,
    ) {
        match event {
            wl_pointer::Event::Enter {
                surface_x,
                surface_y,
                ..
            } => {
                app.pointer_y = surface_y;
                println!("pointer enter {surface_x:.0},{surface_y:.0}");
            }
            wl_pointer::Event::Motion {
                surface_x,
                surface_y,
                ..
            } => {
                app.pointer_y = surface_y;
                println!("pointer at {surface_x:.0},{surface_y:.0}");
            }
            wl_pointer::Event::Button {
                serial,
                button,
                state: WEnum::Value(state),
                ..
            } => {
                let pressed = state == wl_pointer::ButtonState::Pressed;
                println!(
                    "button {button:#x} {}",
                    if pressed { "pressed" } else { "released" }
                );
                if pressed && button == BTN_LEFT && app.pointer_y < TITLE && !app.framed {
                    app.toplevel._move(&app.seat, serial);
                }
            }
            _ => {}
        }
    }
}

impl Dispatch<ZxdgToplevelDecorationV1, ()> for App {
    fn event(
        app: &mut App,
        _: &ZxdgToplevelDecorationV1,
        event: zxdg_toplevel_decoration_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<App>,
    ) {
        if let zxdg_toplevel_decoration_v1::Event::Configure {
            mode: WEnum::Value(mode),
        } = event
        {
            app.framed = mode == zxdg_toplevel_decoration_v1::Mode::ServerSide;
            println!(
                "decoration {}",
                if app.framed {
                    "server_side"
                } else {
                    "client_side"
                }
            );
        }
    }
}

impl Dispatch<WpPresentation, ()> for App {
    fn event(
        app: &mut App,
        _: &WpPresentation,
        event: wp_presentation::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<App>,
    ) {
        if let wp_presentation::Event::ClockId { clk_id } = event {
            app.clock = Some(clk_id);
        }
    }
}

impl Dispatch<WpPresentationFeedback, ()> for App {
    fn event(
        app: &mut App,
        _: &WpPresentationFeedback,
        event: wp_presentation_feedback::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<App>,
    ) {
        match event {
            wp_presentation_feedback::Event::Presented {
                tv_sec_hi,
                tv_sec_lo,
                tv_nsec,
                refresh,
                seq_hi,
                seq_lo,
                flags,
            } => {
                // CLOCK_MONOTONIC, as the compositor names it (clock 1);
                // another clock fails the check by its number.
                let now = rustix::time::clock_gettime(rustix::time::ClockId::Monotonic);
                app.shown.push(Shown {
                    time: Duration::new(
                        (u64::from(tv_sec_hi) << 32) | u64::from(tv_sec_lo),
                        tv_nsec,
                    ),
                    refresh,
                    sequence: (u64::from(seq_hi) << 32) | u64::from(seq_lo),
                    flags: match flags {
                        WEnum::Value(kind) => kind.bits(),
                        WEnum::Unknown(bits) => bits,
                    },
                    arrived: Duration::new(
                        u64::try_from(now.tv_sec).unwrap_or(0),
                        u32::try_from(now.tv_nsec).unwrap_or(0),
                    ),
                });
                app.report_presentation();
            }
            wp_presentation_feedback::Event::Discarded => {
                let frame = app.answered() + 1;
                app.discarded.push(frame);
                app.report_presentation();
            }
            _ => {}
        }
    }
}

delegate_noop!(App: ignore ZxdgDecorationManagerV1);
delegate_noop!(App: ignore WlCompositor);
delegate_noop!(App: ignore WlShm);
delegate_noop!(App: ignore WlShmPool);
delegate_noop!(App: ignore WlSurface);
delegate_noop!(App: ignore WlRegion);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_layout_in_a_keymap() {
        // The head of what libxkbcommon 1.13 sends for de(nodeadkeys).
        let keymap = "xkb_keymap {\nxkb_keycodes \"evdev+aliases(qwertz)\" {\n\
                      \tminimum = 8;\n};\n\
                      xkb_symbols \"pc+de(nodeadkeys)+inet(evdev)+terminate(ctrl_alt_bksp)\" {\n";
        assert_eq!(
            symbols(keymap),
            Some("pc+de(nodeadkeys)+inet(evdev)+terminate(ctrl_alt_bksp)")
        );
    }

    #[test]
    fn the_pattern_has_four_quadrants_and_the_bar() {
        let pixels = pattern((4, 4), None, 4);
        let at = |x: usize, y: usize| &pixels[(y * 4 + x) * 4..(y * 4 + x) * 4 + 4];
        assert_eq!(at(0, 0), RED.to_le_bytes());
        assert_eq!(at(3, 0), GREEN.to_le_bytes());
        assert_eq!(at(0, 3), BLUE.to_le_bytes());
        assert_eq!(at(3, 3), WHITE.to_le_bytes());
        let with_bar = pattern((64, 64), Some(8), 64);
        assert_eq!(&with_bar[8 * 4..8 * 4 + 4], BAR.to_le_bytes());
    }

    #[test]
    fn the_strip_is_the_top_of_the_whole_pattern() {
        let size = (96, 64);
        let whole = pattern(size, Some(40), size.1);
        let strip = pattern(size, Some(40), BAR_SIZE.1);
        assert_eq!(strip.len(), (size.0 * BAR_SIZE.1 * 4) as usize);
        assert_eq!(strip[..], whole[..strip.len()]);
    }
}
