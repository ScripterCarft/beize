//! Input from libinput (devices opened by logind) to the seat: keys to the
//! window with the keyboard, the pointer to the window under it, a click
//! makes a window the active one – not through a panel of the shell above
//! it. Ctrl+Alt+F1…F12 switch the VT, as on
//! every Linux desktop – the compositor holds the keyboard, so it asks
//! logind; Ctrl+Alt+T opens a terminal (`apps`); Super+F switches the
//! active window's fullscreen (ADR 0028 §2).
//!
//! While a fullscreen window is in front, the panels show only with the
//! pointer at their edge of the screen (`State::reveal`).

use smithay::backend::input::{
    AbsolutePositionEvent, Axis, AxisSource, ButtonState, Event, InputBackend, InputEvent,
    KeyState, KeyboardKeyEvent, PointerAxisEvent, PointerButtonEvent, PointerMotionEvent,
};
use smithay::backend::libinput::LibinputInputBackend;
use smithay::backend::session::Session;
use smithay::input::keyboard::{FilterResult, Keysym, ModifiersState};
use smithay::input::pointer::{AxisFrame, ButtonEvent, MotionEvent};
use smithay::utils::{Logical, Point, SERIAL_COUNTER};
use tracing::warn;

use crate::apps;
use crate::decorations::Hit;
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

/// What a key the compositor takes does.
#[derive(Debug, PartialEq)]
enum Action {
    SwitchVt(u32),
    OpenTerminal,
    ToggleFullscreen,
}

fn key<B: InputBackend>(state: &mut State, event: &B::KeyboardKeyEvent) {
    let code = event.key_code();
    let pressed = event.state() == KeyState::Pressed;
    let keyboard = state.keyboard.clone();
    let action = keyboard.input(
        state,
        code,
        event.state(),
        SERIAL_COUNTER.next_serial(),
        event.time_msec(),
        |state, modifiers, keysym| {
            if !pressed {
                return if state.taken_keys.remove(&code) {
                    FilterResult::Intercept(None)
                } else {
                    FilterResult::Forward
                };
            }
            match action(
                modifiers,
                keysym.modified_sym(),
                keysym.raw_latin_sym_or_raw_current_sym(),
            ) {
                Some(action) => {
                    state.taken_keys.insert(code);
                    FilterResult::Intercept(Some(action))
                }
                None => FilterResult::Forward,
            }
        },
    );
    match action.flatten() {
        Some(Action::SwitchVt(vt)) => {
            if let Err(e) = state.session.change_vt(vt as i32) {
                warn!("switching to VT {vt}: {e}");
            }
        }
        Some(Action::OpenTerminal) => apps::open_terminal(),
        Some(Action::ToggleFullscreen) => {
            let active = keyboard
                .current_focus()
                .and_then(|surface| state.window_for(&surface));
            if let Some(toplevel) = active.as_ref().and_then(|w| w.toplevel()) {
                state.toggle_fullscreen(&toplevel.clone());
            }
        }
        None => {}
    }
}

/// The compositor's keys: Ctrl+Alt+F1…F12 (the layout gives them as VT
/// keysyms), Ctrl+Alt+T for a new terminal, as on Ubuntu, and Super+F for
/// fullscreen (Super belongs to the shell). The letter counts as a Latin
/// layout among the loaded ones has it (Smithay), so with Russian next to
/// US the key stays where it is, as in GNOME.
fn action(modifiers: &ModifiersState, sym: Keysym, latin: Option<Keysym>) -> Option<Action> {
    let first = Keysym::XF86_Switch_VT_1.raw();
    if (first..=Keysym::XF86_Switch_VT_12.raw()).contains(&sym.raw()) {
        return Some(Action::SwitchVt(sym.raw() - first + 1));
    }
    let only_ctrl_alt = modifiers.ctrl && modifiers.alt && !modifiers.shift && !modifiers.logo;
    let only_super = modifiers.logo && !modifiers.ctrl && !modifiers.alt && !modifiers.shift;
    match latin {
        Some(Keysym::t) if only_ctrl_alt => Some(Action::OpenTerminal),
        Some(Keysym::f) if only_super => Some(Action::ToggleFullscreen),
        _ => None,
    }
}

