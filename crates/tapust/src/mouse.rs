//! Mouse reports for programs that ask for them (ghostty's
//! input/mouse_encode.zig).

use term::{MouseEncoding, MouseMode};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Button {
    Left,
    Middle,
    Right,
    /// Wheel up.
    Four,
    /// Wheel down.
    Five,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Press,
    Release,
    Motion,
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub struct Mods {
    pub shift: bool,
    pub alt: bool,
    pub ctrl: bool,
}

/// One mouse event over grid cell `cell` (zero-based), at `pixels` from the
/// grid's origin; `button` is none for motion without a pressed button.
#[derive(Clone, Copy)]
pub struct Event {
    pub button: Option<Button>,
    pub action: Action,
    pub mods: Mods,
    pub cell: (u16, u16),
    pub pixels: (i32, i32),
}

fn should_report(mode: MouseMode, event: &Event) -> bool {
    match mode {
        MouseMode::None => false,
        // X10 reports only presses of the left, middle and right buttons.
        MouseMode::X10 => {
            event.action == Action::Press && matches!(event.button, Some(Button::Left | Button::Middle | Button::Right))
        }
        MouseMode::Normal => event.action != Action::Motion,
        MouseMode::ButtonMotion => event.button.is_some(),
        MouseMode::AnyMotion => true,
    }
}

fn button_code(mode: MouseMode, encoding: MouseEncoding, event: &Event) -> u8 {
    let sgr = matches!(encoding, MouseEncoding::Sgr | MouseEncoding::SgrPixels);
    let mut code = match event.button {
        None => 3,
        // Only SGR tells which button was released.
        Some(_) if event.action == Action::Release && !sgr => 3,
        Some(Button::Left) => 0,
        Some(Button::Middle) => 1,
        Some(Button::Right) => 2,
        Some(Button::Four) => 64,
        Some(Button::Five) => 65,
    };
    if mode != MouseMode::X10 {
        let m = event.mods;
        code += 4 * u8::from(m.shift) + 8 * u8::from(m.alt) + 16 * u8::from(m.ctrl);
    }
    if event.action == Action::Motion {
        code += 32;
    }
    code
}

/// Appends the report of `event`, if `mode` reports it.
pub fn encode(out: &mut Vec<u8>, mode: MouseMode, encoding: MouseEncoding, event: Event) {
    if !should_report(mode, &event) {
        return;
    }
    let code = button_code(mode, encoding, &event);
    let (x, y) = (u32::from(event.cell.0), u32::from(event.cell.1));
    let end = if event.action == Action::Release { 'm' } else { 'M' };
    match encoding {
        MouseEncoding::Default => {
            // X10 coordinates are single bytes.
            if x > 222 || y > 222 {
                return;
            }
            out.extend_from_slice(b"\x1b[M");
            out.extend_from_slice(&[32 + code, (33 + x) as u8, (33 + y) as u8]);
        }
        MouseEncoding::Utf8 => {
            out.extend_from_slice(b"\x1b[M");
            for cp in [32 + u32::from(code), 33 + x, 33 + y] {
                let mut buf = [0; 4];
                if let Some(c) = char::from_u32(cp) {
                    out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
                }
            }
        }
        MouseEncoding::Sgr => out.extend_from_slice(format!("\x1b[<{code};{};{}{end}", x + 1, y + 1).as_bytes()),
        MouseEncoding::SgrPixels => {
            out.extend_from_slice(format!("\x1b[<{code};{};{}{end}", event.pixels.0, event.pixels.1).as_bytes())
        }
        MouseEncoding::Urxvt => out.extend_from_slice(format!("\x1b[{};{};{}M", 32 + u32::from(code), x + 1, y + 1).as_bytes()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(button: Option<Button>, action: Action, mods: Mods) -> Event {
        Event { button, action, mods, cell: (2, 3), pixels: (25, 70) }
    }

    #[test]
    fn reports_follow_ghostty() {
        let mut out = Vec::new();
        encode(&mut out, MouseMode::Normal, MouseEncoding::Sgr, event(Some(Button::Four), Action::Press, Mods::default()));
        assert_eq!(out, b"\x1b[<64;3;4M");
        out.clear();
        let ctrl = Mods { ctrl: true, ..Mods::default() };
        encode(&mut out, MouseMode::Normal, MouseEncoding::Default, event(Some(Button::Five), Action::Press, ctrl));
        assert_eq!(out, [0x1b, b'[', b'M', 32 + 65 + 16, 35, 36]);
        out.clear();
        // X10 reports only presses of the three buttons.
        encode(&mut out, MouseMode::X10, MouseEncoding::Default, event(Some(Button::Four), Action::Press, Mods::default()));
        encode(&mut out, MouseMode::X10, MouseEncoding::Default, event(Some(Button::Left), Action::Release, Mods::default()));
        assert!(out.is_empty());
        encode(&mut out, MouseMode::Normal, MouseEncoding::SgrPixels, event(Some(Button::Left), Action::Release, Mods::default()));
        assert_eq!(out, b"\x1b[<0;25;70m");
        out.clear();
        // Releases outside SGR are button 3; motion adds 32.
        encode(&mut out, MouseMode::Normal, MouseEncoding::Urxvt, event(Some(Button::Right), Action::Release, Mods::default()));
        assert_eq!(out, b"\x1b[35;3;4M");
        out.clear();
        encode(&mut out, MouseMode::ButtonMotion, MouseEncoding::Sgr, event(Some(Button::Left), Action::Motion, Mods::default()));
        assert_eq!(out, b"\x1b[<32;3;4M");
        out.clear();
        encode(&mut out, MouseMode::ButtonMotion, MouseEncoding::Sgr, event(None, Action::Motion, Mods::default()));
        assert!(out.is_empty());
    }
}
