//! Keyboard input: GDK key events to multiplexer keys, and keys to the
//! byte sequences xterm sends (xterm ctlseqs, "PC-Style Function Keys").

use gtk::gdk;
use mux::{Key, KeyCode, Mods};

/// Converts a key press. Returns the key and, for characters typed without
/// Ctrl or Alt, the text it produced.
pub fn from_gdk(keyval: gdk::Key, state: gdk::ModifierType) -> Option<(Key, Option<String>)> {
    let mods = Mods {
        ctrl: state.contains(gdk::ModifierType::CONTROL_MASK),
        meta: state.contains(gdk::ModifierType::ALT_MASK),
        shift: state.contains(gdk::ModifierType::SHIFT_MASK),
    };
    let plain = !mods.ctrl && !mods.meta;
    use gdk::Key as K;
    let code = match keyval {
        K::Return | K::KP_Enter => KeyCode::Enter,
        K::Escape => KeyCode::Escape,
        K::Tab | K::ISO_Left_Tab => KeyCode::Tab,
        K::BackSpace => KeyCode::Backspace,
        K::Up | K::KP_Up => KeyCode::Up,
        K::Down | K::KP_Down => KeyCode::Down,
        K::Left | K::KP_Left => KeyCode::Left,
        K::Right | K::KP_Right => KeyCode::Right,
        K::Home | K::KP_Home => KeyCode::Home,
        K::End | K::KP_End => KeyCode::End,
        K::Page_Up | K::KP_Page_Up => KeyCode::PageUp,
        K::Page_Down | K::KP_Page_Down => KeyCode::PageDown,
        K::Insert | K::KP_Insert => KeyCode::Insert,
        K::Delete | K::KP_Delete => KeyCode::Delete,
        K::F1 => KeyCode::F(1),
        K::F2 => KeyCode::F(2),
        K::F3 => KeyCode::F(3),
        K::F4 => KeyCode::F(4),
        K::F5 => KeyCode::F(5),
        K::F6 => KeyCode::F(6),
        K::F7 => KeyCode::F(7),
        K::F8 => KeyCode::F(8),
        K::F9 => KeyCode::F(9),
        K::F10 => KeyCode::F(10),
        K::F11 => KeyCode::F(11),
        K::F12 => KeyCode::F(12),
        _ => {
            // GDK gives the keyval with Shift already applied (`|`, `H`).
            let ch = keyval.to_unicode()?;
            if ch.is_control() {
                return None;
            }
            let text = plain.then(|| ch.to_string());
            return Some((Key { code: KeyCode::Char(ch), mods: Mods { shift: false, ..mods } }, text));
        }
    };
    Some((Key { code, mods }, None))
}

fn control_byte(ch: char) -> Option<u8> {
    Some(match ch {
        'a'..='z' => ch as u8 - b'a' + 1,
        'A'..='Z' => ch as u8 - b'A' + 1,
        '@' | ' ' | '2' => 0,
        '[' | '3' => 0x1b,
        '\\' | '4' => 0x1c,
        ']' | '5' => 0x1d,
        '^' | '6' => 0x1e,
        '_' | '7' | '/' | '-' => 0x1f,
        '8' | '?' => 0x7f,
        _ => return None,
    })
}

