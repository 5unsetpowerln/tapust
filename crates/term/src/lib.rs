//! Terminal state for tapust: an xterm-compatible VT parser feeding a grid
//! with scrollback.
//!
//! The public API below is the contract used by the GUI. The GUI holds a
//! `Terminal` behind a mutex: the pane's I/O thread calls [`Terminal::feed`]
//! with large chunks of PTY output, and the render thread reads the visible
//! rows, styles and dirty marks.

pub mod cell;
mod grid;
mod palette;
mod parser;
mod style;
mod term;
#[cfg(test)]
mod tests;

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
        palette::default_palette()
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
    parser: parser::Parser,
    term: term::Term,
}

impl Terminal {
    /// A terminal with the given viewport size and at most
    /// `scrollback_lines` lines of scrollback on the primary screen.
    pub fn new(cols: u16, rows: u16, scrollback_lines: usize) -> Terminal {
        Terminal {
            parser: parser::Parser::new(),
            term: term::Term::new(cols.max(1) as usize, rows.max(1) as usize, scrollback_lines),
        }
    }

    /// Parses PTY output and applies it. Escape and UTF-8 sequences may be
    /// split across calls.
    pub fn feed(&mut self, bytes: &[u8]) {
        self.parser.advance(&mut self.term, bytes);
    }

    /// Resizes the viewport, reflowing the primary screen and its scrollback
    /// with the soft-wrap marks (the alternate screen is truncated or padded).
    /// Marks every row dirty.
    pub fn resize(&mut self, cols: u16, rows: u16) {
        self.term.resize(cols.max(1) as usize, rows.max(1) as usize);
    }

    pub fn cols(&self) -> u16 {
        self.term.cols as u16
    }

    pub fn rows(&self) -> u16 {
        self.term.rows as u16
    }

    /// The cells of viewport row `y` (0 = top) with the scrollback view
    /// offset applied; exactly `cols()` cells.
    pub fn row(&self, y: u16) -> &[Cell] {
        self.term.grid.slot_cells(self.term.view_slot(y as usize))
    }

    /// The grapheme cluster of a cell whose [`Cell::has_grapheme`] is set
    /// (base character followed by its combining codepoints).
    pub fn grapheme(&self, y: u16, x: u16) -> Option<&str> {
        let slot = self.term.view_slot(y as usize);
        if !self.term.grid.slot_cells(slot).get(x as usize)?.has_grapheme() {
            return None;
        }
        self.term.grid.slot_meta(slot).grapheme(x)
    }

    /// The style of a style id found in a cell; id 0 is [`Style::DEFAULT`].
    pub fn style(&self, id: StyleId) -> &Style {
        self.term.styles.get(id)
    }

    pub fn palette(&self) -> &Palette {
        &self.term.palette
    }

    pub fn cursor(&self) -> Cursor {
        let t = &self.term;
        let vy = t.y + t.view_offset;
        Cursor {
            x: t.x as u16,
            y: vy.min(t.rows - 1) as u16,
            visible: t.cursor_visible && vy < t.rows,
            shape: t.cursor_shape,
            blinking: t.cursor_blink,
        }
    }

    pub fn modes(&self) -> Modes {
        Modes { alt_screen: self.term.alt_active, ..self.term.modes }
    }

    pub fn dirty(&self) -> &Dirty {
        &self.term.dirty
    }

    pub fn clear_dirty(&mut self) {
        self.term.clear_dirty();
    }

    pub fn mark_all_dirty(&mut self) {
        self.term.mark_all_dirty();
    }

    /// Sets the configured colors: the current palette and what OSC
    /// 104/110/111 and RIS reset to. Marks every row dirty.
    pub fn set_default_palette(&mut self, palette: Palette) {
        self.term.palette = palette.clone();
        self.term.base_palette = palette;
        self.term.mark_all_dirty();
    }

    /// Bytes to send back to the application (DA, DSR, DECRQM, OSC color
    /// queries, ...); the caller writes them to the PTY.
    pub fn take_replies(&mut self) -> Option<Vec<u8>> {
        if self.term.replies.is_empty() { None } else { Some(std::mem::take(&mut self.term.replies)) }
    }

    /// The window title set with OSC 0/2.
    pub fn title(&self) -> &str {
        &self.term.title
    }

    /// The working directory reported with OSC 7, as a path.
    pub fn reported_cwd(&self) -> Option<&str> {
        self.term.cwd.as_deref()
    }

    /// True once for each BEL received since the last call.
    pub fn take_bell(&mut self) -> bool {
        std::mem::take(&mut self.term.bell)
    }

    /// Scrolls the view into the scrollback by `delta` lines (positive =
    /// into history). New output does not move a scrolled-back view.
    pub fn scroll_view(&mut self, delta: isize) {
        self.term.scroll_view(delta);
    }

    /// Lines the view is scrolled back (0 = live).
    pub fn view_offset(&self) -> usize {
        self.term.view_offset
    }

    /// Lines currently stored in the primary screen's scrollback.
    pub fn history_len(&self) -> usize {
        self.term.primary().hist_len()
    }
}
