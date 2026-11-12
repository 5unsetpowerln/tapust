//! tmux's copy mode, ported from tmux 3.6 window-copy.c and grid-reader.c:
//! a cursor, a selection, jumps and searches over a frozen copy of the
//! pane's screen (window_copy_clone_screen), carried out for the commands
//! the mux crate's copy-mode key tables send.
//!
//! The backing grid is a [`Terminal::snapshot`]. tmux's coordinates are
//! kept: `py` counts lines from the oldest one in the backing's scrollback
//! (`hsize` lines of history, then `sy` screen rows), `oy` is how far the
//! view is scrolled up and (`cx`, `cy`) is the cursor in the view.

use std::ffi::{CStr, CString};
use std::time::{Duration, Instant};

use mux::{Copied, CopyCommand, CopyResult, ModeKeys, PaneSearch, SearchArg, WORD_SEPARATORS};
use term::{Point, Selection, Terminal, Width};

/// tmux's WHITESPACE.
const WHITESPACE: &str = "\t ";
/// WINDOW_COPY_SEARCH_TIMEOUT, WINDOW_COPY_SEARCH_ALL_TIMEOUT and
/// WINDOW_COPY_SEARCH_MAX_LINE.
const SEARCH_TIMEOUT: Duration = Duration::from_millis(10_000);
const SEARCH_ALL_TIMEOUT: Duration = Duration::from_millis(200);
const SEARCH_MAX_LINE: usize = 2000;
/// The `wrap-search` option.
const WRAP_SEARCH: bool = true;

/// A cell as grid_get_cell gives it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Data<'a> {
    /// The cell after a wide character (GRID_FLAG_PADDING).
    Padding,
    /// A character and its width.
    Char(char, u8),
    /// A grapheme cluster and its width.
    Cluster(&'a str, u8),
}

impl Data<'_> {
    /// The cell's UTF-8 text and width (utf8_data's size and width).
    fn text<'b>(&'b self, buf: &'b mut [u8; 4]) -> (&'b [u8], u8) {
        match *self {
            Data::Padding => (&[], 0),
            Data::Char(c, w) => (c.encode_utf8(buf).as_bytes(), w),
            Data::Cluster(s, w) => (s.as_bytes(), w),
        }
    }

    fn push_to(&self, out: &mut Vec<u8>) {
        let mut buf = [0; 4];
        out.extend_from_slice(self.text(&mut buf).0);
    }

    /// utf8_cstrhas: a single character found in `set`.
    fn in_set(&self, set: &str) -> bool {
        matches!(*self, Data::Char(c, _) if set.contains(c))
    }

    fn is_space(&self) -> bool {
        matches!(*self, Data::Char(' ', _))
    }

    /// The one-byte character of a cell, as the bracket matching reads it.
    fn byte(&self) -> Option<u8> {
        match *self {
            Data::Char(c, _) if c.is_ascii() => Some(c as u8),
            _ => None,
        }
    }
}

/// A cell of the search string laid out as screen_write_nputs lays it out.
#[derive(Clone, Debug, PartialEq, Eq)]
struct SearchCell {
    /// Empty for the padding after a wide character.
    text: String,
    width: u8,
}

/// The search string as cells: wide characters take a padding cell and
/// zero-width ones join the character before.
fn search_cells(s: &str) -> Vec<SearchCell> {
    let mut cells: Vec<SearchCell> = Vec::new();
    for c in s.chars() {
        let width = term::char_width(c) as u8;
        if width == 0 {
            if let Some(cell) = cells.iter_mut().rev().find(|cell| cell.width != 0) {
                cell.text.push(c);
            }
            continue;
        }
        cells.push(SearchCell { text: c.to_string(), width });
        if width == 2 {
            cells.push(SearchCell { text: String::new(), width: 0 });
        }
    }
    cells
}

/// window_copy_is_lowercase: no byte changes with tolower.
fn is_lowercase(s: &str) -> bool {
    !s.bytes().any(|b| b.is_ascii_uppercase())
}

/// A POSIX extended regular expression, compiled and run with the C
/// library's regcomp and regexec as tmux does.
struct Regex(Box<libc::regex_t>);

impl Regex {
    fn new(pattern: &str, icase: bool) -> Option<Regex> {
        let pattern = CString::new(pattern).ok()?;
        // SAFETY: regex_t is plain data that regcomp fills in.
        let mut reg: Box<libc::regex_t> = Box::new(unsafe { std::mem::zeroed() });
        let flags = libc::REG_EXTENDED | if icase { libc::REG_ICASE } else { 0 };
        // SAFETY: `reg` and `pattern` are valid; a failed regcomp needs no regfree.
        (unsafe { libc::regcomp(&mut *reg, pattern.as_ptr(), flags) } == 0).then_some(Regex(reg))
    }

    /// The byte range of the first match in `text`.
    fn exec(&self, text: &CStr, notbol: bool) -> Option<(usize, usize)> {
        let mut m = libc::regmatch_t { rm_so: 0, rm_eo: 0 };
        let flags = if notbol { libc::REG_NOTBOL } else { 0 };
        // SAFETY: the expression was compiled and `text` is NUL terminated.
        let found = unsafe { libc::regexec(&*self.0, text.as_ptr(), 1, &mut m, flags) } == 0;
        found.then_some((m.rm_so as usize, m.rm_eo as usize))
    }
}

impl Drop for Regex {
    fn drop(&mut self) {
        // SAFETY: compiled by regcomp.
        unsafe { libc::regfree(&mut *self.0) };
    }
}

/// A line of text for a regular expression (window_copy_stringify of a
/// line and the lines wrapped after it) with the byte offset of each cell.
struct LineText {
    text: CString,
    /// Where each cell's text starts; padding starts where the next cell does.
    offsets: Vec<usize>,
}

impl LineText {
    /// window_copy_cstrtocellpos: the first cell whose text starts at or
    /// after byte `at`, as a count of cells (one past the end if none).
    fn cell(&self, at: usize) -> usize {
        self.offsets.partition_point(|&o| o < at)
    }
}

/// The backing grid as tmux's grid functions see it.
struct Grid<'a> {
    term: &'a Terminal,
    first: u64,
    hsize: usize,
    sx: usize,
    sy: usize,
}

impl<'a> Grid<'a> {
    fn new(term: &'a Terminal) -> Grid<'a> {
        Grid {
            term,
            first: term.first_line(),
            hsize: term.history_len(),
            sx: usize::from(term.cols()),
            sy: usize::from(term.rows()),
        }
    }

    /// The last line, hsize + sy - 1.
    fn last_line(&self) -> usize {
        self.hsize + self.sy - 1
    }

    /// grid_get_cell: past the end of a line or of the grid, a space.
    fn data(&self, px: usize, py: usize) -> Data<'a> {
        let line = self.first + py as u64;
        let Some(row) = self.term.line(line).filter(|_| px < self.sx) else { return Data::Char(' ', 1) };
        let cell = row[px];
        let width = match cell.width() {
            Width::SpacerTail => return Data::Padding,
            Width::SpacerHead => return Data::Char(' ', 1),
            Width::Narrow => 1,
            Width::Wide => 2,
        };
        if cell.has_grapheme()
            && let Some(cluster) = self.term.line_grapheme(line, px as u16)
        {
            return Data::Cluster(cluster, width);
        }
        Data::Char(cell.char().filter(|&c| c != '\0').unwrap_or(' '), width)
    }

    /// GRID_LINE_WRAPPED
    fn wrapped(&self, py: usize) -> bool {
        self.term.line_wrapped(self.first + py as u64)
    }

    /// The cells a line uses (gl->cellsize): all of them, but for the
    /// last one of a line that wraps a wide character to the next.
    fn cellsize(&self, py: usize) -> usize {
        let row = self.term.line(self.first + py as u64);
        let spacer = row.is_some_and(|row| row[self.sx - 1].width() == Width::SpacerHead);
        self.sx - spacer as usize
    }

    /// grid_line_length: the line without its trailing spaces.
    fn line_length(&self, py: usize) -> usize {
        let mut px = self.cellsize(py);
        while px > 0 && self.data(px - 1, py).is_space() {
            px -= 1;
        }
        px
    }

    /// grid_in_set (no cell is a tab here).
    fn in_set(&self, px: usize, py: usize, set: &str) -> bool {
        self.data(px, py).in_set(set)
    }

    /// window_copy_search_compare
    fn search_compare(&self, px: usize, py: usize, s: &SearchCell, cis: bool) -> bool {
        let mut buf = [0; 4];
        let d = self.data(px, py);
        let (text, width) = d.text(&mut buf);
        if text.len() != s.text.len() || width != s.width {
            return false;
        }
        if cis && text.len() == 1 {
            return text[0].to_ascii_lowercase() == s.text.as_bytes()[0];
        }
        text == s.text.as_bytes()
    }

    /// window_copy_search_lr: the first match starting in [first, last).
    fn search_lr(&self, sgd: &[SearchCell], py: usize, first: usize, last: usize, cis: bool) -> Option<usize> {
        (first..last).find(|&ax| self.matches_at(sgd, ax, py, cis))
    }

    /// window_copy_search_rl: the last match starting in [first, last).
    fn search_rl(&self, sgd: &[SearchCell], py: usize, first: usize, last: usize, cis: bool) -> Option<usize> {
        (first..last).rev().find(|&ax| self.matches_at(sgd, ax, py, cis))
    }

    /// The inner loop of window_copy_search_lr: the search string at `ax`,
    /// continuing on the lines `py` wraps into.
    fn matches_at(&self, sgd: &[SearchCell], ax: usize, py: usize, cis: bool) -> bool {
        let endline = self.last_line();
        for (bx, s) in sgd.iter().enumerate() {
            let mut px = ax + bx;
            let mut pywrap = py;
            while px >= self.sx && pywrap < endline {
                if !self.wrapped(pywrap) {
                    break;
                }
                px -= self.sx;
                pywrap += 1;
            }
            if px >= self.sx || !self.search_compare(px, pywrap, s, cis) {
                return false;
            }
        }
        true
    }

    /// window_copy_stringify of line `py` from `first`, with the lines it
    /// wraps into while shorter than WINDOW_COPY_SEARCH_MAX_LINE.
    fn line_text(&self, py: usize, first: usize) -> LineText {
        let mut text = Vec::new();
        let mut offsets = Vec::new();
        let mut push = |py: usize, from: usize| {
            for px in from..self.sx {
                offsets.push(text.len());
                self.data(px, py).push_to(&mut text);
            }
        };
        push(py, first);
        let mut len = self.sx - first;
        let mut pywrap = py;
        while pywrap <= self.last_line() && len < SEARCH_MAX_LINE {
            if !self.wrapped(pywrap) {
                break;
            }
            pywrap += 1;
            push(pywrap, 0);
            len += self.sx;
        }
        LineText { text: CString::new(text).expect("cells hold no NUL"), offsets }
    }

    /// Cell `n` of a line text that starts at (`first`, `py`).
    fn cell_pos(&self, py: usize, first: usize, n: usize) -> (usize, usize) {
        let at = first + n;
        (at % self.sx, py + at / self.sx)
    }

    /// window_copy_search_lr_regex: the first match starting on line `py`
    /// in [first, last), as its start and its width in cells.
    fn search_lr_regex(&self, py: usize, first: usize, last: usize, reg: &Regex) -> Option<(usize, usize)> {
        if first >= last {
            return None;
        }
        let line = self.line_text(py, first);
        let (so, eo) = reg.exec(&line.text, first != 0)?;
        if so == eo {
            return None;
        }
        let (foundx, foundy) = self.cell_pos(py, first, line.cell(so));
        if foundy != py || foundx >= last {
            return None;
        }
        let end = first + line.cell(eo);
        Some((foundx, end - foundx))
    }

    /// window_copy_search_rl_regex and window_copy_last_regex: the last
    /// match starting on line `py` in [first, last). Every regexec after the
    /// first starts after the previous match, with REG_NOTBOL only when
    /// `first` is not 0.
    fn search_rl_regex(&self, py: usize, first: usize, last: usize, reg: &Regex) -> Option<(usize, usize)> {
        let line = self.line_text(py, first);
        let bytes = line.text.as_bytes_with_nul();
        let notbol = first != 0;
        let mut px = 0;
        let mut saved: Option<(usize, usize)> = None;
        while let Some((so, eo)) = reg.exec(CStr::from_bytes_with_nul(&bytes[px..]).expect("NUL terminated"), notbol) {
            if so == eo {
                break;
            }
            let (startx, starty) = self.cell_pos(py, first, line.cell(px + so));
            if starty > py || startx >= last {
                break;
            }
            let end = first + line.cell(px + eo);
            let (endx, endy) = (end % self.sx, py + end / self.sx);
            if endy > py || endx >= last {
                return Some((startx, end - startx));
            }
            saved = Some((startx, endx - startx));
            px += eo;
        }
        saved.filter(|&(_, width)| width > 0)
    }
}

/// grid-reader.c: a cursor moving over the grid.
struct Reader<'g, 'a> {
    gd: &'g Grid<'a>,
    cx: usize,
    cy: usize,
}

impl Reader<'_, '_> {
    fn line_length(&self) -> usize {
        self.gd.line_length(self.cy)
    }

    fn in_set(&self, set: &str) -> bool {
        self.gd.in_set(self.cx, self.cy, set)
    }

    fn padding(&self) -> bool {
        self.gd.data(self.cx, self.cy) == Data::Padding
    }

