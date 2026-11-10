//! ghostty's default appearance (colors, font, padding).

use term::{Palette, Rgb};

const fn rgb(v: u32) -> Rgb {
    Rgb { r: (v >> 16) as u8, g: (v >> 8) as u8, b: v as u8 }
}

pub const BACKGROUND: Rgb = rgb(0x282C34);
pub const FOREGROUND: Rgb = rgb(0xFFFFFF);

/// ghostty's 16 base colors; the 6x6x6 cube and the grays are xterm's.
const ANSI: [u32; 16] = [
    0x1D1F21, 0xCC6666, 0xB5BD68, 0xF0C674, 0x81A2BE, 0xB294BB, 0x8ABEB7, 0xC5C8C6, //
    0x666666, 0xD54E53, 0xB9CA4A, 0xE7C547, 0x7AA6DA, 0xC397D8, 0x70C0B1, 0xEAEAEA,
];

pub fn palette() -> Palette {
    let mut palette = Palette::default();
    for (slot, &value) in palette.indexed.iter_mut().zip(&ANSI) {
        *slot = rgb(value);
    }
    palette.foreground = FOREGROUND;
    palette.background = BACKGROUND;
    // The cursor uses the foreground color, and text under it the background.
    palette.cursor = None;
    palette
}

pub const FONT_FAMILY: &str = "JetBrains Mono";
pub const FONT_POINTS: f32 = 12.0;
/// `window-padding-x` / `window-padding-y` (top-left and bottom-right), in points.
pub const PADDING_POINTS: f64 = 2.0;
