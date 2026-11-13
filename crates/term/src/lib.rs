//! Terminal state for tapust: an xterm-compatible VT parser feeding a grid
//! with scrollback.
//!
//! The public API below is the contract used by the GUI. The GUI holds a
//! `Terminal` behind a mutex: the pane's I/O thread calls [`Terminal::feed`]
//! with large chunks of PTY output, and the render thread reads the visible
//! rows, styles and dirty marks.

pub mod cell;
mod grid;
mod link;
mod palette;
mod parser;
#[cfg(test)]
mod resize_tests;
mod search;
mod selection;
mod style;
mod term;
#[cfg(test)]
mod tests;

pub use cell::{Attrs, Cell, Color, LinkId, Semantic, Style, StyleId, Underline, Width};
pub use grid::PromptMark;
pub use selection::{Point, Selection};
pub use term::PromptClick;

/// Columns `c` takes on the grid: 1 or 2, 0 for controls and combining marks.
pub fn char_width(c: char) -> usize {
    match term::char_width(c as u32) {
        w @ (1 | 2) => w as usize,
        _ => 0,
    }
}

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
    /// ghostty's block_hollow, only ever the configured default.
    BlockHollow,
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

/// Mouse report encoding (DECSET 1005/1006/1015/1016).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MouseEncoding {
    #[default]
    Default,
    Utf8,
    Sgr,
    Urxvt,
    /// SGR with the position in pixels (DEC 1016).
    SgrPixels,
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

/// The state of a ConEmu progress report (OSC 9;4).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProgressState {
    Remove,
    Set,
    Error,
    Indeterminate,
    Pause,
}

/// ghostty's ProgressReport: the state and the percentage (0 to 100).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProgressReport {
    pub state: ProgressState,
    pub progress: Option<u8>,
}

/// What the program asks of the terminal's surface, in the order it was
/// received (ghostty's surface messages from the stream handler).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Message {
    /// OSC 9 or OSC 777;notify, the title cut to 63 bytes and the body to 255.
    DesktopNotification { title: String, body: String },
    /// OSC 9;4, and a removal at RIS.
    ProgressReport(ProgressReport),
    /// OSC 133;C: the command's output starts.
    StartCommand,
    /// OSC 133;D with the command's exit code.
    StopCommand(u8),
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
    /// The title and the working directory when take_news last looked.
    told: (String, Option<String>),
}

impl Terminal {
    /// A terminal with the given viewport size and at most
    /// `scrollback_lines` lines of scrollback on the primary screen.
    pub fn new(cols: u16, rows: u16, scrollback_lines: usize) -> Terminal {
        Terminal {
            parser: parser::Parser::new(),
            term: term::Term::new(cols.max(1) as usize, rows.max(1) as usize, scrollback_lines),
            told: (String::new(), None),
        }
    }

    /// A frozen copy of the active screen, as tmux's copy mode takes one of
    /// its pane (window_copy_clone_screen): its scrollback and screen, with
    /// their line numbers, become the copy's primary screen, with the cursor
    /// and the styles, links and colors to draw them. The copy has no
    /// selection and reports no mouse.
    pub fn snapshot(&self) -> Terminal {
        Terminal { parser: parser::Parser::new(), term: self.term.snapshot(), told: self.told.clone() }
    }

    /// Parses PTY output and applies it. Escape and UTF-8 sequences may be
    /// split across calls.
    pub fn feed(&mut self, bytes: &[u8]) {
        self.parser.advance(&mut self.term, bytes);
    }

    /// Resizes the viewport mostly as ghostty does: the primary screen and
    /// its scrollback reflow with the soft-wrap marks while DECAWM is set,
    /// the alternate screen is cut or padded. The cursor's row stays on the
    /// screen. The cursors, the saved cursors (clamped to the screen), the
    /// selection's ends and a view scrolled into the scrollback follow their
    /// cells, tab stops stay, and a prompt the shell redraws (OSC 133) is
    /// cleared. Ends a synchronized update and marks every row dirty.
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

