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

/// The split divider (`paned > separator`, one logical pixel): ghostty fills
/// it with the window background and libadwaita adds an inset shadow of the
/// foreground (Yaru's #f7f7f7) at `--border-opacity` (15% on the dark side).
pub const DIVIDER_SHADOW: [u8; 3] = [0xf7, 0xf7, 0xf7];
pub const DIVIDER_SHADOW_ALPHA: f64 = 0.15;
/// ghostty's `widget.unfocused-split`: `unfocused-split-fill` (the
/// background) at opacity `1 - unfocused-split-opacity`.
pub const UNFOCUSED_FILL: [u8; 3] = [0x28, 0x2C, 0x34];
pub const UNFOCUSED_ALPHA: f64 = 1.0 - 0.7;
/// ghostty's drag handle: `.drag-handle { background-color: rgba(0, 0, 0, 0.15); }`,
/// shown on hover with `transition: 0.1s opacity`; its icon has the foreground color.
pub const DRAG_HANDLE_ALPHA: f64 = 0.15;
pub const DRAG_HANDLE_FADE: std::time::Duration = std::time::Duration::from_millis(100);
pub const ICON_COLOR: [u8; 3] = [0xf7, 0xf7, 0xf7];
/// ghostty's drop overlay: the accent background mixed with 80% transparent.
pub const DROP_ALPHA: f64 = 0.2;
/// ghostty's drag icon: the surface scaled by 0.2.
pub const DRAG_PREVIEW_SCALE: f64 = 0.2;
/// GTK's `gtk-dnd-drag-threshold`, in logical pixels along either axis.
pub const DRAG_THRESHOLD: f64 = 8.0;

/// libadwaita's smallest vertical scrollbar (a 40px slider in a trough with
/// 9px margins), which a pane's GtkScrolledWindow keeps room for.
pub const SCROLLBAR_MIN_HEIGHT: u32 = 58;
/// AdwApplicationWindow's minimum size, in logical pixels.
pub const WINDOW_MIN: (u32, u32) = (360, 200);
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
    /// The window behind a dialog: `shade_color` at twice its alpha.
    pub const DIMMING: [u8; 4] = [0, 0, 6, 128];
    /// A plain button or entry: `currentColor` at 10%.
    pub const BUTTON: [u8; 4] = [0xff, 0xff, 0xff, 26];
    /// `destructive_bg_color` (red_4) and `accent_bg_color` (blue_3).
    pub const DESTRUCTIVE: [u8; 4] = [0xc0, 0x1c, 0x28, 0xff];
    pub const SUGGESTED: [u8; 4] = [0x35, 0x84, 0xe4, 0xff];
    pub const TOAST_BG: [u8; 4] = [0x50, 0x50, 0x53, 0xff];
    /// Yaru's orange accent background on the dark side.
    pub const ACCENT_BG: [u8; 4] = [0xcb, 0x43, 0x14, 0xff];
    /// Yaru's window background: Ubuntu's libadwaita loads the Yaru stylesheet.
    pub const WINDOW_BG: [u8; 4] = [0x2c, 0x2c, 0x2c, 0xff];
}

/// ghostty's resize overlay: shown 750 ms, not during the first 250 ms.
pub const RESIZE_OVERLAY: std::time::Duration = std::time::Duration::from_millis(750);
pub const RESIZE_OVERLAY_DELAY: std::time::Duration = std::time::Duration::from_millis(250);
pub const RESIZE_OVERLAY_OUTLINE: [u8; 4] = [0x55, 0x55, 0x55, 0xff];

/// Tab bar geometry in pixels at scale 1: tabbox padding 6 + tab (26 + 2 * 4) + 6.
pub const TAB_BAR_HEIGHT: f64 = 46.0;
pub const TAB_MARGIN: f64 = 6.0;
pub const TAB_RADIUS: f64 = 9.0;
