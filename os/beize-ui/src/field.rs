//! A text field (ADR 0031), for now the one a password goes into: dots in
//! place of the characters, macOS' secure field. Typing adds at the end,
//! Backspace takes the last character; pasting works, copying does not, and
//! nothing shows the text (macOS). A field that asks openly (PAM's
//! `ECHO_ON`) shows what is typed.
//!
//! What is typed lies in one buffer of fixed size, PAM's longest answer
//! (`PAM_MAX_RESP_SIZE`, 512 bytes): it never grows, so no copy of it stays
//! behind in memory a larger one replaced, and it is overwritten when taken
//! or dropped (`zeroize`, RustCrypto's). Input beyond it is refused, as PAM
//! would cut it.

use beize_draw::{Color, Family, Rect, Scene, Style, Text};
use zeroize::{Zeroize, Zeroizing};

use crate::Appearance;
use crate::input::{Key, KeyState, Keysym};
use crate::theme::{self, ACCENT, TEXT_SIZE};

/// PAM's longest answer, Linux-PAM's `PAM_MAX_RESP_SIZE`.
pub const MAX_BYTES: usize = 512;

/// The field's measures, from the mock-up the owner approved (15.11.): as
/// high as a button, its text 12 in, the caret 2 wide and 9 short of its
/// top and bottom; the Caps Lock symbol 17 px, 10 from its right.
pub const HEIGHT: f32 = 35.0;
pub const RADIUS: f32 = 10.0;
const TEXT_INSET: f32 = 12.0;
const CARET_WIDTH: f32 = 2.0;
const CARET_INSET: f32 = 9.0;
const CAPS_LOCK: u32 = 17;
const CAPS_LOCK_INSET: f32 = 10.0;
/// The outline while it has the keyboard, as a picked button's.
const OUTLINE_SPACE: f32 = 2.0;
const OUTLINE_WIDTH: f32 = 2.0;
/// Its fill and rim: white at 78 % and black at 12 % on light glass, white
/// at 9 % and 16 % on dark (the mock-up's).
const FILL_LIGHT: Color = theme::faded(Color::rgb(0xffffff), 200);
const RIM_LIGHT: Color = theme::faded(Color::rgb(0x000000), 30);
const FILL_DARK: Color = theme::faded(Color::rgb(0xffffff), 22);
const RIM_DARK: Color = theme::faded(Color::rgb(0xffffff), 40);
/// The placeholder and the Caps Lock symbol: the ink at 43 %.
const HINT: u8 = 110;
/// What stands for each character typed.
const DOT: char = '•';

/// What a key did to the field.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Edit {
    None,
    /// Its text changed: draw again.
    Changed,
    /// Ctrl+V: the app asks for the clipboard and hands it to `paste`.
    Paste,
}

pub struct Field {
    text: Zeroizing<String>,
    /// It shows dots, not the text.
    secret: bool,
    placeholder: String,
}

impl Field {
    /// A password's: dots, "Password" while empty.
    pub fn password() -> Field {
        Field::with(true, "Password")
    }

    /// One that shows what is typed, with `placeholder` while empty.
    pub fn plain(placeholder: impl Into<String>) -> Field {
        Field::with(false, placeholder)
    }

    fn with(secret: bool, placeholder: impl Into<String>) -> Field {
        Field {
            text: Zeroizing::new(String::with_capacity(MAX_BYTES)),
            secret,
            placeholder: placeholder.into(),
        }
    }

