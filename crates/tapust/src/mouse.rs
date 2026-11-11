//! Mouse reports for programs that ask for them (ghostty's
//! input/mouse_encode.zig).

use term::{MouseEncoding, MouseMode};

#[derive(Clone, Copy, Default)]
pub struct Mods {
    pub shift: bool,
    pub alt: bool,
    pub ctrl: bool,
}

/// Appends the report of a wheel step (button 4 up, 5 down) over grid cell
/// `cell` (zero-based), at `pixels` from the grid's origin for SGR-pixels.
/// X10 mode reports only the left, middle and right buttons, so it gets
/// nothing.
pub fn encode_wheel(
    out: &mut Vec<u8>,
    mode: MouseMode,
    encoding: MouseEncoding,
    up: bool,
    mods: Mods,
    cell: (u16, u16),
    pixels: (i32, i32),
) {
    if matches!(mode, MouseMode::None | MouseMode::X10) {
        return;
    }
    let code = if up { 64 } else { 65 } + 4 * u8::from(mods.shift) + 8 * u8::from(mods.alt) + 16 * u8::from(mods.ctrl);
    let (x, y) = (u32::from(cell.0), u32::from(cell.1));
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
        MouseEncoding::Sgr => out.extend_from_slice(format!("\x1b[<{code};{};{}M", x + 1, y + 1).as_bytes()),
        MouseEncoding::SgrPixels => out.extend_from_slice(format!("\x1b[<{code};{};{}M", pixels.0, pixels.1).as_bytes()),
        MouseEncoding::Urxvt => out.extend_from_slice(format!("\x1b[{};{};{}M", 32 + u32::from(code), x + 1, y + 1).as_bytes()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wheel_reports_follow_ghostty() {
        let mut out = Vec::new();
        encode_wheel(&mut out, MouseMode::Normal, MouseEncoding::Sgr, true, Mods::default(), (2, 3), (25, 70));
        assert_eq!(out, b"\x1b[<64;3;4M");
        out.clear();
        let ctrl = Mods { ctrl: true, ..Mods::default() };
        encode_wheel(&mut out, MouseMode::Normal, MouseEncoding::Default, false, ctrl, (0, 0), (0, 0));
        assert_eq!(out, [0x1b, b'[', b'M', 32 + 65 + 16, 33, 33]);
        out.clear();
        encode_wheel(&mut out, MouseMode::X10, MouseEncoding::Default, true, Mods::default(), (0, 0), (0, 0));
        assert!(out.is_empty());
        encode_wheel(&mut out, MouseMode::Normal, MouseEncoding::SgrPixels, true, Mods::default(), (2, 3), (25, 70));
        assert_eq!(out, b"\x1b[<64;25;70M");
    }
}