/// The pointer over what is under it now, without moving: after a window
/// left the screen or came back.
pub fn refresh_pointer(state: &mut State) {
    let (at, time) = (state.pointer, state.time_msec());
    move_to(state, at, time);
}

fn move_to(state: &mut State, to: Point<f64, Logical>, time: u32) {
    let area = state.screen_area().to_f64();
    // The last pixel is the limit: the cursor never leaves the screen.
    let to = Point::from((
        to.x.clamp(area.loc.x, area.loc.x + area.size.w - 1.0),
        to.y.clamp(area.loc.y, area.loc.y + area.size.h - 1.0),
    ));
    state.pointer = to;
    state.reveal();
    state.bar_hover(to);
    // A window's edge strip in front: no client has the pointer there.
    let under = if state.edge_under(to).is_some() {
        None
    } else {
        state.surface_under(to)
    };
    let own = under.is_none();
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
    if own {
        state.own_cursor();
    }
    // Only the cursor plane moves; nothing is drawn.
    state.changed();
}

fn button<B: InputBackend>(state: &mut State, event: &B::PointerButtonEvent) {
    let serial = SERIAL_COUNTER.next_serial();
    let pressed = event.state() == ButtonState::Pressed;
    let grabbed = state.seat.get_pointer().is_some_and(|p| p.is_grabbed());
    // On a window's edge in front (`edge_under`): the compositor resizes
    // it; the press goes to that drag (`windows`).
    if pressed
        && !grabbed
        && !state.panel_under(state.pointer)
        && !state.resize_from_edge(event.button_code(), serial)
        && let Some((window, hit)) = state.window_under(state.pointer)
    {
        state.focus(Some(window.clone()));
        if hit == Hit::Bar {
            // The compositor's title bar (`decorations`): a drag goes to its
            // grab, like the edge's.
            state.bar_press(&window, event.button_code(), serial);
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
    if !pressed {
        state.bar_release(event.button_code());
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

#[cfg(test)]
mod tests {
    use super::*;

    fn held(ctrl: bool, alt: bool, shift: bool) -> ModifiersState {
        ModifiersState {
            ctrl,
            alt,
            shift,
            ..ModifiersState::default()
        }
    }

    #[test]
    fn takes_only_its_own_keys() {
        let ctrl_alt = held(true, true, false);
        assert_eq!(
            action(&ctrl_alt, Keysym::XF86_Switch_VT_2, None),
            Some(Action::SwitchVt(2))
        );
        assert_eq!(
            action(&ctrl_alt, Keysym::t, Some(Keysym::t)),
            Some(Action::OpenTerminal)
        );
        // Russian active, US loaded too: the key of T gives Cyrillic ie.
        assert_eq!(
            action(&ctrl_alt, Keysym::Cyrillic_ie, Some(Keysym::t)),
            Some(Action::OpenTerminal)
        );
        for modifiers in [
            held(true, false, false),
            held(false, true, false),
            held(true, true, true),
        ] {
            assert_eq!(action(&modifiers, Keysym::t, Some(Keysym::t)), None);
        }
        assert_eq!(action(&ctrl_alt, Keysym::u, Some(Keysym::u)), None);
        let logo = ModifiersState {
            logo: true,
            ..ModifiersState::default()
        };
        assert_eq!(
            action(&logo, Keysym::f, Some(Keysym::f)),
            Some(Action::ToggleFullscreen)
        );
        assert_eq!(action(&ctrl_alt, Keysym::f, Some(Keysym::f)), None);
        assert_eq!(action(&logo, Keysym::t, Some(Keysym::t)), None);
    }
}