    /// grid_reader_cursor_right
    fn cursor_right(&mut self, wrap: bool, all: bool) {
        let px = if all { self.gd.sx } else { self.line_length() };
        if wrap && self.cx >= px && self.cy < self.gd.last_line() {
            self.cursor_start_of_line(false);
            self.cursor_down();
        } else if self.cx < px {
            self.cx += 1;
            while self.cx < px && self.padding() {
                self.cx += 1;
            }
        }
    }

    /// grid_reader_cursor_left
    fn cursor_left(&mut self, wrap: bool) {
        while self.cx > 0 && self.padding() {
            self.cx -= 1;
        }
        if self.cx == 0 && self.cy > 0 && (wrap || self.gd.wrapped(self.cy - 1)) {
            self.cursor_up();
            self.cursor_end_of_line(false, false);
        } else if self.cx > 0 {
            self.cx -= 1;
        }
    }

    /// grid_reader_cursor_down
    fn cursor_down(&mut self) {
        if self.cy < self.gd.last_line() {
            self.cy += 1;
        }
        while self.cx > 0 && self.padding() {
            self.cx -= 1;
        }
    }

    /// grid_reader_cursor_up
    fn cursor_up(&mut self) {
        if self.cy > 0 {
            self.cy -= 1;
        }
        while self.cx > 0 && self.padding() {
            self.cx -= 1;
        }
    }

    /// grid_reader_cursor_start_of_line
    fn cursor_start_of_line(&mut self, wrap: bool) {
        if wrap {
            while self.cy > 0 && self.gd.wrapped(self.cy - 1) {
                self.cy -= 1;
            }
        }
        self.cx = 0;
    }

    /// grid_reader_cursor_end_of_line
    fn cursor_end_of_line(&mut self, wrap: bool, all: bool) {
        if wrap {
            while self.cy < self.gd.last_line() && self.gd.wrapped(self.cy) {
                self.cy += 1;
            }
        }
        self.cx = if all { self.gd.sx } else { self.line_length() };
    }

    /// grid_reader_handle_wrap: keeps the cursor within `xx`, wrapping to
    /// the next line; false past the bottom of the grid.
    fn handle_wrap(&mut self, xx: &mut usize, yy: usize) -> bool {
        while self.cx > *xx {
            if self.cy == yy {
                return false;
            }
            self.cursor_start_of_line(false);
            self.cursor_down();
            *xx = if self.gd.wrapped(self.cy) { self.gd.sx - 1 } else { self.line_length() };
        }
        true
    }

    /// The end of the line for word motions, which do not break wrapped words.
    fn word_xx(&self) -> usize {
        if self.gd.wrapped(self.cy) { self.gd.sx - 1 } else { self.line_length() }
    }

    /// grid_reader_cursor_next_word
    fn cursor_next_word(&mut self, separators: &str) {
        let mut xx = self.word_xx();
        let yy = self.gd.last_line();
        if !self.handle_wrap(&mut xx, yy) {
            return;
        }
        if !self.in_set(WHITESPACE) {
            if self.in_set(separators) {
                loop {
                    self.cx += 1;
                    if !(self.handle_wrap(&mut xx, yy) && self.in_set(separators) && !self.in_set(WHITESPACE)) {
                        break;
                    }
                }
            } else {
                loop {
                    self.cx += 1;
                    if !(self.handle_wrap(&mut xx, yy) && !(self.in_set(separators) || self.in_set(WHITESPACE))) {
                        break;
                    }
                }
            }
        }
        while self.handle_wrap(&mut xx, yy) && self.in_set(WHITESPACE) {
            self.cx += 1;
        }
    }

    /// grid_reader_cursor_next_word_end
    fn cursor_next_word_end(&mut self, separators: &str) {
        let mut xx = self.word_xx();
        let yy = self.gd.last_line();
        while self.handle_wrap(&mut xx, yy) {
            if self.in_set(WHITESPACE) {
                self.cx += 1;
            } else if self.in_set(separators) {
                loop {
                    self.cx += 1;
                    if !(self.handle_wrap(&mut xx, yy) && self.in_set(separators) && !self.in_set(WHITESPACE)) {
                        break;
                    }
                }
                return;
            } else {
                loop {
                    self.cx += 1;
                    if !(self.handle_wrap(&mut xx, yy) && !(self.in_set(WHITESPACE) || self.in_set(separators))) {
                        break;
                    }
                }
                return;
            }
        }
    }

    /// grid_reader_cursor_previous_word
    fn cursor_previous_word(&mut self, separators: &str, already: bool, stop_at_eol: bool) {
        let word_is_letters;
        if already || self.in_set(WHITESPACE) {
            loop {
                if self.cx > 0 {
                    self.cx -= 1;
                    if !self.in_set(WHITESPACE) {
                        word_is_letters = !self.in_set(separators);
                        break;
                    }
                } else {
                    if self.cy == 0 {
                        return;
                    }
                    self.cursor_up();
                    self.cursor_end_of_line(false, false);
                    // Stop if separator at EOL.
                    if stop_at_eol && self.cx > 0 && self.gd.in_set(self.cx - 1, self.cy, WHITESPACE) {
                        word_is_letters = false;
                        break;
                    }
                }
            }
        } else {
            word_is_letters = !self.in_set(separators);
        }
        let (mut oldx, mut oldy);
        loop {
            (oldx, oldy) = (self.cx, self.cy);
            if self.cx == 0 {
                if self.cy == 0 || !self.gd.wrapped(self.cy - 1) {
                    break;
                }
                self.cursor_up();
                self.cursor_end_of_line(false, true);
            }
            if self.cx > 0 {
                self.cx -= 1;
            }
            if self.in_set(WHITESPACE) || word_is_letters == self.in_set(separators) {
                break;
            }
        }
        (self.cx, self.cy) = (oldx, oldy);
    }

    /// grid_reader_cell_equals_data
    fn equals(&self, px: usize, py: usize, jc: char) -> bool {
        matches!(self.gd.data(px, py), Data::Char(c, _) if c == jc)
    }

    /// grid_reader_cursor_jump
    fn cursor_jump(&mut self, jc: char) -> bool {
        let mut px = self.cx;
        let yy = self.gd.last_line();
        for py in self.cy..=yy {
            let xx = self.gd.line_length(py);
            while px < xx {
                if self.equals(px, py, jc) {
                    (self.cx, self.cy) = (px, py);
                    return true;
                }
                px += 1;
            }
            if py == yy || !self.gd.wrapped(py) {
                return false;
            }
            px = 0;
        }
        false
    }

    /// grid_reader_cursor_jump_back
    fn cursor_jump_back(&mut self, jc: char) -> bool {
        let mut xx = self.cx + 1;
        let mut py = self.cy + 1;
        while py > 0 {
            for px in (1..=xx).rev() {
                if self.equals(px - 1, py - 1, jc) {
                    (self.cx, self.cy) = (px - 1, py - 1);
                    return true;
                }
            }
            if py == 1 || !self.gd.wrapped(py - 2) {
                return false;
            }
            xx = self.gd.line_length(py - 2);
            py -= 1;
        }
        false
    }

    /// grid_reader_cursor_back_to_indentation
    fn cursor_back_to_indentation(&mut self) {
        let yy = self.gd.last_line();
        let (oldx, oldy) = (self.cx, self.cy);
        self.cursor_start_of_line(true);
        for py in self.cy..=yy {
            for px in 0..self.gd.line_length(py) {
                let d = self.gd.data(px, py);
                if !d.is_space() && d != Data::Padding {
                    (self.cx, self.cy) = (px, py);
                    return;
                }
            }
            if !self.gd.wrapped(py) {
                break;
            }
        }
        (self.cx, self.cy) = (oldx, oldy);
    }
}