    /// What PAM asks for: shown as the placeholder ("Password: " is
    /// "Password"), the text open when it asks openly (`echo`).
    pub fn set_prompt(&mut self, prompt: &str, echo: bool) {
        let name = prompt.trim().trim_end_matches(':').trim_end();
        self.placeholder = if name.is_empty() { "Password" } else { name }.to_owned();
        self.secret = !echo;
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// What a key does: a character it types goes to the end, Backspace
    /// takes the last one. A press or its repeat; no control characters.
    pub fn key(&mut self, key: &Key) -> Edit {
        if key.state == KeyState::Released {
            return Edit::None;
        }
        if key.mods.ctrl && !key.mods.alt && matches!(key.sym, Keysym::v | Keysym::V) {
            return if key.state == KeyState::Pressed {
                Edit::Paste
            } else {
                Edit::None
            };
        }
        if key.sym == Keysym::BackSpace {
            return if self.text.pop().is_some() {
                Edit::Changed
            } else {
                Edit::None
            };
        }
        match &key.text {
            Some(typed) if !key.mods.ctrl => self.add(typed),
            _ => Edit::None,
        }
    }

    /// Text from the clipboard, at the end, up to its first line break (a
    /// field holds one line).
    pub fn paste(&mut self, text: &str) -> Edit {
        self.add(text.split(['\n', '\r']).next().unwrap_or(""))
    }

    fn add(&mut self, typed: &str) -> Edit {
        if typed.is_empty() || typed.chars().any(char::is_control) {
            return Edit::None;
        }
        if self.text.len() + typed.len() > MAX_BYTES {
            return Edit::None;
        }
        self.text.push_str(typed);
        Edit::Changed
    }

    /// What it holds, which it no longer does: overwritten when dropped.
    pub fn take(&mut self) -> Zeroizing<String> {
        let mut taken = Zeroizing::new(String::with_capacity(MAX_BYTES));
        taken.push_str(&self.text);
        self.clear();
        taken
    }

    /// Empty, the bytes it held overwritten.
    pub fn clear(&mut self) {
        self.text.zeroize();
        // `zeroize` truncates the string and overwrites its spare capacity
        // too, and keeps the allocation.
        debug_assert!(self.text.capacity() >= MAX_BYTES);
    }

    /// Drawn into `scene` at `rect`: the outline while it has the keyboard
    /// (`focused`), the caret too, the Caps Lock symbol while it is on.
    pub fn draw(
        &self,
        scene: &mut Scene,
        text: &mut Text,
        rect: Rect,
        appearance: Appearance,
        focused: bool,
        caps_lock: bool,
    ) {
        let ink = theme::ink(appearance);
        let hint = theme::faded(ink, HINT);
        let (fill, rim) = match appearance {
            Appearance::Light => (FILL_LIGHT, RIM_LIGHT),
            Appearance::Dark => (FILL_DARK, RIM_DARK),
        };
        if focused {
            let out = OUTLINE_SPACE + OUTLINE_WIDTH;
            let around = Rect {
                x: rect.x - out,
                y: rect.y - out,
                w: rect.w + 2.0 * out,
                h: rect.h + 2.0 * out,
            };
            scene.ring(
                around,
                RADIUS + out,
                OUTLINE_WIDTH,
                theme::faded(ACCENT, 230),
            );
        }
        scene.rounded(rect, RADIUS, fill);
        scene.ring(rect, RADIUS, 1.0, rim);
        let style = |color| Style {
            family: Family::Sans,
            size: TEXT_SIZE,
            weight: 400.0,
            color,
        };
        let x = rect.x + TEXT_INSET;
        let mut caret = x;
        if self.text.is_empty() {
            let layout = text.layout(&self.placeholder, &style(hint), None);
            let y = (rect.y + (rect.h - layout.height()) / 2.0).round();
            scene.text(&layout, x + CARET_WIDTH, y);
        } else {
            let shown: Zeroizing<String> = Zeroizing::new(if self.secret {
                std::iter::repeat_n(DOT, self.text.chars().count()).collect()
            } else {
                self.text.as_str().to_owned()
            });
            let layout = text.layout(&shown, &style(ink), None);
            let y = (rect.y + (rect.h - layout.height()) / 2.0).round();
            scene.text(&layout, x, y);
            caret = (x + layout.width() + 1.0).round();
        }
        if focused {
            scene.fill(
                Rect {
                    x: caret,
                    y: rect.y + CARET_INSET,
                    w: CARET_WIDTH,
                    h: rect.h - 2.0 * CARET_INSET,
                },
                ACCENT,
            );
        }
        if caps_lock {
            let size = CAPS_LOCK as f32;
            scene.symbol(
                "arrow-fat-line-up",
                rect.x + rect.w - CAPS_LOCK_INSET - size,
                (rect.y + (rect.h - size) / 2.0).round(),
                CAPS_LOCK,
                hint,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::Modifiers;

    fn key(sym: Keysym, text: Option<&str>, ctrl: bool) -> Key {
        Key {
            sym,
            text: text.map(str::to_owned),
            mods: Modifiers {
                ctrl,
                ..Modifiers::default()
            },
            state: KeyState::Pressed,
        }
    }

    #[test]
    fn typing_adds_and_backspace_takes_the_last_character() {
        let mut field = Field::password();
        assert_eq!(field.key(&key(Keysym::a, Some("a"), false)), Edit::Changed);
        assert_eq!(
            field.key(&key(Keysym::adiaeresis, Some("ä"), false)),
            Edit::Changed
        );
        assert_eq!(
            field.key(&key(Keysym::BackSpace, None, false)),
            Edit::Changed
        );
        assert_eq!(field.take().as_str(), "a");
        assert!(field.is_empty());
        assert_eq!(field.key(&key(Keysym::BackSpace, None, false)), Edit::None);
    }

    #[test]
    fn control_characters_and_ctrl_keys_type_nothing() {
        let mut field = Field::password();
        assert_eq!(field.key(&key(Keysym::c, Some("\u{3}"), true)), Edit::None);
        assert_eq!(
            field.key(&key(Keysym::Return, Some("\r"), false)),
            Edit::None
        );
        assert_eq!(field.key(&key(Keysym::Tab, Some("\t"), false)), Edit::None);
        assert!(field.is_empty());
        assert_eq!(
            field.key(&key(Keysym::v, Some("\u{16}"), true)),
            Edit::Paste
        );
    }

    #[test]
    fn the_prompt_names_the_field() {
        let mut field = Field::password();
        field.set_prompt("Password: ", false);
        assert_eq!(
            (field.placeholder.as_str(), field.secret),
            ("Password", true)
        );
        field.set_prompt("Verification code:", true);
        assert_eq!(
            (field.placeholder.as_str(), field.secret),
            ("Verification code", false)
        );
        field.set_prompt(" ", false);
        assert_eq!(field.placeholder, "Password");
    }

    #[test]
    fn a_paste_takes_its_first_line() {
        let mut field = Field::password();
        assert_eq!(field.paste("secret\nmore"), Edit::Changed);
        assert_eq!(field.take().as_str(), "secret");
    }

    #[test]
    fn it_holds_no_more_than_pam_takes_and_never_grows() {
        let mut field = Field::password();
        let capacity = field.text.capacity();
        assert_eq!(field.paste(&"x".repeat(MAX_BYTES)), Edit::Changed);
        assert_eq!(field.key(&key(Keysym::a, Some("a"), false)), Edit::None);
        assert_eq!(field.text.capacity(), capacity);
        assert_eq!(field.take().len(), MAX_BYTES);
    }

    #[test]
    fn clearing_overwrites_what_it_held() {
        let mut field = Field::password();
        field.paste("secret");
        let bytes = field.text.as_ptr();
        field.clear();
        // SAFETY: the allocation is still the field's (`zeroize` keeps it,
        // asserted in `clear`), and its first bytes were initialized.
        let held = unsafe { std::slice::from_raw_parts(bytes, 6) };
        assert_eq!(held, [0; 6]);
    }
}
