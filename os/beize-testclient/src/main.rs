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
//!
//! `--fifo N` draws N frames as Mesa's Vulkan FIFO mode does: without
//! waiting for "frame done", each commit waiting for the one before it to
//! show (wp_fifo), as long as one of three buffers is free; it prints how
//! many frames were a vblank apart. `--timed N` gives each frame after the
//! first a time, 2.5 refreshes after the last one showed (wp_commit_timing),
//! and prints how many showed at the first vblank at or after their time;
//! the first, which maps the window, gets one 100 ms ahead.
//!
//! `--viewport` shows the pattern through wp_viewporter: the part from
//! `VIEWPORT_SOURCE` – off centre, so its quadrants split at three
//! quarters – scaled to `VIEWPORT_SIZE`, the window's size.
//!
//! `--activation` opens a second window once the first is active and, once
//! that one is, asks for the first to become active again with a token of
//! the second's focus (xdg-activation); then for the window without the
//! keyboard, with a token of that focus the first has taken since, and
//! with one without a serial.
//! A roundtrip after each request tells whether the window got the
//! keyboard, one line each: `activation <case>: granted` or `refused`.
//!
//! `--inhibit` keeps the screen on while its window shows, as a video
//! player does (zwp_idle_inhibit_v1), until it ends.

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
    wl_display::WlDisplay,
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
use wayland_protocols::wp::commit_timing::v1::client::{
    wp_commit_timer_v1::WpCommitTimerV1, wp_commit_timing_manager_v1::WpCommitTimingManagerV1,
};
use wayland_protocols::wp::fifo::v1::client::{
    wp_fifo_manager_v1::WpFifoManagerV1, wp_fifo_v1::WpFifoV1,
};
use wayland_protocols::wp::idle_inhibit::zv1::client::{
    zwp_idle_inhibit_manager_v1::ZwpIdleInhibitManagerV1, zwp_idle_inhibitor_v1::ZwpIdleInhibitorV1,
};
use wayland_protocols::wp::presentation_time::client::{
    wp_presentation::{self, WpPresentation},
    wp_presentation_feedback::{self, WpPresentationFeedback},
};
use wayland_protocols::wp::viewporter::client::{
    wp_viewport::WpViewport, wp_viewporter::WpViewporter,
};
use wayland_protocols::xdg::activation::v1::client::{
    xdg_activation_token_v1::{self, XdgActivationTokenV1},
    xdg_activation_v1::XdgActivationV1,
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
/// The window's size when the compositor leaves the choice.
const CHOSEN_SIZE: (i32, i32) = (640, 480);
/// `--viewport`: the part of the buffer shown (x, y, width, height), and
/// the size it is shown in, 1.5 times.
const VIEWPORT_SOURCE: (f64, f64, f64, f64) = (80.0, 60.0, 320.0, 240.0);
const VIEWPORT_SIZE: (i32, i32) = (480, 360);
/// `--activation`: the second window, white.
const SECOND_SIZE: (i32, i32) = (320, 240);
/// The buffers `--fifo` draws into, as a swapchain of three.
const SWAPCHAIN: usize = 3;
/// How far ahead of the last frame `--timed` sets the next, in refreshes.
const TIMED_AHEAD: f64 = 2.5;
/// How far ahead `--timed` sets its first frame, the one that maps the
/// window: held back until then, as a window not shown yet.
const TIMED_FIRST: Duration = Duration::from_millis(100);
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
    pace: Pace,
    seconds: u64,
    server_side: bool,
    viewport: bool,
    activation: bool,
    inhibit: bool,
}

/// What the next of the frames waits for.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Pace {
    /// "Frame done" (`--frames`).
    Callbacks,
    /// The last frame shown (`--fifo`).
    Fifo,
    /// Its time (`--timed`).
    Timed,
}

fn options() -> Result<Options> {
    let mut options = Options {
        frames: 0,
        pace: Pace::Callbacks,
        seconds: 10,
        server_side: false,
        viewport: false,
        activation: false,
        inhibit: false,
    };
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let mut value = || -> Result<u64> {
            args.next()
                .and_then(|v| v.parse().ok())
                .ok_or(format!("{arg} needs a number"))
        };
        match arg.as_str() {
            "--frames" | "--fifo" | "--timed" => {
                options.pace = match arg.as_str() {
                    "--fifo" => Pace::Fifo,
                    "--timed" => Pace::Timed,
                    _ => Pace::Callbacks,
                };
                options.frames = u32::try_from(value()?).map_err(|e| e.to_string())?;
            }
            "--seconds" => options.seconds = value()?,
            "--server-side" => options.server_side = true,
            "--viewport" => options.viewport = true,
            "--activation" => options.activation = true,
            "--inhibit" => options.inhibit = true,
            other => return Err(format!("unknown argument {other}")),
        }
    }
    Ok(options)
}

