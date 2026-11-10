//! Terminal state for tapust: an xterm-compatible VT parser feeding a grid
//! with scrollback.
//!
//! The public API below is the contract used by the GUI. The GUI holds a
//! `Terminal` behind a mutex: the pane's I/O thread calls [`Terminal::feed`]
//! with large chunks of PTY output, and the render thread reads the visible
//! rows, styles and dirty marks.

pub mod cell;

pub use cell::{Attrs, Cell, Color, Style, StyleId, Underline, Width};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

/// Colors used to resolve [`Color`] values. Applications can change entries
/// with OSC 4/10/11/12 and reset them with OSC 104/110/111/112.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Palette {
    /// The 256 indexed colors (16 ANSI + 6x6x6 cube + 24 grays).
    pub indexed: [Rgb; 256],
    pub foreground: Rgb,
    pub background: Rgb,
    /// `None` means "use the colors of the cell under the cursor, inverted".
    pub cursor: Option<Rgb>,
}

impl Default for Palette {
    /// xterm's 256 colors with a dark default foreground/background.
    fn default() -> Palette {
        todo!()
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CursorShape {
    #[default]
    Block,
    Underline,
    Bar,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cursor {
    /// Column in the viewport.
    pub x: u16,
    /// Row in the viewport.
    pub y: u16,
    /// DECTCEM; false while the view is scrolled back so far that the cursor is off screen.
    pub visible: bool,
    pub shape: CursorShape,
    pub blinking: bool,
}

/// Mouse reporting mode requested by the application (DECSET 9/1000/1002/1003).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MouseMode {
    #[default]
    None,
    X10,
    Normal,
    ButtonMotion,
    AnyMotion,
}

/// Mouse report encoding (DECSET 1005/1006/1015).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MouseEncoding {
    #[default]
    Default,
    Utf8,
    Sgr,
    Urxvt,
}

/// Modes that affect input encoding and rendering.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Modes {
    /// DECCKM: cursor keys send SS3 sequences.
    pub app_cursor: bool,
    /// DECKPAM: the keypad sends application sequences.
    pub app_keypad: bool,
    /// DECSET 2004.
    pub bracketed_paste: bool,
    /// DECSET 1004.
    pub focus_events: bool,
    /// DECSET 1049/1047/47 is active.
    pub alt_screen: bool,
    /// DECSET 2026: a synchronized update is in progress; the renderer should
    /// not present until it ends or a timeout passes.
    pub sync_update: bool,
    /// DECSET 5.
    pub reverse_video: bool,
    pub mouse_mode: MouseMode,
    pub mouse_encoding: MouseEncoding,
    /// DECSET 1007: the wheel sends cursor keys on the alternate screen.
    pub alternate_scroll: bool,
}

/// Dirty marks for the viewport rows: a row is dirty when anything that
/// affects its rendering changed since the last [`Terminal::clear_dirty`].
#[derive(Clone, Debug, Default)]
pub struct Dirty {
    /// One bit per viewport row.
    pub rows: Vec<u64>,
}

impl Dirty {
    #[inline]
    pub fn is_dirty(&self, y: u16) -> bool {
        let y = y as usize;
        self.rows.get(y / 64).is_some_and(|w| w & (1 << (y % 64)) != 0)
    }

    pub fn any(&self) -> bool {
        self.rows.iter().any(|w| *w != 0)
    }
}

/// The terminal: parser state plus the primary and alternate screens.
pub struct Terminal {
    // Implementation-defined.
}

impl Terminal {
    /// A terminal with the given viewport size and at most
    /// `scrollback_lines` lines of scrollback on the primary screen.
    pub fn new(cols: u16, rows: u16, scrollback_lines: usize) -> Terminal {
        let _ = (cols, rows, scrollback_lines);
        todo!()
    }

    /// Parses PTY output and applies it. Escape and UTF-8 sequences may be
    /// split across calls.
    pub fn feed(&mut self, bytes: &[u8]) {
        let _ = bytes;
        todo!()
    }

    /// Resizes the viewport, reflowing the primary screen and its scrollback
    /// with the soft-wrap marks (the alternate screen is truncated or padded).
    /// Marks every row dirty.
    pub fn resize(&mut self, cols: u16, rows: u16) {
        let _ = (cols, rows);
        todo!()
    }

    pub fn cols(&self) -> u16 {
        todo!()
    }

    pub fn rows(&self) -> u16 {
        todo!()
    }

    /// The cells of viewport row `y` (0 = top) with the scrollback view
    /// offset applied; exactly `cols()` cells.
    pub fn row(&self, y: u16) -> &[Cell] {
        let _ = y;
        todo!()
    }

    /// The grapheme cluster of a cell whose [`Cell::has_grapheme`] is set
    /// (base character followed by its combining codepoints).
    pub fn grapheme(&self, y: u16, x: u16) -> Option<&str> {
        let _ = (y, x);
        todo!()
    }

    /// The style of a style id found in a cell; id 0 is [`Style::DEFAULT`].
    pub fn style(&self, id: StyleId) -> &Style {
        let _ = id;
        todo!()
    }

    pub fn palette(&self) -> &Palette {
        todo!()
    }

    pub fn cursor(&self) -> Cursor {
        todo!()
    }

    pub fn modes(&self) -> Modes {
        todo!()
    }

    pub fn dirty(&self) -> &Dirty {
        todo!()
    }

    pub fn clear_dirty(&mut self) {
        todo!()
    }

    pub fn mark_all_dirty(&mut self) {
        todo!()
    }

    /// Bytes to send back to the application (DA, DSR, DECRQM, OSC color
    /// queries, ...); the caller writes them to the PTY.
    pub fn take_replies(&mut self) -> Option<Vec<u8>> {
        todo!()
    }

    /// The window title set with OSC 0/2.
    pub fn title(&self) -> &str {
        todo!()
    }

    /// The working directory reported with OSC 7, as a path.
    pub fn reported_cwd(&self) -> Option<&str> {
        todo!()
    }

    /// True once for each BEL received since the last call.
    pub fn take_bell(&mut self) -> bool {
        todo!()
    }

    /// Scrolls the view into the scrollback by `delta` lines (positive =
    /// into history). New output does not move a scrolled-back view.
    pub fn scroll_view(&mut self, delta: isize) {
        let _ = delta;
        todo!()
    }

    /// Lines the view is scrolled back (0 = live).
    pub fn view_offset(&self) -> usize {
        todo!()
    }

    /// Lines currently stored in the primary screen's scrollback.
    pub fn history_len(&self) -> usize {
        todo!()
    }
}