/// format_grid_word: the word around (`x`, `y`), between word separators
/// and spaces, across wrapped lines.
fn grid_word(gd: &Grid, mut x: usize, mut y: usize) -> String {
    let separator = |d: Data| d.in_set(WORD_SEPARATORS) || d.is_space();
    let mut found = false;
    loop {
        let d = gd.data(x, y);
        if d != Data::Padding && separator(d) {
            found = true;
            break;
        }
        if x == 0 {
            if y == 0 || !gd.wrapped(y - 1) {
                break;
            }
            y -= 1;
            x = gd.line_length(y);
            if x == 0 {
                break;
            }
        }
        x -= 1;
    }
    let mut word = Vec::new();
    loop {
        if found {
            let end = gd.line_length(y);
            if end == 0 || x == end - 1 {
                if y == gd.last_line() || !gd.wrapped(y) {
                    break;
                }
                y += 1;
                x = 0;
            } else {
                x += 1;
            }
        }
        found = true;
        let d = gd.data(x, y);
        if d == Data::Padding {
            continue;
        }
        if separator(d) {
            break;
        }
        d.push_to(&mut word);
    }
    String::from_utf8_lossy(&word).into_owned()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CursorDrag {
    /// The selection is independent of the cursor.
    None,
    /// The end is synchronized with the cursor.
    EndSel,
    /// The start is synchronized with the cursor.
    Sel,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LineSel {
    None,
    LeftRight,
    RightLeft,
}

/// selflag; SEL_WORD is left out, as only the mouse bindings (with the
/// `mouse` option, off by default) select words.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SelMode {
    Char,
    Line,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SearchType {
    Off,
    Up,
    Down,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum JumpType {
    Off,
    Forward,
    Backward,
    ToForward,
    ToBackward,
}

/// window_copy_cmd_action
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Action {
    Nothing,
    Redraw,
    Cancel,
}

/// window_copy_cmd_clear
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Clear {
    Always,
    Never,
    EmacsOnly,
}

/// The `clear` of window_copy_cmd_table: whether a command clears the
/// search marks.
fn clear_of(command: &CopyCommand) -> Clear {
    use CopyCommand::*;
    match command {
        AppendSelectionAndCancel | BackToIndentation | BeginSelection | Cancel | ClearSelection | CopyPipeAndCancel
        | CopyPipeEndOfLineAndCancel | JumpToMark | NextMatchingBracket | PreviousMatchingBracket | RectangleToggle
        | RefreshFromPane | ScrollMiddle | SearchAgain | SearchBackward(_) | SearchBackwardIncremental(_)
        | SearchForward(_) | SearchForwardIncremental(_) | SearchReverse | SelectLine | SetMark => Clear::Always,
        TogglePosition => Clear::Never,
        BottomLine | CursorCentreHorizontal | CursorCentreVertical | CursorDown | CursorLeft | CursorRight | CursorUp
        | EndOfLine | GotoLine(_) | HalfpageDown | HalfpageUp | HistoryBottom | HistoryTop | JumpAgain
        | JumpBackward(_) | JumpForward(_) | JumpReverse | JumpToBackward(_) | JumpToForward(_) | MiddleLine
        | NextParagraph | NextSpace | NextSpaceEnd | NextWord | NextWordEnd | OtherEnd | PageDown | PageUp
        | PreviousParagraph | PreviousSpace | PreviousWord | ScrollDown | ScrollUp | StartOfLine | TopLine => {
            Clear::EmacsOnly
        }
    }
}

/// Whether a command is one of the `search-` ones, which keep the marks.
fn is_search(command: &CopyCommand) -> bool {
    use CopyCommand::*;
    matches!(
        command,
        SearchAgain
            | SearchBackward(_)
            | SearchBackwardIncremental(_)
            | SearchForward(_)
            | SearchForwardIncremental(_)
            | SearchReverse
    )
}

/// window_copy_mode_data for a mode entered with `copy-mode`.
pub struct CopyMode {
    /// The backing screen.
    term: Terminal,
    vi: bool,
    oy: usize,
    selx: usize,
    sely: usize,
    endselx: usize,
    endsely: usize,
    /// The screen has a selection (`screen.sel` is set).
    sel: bool,
    cursordrag: CursorDrag,
    lineflag: LineSel,
    rectflag: bool,
    hide_position: bool,
    selflag: SelMode,
    /// The line selection's start line, and its reset positions.
    dy: usize,
    selrx: usize,
    selry: usize,
    endselrx: usize,
    endselry: usize,
    cx: usize,
    cy: usize,
    /// The cursor's column on the last line with content, and its length.
    lastcx: usize,
    lastsx: usize,
    mx: usize,
    my: usize,
    showmark: bool,
    searchtype: SearchType,
    searchdirection: bool,
    searchregex: bool,
    searchstr: Option<String>,
    /// The search matches on the screen: one generation number per cell.
    searchmark: Option<Vec<u8>>,
    /// The matches counted, -1 when unknown.
    searchcount: i64,
    searchmore: bool,
    searchall: bool,
    /// searchx, searchy and searcho: where an incremental search started.
    search_origin: Option<(usize, usize, usize)>,
    searchgen: u8,
    timeout: bool,
    jumptype: JumpType,
    jumpchar: Option<char>,
    /// The pane's last search (wp->searchstr and wp->searchregex).
    pane_search: Option<PaneSearch>,
    /// What the running command did for the model.
    result: CopyResult,
}

impl CopyMode {
    /// tmux reads mode-keys whenever a key or command needs it.
    pub fn set_keys(&mut self, keys: ModeKeys) {
        self.vi = keys == ModeKeys::Vi;
    }

    /// window_copy_init on `term`, a copy of the pane's screen, with the
    /// pane's last search.
    pub fn new(term: Terminal, keys: ModeKeys, search: Option<PaneSearch>) -> CopyMode {
        let cursor = term.cursor();
        let hsize = term.history_len();
        let (cx, cy) = (usize::from(cursor.x), usize::from(cursor.y));
        let mut mode = CopyMode {
            term,
            vi: keys == ModeKeys::Vi,
            oy: 0,
            selx: 0,
            sely: 0,
            endselx: 0,
            endsely: 0,
            sel: false,
            cursordrag: CursorDrag::None,
            lineflag: LineSel::None,
            rectflag: false,
            hide_position: false,
            selflag: SelMode::Char,
            dy: 0,
            selrx: 0,
            selry: 0,
            endselrx: 0,
            endselry: 0,
            cx,
            cy,
            lastcx: 0,
            lastsx: 0,
            mx: cx,
            my: hsize + cy,
            showmark: false,
            searchtype: if search.is_some() { SearchType::Up } else { SearchType::Off },
            searchdirection: false,
            searchregex: search.as_ref().is_some_and(|s| s.regex),
            searchstr: search.as_ref().map(|s| s.text.clone()),
            searchmark: None,
            searchcount: 0,
            searchmore: false,
            searchall: true,
            search_origin: None,
            searchgen: 0,
            timeout: false,
            jumptype: JumpType::Off,
            jumpchar: None,
            pane_search: search,
            result: CopyResult::default(),
        };
        mode.sync();
        mode
    }

    fn grid(&self) -> Grid<'_> {
        Grid::new(&self.term)
    }

    fn hsize(&self) -> usize {
        self.term.history_len()
    }

    fn sx(&self) -> usize {
        usize::from(self.term.cols())
    }

    fn sy(&self) -> usize {
        usize::from(self.term.rows())
    }

    /// The cursor's line in the backing: hsize + cy - oy.
    fn cursor_line(&self) -> usize {
        self.hsize() + self.cy - self.oy
    }

    fn find_length(&self, py: usize) -> usize {
        self.grid().line_length(py)
    }

    /// The backing screen, its view scrolled to the mode's and its
    /// selection the mode's, for drawing.
    pub fn term_mut(&mut self) -> &mut Terminal {
        &mut self.term
    }

    /// The cursor in the view; past the last column it is drawn there.
    pub fn cursor(&self) -> (u16, u16) {
        ((self.cx.min(self.sx() - 1)) as u16, self.cy as u16)
    }

    /// The position indicator tmux draws at the top right of the pane: the
    /// default copy-mode-position-format, `[scroll position/history size]`
    /// with the search's result count; tapust keeps no time per line, so
    /// `#{top_line_time}` is always empty.
    pub fn position(&self) -> Option<String> {
        if self.hide_position || self.sy() < 2 {
            return None;
        }
        let mut s = format!("[{}/{}]", self.oy, self.hsize());
        if self.timeout {
            s.push_str(" (timed out)");
        } else if self.searchcount > 0 {
            let more = if self.searchmore { "+" } else { "" };
            s.push_str(&format!(" ({}{more} results)", self.searchcount));
        }
        Some(s)
    }

    /// The search matches on the screen in reading order, and the one at the
    /// cursor (window_copy_update_style's current match).
    pub fn marks(&self) -> (Vec<Selection>, Option<usize>) {
        let Some(marks) = &self.searchmark else { return (Vec::new(), None) };
        let (sx, top) = (self.sx(), self.term.view_top());
        let point = |at: usize| Point { line: top + (at / sx) as u64, col: (at % sx) as u16 };
        let mut found = Vec::new();
        let mut current = None;
        let cursor = self.cy * sx + self.cx.min(sx - 1);
        // The match at the cursor, or before it after an emacs search down.
        let at = if cursor != 0 && !self.vi && self.searchdirection { cursor - 1 } else { cursor };
        let mut i = 0;
        while i < marks.len() {
            let mark = marks[i];
            if mark == 0 {
                i += 1;
                continue;
            }
            let start = i;
            while i < marks.len() && marks[i] == mark {
                i += 1;
            }
            if (start..i).contains(&at) {
                current = Some(found.len());
            }
            found.push(Selection { start: point(start), end: point(i - 1), rect: false });
        }
        (found, current)
    }

    /// window_copy_resize: the backing reflows to the new size; the cursor
    /// follows its cell and its line goes to the top of the view, or the
    /// view to the bottom.
    pub fn resize(&mut self, cols: u16, rows: u16) {
        let first = self.term.first_line();
        let point = |x: usize, y: usize| Point { line: first + y as u64, col: x.min(self.sx() - 1) as u16 };
        let (cursor, mark) = (point(self.cx, self.cursor_line()), point(self.mx, self.my));
        // The selection's ends follow their cells through the reflow; tmux
        // leaves the mark where it was, which may then be off the grid.
        self.term.set_selection(Some(Selection { start: cursor, end: mark, rect: false }));
        self.term.resize(cols, rows);
        let (a, b) = self.term.selection().map(|s| (s.start, s.end)).expect("the reflow keeps both ends");
        let (cursor, mark) = if cursor <= mark { (a, b) } else { (b, a) };
        let first = self.term.first_line();
        let hsize = self.hsize();
        let cy = (cursor.line - first) as usize;
        self.cx = usize::from(cursor.col);
        if cy < hsize {
            (self.cy, self.oy) = (0, hsize - cy);
        } else {
            (self.cy, self.oy) = (cy - hsize, 0);
        }
        (self.mx, self.my) = (usize::from(mark.col), (mark.line - first) as usize);
        self.size_changed();
        self.sync();
    }

    /// window_copy_size_changed
    fn size_changed(&mut self) {
        let search = self.searchmark.is_some();
        self.clear_selection();
        self.clear_marks();
        if search && !self.timeout {
            self.search_marks(None, self.searchregex, false);
        }
        self.search_origin = Some((self.cx, self.cy, self.oy));
    }

    /// Shows the mode's view and selection on the backing screen.
    fn sync(&mut self) {
        let delta = self.oy as isize - self.term.view_offset() as isize;
        self.term.scroll_view(delta);
        let selection = self.drawn_selection();
        self.term.set_selection(selection);
    }

    /// The cells screen_check_selection selects, with the selection's ends
    /// in the backing rather than clamped to the screen: a rectangle takes
    /// both ends' columns; otherwise emacs mode-keys leave out the last
    /// character (but for one at the start of a line below the start), and
    /// a selection up to the start of a line leaves that line out.
    fn drawn_selection(&self) -> Option<Selection> {
        if !self.sel {
            return None;
        }
        let first = self.term.first_line();
        let last = self.sx() - 1;
        let pt = |x: usize, y: usize| Point { line: first + y as u64, col: x.min(last) as u16 };
        let (sx, sy, ex, ey) = (self.selx, self.sely, self.endselx, self.endsely);
        if self.rectflag {
            return Some(Selection { start: pt(sx, sy), end: pt(ex, ey), rect: true });
        }
        let emacs = !self.vi;
        let (start, end) = match sy.cmp(&ey) {
            std::cmp::Ordering::Less => (pt(sx, sy), pt(if emacs { ex.saturating_sub(1) } else { ex }, ey)),
            std::cmp::Ordering::Greater if sx == 0 => (pt(ex, ey), pt(last, sy - 1)),
            std::cmp::Ordering::Greater => (pt(ex, ey), pt(if emacs { sx - 1 } else { sx }, sy)),
            std::cmp::Ordering::Equal if ex < sx => (pt(ex, sy), pt(if emacs { sx - 1 } else { sx }, sy)),
            std::cmp::Ordering::Equal => {
                let xx = if emacs { ex.saturating_sub(1) } else { ex };
                if xx < sx && !(emacs && ex == 0) {
                    return None;
                }
                (pt(sx, sy), pt(xx, sy))
            }
        };
        Some(Selection { start, end, rect: false })
    }

    /// window_copy_command: runs `command` `count` times where it repeats.
    /// `pane` makes a new copy of the pane's screen for refresh-from-pane.
    pub fn command(&mut self, command: &CopyCommand, count: u32, pane: impl FnOnce() -> Terminal) -> CopyResult {
        use CopyCommand::*;
        self.result = CopyResult::default();
        let np = count;
        let action = match command {
            AppendSelectionAndCancel => {
                self.append_selection();
                self.clear_selection();
                Action::Cancel
            }
            BackToIndentation => {
                self.cursor_back_to_indentation();
                Action::Nothing
            }
            BeginSelection => {
                self.lineflag = LineSel::None;
                self.selflag = SelMode::Char;
                self.start_selection();
                Action::Redraw
            }
            BottomLine => {
                self.cx = 0;
                self.cy = self.sy() - 1;
                self.update_selection(false);
                Action::Redraw
            }
            Cancel => Action::Cancel,
            ClearSelection => {
                self.clear_selection();
                Action::Redraw
            }
            CopyPipeAndCancel => {
                self.copy_selection();
                self.clear_selection();
                Action::Cancel
            }
            CopyPipeEndOfLineAndCancel => {
                // window_copy_do_copy_end_of_line with a pipe and cancel.
                self.start_selection();
                for _ in 1..np {
                    self.cursor_down(false);
                }
                self.cursor_end_of_line();
                self.copy_selection();
                Action::Cancel
            }
            CursorCentreHorizontal => {
                self.cx = self.sx() / 2;
                self.update_selection(false);
                Action::Redraw
            }
            CursorCentreVertical => {
                self.cy = self.sy() / 2;
                self.update_selection(false);
                Action::Redraw
            }
            CursorDown => {
                for _ in 0..np {
                    self.cursor_down(false);
                }
                Action::Nothing
            }
            CursorLeft => {
                for _ in 0..np {
                    self.cursor_left();
                }
                Action::Nothing
            }
            CursorRight => {
                for _ in 0..np {
                    self.cursor_right(self.sel && self.rectflag);
                }
                Action::Nothing
            }
            CursorUp => {
                for _ in 0..np {
                    self.cursor_up(false);
                }
                Action::Nothing
            }
            EndOfLine => {
                self.cursor_end_of_line();
                Action::Nothing
            }
            GotoLine(line) => {
                if !line.is_empty() {
                    self.goto_line(line);
                }
                Action::Nothing
            }
            HalfpageDown | PageDown => {
                for _ in 0..np {
                    if self.pagedown(*command == HalfpageDown) {
                        return self.finish(command, Action::Cancel);
                    }
                }
                Action::Nothing
            }
            HalfpageUp | PageUp => {
                for _ in 0..np {
                    self.pageup(*command == HalfpageUp);
                }
                Action::Nothing
            }
            HistoryBottom => {
                if self.lineflag == LineSel::RightLeft && self.cursor_line() == self.endsely {
                    self.other_end();
                }
                self.cy = self.sy() - 1;
                self.cx = self.find_length(self.hsize() + self.cy);
                self.oy = 0;
                self.refresh_marks();
                self.update_selection(false);
                Action::Redraw
            }
            HistoryTop => {
                if self.lineflag == LineSel::LeftRight && self.cursor_line() == self.sely {
                    self.other_end();
                }
                (self.cy, self.cx, self.oy) = (0, 0, self.hsize());
                self.refresh_marks();
                self.update_selection(false);
                Action::Redraw
            }
            JumpAgain | JumpReverse => {
                let reverse = *command == JumpReverse;
                for _ in 0..np {
                    match (self.jumptype, reverse) {
                        (JumpType::Forward, false) | (JumpType::Backward, true) => self.cursor_jump(),
                        (JumpType::Backward, false) | (JumpType::Forward, true) => self.cursor_jump_back(),
                        (JumpType::ToForward, false) | (JumpType::ToBackward, true) => self.cursor_jump_to(),
                        (JumpType::ToBackward, false) | (JumpType::ToForward, true) => self.cursor_jump_to_back(),
                        (JumpType::Off, _) => {}
                    }
                }
                Action::Nothing
            }
            JumpBackward(c) | JumpForward(c) | JumpToBackward(c) | JumpToForward(c) => {
                if let Some(c) = c.chars().next() {
                    self.jumpchar = Some(c);
                    self.jumptype = match command {
                        JumpBackward(_) => JumpType::Backward,
                        JumpForward(_) => JumpType::Forward,
                        JumpToBackward(_) => JumpType::ToBackward,
                        _ => JumpType::ToForward,
                    };
                    for _ in 0..np {
                        match self.jumptype {
                            JumpType::Backward => self.cursor_jump_back(),
                            JumpType::Forward => self.cursor_jump(),
                            JumpType::ToBackward => self.cursor_jump_to_back(),
                            _ => self.cursor_jump_to(),
                        }
                    }
                }
                Action::Nothing
            }
            JumpToMark => {
                self.jump_to_mark();
                Action::Nothing
            }
            MiddleLine => {
                self.cx = 0;
                self.cy = (self.sy() - 1) / 2;
                self.update_selection(false);
                Action::Redraw
            }
            NextMatchingBracket => {
                self.next_matching_bracket(np);
                Action::Nothing
            }
            PreviousMatchingBracket => {
                self.previous_matching_bracket(np);
                Action::Nothing
            }
            NextParagraph => {
                for _ in 0..np {
                    self.next_paragraph();
                }
                Action::Nothing
            }
            PreviousParagraph => {
                for _ in 0..np {
                    self.previous_paragraph();
                }
                Action::Nothing
            }
            NextSpace | NextWord => {
                let separators = if *command == NextWord { WORD_SEPARATORS } else { "" };
                for _ in 0..np {
                    self.cursor_next_word(separators);
                }
                Action::Nothing
            }
            NextSpaceEnd | NextWordEnd => {
                let separators = if *command == NextWordEnd { WORD_SEPARATORS } else { "" };
                for _ in 0..np {
                    self.cursor_next_word_end(separators, false);
                }
                Action::Nothing
            }
            PreviousSpace | PreviousWord => {
                let separators = if *command == PreviousWord { WORD_SEPARATORS } else { "" };
                for _ in 0..np {
                    self.cursor_previous_word(separators, true);
                }
                Action::Nothing
            }
            OtherEnd => {
                self.selflag = SelMode::Char;
                if !np.is_multiple_of(2) {
                    self.other_end();
                }
                Action::Nothing
            }
            RectangleToggle => {
                self.lineflag = LineSel::None;
                self.rectangle_set(!self.rectflag);
                Action::Nothing
            }
            RefreshFromPane => {
                self.term = pane();
                self.oy = self.oy.min(self.hsize());
                self.size_changed();
                Action::Redraw
            }
            ScrollDown => {
                for _ in 0..np {
                    self.cursor_down(true);
                }
                Action::Nothing
            }
            ScrollUp => {
                for _ in 0..np {
                    self.cursor_up(true);
                }
                Action::Nothing
            }
            ScrollMiddle => self.scroll_to_row((self.sy() - 1) / 2),
            SearchAgain | SearchReverse => {
                let down = (self.searchtype == SearchType::Down) != (*command == SearchReverse);
                if self.searchtype != SearchType::Off {
                    for _ in 0..np {
                        self.search(down, self.searchregex);
                    }
                }
                Action::Nothing
            }
            SearchBackward(arg) | SearchForward(arg) => {
                if self.expand_search_string(arg) {
                    let down = matches!(command, SearchForward(_));
                    self.searchtype = if down { SearchType::Down } else { SearchType::Up };
                    self.searchregex = true;
                    self.timeout = false;
                    for _ in 0..np {
                        self.search(down, true);
                    }
                }
                Action::Nothing
            }
            SearchBackwardIncremental(arg) => self.search_incremental(arg, false),
            SearchForwardIncremental(arg) => self.search_incremental(arg, true),
            SelectLine => {
                self.select_line(np);
                Action::Redraw
            }
            SetMark => {
                (self.mx, self.my) = (self.cx, self.cursor_line());
                self.showmark = true;
                Action::Redraw
            }
            StartOfLine => {
                self.cursor_start_of_line();
                Action::Nothing
            }
            TogglePosition => {
                self.hide_position = !self.hide_position;
                Action::Redraw
            }
            TopLine => {
                (self.cx, self.cy) = (0, 0);
                self.update_selection(false);
                Action::Redraw
            }
        };
        self.finish(command, action)
    }

    /// The end of window_copy_command: commands other than searches clear
    /// the search marks as the command table says.
    fn finish(&mut self, command: &CopyCommand, action: Action) -> CopyResult {
        if !is_search(command) && self.searchmark.is_some() {
            let clear = match clear_of(command) {
                Clear::EmacsOnly if self.vi => Clear::Never,
                clear => clear,
            };
            if clear != Clear::Never {
                self.clear_marks();
                self.search_origin = None;
            }
        }
        self.result.cancel = action == Action::Cancel;
        self.sync();
        std::mem::take(&mut self.result)
    }

    /// window_copy_expand_search_string
    fn expand_search_string(&mut self, arg: &SearchArg) -> bool {
        let s = match arg {
            SearchArg::Text(s) => s.clone(),
            SearchArg::CursorWord => grid_word(&self.grid(), self.cx, self.cursor_line()),
        };
        if s.is_empty() {
            return false;
        }
        self.searchstr = Some(s);
        true
    }

    /// window_copy_cmd_search_forward_incremental and _backward_: `arg` is
    /// the prompt's text after `=` (changed), `-` or `+`. A changed text
    /// starts again from where the search started.
    fn search_incremental(&mut self, arg: &str, forward: bool) -> Action {
        self.timeout = false;
        let mut chars = arg.chars();
        let prefix = chars.next();
        let arg = chars.as_str();
        let mut action = Action::Nothing;
        match self.search_origin {
            None => self.search_origin = Some((self.cx, self.cy, self.oy)),
            Some((x, y, o)) if self.searchstr.as_deref().is_some_and(|ss| ss != arg) => {
                (self.cx, self.cy, self.oy) = (x, y, o);
                action = Action::Redraw;
            }
            Some(_) => {}
        }
        if arg.is_empty() {
            self.clear_marks();
            return Action::Redraw;
        }
        let down = match prefix {
            Some('+') => true,
            Some('-') => false,
            Some('=') => forward,
            _ => return action,
        };
        self.searchtype = if down { SearchType::Down } else { SearchType::Up };
        self.searchregex = false;
        self.searchstr = Some(arg.to_owned());
        if !self.search(down, false) {
            self.clear_marks();
            return Action::Redraw;
        }
        action
    }

    // ---- Selection -------------------------------------------------------

    /// window_copy_start_selection
    fn start_selection(&mut self) {
        self.selx = self.cx;
        self.sely = self.cursor_line();
        self.endselx = self.selx;
        self.endsely = self.sely;
        self.cursordrag = CursorDrag::EndSel;
        self.set_selection(false);
    }

    /// window_copy_update_selection
    fn update_selection(&mut self, no_reset: bool) -> bool {
        if !self.sel && self.lineflag == LineSel::None {
            return false;
        }
        self.set_selection(no_reset)
    }

    /// window_copy_set_selection: a selection whose ends are both above or
    /// both below the screen is hidden, but stays.
    fn set_selection(&mut self, no_reset: bool) -> bool {
        self.synchronize_cursor(no_reset);
        let top = self.hsize() - self.oy;
        let bottom = top + self.sy() - 1;
        let relpos = |y: usize| (y >= top) as u8 + (y > bottom) as u8;
        let (start, end) = (relpos(self.sely), relpos(self.endsely));
        if start == end && start != 1 {
            return false;
        }
        self.sel = true;
        true
    }

    /// window_copy_synchronize_cursor
    fn synchronize_cursor(&mut self, no_reset: bool) {
        match self.cursordrag {
            CursorDrag::EndSel => self.synchronize_cursor_end(false, no_reset),
            CursorDrag::Sel => self.synchronize_cursor_end(true, no_reset),
            CursorDrag::None => {}
        }
    }

    /// window_copy_synchronize_cursor_end
    fn synchronize_cursor_end(&mut self, mut begin: bool, no_reset: bool) {
        let mut xx = self.cx;
        let mut yy = self.cursor_line();
        match self.selflag {
            SelMode::Line if !no_reset => {
                begin = false;
                if self.dy > yy {
                    xx = 0;
                    begin = true;
                    self.endselx = self.endselrx;
                    self.endsely = self.endselry;
                } else {
                    if yy < self.endselry {
                        yy = self.endselry;
                    }
                    xx = self.find_length(yy);
                    self.selx = self.selrx;
                    self.sely = self.selry;
                }
            }
            _ => {}
        }
        if begin {
            (self.selx, self.sely) = (xx, yy);
        } else {
            (self.endselx, self.endsely) = (xx, yy);
        }
    }

    /// window_copy_clear_selection
    fn clear_selection(&mut self) {
        self.sel = false;
        self.cursordrag = CursorDrag::None;
        self.lineflag = LineSel::None;
        self.selflag = SelMode::Char;
        let px = self.find_length(self.cursor_line());
        if self.cx > px {
            self.cx = px;
        }
    }

    /// window_copy_rectangle_set
    fn rectangle_set(&mut self, rectflag: bool) {
        self.rectflag = rectflag;
        let px = self.find_length(self.cursor_line());
        if self.cx > px {
            self.cx = px;
        }
        self.update_selection(false);
    }

    /// window_copy_other_end
    fn other_end(&mut self) {
        if !self.sel && self.lineflag == LineSel::None {
            return;
        }
        self.lineflag = match self.lineflag {
            LineSel::LeftRight => LineSel::RightLeft,
            LineSel::RightLeft => LineSel::LeftRight,
            LineSel::None => LineSel::None,
        };
        self.cursordrag = match self.cursordrag {
            CursorDrag::None | CursorDrag::Sel => CursorDrag::EndSel,
            CursorDrag::EndSel => CursorDrag::Sel,
        };
        let (selx, sely) = if self.cursordrag == CursorDrag::Sel {
            (self.selx, self.sely)
        } else {
            (self.endselx, self.endsely)
        };
        let yy = self.cursor_line();
        self.cx = selx;
        let hsize = self.hsize();
        let sy = self.sy();
        if sely < hsize - self.oy {
            // Above.
            self.oy = hsize - sely;
            self.cy = 0;
        } else if sely > hsize - self.oy + sy - 1 {
            // Below: tmux compares with `hsize - oy + sy`, which leaves the
            // cursor a row past the screen for a selection end just below it.
            self.oy = hsize + sy - 1 - sely;
            self.cy = sy - 1;
        } else {
            self.cy = self.cy + sely - yy;
        }
        self.update_selection(true);
    }

    /// window_copy_cmd_select_line
    fn select_line(&mut self, np: u32) {
        self.lineflag = LineSel::LeftRight;
        self.rectflag = false;
        self.selflag = SelMode::Line;
        self.dy = self.cursor_line();
        self.cursor_start_of_line();
        self.selrx = self.cx;
        self.selry = self.cursor_line();
        self.endselry = self.selry;
        self.start_selection();
        self.cursor_end_of_line();
        self.endselry = self.cursor_line();
        self.endselrx = self.find_length(self.endselry);
        for _ in 1..np {
            self.cursor_down(false);
            self.cursor_end_of_line();
        }
    }

    // ---- Copying ---------------------------------------------------------

    /// window_copy_get_selection: the selected text, or the search match at
    /// the cursor without a selection.
    fn get_selection(&self) -> Option<Vec<u8>> {
        if !self.sel && self.lineflag == LineSel::None {
            return self.match_at_cursor();
        }
        let mut buf = Vec::new();
        // Find start and end.
        let (xx, yy) = (self.endselx, self.endsely);
        let ((sx, sy), (mut ex, ey)) = if yy < self.sely || (yy == self.sely && xx < self.selx) {
            ((xx, yy), (self.selx, self.sely))
        } else {
            ((self.selx, self.sely), (xx, yy))
        };
        // Trim ex to end of line.
        let ey_last = self.find_length(ey);
        if ex > ey_last {
            ex = ey_last;
        }
        let emacs = !self.vi;
        let (firstsx, lastex, restsx, restex);
        if self.rectflag {
            // The column with the cursor is left out, so it matters which
            // side of the selection the cursor is on.
            let selx = if self.cursordrag == CursorDrag::EndSel { self.selx } else { self.endselx };
            if selx < self.cx {
                lastex = if emacs { self.cx } else { self.cx + 1 };
                restex = lastex;
                firstsx = selx;
                restsx = selx;
            } else {
                lastex = selx + 1;
                restex = selx + 1;
                firstsx = self.cx;
                restsx = self.cx;
            }
        } else {
            lastex = if emacs { ex } else { ex + 1 };
            restex = self.sx();
            firstsx = sx;
            restsx = 0;
        }
        for i in sy..=ey {
            self.copy_line(&mut buf, i, if i == sy { firstsx } else { restsx }, if i == ey { lastex } else { restex });
        }
        if buf.is_empty() {
            return None;
        }
        // Remove final \n (unless at end in vi mode).
        if (emacs || lastex <= ey_last) && (!self.grid().wrapped(ey) || lastex != ey_last) {
            buf.pop();
        }
        Some(buf)
    }

    /// window_copy_copy_line
    fn copy_line(&self, buf: &mut Vec<u8>, sy: usize, mut sx: usize, mut ex: usize) {
        if sx > ex {
            return;
        }
        let gd = self.grid();
        // A line wrapped at the screen edge keeps its trailing spaces.
        let wrapped = gd.wrapped(sy);
        let xx = if wrapped { gd.cellsize(sy) } else { gd.line_length(sy) };
        ex = ex.min(xx);
        sx = sx.min(xx);
        for i in sx..ex {
            let d = gd.data(i, sy);
            if d != Data::Padding {
                d.push_to(buf);
            }
        }
        // Only add a newline if the line wasn't wrapped.
        if !wrapped || ex != xx {
            buf.push(b'\n');
        }
    }

    /// window_copy_copy_buffer through window_copy_copy_pipe with no
    /// command and an empty copy-command: the clipboard (set-clipboard is
    /// external) and a new paste buffer.
    fn copy_selection(&mut self) {
        if let Some(buf) = self.get_selection() {
            self.result.copied = Some(Copied::New(buf));
        }
    }

    /// window_copy_append_selection
    fn append_selection(&mut self) {
        if let Some(buf) = self.get_selection() {
            self.result.copied = Some(Copied::Append(buf));
        }
    }

    // ---- Cursor ----------------------------------------------------------

    /// Runs a grid reader from the cursor and returns where it ended.
    fn read(&self, px: usize, py: usize, f: impl FnOnce(&mut Reader) -> bool) -> Option<(usize, usize)> {
        let gd = self.grid();
        let mut r = Reader { gd: &gd, cx: px, cy: py };
        f(&mut r).then_some((r.cx, r.cy))
    }

    /// window_copy_cursor_start_of_line
    fn cursor_start_of_line(&mut self) {
        let (oldy, oy) = (self.cy, self.oy);
        let (px, py) = self.read(self.cx, self.cursor_line(), |r| {
            r.cursor_start_of_line(true);
            true
        }).expect("moved");
        self.acquire_cursor_up(oy, oldy, px, py);
    }

    /// window_copy_cursor_back_to_indentation
    fn cursor_back_to_indentation(&mut self) {
        let (oldy, oy) = (self.cy, self.oy);
        let (px, py) = self.read(self.cx, self.cursor_line(), |r| {
            r.cursor_back_to_indentation();
            true
        }).expect("moved");
        self.acquire_cursor_up(oy, oldy, px, py);
    }

    /// window_copy_cursor_end_of_line
    fn cursor_end_of_line(&mut self) {
        let (oldy, oy) = (self.cy, self.oy);
        let all = self.sel && self.rectflag;
        let (px, py) = self.read(self.cx, self.cursor_line(), |r| {
            r.cursor_end_of_line(true, all);
            true
        }).expect("moved");
        self.acquire_cursor_down(oy, oldy, px, py, false);
    }

    /// window_copy_cursor_left
    fn cursor_left(&mut self) {
        let (oldy, oy) = (self.cy, self.oy);
        let (px, py) = self.read(self.cx, self.cursor_line(), |r| {
            r.cursor_left(true);
            true
        }).expect("moved");
        self.acquire_cursor_up(oy, oldy, px, py);
    }

    /// window_copy_cursor_right
    fn cursor_right(&mut self, all: bool) {
        let (oldy, oy) = (self.cy, self.oy);
        let (px, py) = self.read(self.cx, self.cursor_line(), |r| {
            r.cursor_right(true, all);
            true
        }).expect("moved");
        self.acquire_cursor_down(oy, oldy, px, py, false);
    }

    /// The column the cursor keeps over shorter lines (lastcx and lastsx).
    fn remember_column(&mut self) {
        let ox = self.find_length(self.cursor_line());
        if self.cx != ox {
            self.lastcx = self.cx;
            self.lastsx = ox;
        }
    }

    /// The end of window_copy_cursor_up and _down: the cursor goes to the end
    /// of a line shorter than its column, or than the line it was at the end
    /// of, and a line selection to the end (or start) of the line.
    fn fit_cursor(&mut self, norectsel: bool) {
        if norectsel {
            let px = self.find_length(self.cursor_line());
            if (self.cx >= self.lastsx && self.cx != px) || self.cx > px {
                self.cx = px;
                self.update_selection(false);
            }
        }
        match self.lineflag {
            LineSel::LeftRight => {
                let px = if self.rectflag { self.sx() } else { self.find_length(self.cursor_line()) };
                self.cx = px;
                self.update_selection(false);
            }
            LineSel::RightLeft => {
                self.cx = 0;
                self.update_selection(false);
            }
            LineSel::None => {}
        }
    }

    /// window_copy_cursor_up
    fn cursor_up(&mut self, scroll_only: bool) {
        let norectsel = !self.sel || !self.rectflag;
        if norectsel {
            self.remember_column();
        }
        if self.lineflag == LineSel::LeftRight && self.cursor_line() == self.sely {
            self.other_end();
        }
        if scroll_only || self.cy == 0 {
            if norectsel {
                self.cx = self.lastcx;
            }
            self.scroll_down(1);
        } else {
            if norectsel {
                self.cx = self.lastcx;
            }
            self.cy -= 1;
            self.update_selection(false);
        }
        self.fit_cursor(norectsel);
    }

    /// window_copy_cursor_down
    fn cursor_down(&mut self, scroll_only: bool) {
        let norectsel = !self.sel || !self.rectflag;
        if norectsel {
            self.remember_column();
        }
        if self.lineflag == LineSel::RightLeft && self.cursor_line() == self.endsely {
            self.other_end();
        }
        if scroll_only || self.cy == self.sy() - 1 {
            if norectsel {
                self.cx = self.lastcx;
            }
            self.scroll_up(1);
        } else {
            if norectsel {
                self.cx = self.lastcx;
            }
            self.cy += 1;
            self.update_selection(false);
        }
        self.fit_cursor(norectsel);
    }

    /// window_copy_acquire_cursor_up: scrolls up when the cursor went above
    /// the screen.
    fn acquire_cursor_up(&mut self, oy: usize, _oldy: usize, px: usize, py: usize) {
        let yy = self.hsize() - oy;
        let (ny, cy) = if py < yy { (yy - py, 0) } else { (0, py - yy) };
        for _ in 0..ny {
            self.cursor_up(true);
        }
        (self.cx, self.cy) = (px, cy);
        self.update_selection(false);
    }

    /// window_copy_acquire_cursor_down: scrolls down when the cursor went
    /// below the screen.
    fn acquire_cursor_down(&mut self, oy: usize, _oldy: usize, px: usize, py: usize, no_reset: bool) {
        let cy = (py + oy).saturating_sub(self.hsize());
        let yy = self.sy() - 1;
        for _ in 0..cy.saturating_sub(yy) {
            self.cursor_down(true);
        }
        (self.cx, self.cy) = (px, cy.min(yy));
        self.update_selection(no_reset);
    }

    /// window_copy_scroll_up: the view moves `ny` lines down the history.
    fn scroll_up(&mut self, ny: usize) {
        let ny = ny.min(self.oy);
        if ny == 0 {
            return;
        }
        self.oy -= ny;
        self.refresh_marks();
        self.update_selection(false);
    }

    /// window_copy_scroll_down: the view moves `ny` lines up the history.
    fn scroll_down(&mut self, ny: usize) {
        let hsize = self.hsize();
        if ny > hsize {
            return;
        }
        let ny = if self.oy > hsize - ny { hsize - self.oy } else { ny };
        if ny == 0 {
            return;
        }
        self.oy += ny;
        self.refresh_marks();
        self.update_selection(false);
    }

    /// The search marks follow the view (window_copy_search_marks of the
    /// visible lines), unless the search timed out.
    fn refresh_marks(&mut self) {
        if self.searchmark.is_some() && !self.timeout {
            self.search_marks(None, self.searchregex, true);
        }
    }

    /// The page size of window_copy_pageup1 and _pagedown1.
    fn page(&self, half: bool) -> usize {
        match self.sy() {
            sy if sy > 2 && half => sy / 2,
            sy if sy > 2 => sy - 2,
            _ => 1,
        }
    }

    /// After a page: the cursor keeps its column within the line, as in
    /// window_copy_pageup1 and _pagedown1.
    fn fit_page_cursor(&mut self) {
        if !self.sel || !self.rectflag {
            let px = self.find_length(self.cursor_line());
            if (self.cx >= self.lastsx && self.cx != px) || self.cx > px {
                self.cursor_end_of_line();
            }
        }
        self.refresh_marks();
        self.update_selection(false);
    }

    /// window_copy_pageup1
    fn pageup(&mut self, half: bool) {
        self.remember_column();
        self.cx = self.lastcx;
        let n = self.page(half);
        let hsize = self.hsize();
        if self.oy + n > hsize {
            self.oy = hsize;
            self.cy = self.cy.saturating_sub(n);
        } else {
            self.oy += n;
        }
        self.fit_page_cursor();
    }

    /// window_copy_pagedown1 without scroll_exit (copy-mode -e is not
    /// bound to a key); it never cancels.
    fn pagedown(&mut self, half: bool) -> bool {
        self.remember_column();
        self.cx = self.lastcx;
        let n = self.page(half);
        if self.oy < n {
            // tmux sets oy to 0 before it moves the cursor by n - oy.
            self.oy = 0;
            self.cy = (self.cy + n).min(self.sy() - 1);
        } else {
            self.oy -= n;
        }
        self.fit_page_cursor();
        false
    }

    /// window_copy_previous_paragraph
    fn previous_paragraph(&mut self) {
        let mut oy = self.cursor_line();
        while oy > 0 && self.find_length(oy) == 0 {
            oy -= 1;
        }
        while oy > 0 && self.find_length(oy) > 0 {
            oy -= 1;
        }
        self.scroll_to(0, oy, false);
    }

    /// window_copy_next_paragraph
    fn next_paragraph(&mut self) {
        let mut oy = self.cursor_line();
        let maxy = self.hsize() + self.sy() - 1;
        while oy < maxy && self.find_length(oy) == 0 {
            oy += 1;
        }
        while oy < maxy && self.find_length(oy) > 0 {
            oy += 1;
        }
        let ox = self.find_length(oy);
        self.scroll_to(ox, oy, false);
    }

    /// window_copy_scroll_to: the cursor to (`px`, `py`), scrolling the line
    /// a quarter of the screen from the edge when it is off the screen.
    fn scroll_to(&mut self, px: usize, py: usize, no_redraw: bool) {
        let (hsize, sy) = (self.hsize(), self.sy());
        self.cx = px;
        if py >= hsize - self.oy && py < hsize - self.oy + sy {
            self.cy = py - (hsize - self.oy);
        } else {
            let gap = sy / 4;
            let offset = if py < sy {
                self.cy = py;
                0
            } else if py > hsize + sy - gap {
                self.cy = py - hsize;
                hsize
            } else {
                let offset = py + gap - sy;
                self.cy = py - offset;
                offset
            };
            self.oy = hsize - offset;
        }
        if !no_redraw {
            self.refresh_marks();
        }
        self.update_selection(false);
    }

    /// window_copy_cmd_scroll_to: the cursor's line to row `to`.
    fn scroll_to_row(&mut self, to: usize) -> Action {
        let below = self.hsize() - self.oy;
        if self.cy > to && self.oy >= self.cy - to {
            let delta = self.cy - to;
            self.scroll_up(delta);
            self.cy -= delta;
        } else if self.cy < to && below >= to - self.cy {
            let delta = to - self.cy;
            self.scroll_down(delta);
            self.cy += delta;
        }
        self.update_selection(false);
        Action::Redraw
    }

    /// window_copy_goto_line: the view `linestr` lines up the history.
    fn goto_line(&mut self, linestr: &str) {
        let Ok(lineno) = linestr.parse::<i64>() else { return };
        if !(-1..=i64::from(i32::MAX)).contains(&lineno) {
            return;
        }
        let hsize = self.hsize();
        self.oy = if lineno < 0 || lineno as usize > hsize { hsize } else { lineno as usize };
        self.update_selection(false);
    }

    /// window_copy_jump_to_mark: the cursor and the mark swap.
    fn jump_to_mark(&mut self) {
        let (tmx, tmy) = (self.cx, self.cursor_line());
        let hsize = self.hsize();
        self.cx = self.mx;
        if self.my < hsize {
            self.cy = 0;
            self.oy = hsize - self.my;
        } else {
            self.cy = self.my - hsize;
            self.oy = 0;
        }
        (self.mx, self.my) = (tmx, tmy);
        self.showmark = true;
        self.update_selection(false);
    }

    /// window_copy_cursor_jump
    fn cursor_jump(&mut self) {
        let Some(jc) = self.jumpchar else { return };
        let (oldy, oy) = (self.cy, self.oy);
        if let Some((px, py)) = self.read(self.cx + 1, self.cursor_line(), |r| r.cursor_jump(jc)) {
            self.acquire_cursor_down(oy, oldy, px, py, false);
        }
    }

    /// window_copy_cursor_jump_back
    fn cursor_jump_back(&mut self) {
        let Some(jc) = self.jumpchar else { return };
        let (oldy, oy) = (self.cy, self.oy);
        let found = self.read(self.cx, self.cursor_line(), |r| {
            r.cursor_left(false);
            r.cursor_jump_back(jc)
        });
        if let Some((px, py)) = found {
            self.acquire_cursor_up(oy, oldy, px, py);
        }
    }

    /// window_copy_cursor_jump_to
    fn cursor_jump_to(&mut self) {
        let Some(jc) = self.jumpchar else { return };
        let (oldy, oy) = (self.cy, self.oy);
        let found = self.read(self.cx + 2, self.cursor_line(), |r| {
            let found = r.cursor_jump(jc);
            if found {
                r.cursor_left(true);
            }
            found
        });
        if let Some((px, py)) = found {
            self.acquire_cursor_down(oy, oldy, px, py, false);
        }
    }

    /// window_copy_cursor_jump_to_back
    fn cursor_jump_to_back(&mut self) {
        let Some(jc) = self.jumpchar else { return };
        let (oldy, oy) = (self.cy, self.oy);
        let found = self.read(self.cx, self.cursor_line(), |r| {
            r.cursor_left(false);
            r.cursor_left(false);
            let found = r.cursor_jump_back(jc);
            if found {
                r.cursor_right(true, false);
            }
            found
        });
        if let Some((px, py)) = found {
            self.acquire_cursor_up(oy, oldy, px, py);
        }
    }

    /// window_copy_cursor_next_word
    fn cursor_next_word(&mut self, separators: &str) {
        let (oldy, oy) = (self.cy, self.oy);
        let (px, py) = self.read(self.cx, self.cursor_line(), |r| {
            r.cursor_next_word(separators);
            true
        }).expect("moved");
        self.acquire_cursor_down(oy, oldy, px, py, false);
    }

    /// window_copy_cursor_next_word_end_pos: in vi mode, the last character
    /// of the word rather than the cell after it.
    fn cursor_next_word_end_pos(&self, separators: &str) -> (usize, usize) {
        let vi = self.vi;
        self.read(self.cx, self.cursor_line(), |r| {
            if vi {
                if !r.in_set(WHITESPACE) {
                    r.cursor_right(false, false);
                }
                r.cursor_next_word_end(separators);
                r.cursor_left(true);
            } else {
                r.cursor_next_word_end(separators);
            }
            true
        })
        .expect("moved")
    }

    /// window_copy_cursor_next_word_end
    fn cursor_next_word_end(&mut self, separators: &str, no_reset: bool) {
        let (oldy, oy) = (self.cy, self.oy);
        let (px, py) = self.cursor_next_word_end_pos(separators);
        self.acquire_cursor_down(oy, oldy, px, py, no_reset);
    }

    /// window_copy_cursor_previous_word: emacs mode-keys stop at the end of
    /// a line that ends in whitespace.
    fn cursor_previous_word(&mut self, separators: &str, already: bool) {
        let (oldy, oy) = (self.cy, self.oy);
        let stop_at_eol = !self.vi;
        let (px, py) = self.read(self.cx, self.cursor_line(), |r| {
            r.cursor_previous_word(separators, already, stop_at_eol);
            true
        }).expect("moved");
        self.acquire_cursor_up(oy, oldy, px, py);
    }

    /// window_copy_cmd_previous_matching_bracket: the opening bracket that
    /// matches the closing one at the cursor (or, in emacs mode, before it;
    /// otherwise emacs goes to the previous word).
    fn previous_matching_bracket(&mut self, np: u32) {
        const OPEN: &[u8] = b"{[(";
        const CLOSE: &[u8] = b"}])";
        for _ in 0..np {
            let (mut px, mut py) = (self.cx, self.cursor_line());
            if self.find_length(py) == 0 {
                break;
            }
            let mut tried = false;
            let found = loop {
                match self.grid().data(px, py).byte().filter(|b| CLOSE.contains(b)) {
                    Some(b) => break Some(b),
                    None if !self.vi && !tried && px > 0 => {
                        px -= 1;
                        tried = true;
                    }
                    None => break None,
                }
            };
            let Some(found) = found else {
                if !self.vi {
                    self.cursor_previous_word("}])", true);
                }
                continue;
            };
            let start = OPEN[CLOSE.iter().position(|&b| b == found).expect("a closing bracket")];
            // Walk backward until the matching bracket is reached.
            let gd = self.grid();
            let mut n = 1;
            let mut failed = false;
            while n != 0 {
                if px == 0 {
                    if py == 0 {
                        failed = true;
                        break;
                    }
                    let mut xx;
                    loop {
                        py -= 1;
                        xx = gd.line_length(py);
                        if xx != 0 || py == 0 {
                            break;
                        }
                    }
                    if xx == 0 && py == 0 {
                        failed = true;
                        break;
                    }
                    px = xx - 1;
                } else {
                    px -= 1;
                }
                match gd.data(px, py).byte() {
                    Some(b) if b == found => n += 1,
                    Some(b) if b == start => n -= 1,
                    _ => {}
                }
            }
            if !failed {
                self.scroll_to(px, py, false);
            }
        }
    }

    /// window_copy_cmd_next_matching_bracket: the closing bracket that
    /// matches the opening one at or after the cursor; vi goes back to the
    /// opening bracket of a closing one, and searches the rest of the line.
    fn next_matching_bracket(&mut self, np: u32) {
        const OPEN: &[u8] = b"{[(";
        const CLOSE: &[u8] = b"}])";
        for _ in 0..np {
            let (mut px, mut py) = (self.cx, self.cursor_line());
            let mut xx = self.find_length(py);
            let yy = self.hsize() + self.sy() - 1;
            if xx == 0 {
                break;
            }
            let mut tried = false;
            let found = loop {
                let byte = self.grid().data(px, py).byte();
                if let Some(b) = byte
                    && self.vi
                    && CLOSE.contains(&b)
                {
                    // vi goes to the opening bracket of a closing one, and
                    // back where it was if there is none.
                    let (sx, sy) = (self.cx, self.cursor_line());
                    self.scroll_to(px, py, false);
                    self.previous_matching_bracket(np);
                    let at = self.grid().data(self.cx, self.cursor_line()).byte();
                    if at.is_some_and(|b| CLOSE.contains(&b)) {
                        self.scroll_to(sx, sy, false);
                    }
                    return;
                }
                match byte.filter(|b| OPEN.contains(b)) {
                    Some(b) => break Some(b),
                    None if !self.vi => {
                        if !tried && px <= xx {
                            px += 1;
                            tried = true;
                            continue;
                        }
                        break None;
                    }
                    None => {
                        // vi looks on until the end of the line.
                        if px > xx {
                            if py == yy || !self.grid().wrapped(py) {
                                break None;
                            }
                            px = 0;
                            py += 1;
                            xx = self.find_length(py);
                        } else {
                            px += 1;
                        }
                    }
                }
            };
            let Some(found) = found else {
                if !self.vi {
                    self.cursor_next_word_end("{[(", false);
                }
                continue;
            };
            let end = CLOSE[OPEN.iter().position(|&b| b == found).expect("an opening bracket")];
            // Walk forward until the matching bracket is reached.
            let gd = self.grid();
            let mut n = 1;
            let mut failed = false;
            while n != 0 {
                if px > xx {
                    if py == yy {
                        failed = true;
                        break;
                    }
                    px = 0;
                    py += 1;
                    xx = gd.line_length(py);
                } else {
                    px += 1;
                }
                match gd.data(px, py).byte() {
                    Some(b) if b == found => n += 1,
                    Some(b) if b == end => n -= 1,
                    _ => {}
                }
            }
            if !failed {
                self.scroll_to(px, py, false);
            }
        }
    }

    // ---- Searching -------------------------------------------------------

    /// window_copy_move_left over the backing, wrapping to the bottom right.
    fn move_left(&self, fx: &mut usize, fy: &mut usize, wrapflag: bool) {
        if *fx == 0 {
            if *fy == 0 {
                if wrapflag {
                    *fx = self.sx() - 1;
                    *fy = self.hsize() + self.sy() - 1;
                }
                return;
            }
            *fx = self.sx() - 1;
            *fy -= 1;
        } else {
            *fx -= 1;
        }
    }

    /// window_copy_move_right over the backing, wrapping to the top left.
    fn move_right(&self, fx: &mut usize, fy: &mut usize, wrapflag: bool) {
        if *fx == self.sx() - 1 {
            if *fy == self.hsize() + self.sy() - 1 {
                if wrapflag {
                    *fx = 0;
                    *fy = 0;
                }
                return;
            }
            *fx = 0;
            *fy += 1;
        } else {
            *fx += 1;
        }
    }

    /// window_copy_search_mark_at: the screen cell of (`px`, `py`).
    fn search_mark_at(&self, px: usize, py: usize) -> Option<usize> {
        let top = self.hsize() - self.oy;
        let at = (py >= top && py < top + self.sy()).then(|| (py - top) * self.sx() + px)?;
        // A cursor past the last column of the last row is past the screen.
        (at < self.sx() * self.sy()).then_some(at)
    }

    /// window_copy_move_after_search_mark: past the match at (`fx`, `fy`).
    fn move_after_search_mark(&self, fx: &mut usize, fy: &mut usize, wrapflag: bool) {
        let Some(marks) = &self.searchmark else { return };
        let Some(start) = self.search_mark_at(*fx, *fy).filter(|&at| marks[at] != 0) else { return };
        let last = (self.sx() - 1, self.hsize() + self.sy() - 1);
        while let Some(at) = self.search_mark_at(*fx, *fy) {
            if marks[at] != marks[start] {
                break;
            }
            // Stop if not wrapping and at the end of the grid.
            if !wrapflag && (*fx, *fy) == last {
                break;
            }
            self.move_right(fx, fy, wrapflag);
        }
    }

    /// window_copy_search: searches for searchstr up or down (`direction`)
    /// from the cursor and moves to the match; returns whether it found one.
    fn search(&mut self, direction: bool, regex: bool) -> bool {
        let Some(s) = self.searchstr.clone() else { return false };
        let regex = regex && s.contains(['^', '$', '*', '+', '(', ')', '?', '[', ']', '.', '\\']);
        self.searchdirection = direction;
        if self.timeout {
            return false;
        }
        let visible_only = match &self.pane_search {
            Some(last) if !self.searchall && last.regex == regex => last.text == s,
            _ => {
                self.searchall = false;
                false
            }
        };
        if !visible_only && self.searchmark.is_some() {
            self.clear_marks();
        }
        self.pane_search = Some(PaneSearch { text: s.clone(), regex });
        self.result.search = self.pane_search.clone();

        let mut fx = self.cx;
        let mut fy = self.cursor_line();
        let ss = search_cells(&s);
        if ss.is_empty() {
            return false;
        }
        let wrapflag = WRAP_SEARCH;
        let cis = is_lowercase(&s);
        let endline = if direction {
            // Emacs leaves the cursor after the match and vi at its start,
            // so vi starts the next search after the match it is on.
            if self.vi {
                if self.searchmark.is_some() {
                    self.move_after_search_mark(&mut fx, &mut fy, wrapflag);
                } else {
                    self.move_right(&mut fx, &mut fy, wrapflag);
                }
            }
            self.hsize() + self.sy() - 1
        } else {
            self.move_left(&mut fx, &mut fy, wrapflag);
            0
        };
        let found = self.search_jump(&ss, fx, fy, endline, cis, wrapflag, direction, regex);
        if found {
            self.search_marks(Some(&ss), regex, visible_only);
            let (mut fx, mut fy) = (self.cx, self.cursor_line());
            // When searching forward, if the cursor is not at the beginning
            // of the mark, search again.
            let at = self.search_mark_at(fx, fy);
            let inside = self.searchmark.as_ref().zip(at).is_some_and(|(m, at)| at > 0 && m[at] == m[at - 1]);
            if direction && inside {
                self.move_after_search_mark(&mut fx, &mut fy, wrapflag);
                self.search_jump(&ss, fx, fy, endline, cis, wrapflag, direction, regex);
                (fx, fy) = (self.cx, self.cursor_line());
            }
            if direction {
                // Emacs puts the cursor just after the match.
                if !self.vi {
                    self.move_after_search_mark(&mut fx, &mut fy, wrapflag);
                    self.cx = fx;
                    self.cy = fy + self.oy - self.hsize();
                }
            } else if let Some(marks) = self.searchmark.clone()
                && let Some(start) = self.search_mark_at(fx, fy)
            {
                // Searching backward, the cursor goes to the start of the match.
                while let Some(at) = self.search_mark_at(fx, fy) {
                    if marks[at] != marks[start] {
                        break;
                    }
                    self.cx = fx;
                    self.cy = fy + self.oy - self.hsize();
                    if at == 0 {
                        break;
                    }
                    self.move_left(&mut fx, &mut fy, false);
                }
            }
        }
        found
    }

    /// window_copy_search_jump: the first match from (`fx`, `fy`) towards
    /// `endline`, then from the other end of the grid when `wrap`; the
    /// cursor goes to it.
    #[allow(clippy::too_many_arguments)]
    fn search_jump(
        &mut self,
        sgd: &[SearchCell],
        fx: usize,
        fy: usize,
        endline: usize,
        cis: bool,
        wrap: bool,
        direction: bool,
        regex: bool,
    ) -> bool {
        let reg = if regex {
            match Regex::new(&pattern(sgd), cis) {
                Some(reg) => Some(reg),
                None => return false,
            }
        } else {
            None
        };
        let found = {
            let gd = self.grid();
            let sx = gd.sx;
            let mut fx = fx;
            let mut found = None;
            if direction {
                for i in fy..=endline {
                    let px = match &reg {
                        Some(reg) => gd.search_lr_regex(i, fx, sx, reg).map(|(px, _)| px),
                        None => gd.search_lr(sgd, i, fx, sx, cis),
                    };
                    if let Some(px) = px {
                        found = Some((px, i));
                        break;
                    }
                    fx = 0;
                }
            } else {
                // Line i - 1, from the cursor's line up to endline.
                let mut i = fy + 1;
                while endline < i {
                    let px = match &reg {
                        Some(reg) => match gd.search_rl_regex(i - 1, 0, fx + 1, reg) {
                            Some((px, width)) => {
                                let (px, line) = search_back_overlap(&gd, reg, px, width, i, endline);
                                i = line;
                                Some(px)
                            }
                            None => None,
                        },
                        None => gd.search_rl(sgd, i - 1, 0, fx + 1, cis),
                    };
                    if let Some(px) = px {
                        found = Some((px, i - 1));
                        break;
                    }
                    fx = sx - 1;
                    i -= 1;
                }
            }
            found
        };
        if let Some((px, py)) = found {
            self.scroll_to(px, py, true);
            return true;
        }
        if wrap {
            let (wx, wy) = if direction { (0, 0) } else { (self.sx() - 1, self.hsize() + self.sy() - 1) };
            return self.search_jump(sgd, wx, wy, fy, cis, false, direction, regex);
        }
        false
    }

    /// window_copy_visible_lines: the screen's lines, from the start of the
    /// line wrapped into its first row.
    fn visible_lines(&self) -> (usize, usize) {
        let gd = self.grid();
        let mut start = self.hsize() - self.oy;
        while start > 0 && gd.wrapped(start - 1) {
            start -= 1;
        }
        (start, self.hsize() - self.oy + self.sy())
    }

    /// window_copy_search_mark_match: marks the screen cells of a match
    /// `width` cells long with the next generation; returns its width.
    fn search_mark_match(&mut self, px: usize, py: usize, width: usize) -> usize {
        let Some(b) = self.search_mark_at(px, py) else { return width };
        let size = self.sx() * self.sy();
        // window_copy_clip_width
        let w = if b + width > size { size - b } else { width };
        let generation = self.searchgen;
        if let Some(marks) = self.searchmark.as_mut() {
            for mark in &mut marks[b..b + w] {
                if *mark == 0 {
                    *mark = generation;
                }
            }
        }
        self.searchgen = if self.searchgen == u8::MAX { 1 } else { self.searchgen + 1 };
        w
    }

    /// window_copy_search_marks: marks the matches of searchstr (laid out
    /// as `ssp`) on the screen and counts them in the whole backing, for at
    /// most WINDOW_COPY_SEARCH_ALL_TIMEOUT before counting the screen only.
    fn search_marks(&mut self, ssp: Option<&[SearchCell]>, regex: bool, visible_only: bool) -> bool {
        let Some(s) = self.searchstr.clone() else { return false };
        let owned;
        let ss = match ssp {
            Some(ss) => ss,
            None => {
                owned = search_cells(&s);
                &owned
            }
        };
        let cis = is_lowercase(&s);
        let reg = if regex {
            match Regex::new(&pattern(ss), cis) {
                Some(reg) => Some(reg),
                None => return false,
            }
        } else {
            None
        };
        let (sx, sy) = (self.sx(), self.sy());
        let tstart = Instant::now();
        let (mut start, mut end, mut stop) = if visible_only {
            let (start, end) = self.visible_lines();
            (start, end, None)
        } else {
            (0, self.hsize() + sy, Some(tstart + SEARCH_ALL_TIMEOUT))
        };
        let mut nfound: i64 = 0;
        let mut stopped = false;
        loop {
            self.searchmark = Some(vec![0; sx * sy]);
            self.searchgen = 1;
            for py in start..end {
                let mut px = 0;
                loop {
                    let found = match &reg {
                        Some(reg) => self.grid().search_lr_regex(py, px, sx, reg),
                        None => self.grid().search_lr(ss, py, px, sx, cis).map(|px| (px, ss.len())),
                    };
                    let Some((x, width)) = found else { break };
                    nfound += 1;
                    // A match always takes a cell, which tmux takes for granted.
                    px = x + self.search_mark_match(x, py, width).max(1);
                }
                let t = Instant::now();
                if t - tstart > SEARCH_TIMEOUT {
                    self.timeout = true;
                    break;
                }
                if stop.is_some_and(|stop| t > stop) {
                    stopped = true;
                    break;
                }
            }
            if self.timeout {
                self.clear_marks();
                return true;
            }
            if stopped && stop.is_some() {
                // Try again but just the visible context.
                (start, end) = self.visible_lines();
                stop = None;
                continue;
            }
            break;
        }
        if !visible_only {
            if stopped {
                self.searchcount = match nfound {
                    n if n > 1000 => 1000,
                    n if n > 100 => 100,
                    n if n > 10 => 10,
                    _ => -1,
                };
                self.searchmore = true;
            } else {
                self.searchcount = nfound;
                self.searchmore = false;
            }
        }
        true
    }

    /// window_copy_clear_marks
    fn clear_marks(&mut self) {
        self.searchmark = None;
    }

    /// window_copy_match_at_cursor: the text of the search match at the
    /// cursor, or ending just before it.
    fn match_at_cursor(&self) -> Option<Vec<u8>> {
        let marks = self.searchmark.as_ref()?;
        let mut at = self.search_mark_at(self.cx, self.cursor_line()).filter(|&at| at < marks.len())?;
        if marks[at] == 0 {
            // Allow one position after the match.
            if at == 0 || marks[at - 1] == 0 {
                return None;
            }
            at -= 1;
        }
        // window_copy_match_start_end
        let mark = marks[at];
        let (mut start, mut end) = (at, at);
        while start != 0 && marks[start] == mark {
            start -= 1;
        }
        if marks[start] != mark {
            start += 1;
        }
        while end != marks.len() - 1 && marks[end] == mark {
            end += 1;
        }
        if marks[end] != mark {
            end -= 1;
        }
        let (sx, top) = (self.sx(), self.hsize() - self.oy);
        let gd = self.grid();
        let mut buf = Vec::new();
        for at in start..=end {
            let d = gd.data(at % sx, top + at / sx);
            if d != Data::Padding {
                d.push_to(&mut buf);
            }
        }
        (!buf.is_empty()).then_some(buf)
    }
}

/// The search string as regcomp gets it: the cells' text
/// (window_copy_stringify of the search screen).
fn pattern(cells: &[SearchCell]) -> String {
    cells.iter().map(|c| c.text.as_str()).collect()
}

/// window_copy_search_back_overlap: a match found going up that starts a
/// line may be the end of a longer match from the lines wrapped into it;
/// the start moves up to the longest such match ending at the same cell.
/// `py` is one past the match's line; returns the match's column and `py`.
fn search_back_overlap(gd: &Grid, reg: &Regex, px: usize, width: usize, py: usize, endline: usize) -> (usize, usize) {
    let sx = gd.sx;
    let (mut oldendx, mut oldendy) = (px + width, py - 1);
    while oldendx > sx - 1 {
        oldendx -= sx;
        oldendy += 1;
    }
    let (mut endx, mut endy) = (oldendx, oldendy);
    let (mut ppx, mut ppy) = (px, py);
    let (mut px, mut py) = (px, py);
    let mut found = true;
    while found && px == 0 && py - 1 > endline && gd.wrapped(py - 2) && (endx, endy) == (oldendx, oldendy) {
        py -= 1;
        match gd.search_rl_regex(py - 1, 0, sx, reg) {
            Some((x, w)) => {
                px = x;
                (endx, endy) = (px + w, py - 1);
                while endx > sx - 1 {
                    endx -= sx;
                    endy += 1;
                }
                if (endx, endy) == (oldendx, oldendy) {
                    (ppx, ppy) = (px, py);
                }
            }
            None => found = false,
        }
    }
    (ppx, ppy)
}

#[cfg(test)]
mod tests {
    use super::*;
    use CopyCommand::*;

    fn term(cols: u16, rows: u16, text: &str) -> Terminal {
        let mut t = Terminal::new(cols, rows, 1000);
        t.feed(text.replace('\n', "\r\n").as_bytes());
        t
    }

    fn mode(t: &Terminal, keys: ModeKeys) -> CopyMode {
        CopyMode::new(t.snapshot(), keys, None)
    }

    fn run(m: &mut CopyMode, command: CopyCommand) -> CopyResult {
        m.command(&command, 1, || unreachable!())
    }

    fn run_n(m: &mut CopyMode, command: CopyCommand, count: u32) -> CopyResult {
        m.command(&command, count, || unreachable!())
    }

    /// The cursor as (column, backing line).
    fn pos(m: &CopyMode) -> (usize, usize) {
        (m.cx, m.cursor_line())
    }

    fn copied(r: &CopyResult) -> String {
        match &r.copied {
            Some(Copied::New(data) | Copied::Append(data)) => String::from_utf8(data.clone()).unwrap(),
            None => panic!("nothing copied: {r:?}"),
        }
    }

    /// `seq 1 100` run at a prompt in a 20x10 pane: lines 0 to 101, the
    /// screen shows "92" to "100" and the prompt, with the cursor after it.
    fn seq() -> Terminal {
        let mut text = "$ seq 1 100\n".to_owned();
        for i in 1..=100 {
            text.push_str(&format!("{i}\n"));
        }
        text.push_str("$ ");
        term(20, 10, &text)
    }

    #[test]
    fn starts_at_the_terminal_cursor_on_a_frozen_copy() {
        let mut t = seq();
        let mut m = mode(&t, ModeKeys::Vi);
        assert_eq!((m.cx, m.cy, m.oy, m.hsize()), (2, 9, 0, 92));
        assert_eq!(m.position().as_deref(), Some("[0/92]"));
        // Output after entering does not reach the copy.
        t.feed(b"\r\nmore\r\nlines\r\n");
        run_n(&mut m, CursorUp, 3);
        assert_eq!(pos(&m), (2, 98));
        assert_eq!(m.term_mut().row(6)[0].char(), Some('9'));
    }

    #[test]
    fn up_keeps_the_end_of_line_and_vi_copies_through_the_cursor() {
        let mut m = mode(&seq(), ModeKeys::Vi);
        // From past the end of the prompt, the cursor sticks to line ends.
        run(&mut m, CursorUp);
        assert_eq!(pos(&m), (3, 100));
        run_n(&mut m, CursorUp, 2);
        assert_eq!(pos(&m), (2, 98));
        run(&mut m, BeginSelection);
        run(&mut m, CursorUp);
        run(&mut m, StartOfLine);
        assert_eq!(pos(&m), (0, 97));
        let first = m.term_mut().first_line();
        let p = |line: u64, col| Point { line: first + line, col };
        assert_eq!(m.term_mut().selection(), Some(Selection { start: p(97, 0), end: p(98, 2), rect: false }));
        // vi keeps the character under the cursor; past the end of the
        // line the newline is kept too.
        let r = run(&mut m, CopyPipeAndCancel);
        assert!(r.cancel);
        assert_eq!(copied(&r), "97\n98\n");
    }

    #[test]
    fn emacs_copies_up_to_the_cursor() {
        let mut m = mode(&seq(), ModeKeys::Emacs);
        run_n(&mut m, CursorUp, 3);
        run(&mut m, BeginSelection);
        run(&mut m, CursorUp);
        run(&mut m, StartOfLine);
        let first = m.term_mut().first_line();
        let p = |line: u64, col| Point { line: first + line, col };
        assert_eq!(m.term_mut().selection(), Some(Selection { start: p(97, 0), end: p(98, 1), rect: false }));
        assert_eq!(copied(&run(&mut m, CopyPipeAndCancel)), "97\n98");
        // Within a line the cell under the cursor is left out.
        let mut m = mode(&term(20, 3, "hello world"), ModeKeys::Emacs);
        run(&mut m, StartOfLine);
        run(&mut m, BeginSelection);
        run_n(&mut m, CursorRight, 4);
        assert_eq!(copied(&run(&mut m, CopyPipeAndCancel)), "hell");
        let mut m = mode(&term(20, 3, "hello world"), ModeKeys::Vi);
        run(&mut m, StartOfLine);
        run(&mut m, BeginSelection);
        run_n(&mut m, CursorRight, 4);
        assert_eq!(copied(&run(&mut m, CopyPipeAndCancel)), "hello");
    }

    #[test]
    fn copying_without_a_selection_takes_nothing_and_cancels() {
        let mut m = mode(&seq(), ModeKeys::Vi);
        let r = run(&mut m, CopyPipeAndCancel);
        assert_eq!((r.cancel, r.copied), (true, None));
        let r = run(&mut m, ClearSelection);
        assert!(!r.cancel);
    }

    #[test]
    fn word_motions_follow_word_separators() {
        let t = term(40, 3, "foo.bar  baz-qux end\n");
        let mut m = mode(&t, ModeKeys::Vi);
        run(&mut m, CursorUp);
        run(&mut m, StartOfLine);
        let cols = |m: &mut CopyMode, command: CopyCommand| {
            run(m, command);
            m.cx
        };
        assert_eq!(cols(&mut m, NextWord), 3);
        assert_eq!(cols(&mut m, NextWord), 4);
        assert_eq!(cols(&mut m, NextWord), 9);
        assert_eq!(cols(&mut m, NextWordEnd), 11);
        assert_eq!(cols(&mut m, PreviousWord), 9);
        assert_eq!(cols(&mut m, PreviousWord), 4);
        assert_eq!(cols(&mut m, NextSpace), 9);
        assert_eq!(cols(&mut m, NextSpaceEnd), 15);
        assert_eq!(cols(&mut m, PreviousSpace), 9);
        assert_eq!(cols(&mut m, EndOfLine), 20);
        assert_eq!(cols(&mut m, BackToIndentation), 0);
        // Emacs ends a word after its last character, vi on it.
        let mut m = mode(&t, ModeKeys::Emacs);
        run(&mut m, CursorUp);
        run(&mut m, StartOfLine);
        assert_eq!(cols(&mut m, NextWordEnd), 3);
        assert_eq!(cols(&mut m, NextWordEnd), 4);
        assert_eq!(cols(&mut m, NextWordEnd), 7);
    }

    #[test]
    fn jumps_find_characters_on_the_line() {
        let mut m = mode(&term(20, 3, "a,b,c,d\n"), ModeKeys::Vi);
        run(&mut m, CursorUp);
        run(&mut m, StartOfLine);
        run(&mut m, JumpForward(",".to_owned()));
        assert_eq!(m.cx, 1);
        run(&mut m, JumpAgain);
        assert_eq!(m.cx, 3);
        run(&mut m, JumpReverse);
        assert_eq!(m.cx, 1);
        run(&mut m, StartOfLine);
        // jump-to stops before the character, from the one after the next.
        run(&mut m, JumpToForward(",".to_owned()));
        assert_eq!(m.cx, 2);
        run(&mut m, EndOfLine);
        run(&mut m, JumpToBackward(",".to_owned()));
        assert_eq!(m.cx, 6);
        run_n(&mut m, JumpBackward("b".to_owned()), 1);
        assert_eq!(m.cx, 2);
        // Nothing to find: the cursor stays.
        run(&mut m, JumpForward("z".to_owned()));
        assert_eq!(m.cx, 2);
    }

    #[test]
    fn pages_and_history_ends_scroll_the_view() {
        let mut m = mode(&seq(), ModeKeys::Vi);
        run(&mut m, PageUp);
        assert_eq!((m.oy, m.cy), (8, 9));
        assert_eq!(m.position().as_deref(), Some("[8/92]"));
        run(&mut m, HalfpageUp);
        assert_eq!(m.oy, 13);
        run(&mut m, HalfpageDown);
        run(&mut m, PageDown);
        assert_eq!(m.oy, 0);
        run(&mut m, HistoryTop);
        assert_eq!((m.cx, m.cy, m.oy), (0, 0, 92));
        assert_eq!(m.term_mut().view_offset(), 92);
        run(&mut m, HistoryBottom);
        assert_eq!((m.cy, m.oy, m.cx), (9, 0, 1));
        run(&mut m, GotoLine("40".to_owned()));
        assert_eq!((m.oy, m.cy), (40, 9));
        // scroll-middle brings the cursor's line to the middle row.
        run(&mut m, ScrollMiddle);
        assert_eq!((m.oy, m.cy), (35, 4));
        run(&mut m, GotoLine("1000".to_owned()));
        assert_eq!(m.oy, 92);
        run(&mut m, GotoLine("x".to_owned()));
        assert_eq!(m.oy, 92);
        run(&mut m, MiddleLine);
        run(&mut m, BottomLine);
        assert_eq!(m.cy, 9);
        run(&mut m, TopLine);
        assert_eq!(m.cy, 0);
        // scroll-down moves the view, not the cursor's row.
        run_n(&mut m, ScrollDown, 3);
        assert_eq!((m.oy, m.cy), (89, 0));
        // Too close to the top to bring the line to the middle.
        run(&mut m, ScrollMiddle);
        assert_eq!((m.oy, m.cy), (89, 0));
    }

    #[test]
    fn line_selection_and_the_other_end() {
        let line_selection = || {
            let mut m = mode(&seq(), ModeKeys::Vi);
            run_n(&mut m, CursorUp, 2);
            run(&mut m, SelectLine);
            assert_eq!(pos(&m), (2, 99));
            // Going up from the selected line turns the selection around.
            run(&mut m, CursorUp);
            assert_eq!(pos(&m), (0, 98));
            m
        };
        assert_eq!(copied(&run(&mut line_selection(), CopyPipeAndCancel)), "98\n99\n");
        let mut m = line_selection();
        run(&mut m, OtherEnd);
        assert_eq!(pos(&m), (2, 99));
        assert_eq!(copied(&run(&mut m, CopyPipeAndCancel)), "98\n99\n");
    }

    #[test]
    fn rectangles_copy_columns() {
        let mut m = mode(&term(20, 4, "abcdef\nghijkl\nmnopqr\n"), ModeKeys::Vi);
        run(&mut m, HistoryTop);
        run(&mut m, CursorRight);
        run(&mut m, BeginSelection);
        run(&mut m, RectangleToggle);
        run_n(&mut m, CursorDown, 2);
        run_n(&mut m, CursorRight, 2);
        let sel = m.term_mut().selection().unwrap();
        assert!(sel.rect);
        assert_eq!((sel.start.col, sel.end.col), (1, 3));
        assert_eq!(copied(&run(&mut m, CopyPipeAndCancel)), "bcd\nhij\nnop");
    }

    #[test]
    fn copy_end_of_line_copies_from_the_cursor() {
        let mut m = mode(&term(20, 4, "one two\nthree\n"), ModeKeys::Vi);
        run(&mut m, HistoryTop);
        run(&mut m, NextWord);
        // vi copies the line's end too.
        assert_eq!(copied(&run(&mut m, CopyPipeEndOfLineAndCancel)), "two\n");
        let mut m = mode(&term(20, 4, "one two\nthree\n"), ModeKeys::Vi);
        run(&mut m, HistoryTop);
        let r = run_n(&mut m, CopyPipeEndOfLineAndCancel, 2);
        assert_eq!((copied(&r), r.cancel), ("one two\nthree\n".to_owned(), true));
    }

    #[test]
    fn append_selection_appends() {
        let mut m = mode(&term(20, 3, "abc"), ModeKeys::Vi);
        run(&mut m, StartOfLine);
        run(&mut m, BeginSelection);
        run(&mut m, CursorRight);
        let r = run(&mut m, AppendSelectionAndCancel);
        assert_eq!((r.copied, r.cancel), (Some(Copied::Append(b"ab".to_vec())), true));
    }

    /// Lines 0 to 4 of a 20x5 pane without history.
    fn words() -> Terminal {
        term(20, 5, "alpha foo\nbeta\nfoo gamma\nFOO\n$ ")
    }

    #[test]
    fn vi_searches_stop_at_the_start_of_matches_and_count_them() {
        let mut m = mode(&words(), ModeKeys::Vi);
        let r = run(&mut m, SearchForward(SearchArg::Text("foo".to_owned())));
        assert_eq!(r.search, Some(PaneSearch { text: "foo".to_owned(), regex: false }));
        // A search in lowercase ignores case; it wraps from the bottom.
        assert_eq!(pos(&m), (6, 0));
        assert_eq!(m.position().as_deref(), Some("[0/0] (3 results)"));
        let (marks, current) = m.marks();
        assert_eq!(marks.len(), 3);
        assert_eq!((marks[0].start.col, marks[0].end.col, current), (6, 8, Some(0)));
        run(&mut m, SearchAgain);
        assert_eq!(pos(&m), (0, 2));
        run(&mut m, SearchAgain);
        assert_eq!(pos(&m), (0, 3));
        run(&mut m, SearchReverse);
        assert_eq!(pos(&m), (0, 2));
        assert_eq!(m.marks().1, Some(1));
        // Copying without a selection takes the match at the cursor.
        assert_eq!(copied(&run(&mut m, CopyPipeAndCancel)), "foo");
    }

    #[test]
    fn emacs_searches_leave_the_cursor_after_the_match() {
        let mut m = mode(&words(), ModeKeys::Emacs);
        run(&mut m, SearchForward(SearchArg::Text("foo".to_owned())));
        assert_eq!(pos(&m), (9, 0));
        // The match before the cursor is the current one.
        assert_eq!(m.marks().1, Some(0));
        run(&mut m, SearchBackward(SearchArg::Text("FOO".to_owned())));
        // In capitals the search keeps case.
        assert_eq!(pos(&m), (0, 3));
        assert_eq!(m.position().as_deref(), Some("[0/0] (1 results)"));
        // Moving clears the marks in emacs; vi keeps them.
        run(&mut m, CursorUp);
        assert!(m.searchmark.is_none());
    }

    #[test]
    fn regular_expressions_and_the_word_at_the_cursor() {
        let mut m = mode(&words(), ModeKeys::Vi);
        let r = run(&mut m, SearchForward(SearchArg::Text("g.mma".to_owned())));
        assert_eq!(r.search, Some(PaneSearch { text: "g.mma".to_owned(), regex: true }));
        assert_eq!(pos(&m), (4, 2));
        run(&mut m, HistoryTop);
        run(&mut m, SearchForward(SearchArg::Text("^b".to_owned())));
        assert_eq!(pos(&m), (0, 1));
        run(&mut m, SearchBackward(SearchArg::CursorWord));
        assert_eq!(m.searchstr.as_deref(), Some("beta"));
        run(&mut m, SearchForward(SearchArg::Text("[".to_owned())));
        assert_eq!(pos(&m), (0, 1));
    }

    #[test]
    fn incremental_searches_start_again_from_where_they_started() {
        let mut m = mode(&words(), ModeKeys::Emacs);
        run(&mut m, SearchForwardIncremental("=".to_owned()));
        assert_eq!(m.search_origin, Some((2, 4, 0)));
        run(&mut m, SearchForwardIncremental("=f".to_owned()));
        assert_eq!(pos(&m), (7, 0));
        run(&mut m, SearchForwardIncremental("=fo".to_owned()));
        assert_eq!(pos(&m), (8, 0));
        run(&mut m, SearchForwardIncremental("+fo".to_owned()));
        assert_eq!(pos(&m), (2, 2));
        // Up from just after a match finds that match.
        run(&mut m, SearchForwardIncremental("-fo".to_owned()));
        assert_eq!(pos(&m), (0, 2));
        // An edit goes back to the start before searching.
        run(&mut m, SearchForwardIncremental("=gam".to_owned()));
        assert_eq!(pos(&m), (7, 2));
        // No match: the cursor stays where the search started.
        run(&mut m, SearchForwardIncremental("=zzz".to_owned()));
        assert!(m.searchmark.is_none());
        assert_eq!(pos(&m), (2, 4));
    }

    /// "abcdefghijklmno xyz" wrapped in a 10 column pane, then "next"; the
    /// expected results are what tmux 3.6 does.
    fn wrapped(keys: ModeKeys) -> CopyMode {
        let mut m = mode(&term(10, 5, "abcdefghijklmno xyz\nnext\n"), keys);
        run(&mut m, HistoryTop);
        m
    }

    #[test]
    fn wrapped_lines_are_one_line() {
        let mut m = wrapped(ModeKeys::Vi);
        run(&mut m, SelectLine);
        assert_eq!(pos(&m), (9, 1));
        assert_eq!(copied(&run(&mut m, CopyPipeAndCancel)), "abcdefghijklmno xyz\n");
        let mut m = wrapped(ModeKeys::Emacs);
        run(&mut m, BeginSelection);
        run(&mut m, CursorDown);
        run(&mut m, EndOfLine);
        assert_eq!(pos(&m), (9, 1));
        assert_eq!(copied(&run(&mut m, CopyPipeAndCancel)), "abcdefghijklmno xyz");
        let mut m = wrapped(ModeKeys::Vi);
        run(&mut m, EndOfLine);
        assert_eq!(pos(&m), (9, 1));
        run(&mut m, StartOfLine);
        assert_eq!(pos(&m), (0, 0));
        run_n(&mut m, CursorDown, 2);
        assert_eq!(pos(&m), (0, 2));
    }

    #[test]
    fn searches_and_words_cross_wraps() {
        let mut m = wrapped(ModeKeys::Vi);
        let search = |m: &mut CopyMode, s: &str| {
            run(m, SearchForward(SearchArg::Text(s.to_owned())));
            pos(m)
        };
        assert_eq!(search(&mut m, "ij"), (8, 0));
        assert_eq!(search(&mut m, "o x"), (4, 1));
        assert_eq!(search(&mut m, "n.x"), (0, 2));
        run(&mut m, NextWord);
        assert_eq!(pos(&m), (1, 4));
        run(&mut m, PreviousWord);
        assert_eq!(pos(&m), (0, 2));
    }

    #[test]
    fn the_panes_last_search_is_searched_again_up() {
        let search = PaneSearch { text: "foo".to_owned(), regex: false };
        let mut m = CopyMode::new(words().snapshot(), ModeKeys::Vi, Some(search));
        run(&mut m, SearchAgain);
        assert_eq!(pos(&m), (0, 3));
    }

    #[test]
    fn brackets_and_paragraphs() {
        let mut m = mode(&term(30, 6, "a (b [c] d) e\n\npara\ngraph\n"), ModeKeys::Vi);
        run(&mut m, HistoryTop);
        run(&mut m, JumpForward("(".to_owned()));
        run(&mut m, NextMatchingBracket);
        assert_eq!(pos(&m), (10, 0));
        run(&mut m, NextMatchingBracket);
        assert_eq!(pos(&m), (2, 0));
        run(&mut m, NextParagraph);
        assert_eq!(pos(&m), (0, 1));
        run(&mut m, NextParagraph);
        assert_eq!(pos(&m), (0, 4));
        run(&mut m, PreviousParagraph);
        assert_eq!(pos(&m), (0, 1));
    }

    #[test]
    fn marks_swap_with_the_cursor() {
        let mut m = mode(&seq(), ModeKeys::Vi);
        run_n(&mut m, CursorUp, 5);
        run(&mut m, SetMark);
        run(&mut m, HistoryTop);
        run(&mut m, JumpToMark);
        assert_eq!(pos(&m), (2, 96));
        run(&mut m, JumpToMark);
        assert_eq!(pos(&m), (0, 0));
    }

    #[test]
    fn wide_characters_take_two_cells() {
        let mut m = mode(&term(20, 3, "a\u{65E5}\u{672C}b\n"), ModeKeys::Vi);
        run(&mut m, CursorUp);
        run(&mut m, StartOfLine);
        let mut cols = Vec::new();
        for _ in 0..4 {
            run(&mut m, CursorRight);
            cols.push(m.cx);
        }
        assert_eq!(cols, [1, 3, 5, 6]);
        run(&mut m, StartOfLine);
        run(&mut m, BeginSelection);
        run(&mut m, EndOfLine);
        assert_eq!(copied(&run(&mut m, CopyPipeAndCancel)), "a\u{65E5}\u{672C}b\n");
    }

    #[test]
    fn toggle_position_and_resize() {
        let mut m = mode(&seq(), ModeKeys::Vi);
        run(&mut m, TogglePosition);
        assert_eq!(m.position(), None);
        run(&mut m, TogglePosition);
        run_n(&mut m, CursorUp, 20);
        assert_eq!(pos(&m), (2, 81));
        m.resize(20, 5);
        // The cursor's line goes to the top of the view.
        assert_eq!((pos(&m), m.cy, m.term_mut().rows()), ((2, 81), 0, 5));
        assert_eq!(m.oy, m.hsize() - 81);
    }

    #[test]
    fn refresh_takes_a_new_copy() {
        let mut t = seq();
        let mut m = mode(&t, ModeKeys::Vi);
        t.feed(b"\r\nnew");
        m.command(&RefreshFromPane, 1, || t.snapshot());
        assert_eq!(m.hsize(), 93);
        assert_eq!(m.term_mut().row(9)[0].char(), Some('n'));
    }
}