/// One shm buffer: its own memfd and pool, so it can be replaced alone.
struct Buffer {
    /// What its release names: an index changes as buffers go.
    id: u32,
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
    /// The buffers made so far, for the next one's `id`.
    made: u32,
    pace: Pace,
    /// `--fifo`: the surface's fifo; `--timed`: its timer, and the time for
    /// the next frame.
    fifo: Option<WpFifoV1>,
    timer: Option<WpCommitTimerV1>,
    next_due: Option<Duration>,
    /// The time each frame was given, by its number (the first is 1).
    dues: Vec<Option<Duration>>,
    /// `--fifo`, `--timed`: the first frame is drawn.
    started: bool,
    frames_left: u32,
    /// A "frame done" is on its way.
    frame_asked: bool,
    /// "Frame done" times, in milliseconds, and how many frames had been
    /// answered as shown or discarded when each arrived.
    frame_times: Vec<u32>,
    done_answered: Vec<u32>,
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
    /// `--viewport`: how the surface crops and scales its buffer.
    viewport: Option<WpViewport>,
    /// `--activation`: its second window and requests.
    activation: Option<Activation>,
    running: bool,
}

/// `--activation`: the second window and the requests, in turn (`Step`).
struct Activation {
    manager: XdgActivationV1,
    wm_base: XdgWmBase,
    display: WlDisplay,
    step: Step,
    second: Option<Second>,
    /// The serial of the second window's keyboard enter.
    second_entered: Option<u32>,
    /// The window with the keyboard: a request asks for the other, so
    /// each case stands on its own.
    focused: Option<WlSurface>,
    /// The window a request asked for, and whether it got the keyboard
    /// since.
    target: Option<WlSurface>,
    got_it: bool,
}

/// What `--activation` waits for, in this order.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Step {
    /// The first window to become active; it then opens the second.
    First,
    /// The second to become active.
    Second,
    /// The first, asked for with a token of the second's focus.
    ActiveApp,
    /// The window without the keyboard, with a token of the second's
    /// focus, which the first has since.
    FocusMoved,
    /// The window without the keyboard, with a token without a serial.
    NoSerial,
    Done,
}

impl Step {
    fn name(self) -> &'static str {
        match self {
            Step::ActiveApp => "active app",
            Step::FocusMoved => "focus moved",
            Step::NoSerial => "no serial",
            Step::First | Step::Second | Step::Done => "",
        }
    }
}

/// The second window of `--activation`, white in its one buffer.
struct Second {
    surface: WlSurface,
    _xdg_surface: XdgSurface,
    _toplevel: XdgToplevel,
    buffer: WlBuffer,
    _file: File,
}

/// The roundtrip after a request of `--activation`.
struct Roundtrip;

/// The objects of `--activation`'s second window.
struct SecondWindow;