    /// The URI of a hyperlink found in a cell. The terminal reuses the id
    /// of a link once no cell has it, so a link kept across output may be
    /// gone or another one.
    pub fn link_uri(&self, link: LinkId) -> Option<&str> {
        self.term.links.uri(link.0.get())
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

    /// Changes the configured colors, keeping those a program set.
    pub fn change_default_palette(&mut self, palette: Palette) {
        self.term.change_default_palette(palette);
    }

    /// The configured cursor shape and blinking (`None`: blinking, which
    /// DEC mode 12 may change), which DECSCUSR 0 and RIS restore.
    pub fn set_default_cursor(&mut self, shape: CursorShape, blink: Option<bool>) {
        self.term.set_default_cursor((shape, blink));
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

    /// ghostty's cursorIsAtPrompt: never on the alternate screen; on a row
    /// marked as a prompt, or while the shell prints its prompt or input.
    pub fn cursor_at_prompt(&self) -> bool {
        let t = &self.term;
        if t.alt_active {
            return false;
        }
        t.grid.meta(t.y).prompt != PromptMark::None || t.semantic != Semantic::Output
    }

    /// How a click at the prompt should move the cursor, if the shell said.
    pub fn prompt_click(&self) -> Option<PromptClick> {
        self.term.prompt_click
    }

    /// What the shell is printing now (OSC 133).
    pub fn cursor_semantic(&self) -> Semantic {
        self.term.semantic
    }

    /// The prompt mark of absolute line `line`.
    pub fn line_prompt(&self, line: u64) -> PromptMark {
        self.line_slot(line).map_or(PromptMark::None, |slot| self.term.grid.slot_meta(slot).prompt)
    }

    /// The pointer's CSS cursor name: `text`, `default` while reporting the
    /// mouse, or what OSC 22 set.
    pub fn mouse_shape(&self) -> &'static str {
        self.term.mouse_shape
    }

    /// Tells the terminal whether its surface has focus; a DEC 1004 report
    /// enabled later starts from it.
    pub fn set_focused(&mut self, focused: bool) {
        self.term.focused = focused;
    }

    /// Whether there is news besides the cells: a title or working directory
    /// other than at the last call, or a bell or messages not taken yet. The
    /// application reads these when woken, which it must be for them even
    /// while the cells it was woken for are not drawn yet.
    pub fn take_news(&mut self) -> bool {
        let t = &self.term;
        let moved = t.title != self.told.0 || t.cwd != self.told.1;
        if moved {
            self.told = (t.title.clone(), t.cwd.clone());
        }
        moved || t.bell || !t.messages.is_empty()
    }

    /// True once for each BEL received since the last call.
    pub fn take_bell(&mut self) -> bool {
        std::mem::take(&mut self.term.bell)
    }

    /// The messages received since the last call, oldest first.
    pub fn take_messages(&mut self) -> Vec<Message> {
        std::mem::take(&mut self.term.messages)
    }

    /// ghostty's Terminal.fullReset (the reset action): RIS without the
    /// messages a program's RIS sends.
    pub fn full_reset(&mut self) {
        self.term.full_reset();
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

    /// Absolute line numbers count every line that ever scrolled from the
    /// top of the active screen into its scrollback, so a line keeps its
    /// number while output scrolls (a resize renumbers the lines after the
    /// oldest one). The oldest line still stored:
    pub fn first_line(&self) -> u64 {
        self.term.grid.first_line()
    }

    /// The absolute line shown at the top of the viewport.
    pub fn view_top(&self) -> u64 {
        self.term.grid.pushed() - self.term.view_offset as u64
    }

    /// One past the absolute line of the last screen row.
    pub fn end_line(&self) -> u64 {
        self.term.grid.pushed() + self.term.rows as u64
    }

    fn line_slot(&self, line: u64) -> Option<usize> {
        let first = self.first_line();
        (line >= first && line < self.end_line()).then(|| self.term.grid.line_slot((line - first) as usize))
    }

    /// The cells of absolute line `line`, if it is still stored.
    pub fn line(&self, line: u64) -> Option<&[Cell]> {
        self.line_slot(line).map(|slot| self.term.grid.slot_cells(slot))
    }

    /// Whether absolute line `line` was soft-wrapped into the next one.
    pub fn line_wrapped(&self, line: u64) -> bool {
        self.line_slot(line).is_some_and(|slot| self.term.grid.slot_meta(slot).wrapped)
    }

    /// The grapheme cluster of a cell of absolute line `line` whose
    /// [`Cell::has_grapheme`] is set.
    pub fn line_grapheme(&self, line: u64, x: u16) -> Option<&str> {
        let slot = self.line_slot(line)?;
        if !self.term.grid.slot_cells(slot).get(x as usize)?.has_grapheme() {
            return None;
        }
        self.term.grid.slot_meta(slot).grapheme(x)
    }

    /// The active screen's selection, first end first. Once lines leave the
    /// scrollback, it starts at the oldest stored line, or is gone if no
    /// selected line is left.
    pub fn selection(&self) -> Option<Selection> {
        let first = self.first_line();
        let (start, end) = self.term.selection?.ordered();
        // Nothing is left once the last selected line is gone.
        if end.line < first {
            return None;
        }
        let start = if start.line < first { Point { line: first, col: 0 } } else { start };
        Some(Selection { start, end, ..self.term.selection? })
    }

    /// ghostty's Screen.select: replaces the active screen's selection, which
    /// then follows its cells until RIS or a screen switch clears it. Marks
    /// every row dirty if it changed.
    pub fn set_selection(&mut self, selection: Option<Selection>) {
        if self.term.selection != selection {
            self.term.selection = selection;
            self.term.mark_all_dirty();
        }
    }

    /// Lines currently stored in the primary screen's scrollback.
    pub fn history_len(&self) -> usize {
        self.term.primary().hist_len()
    }
}
