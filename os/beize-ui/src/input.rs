//! Input (ADR 0027 §9): the keyboard in the compositor's keymap
//! (libxkbcommon), dead keys through the locale's Compose table, key repeat
//! on the window's side as Wayland asks; the pointer in the window's
//! coordinates.

use std::ffi::OsString;
use std::os::fd::OwnedFd;
use std::time::{Duration, Instant};

use tracing::{debug, warn};
use wayland_client::WEnum;
use wayland_client::protocol::{wl_keyboard, wl_pointer};
use xkbcommon::xkb::{self, Keycode, compose};

pub use xkbcommon::xkb::Keysym;

/// What the user did, in the order it happened.
#[derive(Debug, Clone, PartialEq)]
pub enum Input {
    Key(Key),
    /// The window got (true) or lost the keyboard.
    Focus(bool),
    Pointer(Pointer),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Key {
    /// The key's symbol in the layout, with the modifiers applied (`a`,
    /// `A`, `Return`); for a finished Compose sequence its result.
    pub sym: Keysym,
    /// What the key types, if anything: Compose applied, and with Ctrl the
    /// control character (Ctrl+C is "\x03") – what a terminal sends on.
    /// None for keys that type nothing and inside a Compose sequence.
    pub text: Option<String>,
    pub mods: Modifiers,
    pub state: KeyState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyState {
    Pressed,
    /// Held down: again, at the rate the compositor asks for.
    Repeated,
    Released,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Modifiers {
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
    /// The Super key (Windows key; Cmd on Apple keyboards).
    pub logo: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Pointer {
    /// Entered the window, at x, y.
    Enter(f64, f64),
    Leave,
    Motion(f64, f64),
    Button {
        button: Button,
        pressed: bool,
    },
    /// Scrolled, in pixels (touchpad); `steps` are a wheel's notches
    /// (fractions on high-resolution wheels), zero for a touchpad.
    Scroll {
        x: f64,
        y: f64,
        steps_x: f64,
        steps_y: f64,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Button {
    Left,
    Right,
    Middle,
    /// Another button, by its evdev code.
    Other(u32),
}

/// Wayland's key codes are evdev's; XKB's are 8 higher (X11's heritage).
const XKB_OFFSET: u32 = 8;
/// Evdev's button codes (linux/input-event-codes.h).
const BTN_LEFT: u32 = 0x110;
const BTN_RIGHT: u32 = 0x111;
const BTN_MIDDLE: u32 = 0x112;
/// One wheel notch in `axis_value120`.
const NOTCH: f64 = 120.0;

/// The keyboard: keymap and state from the compositor, Compose from the
/// locale.
pub(crate) struct Keyboard {
    context: xkb::Context,
    xkb: Option<(xkb::Keymap, xkb::State)>,
    compose: Option<compose::State>,
    mods: Modifiers,
    /// Delay and interval; None: the compositor turned repeating off.
    rate: Option<(Duration, Duration)>,
    repeat: Option<Repeat>,
}

struct Repeat {
    code: Keycode,
    next: Instant,
}

impl Keyboard {
    pub(crate) fn new() -> Keyboard {
        let context = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
        let table = compose_table(&context);
        Keyboard::with(context, table)
    }

    fn with(context: xkb::Context, table: Option<compose::Table>) -> Keyboard {
        Keyboard {
            context,
            xkb: None,
            compose: table.map(|t| compose::State::new(&t, compose::STATE_NO_FLAGS)),
            mods: Modifiers::default(),
            // Until repeat_info comes (from wl_seat 4 on it always does):
            // the compositor's own (ADR 0027 §8).
            rate: Some((Duration::from_millis(500), Duration::from_secs(1) / 33)),
            repeat: None,
        }
    }

    /// When the held key repeats next.
    pub(crate) fn next_repeat(&self) -> Option<Instant> {
        self.repeat.as_ref().map(|r| r.next)
    }

    /// The repeat of the held key, if it is due.
    pub(crate) fn repeat_due(&mut self, now: Instant) -> Option<Input> {
        let (_, interval) = self.rate?;
        let repeat = self.repeat.as_mut()?;
        if repeat.next > now {
            return None;
        }
        // Late (the app was busy): one repeat, not a burst to catch up.
        repeat.next = (repeat.next + interval).max(now + interval / 2);
        let code = repeat.code;
        // With the modifiers as they are now, without Compose: like a press
        // of the same key again.
        let (_, state) = self.xkb.as_ref()?;
        Some(Input::Key(Key {
            sym: state.key_get_one_sym(code),
            text: typed(state.key_get_utf8(code)),
            mods: self.mods,
            state: KeyState::Repeated,
        }))
    }

    /// Handles one wl_keyboard event; what it means for the app goes to
    /// `out`.
    pub(crate) fn event(&mut self, event: wl_keyboard::Event, out: &mut Vec<Input>) {
        use wl_keyboard::Event;
        match event {
            Event::Keymap { format, fd, size } => self.keymap(format, fd, size),
            Event::Enter { .. } => out.push(Input::Focus(true)),
            Event::Leave { .. } => {
                self.repeat = None;
                if let Some(compose) = &mut self.compose {
                    compose.reset();
                }
                out.push(Input::Focus(false));
            }
            Event::Modifiers {
                mods_depressed,
                mods_latched,
                mods_locked,
                group,
                ..
            } => {
                if let Some((_, state)) = &mut self.xkb {
                    state.update_mask(mods_depressed, mods_latched, mods_locked, 0, 0, group);
                    let active = |name| state.mod_name_is_active(name, xkb::STATE_MODS_EFFECTIVE);
                    self.mods = Modifiers {
                        shift: active(xkb::MOD_NAME_SHIFT),
                        ctrl: active(xkb::MOD_NAME_CTRL),
                        alt: active(xkb::MOD_NAME_ALT),
                        logo: active(xkb::MOD_NAME_LOGO),
                    };
                }
            }
            Event::Key { key, state, .. } => {
                let code = Keycode::new(key + XKB_OFFSET);
                let pressed = matches!(state, WEnum::Value(wl_keyboard::KeyState::Pressed));
                if let Some(key) = self.key(code, pressed) {
                    out.push(Input::Key(key));
                }
            }
            Event::RepeatInfo { rate, delay } => {
                self.rate = (rate > 0).then(|| {
                    (
                        Duration::from_millis(delay.max(0) as u64),
                        Duration::from_secs(1) / rate as u32,
                    )
                });
                if self.rate.is_none() {
                    self.repeat = None;
                }
            }
            _ => {}
        }
    }

    fn keymap(&mut self, format: WEnum<wl_keyboard::KeymapFormat>, fd: OwnedFd, size: u32) {
        self.xkb = None;
        self.repeat = None;
        if format != WEnum::Value(wl_keyboard::KeymapFormat::XkbV1) {
            warn!("the compositor sent a keymap that is not XKB: no keys");
            return;
        }
        // SAFETY: the fd is the compositor's keymap, `size` bytes, sent for
        // exactly this (wl_keyboard.keymap); xkbcommon maps it privately and
        // read-only, compiles it and unmaps it before returning.
        let keymap = unsafe {
            xkb::Keymap::new_from_fd(
                &self.context,
                fd,
                size as usize,
                xkb::KEYMAP_FORMAT_TEXT_V1,
                xkb::COMPILE_NO_FLAGS,
            )
        };
        match keymap {
            Ok(Some(keymap)) => {
                let state = xkb::State::new(&keymap);
                self.xkb = Some((keymap, state));
            }
            Ok(None) => warn!("the compositor's keymap does not compile: no keys"),
            Err(e) => warn!("reading the compositor's keymap: {e}"),
        }
    }

    fn key(&mut self, code: Keycode, pressed: bool) -> Option<Key> {
        let (keymap, state) = self.xkb.as_ref()?;
        let sym = state.key_get_one_sym(code);
        if !pressed {
            if self.repeat.as_ref().is_some_and(|r| r.code == code) {
                self.repeat = None;
            }
            return Some(Key {
                sym,
                text: None,
                mods: self.mods,
                state: KeyState::Released,
            });
        }
        let compose = self.compose.as_mut().and_then(|c| match c.feed(sym) {
            compose::FeedResult::Accepted => Some(c),
            // Modifiers pass Compose by.
            compose::FeedResult::Ignored => None,
        });
        let (sym, text) = match compose.map(|c| (c.status(), c)) {
            Some((compose::Status::Composing, _)) => (sym, None),
            Some((compose::Status::Composed, c)) => {
                let composed = (c.keysym().unwrap_or(sym), c.utf8());
                c.reset();
                composed
            }
            Some((compose::Status::Cancelled, c)) => {
                c.reset();
                (sym, None)
            }
            Some((compose::Status::Nothing, _)) | None => (sym, typed(state.key_get_utf8(code))),
        };
        // A key that does not repeat (a modifier) leaves the held one
        // repeating.
        if keymap.key_repeats(code) {
            self.repeat = self.rate.map(|(delay, _)| Repeat {
                code,
                next: Instant::now() + delay,
            });
        }
        Some(Key {
            sym,
            text,
            mods: self.mods,
            state: KeyState::Pressed,
        })
    }
}

fn typed(text: String) -> Option<String> {
    (!text.is_empty()).then_some(text)
}

/// The Compose table of the user's locale (LC_ALL, LC_CTYPE, LANG, as
/// setlocale(3) looks); Beize has only UTF-8 locales and only their
/// tables (recipes/libx11-compose.toml), so without a locale, or with one
/// that has none, C.UTF-8's.
fn compose_table(context: &xkb::Context) -> Option<compose::Table> {
    let locale = ["LC_ALL", "LC_CTYPE", "LANG"]
        .into_iter()
        .filter_map(std::env::var_os)
        .find(|v| !v.is_empty());
    let fallback = OsString::from("C.UTF-8");
    for locale in locale.iter().chain([&fallback]) {
        match compose::Table::new_from_locale(context, locale, compose::COMPILE_NO_FLAGS) {
            Ok(table) => {
                debug!("Compose table of {locale:?}");
                return Some(table);
            }
            Err(()) => debug!("no Compose table for {locale:?}"),
        }
    }
    warn!("no Compose table: dead keys type nothing");
    None
}

/// The pointer; scrolling is summed up to the end of each pointer frame.
#[derive(Default)]
pub(crate) struct PointerState {
    scroll: Option<[f64; 4]>,
}

impl PointerState {
    pub(crate) fn event(&mut self, event: wl_pointer::Event, out: &mut Vec<Input>) {
        use wl_pointer::{Axis, Event};
        let pointer = match event {
            Event::Enter {
                surface_x,
                surface_y,
                ..
            } => Pointer::Enter(surface_x, surface_y),
            Event::Leave { .. } => Pointer::Leave,
            Event::Motion {
                surface_x,
                surface_y,
                ..
            } => Pointer::Motion(surface_x, surface_y),
            Event::Button { button, state, .. } => Pointer::Button {
                button: match button {
                    BTN_LEFT => Button::Left,
                    BTN_RIGHT => Button::Right,
                    BTN_MIDDLE => Button::Middle,
                    other => Button::Other(other),
                },
                pressed: matches!(state, WEnum::Value(wl_pointer::ButtonState::Pressed)),
            },
            Event::Axis { axis, value, .. } => {
                let scroll = self.scroll.get_or_insert_default();
                match axis {
                    WEnum::Value(Axis::HorizontalScroll) => scroll[0] += value,
                    WEnum::Value(Axis::VerticalScroll) => scroll[1] += value,
                    _ => {}
                }
                return;
            }
            Event::AxisValue120 { axis, value120 } => {
                let scroll = self.scroll.get_or_insert_default();
                match axis {
                    WEnum::Value(Axis::HorizontalScroll) => scroll[2] += value120 as f64 / NOTCH,
                    WEnum::Value(Axis::VerticalScroll) => scroll[3] += value120 as f64 / NOTCH,
                    _ => {}
                }
                return;
            }
            Event::Frame => match self.scroll.take() {
                Some([x, y, steps_x, steps_y]) => Pointer::Scroll {
                    x,
                    y,
                    steps_x,
                    steps_y,
                },
                None => return,
            },
            _ => return,
        };
        out.push(Input::Pointer(pointer));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Evdev key codes (linux/input-event-codes.h).
    const KEY_GRAVE: u32 = 41;
    const KEY_E: u32 = 18;
    const KEY_Y: u32 = 21;
    const KEY_A: u32 = 30;
    const KEY_C: u32 = 46;
    const KEY_LEFTSHIFT: u32 = 42;

    /// A keyboard with the German layout (dead circumflex left of 1, Z
    /// where US has Y), sent as the compositor sends it, and one Compose
    /// sequence.
    fn german() -> (Keyboard, xkb::Keymap) {
        let context = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
        let table = compose::Table::new_from_buffer(
            &context,
            "<dead_circumflex> <e> : \"ê\" ecircumflex\n",
            "C",
            compose::FORMAT_TEXT_V1,
            compose::COMPILE_NO_FLAGS,
        )
        .ok();
        assert!(table.is_some());
        let keymap = xkb::Keymap::new_from_names(
            &context,
            "evdev",
            "pc105",
            "de",
            "",
            None,
            xkb::COMPILE_NO_FLAGS,
        )
        .unwrap();
        let mut text = keymap
            .get_as_string(xkb::KEYMAP_FORMAT_TEXT_V1)
            .into_bytes();
        // Wayland sends it with its terminating zero.
        text.push(0);
        // One file per test: they run in parallel.
        static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let name = format!("beize-ui-keymap-{}-{n}", std::process::id());
        let path = std::env::temp_dir().join(name);
        std::fs::write(&path, &text).unwrap();
        let fd = OwnedFd::from(std::fs::File::open(&path).unwrap());
        std::fs::remove_file(&path).unwrap();
        let mut keyboard = Keyboard::with(context, table);
        let keymap_event = wl_keyboard::Event::Keymap {
            format: WEnum::Value(wl_keyboard::KeymapFormat::XkbV1),
            fd,
            size: text.len() as u32,
        };
        keyboard.event(keymap_event, &mut Vec::new());
        assert!(keyboard.xkb.is_some());
        (keyboard, keymap)
    }

    fn mods(keyboard: &mut Keyboard, keymap: &xkb::Keymap, names: &[&str]) {
        let depressed = names
            .iter()
            .map(|n| 1 << keymap.mod_get_index(*n))
            .fold(0, |a, b| a | b);
        let event = wl_keyboard::Event::Modifiers {
            serial: 0,
            mods_depressed: depressed,
            mods_latched: 0,
            mods_locked: 0,
            group: 0,
        };
        keyboard.event(event, &mut Vec::new());
    }

    fn press(keyboard: &mut Keyboard, key: u32, pressed: bool) -> Vec<Input> {
        let state = if pressed {
            wl_keyboard::KeyState::Pressed
        } else {
            wl_keyboard::KeyState::Released
        };
        let mut out = Vec::new();
        let event = wl_keyboard::Event::Key {
            serial: 0,
            time: 0,
            key,
            state: WEnum::Value(state),
        };
        keyboard.event(event, &mut out);
        out
    }

    fn typed_by(input: &[Input]) -> Vec<Option<&str>> {
        input
            .iter()
            .map(|i| match i {
                Input::Key(k) => k.text.as_deref(),
                other => panic!("not a key: {other:?}"),
            })
            .collect()
    }

    #[test]
    fn keys_follow_the_layout_and_the_modifiers() {
        let (mut keyboard, keymap) = german();
        assert_eq!(typed_by(&press(&mut keyboard, KEY_Y, true)), [Some("z")]);
        mods(&mut keyboard, &keymap, &[xkb::MOD_NAME_SHIFT]);
        let shifted = press(&mut keyboard, KEY_Y, true);
        assert_eq!(typed_by(&shifted), [Some("Z")]);
        let Input::Key(key) = &shifted[0] else {
            unreachable!()
        };
        assert_eq!(key.sym, Keysym::Z);
        assert!(key.mods.shift && !key.mods.ctrl);
        // With Ctrl, what a terminal sends: ETX.
        mods(&mut keyboard, &keymap, &[xkb::MOD_NAME_CTRL]);
        assert_eq!(
            typed_by(&press(&mut keyboard, KEY_C, true)),
            [Some("\u{3}")]
        );
        // A release types nothing.
        assert_eq!(typed_by(&press(&mut keyboard, KEY_C, false)), [None]);
    }

    #[test]
    fn a_dead_key_and_a_letter_make_one_character() {
        let (mut keyboard, _) = german();
        assert_eq!(typed_by(&press(&mut keyboard, KEY_GRAVE, true)), [None]);
        let composed = press(&mut keyboard, KEY_E, true);
        assert_eq!(typed_by(&composed), [Some("ê")]);
        let Input::Key(key) = &composed[0] else {
            unreachable!()
        };
        assert_eq!(key.sym, Keysym::ecircumflex);
        // Done: the next e is an e.
        assert_eq!(typed_by(&press(&mut keyboard, KEY_E, true)), [Some("e")]);
    }

    #[test]
    fn a_held_key_repeats_at_the_compositors_rate() {
        let (mut keyboard, _) = german();
        let info = wl_keyboard::Event::RepeatInfo {
            rate: 25,
            delay: 400,
        };
        keyboard.event(info, &mut Vec::new());
        let pressed = Instant::now();
        press(&mut keyboard, KEY_A, true);
        let next = keyboard.next_repeat().unwrap();
        assert!(next >= pressed + Duration::from_millis(400));
        assert_eq!(keyboard.repeat_due(next - Duration::from_millis(1)), None);
        let repeated = keyboard.repeat_due(next).unwrap();
        let Input::Key(key) = &repeated else {
            unreachable!()
        };
        assert_eq!(
            (key.text.as_deref(), key.state),
            (Some("a"), KeyState::Repeated)
        );
        assert_eq!(
            keyboard.next_repeat(),
            Some(next + Duration::from_millis(40))
        );
        // Shift going down does not stop it; letting go of the key does.
        press(&mut keyboard, KEY_LEFTSHIFT, true);
        assert!(keyboard.next_repeat().is_some());
        press(&mut keyboard, KEY_A, false);
        assert_eq!(keyboard.next_repeat(), None);
        // Rate 0: the compositor turned repeating off.
        let off = wl_keyboard::Event::RepeatInfo { rate: 0, delay: 0 };
        keyboard.event(off, &mut Vec::new());
        press(&mut keyboard, KEY_A, true);
        assert_eq!(keyboard.next_repeat(), None);
    }

    #[test]
    fn scrolling_comes_once_per_pointer_frame() {
        let mut pointer = PointerState::default();
        let mut out = Vec::new();
        let vertical = WEnum::Value(wl_pointer::Axis::VerticalScroll);
        pointer.event(
            wl_pointer::Event::Axis {
                time: 0,
                axis: vertical,
                value: 15.0,
            },
            &mut out,
        );
        pointer.event(
            wl_pointer::Event::AxisValue120 {
                axis: vertical,
                value120: 60,
            },
            &mut out,
        );
        assert!(out.is_empty());
        pointer.event(wl_pointer::Event::Frame, &mut out);
        pointer.event(wl_pointer::Event::Frame, &mut out);
        assert_eq!(
            out,
            [Input::Pointer(Pointer::Scroll {
                x: 0.0,
                y: 15.0,
                steps_x: 0.0,
                steps_y: 0.5,
            })]
        );
    }
}