/// A frame on the screen, as wp_presentation_feedback.presented says.
struct Shown {
    /// Its number, the first is 1.
    frame: u32,
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
    let viewport = if options.viewport {
        let viewporter: WpViewporter = globals
            .bind(&qh, 1..=1, ())
            .map_err(missing("wp_viewporter"))?;
        let viewport = viewporter.get_viewport(&surface, &qh, ());
        let (x, y, w, h) = VIEWPORT_SOURCE;
        viewport.set_source(x, y, w, h);
        viewport.set_destination(VIEWPORT_SIZE.0, VIEWPORT_SIZE.1);
        Some(viewport)
    } else {
        None
    };
    let activation = if options.activation {
        let manager: XdgActivationV1 = globals
            .bind(&qh, 1..=1, ())
            .map_err(missing("xdg_activation_v1"))?;
        Some(Activation {
            manager,
            wm_base: wm_base.clone(),
            display: connection.display(),
            step: Step::First,
            second: None,
            second_entered: None,
            focused: None,
            target: None,
            got_it: false,
        })
    } else {
        None
    };
    let fifo = match options.pace {
        Pace::Fifo => {
            let manager: WpFifoManagerV1 = globals
                .bind(&qh, 1..=1, ())
                .map_err(missing("wp_fifo_manager_v1"))?;
            Some(manager.get_fifo(&surface, &qh, ()))
        }
        _ => None,
    };
    let timer = match options.pace {
        Pace::Timed => {
            let manager: WpCommitTimingManagerV1 = globals
                .bind(&qh, 1..=1, ())
                .map_err(missing("wp_commit_timing_manager_v1"))?;
            Some(manager.get_timer(&surface, &qh, ()))
        }
        _ => None,
    };
    // Kept until the end, which ends it.
    let _inhibitor = if options.inhibit {
        let manager: ZwpIdleInhibitManagerV1 = globals
            .bind(&qh, 1..=1, ())
            .map_err(missing("zwp_idle_inhibit_manager_v1"))?;
        Some(manager.create_inhibitor(&surface, &qh, ()))
    } else {
        None
    };
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
        made: 0,
        pace: options.pace,
        fifo,
        timer,
        next_due: None,
        dues: Vec::new(),
        started: false,
        frames_left: options.frames,
        frame_asked: false,
        frame_times: Vec::new(),
        done_answered: Vec::new(),
        draw_times: Vec::new(),
        presentation,
        clock: None,
        asked: 0,
        shown: Vec::new(),
        discarded: Vec::new(),
        presented_all: false,
        bar: 0,
        viewport,
        activation,
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

/// Now on CLOCK_MONOTONIC, the compositor's clock (wp_presentation).
fn monotonic() -> Duration {
    let now = rustix::time::clock_gettime(rustix::time::ClockId::Monotonic);
    Duration::new(
        u64::try_from(now.tv_sec).unwrap_or(0),
        u32::try_from(now.tv_nsec).unwrap_or(0),
    )
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
        "drawing: {} frames, {:.3} ms per frame (median), at most {:.3} ms",
        ms.len(),
        ms[ms.len() / 2],
        ms[ms.len() - 1]
    );
}

