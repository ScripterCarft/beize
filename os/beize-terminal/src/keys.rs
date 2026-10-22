//! Keys to bytes for the shell: what xterm sends, as Alacritty and VTE do
//! by default. Cursor keys and Home/End follow the cursor key mode
//! (DECCKM); modifiers go into the sequence as xterm's parameter
//! (1 + Shift 1 + Alt 2 + Ctrl 4); Alt in front of a character is ESC.

use beize_ui::{Key, KeyState, Keysym, Modifiers};

/// The bytes `key` sends; None for keys that send nothing (modifiers,
/// releases).
pub fn bytes(key: &Key, app_cursor: bool) -> Option<Vec<u8>> {
    if key.state == KeyState::Released {
        return None;
    }
    let mods = key.mods;
    let csi = |end: &str| sequence(mods, end, app_cursor);
    let tilde = |n: u8| -> Vec<u8> {
        match parameter(mods) {
            1 => format!("\x1b[{n}~"),
            m => format!("\x1b[{n};{m}~"),
        }
        .into_bytes()
    };
    let bytes = match key.sym {
        Keysym::Up => csi("A"),
        Keysym::Down => csi("B"),
        Keysym::Right => csi("C"),
        Keysym::Left => csi("D"),
        Keysym::Home => csi("H"),
        Keysym::End => csi("F"),
        Keysym::Insert => tilde(2),
        Keysym::Delete => tilde(3),
        Keysym::Page_Up => tilde(5),
        Keysym::Page_Down => tilde(6),
        // F1–F4 as SS3 (no modifiers) or CSI with them, as xterm.
        Keysym::F1 => function(mods, "P"),
        Keysym::F2 => function(mods, "Q"),
        Keysym::F3 => function(mods, "R"),
        Keysym::F4 => function(mods, "S"),
        Keysym::F5 => tilde(15),
        Keysym::F6 => tilde(17),
        Keysym::F7 => tilde(18),
        Keysym::F8 => tilde(19),
        Keysym::F9 => tilde(20),
        Keysym::F10 => tilde(21),
        Keysym::F11 => tilde(23),
        Keysym::F12 => tilde(24),
        Keysym::BackSpace if mods.ctrl => alt(mods, b"\x08"),
        Keysym::BackSpace => alt(mods, b"\x7f"),
        Keysym::Return | Keysym::KP_Enter => alt(mods, b"\r"),
        Keysym::Tab if mods.shift => b"\x1b[Z".to_vec(),
        // Shift+Tab arrives as ISO_Left_Tab in most layouts.
        Keysym::ISO_Left_Tab => b"\x1b[Z".to_vec(),
        Keysym::Tab => alt(mods, b"\t"),
        Keysym::Escape => alt(mods, b"\x1b"),
        // Ctrl+Space is NUL; XKB gives no text for it.
        Keysym::space if mods.ctrl => alt(mods, b"\0"),
        _ => alt(mods, key.text.as_deref()?.as_bytes()),
    };
    Some(bytes)
}

/// xterm's modifier parameter.
fn parameter(mods: Modifiers) -> u8 {
    1 + u8::from(mods.shift) + 2 * u8::from(mods.alt) + 4 * u8::from(mods.ctrl)
}

/// A cursor key or Home/End: SS3 in application mode without modifiers,
/// else CSI, with the modifiers as `1;m`.
fn sequence(mods: Modifiers, end: &str, app_cursor: bool) -> Vec<u8> {
    match parameter(mods) {
        1 if app_cursor => format!("\x1bO{end}"),
        1 => format!("\x1b[{end}"),
        m => format!("\x1b[1;{m}{end}"),
    }
    .into_bytes()
}

fn function(mods: Modifiers, end: &str) -> Vec<u8> {
    match parameter(mods) {
        1 => format!("\x1bO{end}"),
        m => format!("\x1b[1;{m}{end}"),
    }
    .into_bytes()
}

/// `bytes` with ESC in front if Alt is held (xterm's metaSendsEscape).
fn alt(mods: Modifiers, bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len() + 1);
    if mods.alt {
        out.push(0x1b);
    }
    out.extend_from_slice(bytes);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(sym: Keysym, text: Option<&str>, mods: Modifiers) -> Key {
        Key {
            sym,
            text: text.map(String::from),
            mods,
            state: KeyState::Pressed,
        }
    }

    const NONE: Modifiers = Modifiers {
        shift: false,
        ctrl: false,
        alt: false,
        logo: false,
    };
    const CTRL: Modifiers = Modifiers { ctrl: true, ..NONE };
    const ALT: Modifiers = Modifiers { alt: true, ..NONE };
    const SHIFT: Modifiers = Modifiers {
        shift: true,
        ..NONE
    };

    fn sent(sym: Keysym, text: Option<&str>, mods: Modifiers, app: bool) -> Option<String> {
        bytes(&key(sym, text, mods), app).map(|b| String::from_utf8(b).unwrap())
    }

    #[test]
    fn characters_go_as_typed() {
        assert_eq!(sent(Keysym::a, Some("a"), NONE, false).unwrap(), "a");
        assert_eq!(
            sent(Keysym::ecircumflex, Some("ê"), NONE, false).unwrap(),
            "ê"
        );
        // Ctrl+C: the control character XKB made.
        assert_eq!(
            sent(Keysym::c, Some("\u{3}"), CTRL, false).unwrap(),
            "\u{3}"
        );
        assert_eq!(sent(Keysym::b, Some("b"), ALT, false).unwrap(), "\x1bb");
        assert_eq!(sent(Keysym::Shift_L, None, SHIFT, false), None);
        let released = Key {
            state: KeyState::Released,
            ..key(Keysym::a, Some("a"), NONE)
        };
        assert_eq!(bytes(&released, false), None);
    }

    #[test]
    fn cursor_keys_follow_the_mode_and_the_modifiers() {
        assert_eq!(sent(Keysym::Up, None, NONE, false).unwrap(), "\x1b[A");
        assert_eq!(sent(Keysym::Up, None, NONE, true).unwrap(), "\x1bOA");
        assert_eq!(sent(Keysym::Left, None, CTRL, true).unwrap(), "\x1b[1;5D");
        assert_eq!(sent(Keysym::End, None, SHIFT, false).unwrap(), "\x1b[1;2F");
        assert_eq!(sent(Keysym::Delete, None, NONE, false).unwrap(), "\x1b[3~");
        assert_eq!(
            sent(Keysym::Page_Up, None, CTRL, false).unwrap(),
            "\x1b[5;5~"
        );
        assert_eq!(sent(Keysym::F1, None, NONE, false).unwrap(), "\x1bOP");
        assert_eq!(sent(Keysym::F5, None, ALT, false).unwrap(), "\x1b[15;3~");
    }

    #[test]
    fn editing_keys_send_what_xterm_does() {
        assert_eq!(sent(Keysym::BackSpace, None, NONE, false).unwrap(), "\x7f");
        assert_eq!(sent(Keysym::BackSpace, None, CTRL, false).unwrap(), "\x08");
        assert_eq!(sent(Keysym::Return, Some("\r"), NONE, false).unwrap(), "\r");
        assert_eq!(
            sent(Keysym::ISO_Left_Tab, None, SHIFT, false).unwrap(),
            "\x1b[Z"
        );
        assert_eq!(sent(Keysym::space, None, CTRL, false).unwrap(), "\0");
    }
}
