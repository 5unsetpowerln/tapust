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

/// ghostty's `widget.unfocused-split`: `unfocused-split-fill` (the
/// background) at opacity `1 - unfocused-split-opacity`.
pub const UNFOCUSED_FILL: [u8; 3] = [0x28, 0x2C, 0x34];
pub const UNFOCUSED_ALPHA: f64 = 1.0 - 0.7;
/// ghostty's drag icon: the surface scaled by 0.2.
pub const DRAG_PREVIEW_SCALE: f64 = 0.2;
/// ghostty's GTK stylesheet and its runtime rules.
pub const GHOSTTY_CSS: &str = include_str!("ghostty.css");
/// `cursor-style-blink` interval.
pub const BLINK: std::time::Duration = std::time::Duration::from_millis(600);
/// ghostty resets the blink on output at most this often.
pub const BLINK_RESET: std::time::Duration = std::time::Duration::from_millis(500);

/// ghostty's resize overlay: shown 750 ms, not during the first 250 ms.
pub const RESIZE_OVERLAY: std::time::Duration = std::time::Duration::from_millis(750);
pub const RESIZE_OVERLAY_DELAY: std::time::Duration = std::time::Duration::from_millis(250);

/// The window and About icon: the icon theme's generic terminal, since
/// tapust installs no icon of its own (ghostty uses its bundle id).
pub const ICON: &str = "utilities-terminal";

/// ghostty refocuses the active surface this long after the tab overview
/// closes.
pub const OVERVIEW_FOCUS: std::time::Duration = std::time::Duration::from_millis(500);

/// How often ghostty checks the PTY's termios for a password prompt.
pub const TERMIOS_POLL: std::time::Duration = std::time::Duration::from_millis(200);

/// ghostty's mouse-scroll-multiplier: rows per wheel detent, and the factor
/// for touchpad (precision) scrolling.
pub const SCROLL_DISCRETE: f64 = 3.0;
pub const SCROLL_PRECISION: f64 = 1.0;

/// ghostty's click-repeat-interval on Linux (0 in the config, replaced by
/// 500ms when the OS reports none).
pub const CLICK_INTERVAL: std::time::Duration = std::time::Duration::from_millis(500);
/// How often ghostty scrolls while a selection is dragged past the edge.
pub const SELECTION_SCROLL: std::time::Duration = std::time::Duration::from_millis(15);

/// ghostty's gtk-horizontal-tab-scroll: touchpad units to a tab switch, and
/// how long the movement is remembered.
pub const SWIPE_TAB: f64 = 120.0;
pub const SWIPE_RESET: std::time::Duration = std::time::Duration::from_millis(500);
