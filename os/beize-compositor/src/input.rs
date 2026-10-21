//! Input from libinput (devices opened by logind) to the seat: keys to the
//! window with the keyboard, the pointer to the window under it, a click
//! makes a window the active one. Ctrl+Alt+F1…F12 switch the VT, as on
//! every Linux desktop – the compositor holds the keyboard, so it asks
//! logind.

use smithay::backend::input::{
    AbsolutePositionEvent, Axis, AxisSource, ButtonState, Event, InputBackend, InputEvent,
    KeyState, KeyboardKeyEvent, PointerAxisEvent, PointerButtonEvent, PointerMotionEvent,
};
use smithay::backend::libinput::LibinputInputBackend;
use smithay::backend::session::Session;
use smithay::input::keyboard::{FilterResult, Keysym};
use smithay::input::pointer::{AxisFrame, ButtonEvent, MotionEvent};
use smithay::utils::{Logical, Point, SERIAL_COUNTER};
use tracing::warn;

use crate::state::State;

pub fn handle(state: &mut State, event: InputEvent<LibinputInputBackend>) {
    match event {
        InputEvent::Keyboard { event } => key::<LibinputInputBackend>(state, &event),
        InputEvent::PointerMotion { event } => {
            let to = state.pointer + event.delta();
            move_to(state, to, event.time_msec());
        }
        InputEvent::PointerMotionAbsolute { event } => {
            let area = state.screen_area();
            let to = event.position_transformed(area.size) + area.loc.to_f64();
            move_to(state, to, event.time_msec());
        }
        InputEvent::PointerButton { event } => button::<LibinputInputBackend>(state, &event),
        InputEvent::PointerAxis { event } => axis::<LibinputInputBackend>(state, &event),
        _ => {}
    }
}

fn key<B: InputBackend>(state: &mut State, event: &B::KeyboardKeyEvent) {
    let pressed = event.state() == KeyState::Pressed;
    let keyboard = state.keyboard.clone();
    let vt = keyboard.input(
        state,
        event.key_code(),
        event.state(),
        SERIAL_COUNTER.next_serial(),
        event.time_msec(),
        |_, _, keysym| {
            let sym = keysym.modified_sym().raw();
            let first = Keysym::XF86_Switch_VT_1.raw();
            if pressed && (first..=Keysym::XF86_Switch_VT_12.raw()).contains(&sym) {
                FilterResult::Intercept(sym - first + 1)
            } else {
                FilterResult::Forward
            }
        },
    );
    if let Some(vt) = vt
        && let Err(e) = state.session.change_vt(vt as i32)
    {
        warn!("switching to VT {vt}: {e}");
    }
}

fn move_to(state: &mut State, to: Point<f64, Logical>, time: u32) {
    let area = state.screen_area().to_f64();
    // The last pixel is the limit: the cursor never leaves the screen.
    let to = Point::from((
        to.x.clamp(area.loc.x, area.loc.x + area.size.w - 1.0),
        to.y.clamp(area.loc.y, area.loc.y + area.size.h - 1.0),
    ));
    state.pointer = to;
    let under = state.surface_under(to);
    if let Some(pointer) = state.seat.get_pointer() {
        pointer.motion(
            state,
            under,
            &MotionEvent {
                location: to,
                serial: SERIAL_COUNTER.next_serial(),
                time,
            },
        );
        pointer.frame(state);
    }
    // Only the cursor plane moves; nothing is drawn.
    state.changed();
}

fn button<B: InputBackend>(state: &mut State, event: &B::PointerButtonEvent) {
    let serial = SERIAL_COUNTER.next_serial();
    if event.state() == ButtonState::Pressed {
        let window = state
            .space
            .element_under(state.pointer)
            .map(|(window, _)| window.clone());
        if window.is_some() {
            state.focus(window);
        }
    }
    if let Some(pointer) = state.seat.get_pointer() {
        pointer.button(
            state,
            &ButtonEvent {
                button: event.button_code(),
                state: event.state(),
                serial,
                time: event.time_msec(),
            },
        );
        pointer.frame(state);
    }
}

/// Scrolling, smooth (touchpads) and by wheel clicks (`v120`), as libinput
/// reports it.
fn axis<B: InputBackend>(state: &mut State, event: &B::PointerAxisEvent) {
    let source = event.source();
    let mut frame = AxisFrame::new(event.time_msec()).source(source);
    for axis in [Axis::Horizontal, Axis::Vertical] {
        let v120 = event.amount_v120(axis);
        // A wheel click is 15 units of smooth scrolling, as in libinput.
        let amount = event
            .amount(axis)
            .unwrap_or_else(|| v120.unwrap_or(0.0) * 15.0 / 120.0);
        if amount != 0.0 {
            frame = frame.value(axis, amount);
            if let Some(v120) = v120 {
                frame = frame.v120(axis, v120 as i32);
            }
        } else if source == AxisSource::Finger {
            frame = frame.stop(axis);
        }
    }
    if let Some(pointer) = state.seat.get_pointer() {
        pointer.axis(state, frame);
        pointer.frame(state);
    }
}