impl App {
    fn size(&self) -> (i32, i32) {
        // The viewport gives the window its size, from this buffer.
        match self.size {
            _ if self.viewport.is_some() => CHOSEN_SIZE,
            (0, _) | (_, 0) => CHOSEN_SIZE,
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
        // One "frame done" asked for at a time: the first frame drawn for a
        // configure starts no second chain.
        let counted = self.frames_left > 0 && (self.pace != Pace::Callbacks || !self.frame_asked);
        if counted {
            match self.pace {
                Pace::Callbacks => {
                    self.frame_asked = true;
                    self.surface.frame(qh, ());
                }
                // As Mesa's FIFO: wait for the last frame, be waited for.
                Pace::Fifo => {
                    if let Some(fifo) = &self.fifo {
                        fifo.wait_barrier();
                        fifo.set_barrier();
                    }
                    self.frames_left -= 1;
                }
                Pace::Timed => {
                    let set = |timer: &WpCommitTimerV1, due: Duration| {
                        let secs = due.as_secs();
                        timer.set_timestamp((secs >> 32) as u32, secs as u32, due.subsec_nanos());
                    };
                    match (&self.timer, self.next_due.take()) {
                        (Some(timer), Some(due)) => {
                            set(timer, due);
                            self.dues.resize(self.asked as usize, None);
                            self.dues.push(Some(due));
                        }
                        // The first, which maps the window, a moment ahead:
                        // the compositor lets it go though nothing shows it
                        // yet. Not counted, the frames after it are timed
                        // against their last one's showing.
                        (Some(timer), None) if self.asked == 0 => {
                            set(timer, monotonic() + TIMED_FIRST);
                        }
                        _ => {}
                    }
                    self.frames_left -= 1;
                }
            }
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

    /// `--fifo`: draws while frames are left and one of the swapchain's
    /// buffers is free; the next commit waits in the compositor.
    fn pump(&mut self, qh: &QueueHandle<App>) {
        while self.frames_left > 0
            && (self.buffers.len() < SWAPCHAIN || self.buffers.iter().any(|b| !b.busy))
        {
            if let Err(e) = self.draw(qh) {
                eprintln!("beize-testclient: {e}");
                self.running = false;
                return;
            }
        }
    }

    /// A buffer of `size` the compositor does not read, made if needed.
    /// Free ones of another size are destroyed: only dropped, the
    /// compositor kept them and their memory (a resize on the owner's PC
    /// grew the client to 11 GB, 26.10.).
    fn free_buffer(&mut self, qh: &QueueHandle<App>, size: (i32, i32)) -> Result<usize> {
        self.buffers.retain(|b| {
            let keep = b.busy || b.size == size;
            if !keep {
                b.buffer.destroy();
            }
            keep
        });
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
        let id = self.made;
        self.made += 1;
        let buffer = pool.create_buffer(
            0,
            size.0,
            size.1,
            size.0 * 4,
            wl_shm::Format::Xrgb8888,
            qh,
            id,
        );
        pool.destroy();
        self.buffers.push(Buffer {
            id,
            file,
            buffer,
            size,
            busy: false,
            filled: false,
        });
        Ok(self.buffers.len() - 1)
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
        match self.pace {
            // Its "frame done" once the compositor drew it, ahead of the
            // answer that it showed at the vblank: a frame's time to draw
            // the next (Weston answers at repaint; at the vblank, a 240 Hz
            // screen left a client ~1 ms, 26.10.). Told by the order the
            // events came in, which the connection keeps: when the client
            // read them depends on when it ran (timed so, a smoke run with
            // five machines once had 119 of 120).
            Pace::Callbacks => {
                let ahead = shown
                    .iter()
                    .filter(|s| {
                        let done = self.done_answered.get(s.frame as usize - 1);
                        done.is_some_and(|answered| *answered < s.frame)
                    })
                    .count();
                println!("frame done: {ahead} of {} before they showed", shown.len());
            }
            Pace::Fifo => {
                let steps = shown.windows(2).filter(|w| w[1].frame == w[0].frame + 1);
                let apart = steps
                    .clone()
                    .filter(|w| w[1].sequence == w[0].sequence + 1)
                    .count();
                // The others: the frame and how many vblanks after the last.
                let others: Vec<String> = steps
                    .clone()
                    .filter(|w| w[1].sequence != w[0].sequence + 1)
                    .map(|w| {
                        format!(
                            "{}:{}",
                            w[1].frame,
                            w[1].sequence.wrapping_sub(w[0].sequence)
                        )
                    })
                    .collect();
                println!(
                    "fifo: {apart} of {} a vblank apart, others [{}]",
                    steps.count(),
                    others.join(" ")
                );
            }
            Pace::Timed => {
                // Each at the first vblank at or after its time: not before
                // it, and less than a refresh after.
                let late: Vec<f64> = shown
                    .iter()
                    .filter_map(|s| {
                        let due = (*self.dues.get(s.frame as usize - 1)?)?;
                        let after = s.time.as_secs_f64() - due.as_secs_f64();
                        Some((after, f64::from(s.refresh) / 1e9))
                    })
                    .map(|(after, refresh)| after / refresh)
                    .collect();
                let on_time = late.iter().filter(|&&r| (0.0..1.0).contains(&r)).count();
                let early = late.iter().filter(|&&r| r < 0.0).count();
                let ms = |r: Option<&f64>| r.map_or(0.0, |r| r * f64::from(refresh) / 1e6);
                let mut sorted = late.clone();
                sorted.sort_by(f64::total_cmp);
                println!(
                    "timed: {on_time} of {} at the first vblank at or after their time, {early} \
                     before it, from {:+.2} to {:+.2} ms of it",
                    late.len(),
                    ms(sorted.first()),
                    ms(sorted.last()),
                );
            }
        }
    }

    /// `--activation`: a window got the keyboard with `serial`; the next
    /// step, if it waited for that.
    fn entered(&mut self, surface: &WlSurface, serial: u32, qh: &QueueHandle<App>) {
        let first = self.surface.clone();
        let Some(activation) = &mut self.activation else {
            return;
        };
        if activation.target.as_ref() == Some(surface) {
            activation.got_it = true;
        }
        activation.focused = Some(surface.clone());
        let second = activation.second.as_ref().map(|s| s.surface.clone());
        match activation.step {
            Step::First if *surface == first => {
                match open_second(&self.compositor, &self.shm, &activation.wm_base, qh) {
                    Ok(window) => activation.second = Some(window),
                    Err(e) => {
                        eprintln!("beize-testclient: {e}");
                        self.running = false;
                        return;
                    }
                }
                activation.step = Step::Second;
            }
            Step::Second if Some(surface) == second.as_ref() => {
                activation.second_entered = Some(serial);
                activation.step = Step::ActiveApp;
                self.ask_token(qh);
            }
            _ => {}
        }
    }

    /// `--activation`: a token from the second window, with the serial its
    /// step calls for.
    fn ask_token(&self, qh: &QueueHandle<App>) {
        let Some(activation) = &self.activation else {
            return;
        };
        let token = activation.manager.get_activation_token(qh, ());
        if activation.step != Step::NoSerial
            && let Some(serial) = activation.second_entered
        {
            token.set_serial(serial, &self.seat);
        }
        if let Some(second) = &activation.second {
            token.set_surface(&second.surface);
        }
        token.commit();
    }

    /// `--activation`: the token arrived; the request of this step, and a
    /// roundtrip after it.
    fn activate(&mut self, token: String, qh: &QueueHandle<App>) {
        let first = self.surface.clone();
        let Some(activation) = &mut self.activation else {
            return;
        };
        let second = activation.second.as_ref().map(|s| s.surface.clone());
        let target = match activation.step {
            Step::ActiveApp => Some(first),
            _ if activation.focused.as_ref() == Some(&first) => second,
            _ => Some(first),
        };
        let Some(target) = target else {
            return;
        };
        activation.manager.activate(token, &target);
        activation.target = Some(target);
        activation.got_it = false;
        activation.display.sync(qh, Roundtrip);
    }

    /// `--activation`: the roundtrip after a request is back – the window
    /// asked for has the keyboard by now, or it will not; the next step.
    fn answered_activation(&mut self, qh: &QueueHandle<App>) {
        let Some(activation) = &mut self.activation else {
            return;
        };
        let step = activation.step;
        println!(
            "activation {}: {}",
            step.name(),
            if activation.got_it {
                "granted"
            } else {
                "refused"
            }
        );
        activation.target = None;
        activation.step = match step {
            Step::ActiveApp => Step::FocusMoved,
            Step::FocusMoved => Step::NoSerial,
            _ => Step::Done,
        };
        if activation.step == Step::Done {
            println!("activation done");
        } else {
            self.ask_token(qh);
        }
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

/// `--activation`'s second window, white; it shows its buffer on its
/// first configure.
fn open_second(
    compositor: &WlCompositor,
    shm: &WlShm,
    wm_base: &XdgWmBase,
    qh: &QueueHandle<App>,
) -> Result<Second> {
    let (w, h) = SECOND_SIZE;
    let bytes = w * h * 4;
    let fd = rustix::fs::memfd_create("beize-testclient", rustix::fs::MemfdFlags::CLOEXEC)
        .map_err(|e| format!("memfd: {e}"))?;
    let file = File::from(fd);
    let pixels: Vec<u8> = (0..w * h).flat_map(|_| WHITE.to_le_bytes()).collect();
    file.write_all_at(&pixels, 0)
        .map_err(|e| format!("writing the buffer: {e}"))?;
    let pool: WlShmPool = shm.create_pool(file.as_fd(), bytes, qh, ());
    let buffer = pool.create_buffer(0, w, h, w * 4, wl_shm::Format::Xrgb8888, qh, SecondWindow);
    pool.destroy();
    let surface = compositor.create_surface(qh, ());
    let xdg_surface = wm_base.get_xdg_surface(&surface, qh, SecondWindow);
    let toplevel = xdg_surface.get_toplevel(qh, SecondWindow);
    toplevel.set_title("beize-testclient second".into());
    toplevel.set_app_id("beize-testclient".into());
    surface.commit();
    Ok(Second {
        surface,
        _xdg_surface: xdg_surface,
        _toplevel: toplevel,
        buffer,
        _file: file,
    })
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
            // `--fifo` and `--timed` draw on their own pace once started;
            // their next frame acknowledges the configure.
            if app.pace != Pace::Callbacks && std::mem::replace(&mut app.started, true) {
                return;
            }
            // With a "frame done" on its way, that draws the new size, as
            // GTK's frame clock does: drawn at once, a 1000 Hz mouse's
            // configures came faster than frames.
            if app.frame_asked {
                return;
            }
            if app.pace == Pace::Fifo {
                app.pump(qh);
            } else if let Err(e) = app.draw(qh) {
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
            let answered = app.answered();
            app.done_answered.push(answered);
            app.frames_left = app.frames_left.saturating_sub(1);
            app.report_presentation();
            if (app.frames_left > 0 || app.full)
                && let Err(e) = app.draw(qh)
            {
                eprintln!("beize-testclient: {e}");
                app.running = false;
            }
        }
    }
}

impl Dispatch<WlBuffer, u32> for App {
    fn event(
        app: &mut App,
        _: &WlBuffer,
        event: wl_buffer::Event,
        id: &u32,
        _: &Connection,
        qh: &QueueHandle<App>,
    ) {
        if let wl_buffer::Event::Release = event
            && let Some(buffer) = app.buffers.iter_mut().find(|b| b.id == *id)
        {
            buffer.busy = false;
            if app.pace == Pace::Fifo && app.started {
                app.pump(qh);
            }
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
        qh: &QueueHandle<App>,
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
            wl_keyboard::Event::Enter {
                serial, surface, ..
            } => {
                if surface == app.surface {
                    println!("keyboard focus");
                } else {
                    println!("keyboard focus, second window");
                }
                app.entered(&surface, serial, qh);
            }
            wl_keyboard::Event::Leave { .. } => {
                println!("keyboard focus lost");
                if let Some(activation) = &mut app.activation {
                    activation.focused = None;
                }
            }
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
        qh: &QueueHandle<App>,
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
                let time =
                    Duration::new((u64::from(tv_sec_hi) << 32) | u64::from(tv_sec_lo), tv_nsec);
                app.shown.push(Shown {
                    frame: app.answered() + 1,
                    time,
                    refresh,
                    sequence: (u64::from(seq_hi) << 32) | u64::from(seq_lo),
                    flags: match flags {
                        WEnum::Value(kind) => kind.bits(),
                        WEnum::Unknown(bits) => bits,
                    },
                    arrived: monotonic(),
                });
                // `--timed`: the next frame, for its time after this one.
                if app.pace == Pace::Timed && app.frames_left > 0 {
                    let ahead = Duration::from_nanos(u64::from(refresh)).mul_f64(TIMED_AHEAD);
                    app.next_due = Some(time + ahead);
                    if let Err(e) = app.draw(qh) {
                        eprintln!("beize-testclient: {e}");
                        app.running = false;
                    }
                }
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

/// `--activation`: the second window shows its buffer.
impl Dispatch<XdgSurface, SecondWindow> for App {
    fn event(
        app: &mut App,
        xdg_surface: &XdgSurface,
        event: xdg_surface::Event,
        _: &SecondWindow,
        _: &Connection,
        _: &QueueHandle<App>,
    ) {
        if let xdg_surface::Event::Configure { serial } = event {
            xdg_surface.ack_configure(serial);
            if let Some(second) = app.activation.as_ref().and_then(|a| a.second.as_ref()) {
                let (w, h) = SECOND_SIZE;
                second.surface.attach(Some(&second.buffer), 0, 0);
                second.surface.damage_buffer(0, 0, w, h);
                second.surface.commit();
            }
        }
    }
}

impl Dispatch<XdgToplevel, SecondWindow> for App {
    fn event(
        _: &mut App,
        _: &XdgToplevel,
        _: xdg_toplevel::Event,
        _: &SecondWindow,
        _: &Connection,
        _: &QueueHandle<App>,
    ) {
    }
}

/// The second window's one buffer stays as it is.
impl Dispatch<WlBuffer, SecondWindow> for App {
    fn event(
        _: &mut App,
        _: &WlBuffer,
        _: wl_buffer::Event,
        _: &SecondWindow,
        _: &Connection,
        _: &QueueHandle<App>,
    ) {
    }
}

impl Dispatch<WlCallback, Roundtrip> for App {
    fn event(
        app: &mut App,
        _: &WlCallback,
        event: wl_callback::Event,
        _: &Roundtrip,
        _: &Connection,
        qh: &QueueHandle<App>,
    ) {
        if let wl_callback::Event::Done { .. } = event {
            app.answered_activation(qh);
        }
    }
}

impl Dispatch<XdgActivationTokenV1, ()> for App {
    fn event(
        app: &mut App,
        token: &XdgActivationTokenV1,
        event: xdg_activation_token_v1::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<App>,
    ) {
        if let xdg_activation_token_v1::Event::Done { token: made } = event {
            token.destroy();
            app.activate(made, qh);
        }
    }
}

delegate_noop!(App: ignore XdgActivationV1);
delegate_noop!(App: ignore ZwpIdleInhibitManagerV1);
delegate_noop!(App: ignore ZwpIdleInhibitorV1);
delegate_noop!(App: ignore WpViewporter);
delegate_noop!(App: ignore WpViewport);
delegate_noop!(App: ignore ZxdgDecorationManagerV1);
delegate_noop!(App: ignore WpFifoManagerV1);
delegate_noop!(App: ignore WpFifoV1);
delegate_noop!(App: ignore WpCommitTimingManagerV1);
delegate_noop!(App: ignore WpCommitTimerV1);
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
