//! `beize-compositor` – Beize's compositor (ADR 0027). Runs as the logged-in
//! user, never as root: logind opens the GPU and input devices for it
//! (libseat). Its parts:
//!
//! - `gpu`, `output`: the GPU, the screen and Beize's own frame loop with
//!   the Vulkan renderer (`render`, §7); it draws only what changed.
//! - `state`: the Wayland protocols and when the screen is drawn;
//!   `shell`: windows (xdg-shell).
//! - `input`, `keyboard`, `cursor`: libinput, the layout localed set, the
//!   pointer on the cursor plane.
//!
//! Clients connect on `$XDG_RUNTIME_DIR/wayland-N`, N the first free one.

mod cursor;
mod gpu;
mod input;
mod keyboard;
mod output;
mod shell;
mod state;

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

use beize_compositor::error::{Context, Result, error};
use beize_compositor::log;
use state::State;

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
    let socket = ListeningSocketSource::new_auto().context("Wayland socket")?;
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
            // Once per round of events: what they changed is drawn
            // together, and the clients hear everything before we sleep.
            if state.wants_render() {
                state.render();
            }
            if let Err(e) = state.display.flush_clients() {
                warn!("sending to clients: {e}");
            }
        })
        .context("event loop")?;
    state.failure().map_or(Ok(()), Err)
}