/// Appends the bytes xterm sends for `key`; `text` is the typed text for
/// plain characters.
pub fn encode(key: Key, text: Option<&str>, app_cursor: bool, out: &mut Vec<u8>) {
    let mods = key.mods;
    let modified = mods.shift || mods.meta || mods.ctrl;
    // xterm's modifier parameter: 1 + Shift 1 + Alt 2 + Control 4.
    let m = 1 + mods.shift as u8 + 2 * mods.meta as u8 + 4 * mods.ctrl as u8;
    // Cursor and Home/End keys: CSI x, SS3 x in DECCKM, CSI 1;m x when modified.
    let cursor = |out: &mut Vec<u8>, final_byte: u8| {
        if modified {
            out.extend_from_slice(format!("\x1b[1;{m}").as_bytes());
        } else {
            out.extend_from_slice(if app_cursor { b"\x1bO" } else { b"\x1b[" });
        }
        out.push(final_byte);
    };
    // Editing and F5-F12 keys: CSI n ~, CSI n;m ~ when modified.
    let tilde = |out: &mut Vec<u8>, n: u8| {
        out.extend_from_slice(format!("\x1b[{n}").as_bytes());
        if modified {
            out.extend_from_slice(format!(";{m}").as_bytes());
        }
        out.push(b'~');
    };
    let meta = |out: &mut Vec<u8>| {
        if mods.meta {
            out.push(0x1b);
        }
    };
    match key.code {
        KeyCode::Char(ch) => {
            meta(out);
            match (mods.ctrl, control_byte(ch), text) {
                (true, Some(b), _) => out.push(b),
                (false, _, Some(text)) => out.extend_from_slice(text.as_bytes()),
                _ => out.extend_from_slice(ch.encode_utf8(&mut [0; 4]).as_bytes()),
            }
        }
        KeyCode::Enter => {
            meta(out);
            out.push(b'\r');
        }
        KeyCode::Escape => {
            meta(out);
            out.push(0x1b);
        }
        KeyCode::Tab if mods.shift => out.extend_from_slice(b"\x1b[Z"),
        KeyCode::Tab => {
            meta(out);
            out.push(b'\t');
        }
        KeyCode::Backspace => {
            meta(out);
            out.push(if mods.ctrl { 0x08 } else { 0x7f });
        }
        KeyCode::Up => cursor(out, b'A'),
        KeyCode::Down => cursor(out, b'B'),
        KeyCode::Right => cursor(out, b'C'),
        KeyCode::Left => cursor(out, b'D'),
        KeyCode::Home => cursor(out, b'H'),
        KeyCode::End => cursor(out, b'F'),
        KeyCode::Insert => tilde(out, 2),
        KeyCode::Delete => tilde(out, 3),
        KeyCode::PageUp => tilde(out, 5),
        KeyCode::PageDown => tilde(out, 6),
        KeyCode::F(n @ 1..=4) => {
            out.extend_from_slice(if modified { format!("\x1b[1;{m}") } else { "\x1bO".to_owned() }.as_bytes());
            out.push(b"PQRS"[n as usize - 1]);
        }
        KeyCode::F(n @ 5..=12) => tilde(out, [15, 17, 18, 19, 20, 21, 23, 24][n as usize - 5]),
        KeyCode::F(_) => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NONE: Mods = Mods { ctrl: false, meta: false, shift: false };
    const CTRL: Mods = Mods { ctrl: true, meta: false, shift: false };
    const ALT: Mods = Mods { ctrl: false, meta: true, shift: false };
    const SHIFT: Mods = Mods { ctrl: false, meta: false, shift: true };

    fn enc(code: KeyCode, mods: Mods, app_cursor: bool) -> Vec<u8> {
        let mut out = Vec::new();
        encode(Key { code, mods }, None, app_cursor, &mut out);
        out
    }

    // Expected values follow xterm's ctlseqs tables (normal / application
    // cursor keys, PC-style function keys, modifier codes 2 Shift, 3 Alt, 5 Control).
    #[test]
    fn cursor_keys() {
        assert_eq!(enc(KeyCode::Up, NONE, false), b"\x1b[A");
        assert_eq!(enc(KeyCode::Up, NONE, true), b"\x1bOA");
        assert_eq!(enc(KeyCode::Left, CTRL, true), b"\x1b[1;5D");
        assert_eq!(enc(KeyCode::Right, ALT, false), b"\x1b[1;3C");
        assert_eq!(enc(KeyCode::Down, SHIFT, false), b"\x1b[1;2B");
        assert_eq!(enc(KeyCode::Home, NONE, false), b"\x1b[H");
        assert_eq!(enc(KeyCode::End, NONE, true), b"\x1bOF");
    }

    #[test]
    fn editing_and_function_keys() {
        assert_eq!(enc(KeyCode::Insert, NONE, false), b"\x1b[2~");
        assert_eq!(enc(KeyCode::Delete, NONE, false), b"\x1b[3~");
        assert_eq!(enc(KeyCode::PageUp, CTRL, false), b"\x1b[5;5~");
        assert_eq!(enc(KeyCode::F(1), NONE, false), b"\x1bOP");
        assert_eq!(enc(KeyCode::F(4), SHIFT, false), b"\x1b[1;2S");
        assert_eq!(enc(KeyCode::F(5), SHIFT, false), b"\x1b[15;2~");
        assert_eq!(enc(KeyCode::F(6), NONE, false), b"\x1b[17~");
        assert_eq!(enc(KeyCode::F(12), CTRL, false), b"\x1b[24;5~");
    }

    #[test]
    fn characters_with_modifiers() {
        assert_eq!(enc(KeyCode::Char('b'), CTRL, false), [0x02]);
        assert_eq!(enc(KeyCode::Char(' '), CTRL, false), [0x00]);
        assert_eq!(enc(KeyCode::Char('['), CTRL, false), [0x1b]);
        assert_eq!(enc(KeyCode::Char('x'), ALT, false), b"\x1bx");
        assert_eq!(enc(KeyCode::Char('c'), Mods { ctrl: true, meta: true, shift: false }, false), b"\x1b\x03");
        let mut out = Vec::new();
        encode(Key { code: KeyCode::Char('e'), mods: NONE }, Some("\u{e9}"), false, &mut out);
        assert_eq!(out, "\u{e9}".as_bytes());
    }

    #[test]
    fn special_keys() {
        assert_eq!(enc(KeyCode::Enter, NONE, false), b"\r");
        assert_eq!(enc(KeyCode::Backspace, NONE, false), [0x7f]);
        assert_eq!(enc(KeyCode::Backspace, CTRL, false), [0x08]);
        assert_eq!(enc(KeyCode::Tab, SHIFT, false), b"\x1b[Z");
        assert_eq!(enc(KeyCode::Escape, ALT, false), b"\x1b\x1b");
    }
}
