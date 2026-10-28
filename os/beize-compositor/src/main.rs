//! `beize-compositor` – Beize's compositor (ADR 0027). Runs as the logged-in
//! user, never as root: logind opens the GPU and input devices for it
//! (libseat). Its parts:
//!
//! - `gpu`, `output`: the GPU, the screen and Beize's own frame loop with
//!   the Vulkan renderer (`render`, §7); it draws only what changed, just
//!   before the vblank (`schedule`), and measures itself (`pacing`).
//! - `state`: the Wayland protocols and when the screen is drawn;
//!   `scene` what a frame shows, front to back, and its live glass;
//!   `shell`: windows (xdg-shell), `windows` their places, `decorations`
//!   the frames of other apps' windows (xdg-decoration), `corners` round
//!   corners and shadows of all; `layers`: the
//!   panels of Beize's shell (wlr-layer-shell), which only the shell may
//!   draw (`trust`).
//! - `input`, `keyboard`, `cursor`: libinput, the layout localed set, the
//!   pointer on the cursor plane.
//! - `opening`: the desktop appears in one piece, once the shell is ready.
//! - `activation`: windows that ask to become active (xdg-activation).
//! - `apps`: apps in the user's service manager (the terminal).
//!
//! Clients connect on `$XDG_RUNTIME_DIR/wayland-0`. It runs as
//! `beize-compositor.service`, which beize-session starts.

mod activation;
mod apps;
mod backdrop;
mod corners;
mod cursor;
mod decorations;
mod error;
mod glass;
mod gpu;
mod idle;
mod input;
mod keyboard;
mod layers;
mod log;
mod opening;
mod output;
mod pacing;
mod render;
mod scene;
mod schedule;
mod screenshot;
mod shell;
mod shell_v1;
mod state;
mod syncobj;
mod timing;
mod trust;
mod windows;

use std::ffi::{c_char, c_int};
use std::process::ExitCode;

use smithay::backend::libinput::{LibinputInputBackend, LibinputSessionInterface};
use smithay::backend::session::libseat::LibSeatSession;
use smithay::backend::session::{Event as SessionEvent, Session};
use smithay::reexports::calloop::generic::Generic;
use smithay::reexports::calloop::{EventLoop, Interest, Mode, PostAction};
use smithay::reexports::input::Libinput;
use smithay::reexports::wayland_server::Display;
use smithay::wayland::socket::ListeningSocketSource;
use tracing::{error, info, warn};

use crate::error::{Context, Result, error};
use state::State;

/// The socket clients connect on: a fixed name, as mutter's and KWin's,
/// since each user has one compositor; beize-session tells the user's
/// service manager the same as WAYLAND_DISPLAY.
const SOCKET: &str = "wayland-0";

#[link(name = "systemd")]
unsafe extern "C" {
    fn sd_notify(unset_environment: c_int, state: *const c_char) -> c_int;
}

/// Tells systemd the desktop can start (Type=notify): beize-session.target
/// becomes active, the shell starts. Once the first screen is taken
/// (`State::first_screen`); started by hand, there is no one to tell.
pub(crate) fn notify_ready() {
    // SAFETY: a NUL-terminated string that sd_notify only reads.
    let r = unsafe { sd_notify(0, c"READY=1".as_ptr()) };
    if r < 0 {
        warn!(
            "telling systemd it is ready: {}",
            std::io::Error::from_raw_os_error(-r)
        );
    }
}

fn main() -> ExitCode {
    log::init();
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            error!("{e}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<()> {
    let mut event_loop: EventLoop<State> = EventLoop::try_new().context("event loop")?;
    let handle = event_loop.handle();
    info!("starting");
    let (session, session_events) =
        LibSeatSession::new().context("the logind session (libseat)")?;

    let mut libinput =
        Libinput::new_with_udev::<LibinputSessionInterface<LibSeatSession>>(session.clone().into());
    libinput
        .udev_assign_seat(&session.seat())
        .map_err(|()| error("libinput: no seat"))?;
    handle
        .insert_source(
            LibinputInputBackend::new(libinput.clone()),
            |event, _, state| input::handle(state, event),
        )
        .map_err(|e| error(format!("input events: {}", e.error)))?;

    let display: Display<State> = Display::new().context("Wayland display")?;
    let mut state = State::new(
        display.handle(),
        handle.clone(),
        event_loop.get_signal(),
        session,
        libinput,
    )?;
    handle
        .insert_source(
            Generic::new(display, Interest::READ, Mode::Level),
            |_, display, state| {
                // SAFETY: the display is only borrowed here, never moved or
                // dropped while the source holds it.
                unsafe { display.get_mut() }
                    .dispatch_clients(state)
                    .map_err(std::io::Error::other)?;
                Ok(PostAction::Continue)
            },
        )
        .map_err(|e| error(format!("Wayland events: {}", e.error)))?;
    let socket = ListeningSocketSource::with_name(SOCKET).context("Wayland socket")?;
    info!(
        "clients connect on {}",
        socket.socket_name().to_string_lossy()
    );
    handle
        .insert_source(socket, |stream, _, state| {
            state::insert_client(state, stream)
        })
        .map_err(|e| error(format!("Wayland socket: {}", e.error)))?;

    match keyboard::Watch::new() {
        Ok(watch) => {
            handle
                .insert_source(
                    Generic::new(watch, Interest::READ, Mode::Level),
                    |_, watch, state| {
                        if watch.changed() {
                            state.reload_layout();
                        }
                        Ok(PostAction::Continue)
                    },
                )
                .map_err(|e| error(format!("layout changes: {}", e.error)))?;
        }
        Err(e) => warn!("layout changes are not followed: {e}"),
    }

    handle
        .insert_source(session_events, |event, _, state| match event {
            SessionEvent::PauseSession => {
                info!("session paused");
                state.pause();
            }
            SessionEvent::ActivateSession => {
                info!("session active");
                if let Err(e) = state.activate() {
                    state.fail(e);
                }
            }
        })
        .map_err(|e| error(format!("session events: {}", e.error)))?;

    if state.session.is_active() {
        state.activate()?;
    } else {
        info!("waiting for the session to become active");
    }
    event_loop
        .run(None, &mut state, |state| {
            state.popup_grab_ended();
            // Changes are drawn at their vblank's deadline (`State::changed`);
            // the clients hear everything before we sleep.
            if let Err(e) = state.display.flush_clients() {
                warn!("sending to clients: {e}");
            }
        })
        .context("event loop")?;
    state.failure().map_or(Ok(()), Err)
}
