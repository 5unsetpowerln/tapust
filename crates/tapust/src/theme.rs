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
/// GTK's interface font, used by ghostty's tab bar and dialogs.
pub const UI_FONT_FAMILY: &str = "sans-serif";
pub const UI_FONT_POINTS: f32 = 11.0;
/// `window-padding-x` / `window-padding-y` (top-left and bottom-right), in points.
pub const PADDING_POINTS: f64 = 2.0;

/// The split divider: libadwaita's 1px `paned > separator` over the window background.
pub const DIVIDER: [u8; 4] = [0x43, 0x43, 0x47, 0xff];
/// `unfocused-split-fill` (the background) at `1 - unfocused-split-opacity` (0.30).
pub const UNFOCUSED_DIM: [u8; 4] = [0x28, 0x2C, 0x34, 77];
/// `cursor-style-blink` interval.
pub const BLINK: std::time::Duration = std::time::Duration::from_millis(600);

/// libadwaita's dark colors, which ghostty's tab bar and dialogs use.
pub mod adw {
    pub const HEADERBAR_BG: [u8; 4] = [0x2e, 0x2e, 0x32, 0xff];
    /// `headerbar_shade_color`, the line under the tab bar.
    pub const HEADERBAR_SHADE: [u8; 4] = [0, 0, 6, 92];
    pub const FG: [u8; 4] = [0xff, 0xff, 0xff, 0xff];
    /// `currentColor` at 10% (selected tab) and at `--border-opacity` 15% (outlines, separators).
    pub const SELECTED: [u8; 4] = [0xff, 0xff, 0xff, 26];
    pub const BORDER: [u8; 4] = [0xff, 0xff, 0xff, 38];
    pub const DIALOG_BG: [u8; 4] = [0x36, 0x36, 0x3a, 0xff];
}

/// Tab bar geometry in pixels at scale 1: tabbox padding 6 + tab (26 + 2 * 4) + 6.
pub const TAB_BAR_HEIGHT: f64 = 46.0;
pub const TAB_MARGIN: f64 = 6.0;
pub const TAB_RADIUS: f64 = 9.0;
