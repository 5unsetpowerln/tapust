//! Terminal state and the actions dispatched by the parser.

use std::io::Write as _;

use unicode_width::UnicodeWidthChar;

use crate::cell::{Cell, Semantic, StyleId, Width};
use crate::grid::{Grid, PromptMark, attach_mark, blankify};
use crate::palette::{default_palette, parse_color};
use crate::parser::Params;
use crate::style::{Pen, StyleTable, apply_sgr};
use crate::{
    CursorShape, Dirty, Message, Modes, MouseEncoding, MouseMode, Palette, Point, ProgressReport, ProgressState, Rgb,
    Selection,
};

const WIDTH_SHIFT: u32 = 21;
const WIDTH_MASK: u64 = 3 << WIDTH_SHIFT;
const WIDE: u64 = (Width::Wide as u64) << WIDTH_SHIFT;
const SPACER_TAIL: u64 = (Width::SpacerTail as u64) << WIDTH_SHIFT;
const SPACER_HEAD: u64 = (Width::SpacerHead as u64) << WIDTH_SHIFT;
const STYLE_MASK: u64 = 0xFFFF << 32;
/// `char_width` result for characters that are not printed.
const UNPRINTABLE: u8 = 0xFF;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Charset {
    Ascii,
    DecSpecial,
}

/// DEC special graphics for 0x5F..=0x7E.
const DEC_SPECIAL: [u32; 32] = [
    0x0020, 0x25C6, 0x2592, 0x2409, 0x240C, 0x240D, 0x240A, 0x00B0, 0x00B1, 0x2424, 0x240B, 0x2518, 0x2510, 0x250C,
    0x2514, 0x253C, 0x23BA, 0x23BB, 0x2500, 0x23BC, 0x23BD, 0x251C, 0x2524, 0x2534, 0x252C, 0x2502, 0x2264, 0x2265,
    0x03C0, 0x2260, 0x00A3, 0x00B7,
];

#[inline(always)]
pub(crate) fn char_width(cp: u32) -> u8 {
    if cp < 0x7F {
        return if cp >= 0x20 { 1 } else { UNPRINTABLE };
    }
    match char::from_u32(cp).and_then(|c| c.width()) {
        Some(w) => w as u8,
        None => UNPRINTABLE,
    }
}

/// VS16 requests emoji presentation, two columns wide, of a narrow character.
#[inline(always)]
fn widens(cp: u32, base: Cell) -> bool {
    cp == 0xFE0F && base.width() == Width::Narrow
}

/// Makes the narrow cell `x` wide, taking cell `x + 1` for its tail.
fn widen(row: &mut [Cell], x: usize) {
    let base = row[x];
    row[x] = Cell((base.0 & !WIDTH_MASK) | WIDE);
    if row[x + 1].width() == Width::Wide && x + 2 < row.len() {
        row[x + 2] = blankify(row[x + 2]);
    }
    row[x + 1] = Cell((base.0 & STYLE_MASK) | SPACER_TAIL);
}

/// Cursor state saved by DECSC.
#[derive(Clone)]
struct Saved {
    x: usize,
    y: usize,
    pending_wrap: bool,
    pen: Pen,
    charsets: [Charset; 4],
    gl: u8,
    origin: bool,
}

impl Default for Saved {
    fn default() -> Saved {
        Saved {
            x: 0,
            y: 0,
            pending_wrap: false,
            pen: Pen::DEFAULT,
            charsets: [Charset::Ascii; 4],
            gl: 0,
            origin: false,
        }
    }
}

/// ghostty keeps a cursor per screen. The active screen's is in `Term`; the
/// inactive screen's stays where it was when the screen was left.
#[derive(Clone, Copy, Default)]
struct ScreenCursor {
    x: usize,
    y: usize,
    pending_wrap: bool,
    semantic: Semantic,
}

/// ghostty's shell_redraws_prompt (OSC 133;A redraw): which rows of the
/// prompt the cursor is on a resize clears for the shell to redraw.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Redraw {
    None,
    All,
    Last,
}

/// What of a screen follows its cells through a resize as ghostty's tracked pins.
struct Tracked<'a> {
    cursor: &'a mut ScreenCursor,
    saved: &'a mut Option<Saved>,
    selection: &'a mut Option<Selection>,
    view_offset: &'a mut usize,
}

/// ghostty's Screen.resize. The cursor, the saved cursor, the selection's
/// ends and the top row of a view scrolled into the history are pins. The
/// cursor's row stays on the screen, and a saved cursor whose row left it
/// is clamped to it.
fn resize_screen(grid: &Grid, cols: usize, rows: usize, reflow: bool, redraw: Redraw, t: Tracked) -> Grid {
    let (first, hist) = (grid.first_line(), grid.hist_len());
    let cursor = t.cursor;
    // The order of ghostty's tracked pins after the cursor's.
    let mut pins = vec![(hist + cursor.y, cursor.x)];
    let view = (*t.view_offset > 0).then(|| {
        pins.push((hist - *t.view_offset, 0));
        pins.len() - 1
    });
    let selection = t.selection.as_ref().map(|s| {
        let (start, end) = s.ordered();
        for p in [start, end] {
            pins.push(p.line.checked_sub(first).map_or((0, 0), |line| (line as usize, p.col as usize)));
        }
        pins.len() - 2
    });
    let saved = t.saved.as_ref().map(|s| {
        pins.push((hist + s.y, s.x));
        pins.len() - 1
    });
    let mut grid = grid.resize(cols, rows, reflow, &mut pins);
    let hist = grid.hist_len();
    (cursor.x, cursor.y) = (pins[0].1, pins[0].0 - hist);
    // A pending wrap off the last column becomes the next cell.
    if cursor.pending_wrap && cursor.x != cols - 1 {
        cursor.pending_wrap = false;
        cursor.x += 1;
    }
    if cursor.semantic != Semantic::Output && redraw != Redraw::None {
        grid.clear_prompt(hist + cursor.y, redraw == Redraw::All);
    }
    if let (Some(s), Some(i)) = (t.saved.as_mut(), saved) {
        let (line, x) = pins[i];
        (s.x, s.y) = (x, line.saturating_sub(hist).min(rows - 1));
        if s.pending_wrap && x != cols - 1 {
            s.pending_wrap = false;
            s.x += 1;
        }
    }
    if let Some(i) = selection {
        let first = grid.first_line();
        let rect = t.selection.is_some_and(|s| s.rect);
        let point = |(line, col): (usize, usize)| Point { line: first + line as u64, col: col as u16 };
        let (start, end) = (pins[i], pins[i + 1]);
        // An end on a row cut below the screen moves to the last cell kept
        // above it; a selection with nothing kept is gone.
        let last_kept = (hist + rows - 1, if rect { end.1 } else { cols - 1 });
        *t.selection = match (start.0 < hist + rows, end.0 < hist + rows) {
            (true, true) => Some(Selection { start: point(start), end: point(end), rect }),
            (true, false) => Some(Selection { start: point(start), end: point(last_kept), rect }),
            _ => None,
        };
    }
    if let Some(i) = view {
        *t.view_offset = hist.saturating_sub(pins[i].0);
    }
    grid
}

fn default_tabs(cols: usize) -> Vec<bool> {
    (0..cols).map(|x| x != 0 && x % 8 == 0).collect()
}

#[inline(always)]
fn mark(dirty: &mut Dirty, all: bool, rows: usize, view_offset: usize, y: usize) {
    let vy = y + view_offset;
    if !all && vy < rows {
        dirty.rows[vy >> 6] |= 1 << (vy & 63);
    }
}

pub(crate) struct Term {
    /// The active screen.
    pub grid: Grid,
    /// The inactive screen: the alternate screen, or the primary while the
    /// alternate screen is active.
    pub other: Grid,
    pub alt_active: bool,
    /// The active screen's selection. ghostty keeps one per screen but
    /// clears it on every switch, so the inactive one is never seen again.
    pub selection: Option<Selection>,
    pub cols: usize,
    pub rows: usize,
    pub x: usize,
    pub y: usize,
    pub pending_wrap: bool,
    pen: Pen,
    /// The pen `tpl` was resolved from.
    tpl_pen: Pen,
    /// Cell template for printing: the pen's style id, no content.
    tpl: u64,
    pen_dirty: bool,
    /// Erased cells: no content, the pen's background.
    blank: Cell,
    blank_dirty: bool,
    pub styles: StyleTable,
    charsets: [Charset; 4],
    gl: u8,
    single_shift: u8,
    /// Insert mode, no autowrap, a non-ASCII charset or a pending single shift.
    slow: bool,
    insert: bool,
    autowrap: bool,
    origin: bool,
    lnm: bool,
    pub modes: Modes,
    pub cursor_visible: bool,
    pub cursor_shape: CursorShape,
    pub cursor_blink: bool,
    /// Whether the terminal's surface has focus (for DEC 1004 reports).
    pub focused: bool,
    /// The pointer's CSS cursor name (OSC 22, mouse reporting).
    pub mouse_shape: &'static str,
    /// ghostty's cursor semantic_content: what the shell is printing now
    /// (OSC 133), and whether input ends at the end of the line.
    pub semantic: Semantic,
    semantic_clear_eol: bool,
    /// How a click at the prompt moves the cursor (OSC 133;A options).
    pub prompt_click: Option<PromptClick>,
    top: usize,
    bottom: usize,
    tabs: Vec<bool>,
    /// DECSC slots of the primary and alternate screens.
    saved: [Option<Saved>; 2],
    other_cursor: ScreenCursor,
    redraw: Redraw,
    pub dirty: Dirty,
    /// Every row is marked in `dirty`.
    all_dirty: bool,
    pub view_offset: usize,
    pub replies: Vec<u8>,
    pub title: String,
    /// A program set the title; until then ghostty shows the pwd as title.
    seen_title: bool,
    pub cwd: Option<String>,
    pub bell: bool,
    pub messages: Vec<Message>,
    pub palette: Palette,
    /// What OSC 104/110/111 and RIS reset the palette to.
    pub base_palette: Palette,
    last_char: u32,
}

impl Term {
    pub fn new(cols: usize, rows: usize, hist: usize) -> Term {
        let mut t = Term {
            grid: Grid::new(cols, rows, hist),
            other: Grid::new(cols, rows, 0),
            alt_active: false,
            selection: None,
            cols,
            rows,
            x: 0,
            y: 0,
            pending_wrap: false,
            pen: Pen::DEFAULT,
            tpl_pen: Pen::DEFAULT,
            tpl: 0,
            pen_dirty: false,
            blank: Cell::EMPTY,
            blank_dirty: false,
            styles: StyleTable::new(),
            charsets: [Charset::Ascii; 4],
            gl: 0,
            single_shift: 0,
            slow: false,
            insert: false,
            autowrap: true,
            origin: false,
            lnm: false,
            // ghostty starts with DEC 1007 (mouse_alternate_scroll) set.
            modes: Modes { alternate_scroll: true, ..Modes::default() },
            cursor_visible: true,
            cursor_shape: CursorShape::Block,
            // ghostty's default for DEC mode 12 (cursor-style-blink unset).
            cursor_blink: true,
            focused: true,
            mouse_shape: "text",
            semantic: Semantic::Output,
            semantic_clear_eol: false,
            prompt_click: None,
            top: 0,
            bottom: rows - 1,
            tabs: default_tabs(cols),
            saved: [None, None],
            other_cursor: ScreenCursor::default(),
            redraw: Redraw::All,
            dirty: Dirty { rows: vec![0; rows.div_ceil(64)] },
            all_dirty: false,
            view_offset: 0,
            replies: Vec::new(),
            title: String::new(),
            seen_title: false,
            cwd: None,
            bell: false,
            messages: Vec::new(),
            palette: default_palette(),
            base_palette: default_palette(),
            last_char: 0,
        };
        t.mark_all_dirty();
        t
    }

    // ---- Queries -------------------------------------------------------

    /// The slot shown on viewport row `y`.
    #[inline]
    pub fn view_slot(&self, y: usize) -> usize {
        if self.view_offset == 0 {
            self.grid.slot(y)
        } else {
            self.grid.line_slot(self.grid.hist_len() - self.view_offset + y)
        }
    }

    /// The scrollback of the primary screen.
    pub fn primary(&self) -> &Grid {
        if self.alt_active { &self.other } else { &self.grid }
    }

    #[inline]
    pub fn mark_all_dirty(&mut self) {
        if !self.all_dirty {
            self.set_all_dirty();
        }
    }

    pub fn clear_dirty(&mut self) {
        self.dirty.rows.fill(0);
        self.all_dirty = false;
    }

    fn set_all_dirty(&mut self) {
        self.all_dirty = true;
        let n = self.rows;
        for (i, w) in self.dirty.rows.iter_mut().enumerate() {
            let left = n - i * 64;
            *w = if left >= 64 { !0 } else { (1u64 << left) - 1 };
        }
    }

    pub fn scroll_view(&mut self, delta: isize) {
        let max = if self.alt_active { 0 } else { self.grid.hist_len() };
        let new = self.view_offset.saturating_add_signed(delta).min(max);
        if new != self.view_offset {
            self.view_offset = new;
            self.mark_all_dirty();
        }
    }

    // ---- Styles --------------------------------------------------------

    fn intern(&mut self, pen: Pen) -> StyleId {
        if let Some(id) = self.styles.intern(pen) {
            return id;
        }
        self.compact_styles();
        self.styles.intern(pen).unwrap_or(StyleId::DEFAULT)
    }

    /// Drops style ids no cell references and renumbers the rest.
    fn compact_styles(&mut self) {
        let mut live = vec![false; self.styles.len()];
        live[0] = true;
        for c in self.grid.cells().iter().chain(self.other.cells()) {
            live[c.style().0 as usize] = true;
        }
        let remap = self.styles.compact(&live);
        self.grid.remap_styles(&remap);
        self.other.remap_styles(&remap);
        // Printing may hold the template across this call, so resolve it now.
        let id = self.styles.intern(self.pen).unwrap_or(StyleId::DEFAULT);
        self.tpl = (id.0 as u64) << 32 | self.semantic_bits();
        self.tpl_pen = self.pen;
        self.pen_dirty = false;
        self.blank_dirty = true;
        self.mark_all_dirty();
    }

    #[inline(always)]
    fn resolve_pen(&mut self) {
        if self.pen_dirty {
            self.pen_dirty = false;
            if self.pen != self.tpl_pen {
                let pen = self.pen;
                self.tpl = (self.intern(pen).0 as u64) << 32 | self.semantic_bits();
                self.tpl_pen = pen;
            }
        }
    }

    #[inline]
    fn blank(&mut self) -> Cell {
        if self.blank_dirty {
            self.blank_dirty = false;
            let bg = self.pen.background();
            self.blank = if bg == Pen::DEFAULT { Cell::EMPTY } else { Cell::new(0, Width::Narrow, self.intern(bg)) };
        }
        self.blank
    }

    fn set_pen(&mut self, pen: Pen) {
        if pen.background() != self.pen.background() {
            self.blank_dirty = true;
        }
        self.pen = pen;
        self.pen_dirty = true;
    }

    fn update_slow(&mut self) {
        self.slow = self.insert
            || !self.autowrap
            || self.single_shift != 0
            || self.charsets[self.gl as usize] != Charset::Ascii;
    }

    // ---- Printing ------------------------------------------------------

    /// Prints a run of printable ASCII.
    #[inline(always)]
    pub fn print_ascii(&mut self, mut s: &[u8]) {
        if self.slow {
            return self.print_slow(s);
        }
        self.resolve_pen();
        let cols = self.cols;
        loop {
            if self.pending_wrap {
                self.wrap();
            }
            let tpl = self.tpl;
            let x = self.x;
            let y = self.y;
            let n = (cols - x).min(s.len());
            let end = x + n;
            let (row, meta) = self.grid.row_meta_mut(y);
            if row[x].width() == Width::SpacerTail && x > 0 {
                row[x - 1] = blankify(row[x - 1]);
            }
            if end < cols && row[end].width() == Width::SpacerTail {
                row[end] = blankify(row[end]);
            }
            for (d, &b) in row[x..end].iter_mut().zip(&s[..n]) {
                *d = Cell(tpl | b as u64);
            }
            meta.touch(end);
            mark(&mut self.dirty, self.all_dirty, self.rows, self.view_offset, y);
            self.last_char = s[n - 1] as u32;
            if end == cols {
                self.x = cols - 1;
                self.pending_wrap = true;
            } else {
                self.x = end;
            }
            s = &s[n..];
            if s.is_empty() {
                return;
            }
        }
    }

    #[cold]
    fn print_slow(&mut self, s: &[u8]) {
        self.resolve_pen();
        for &b in s {
            self.print_char(b as u32);
        }
    }

    /// Prints decoded codepoints.
    pub fn print_run(&mut self, cps: &[u32]) {
        self.resolve_pen();
        if self.slow {
            for &cp in cps {
                self.print_char(cp);
            }
            return;
        }
        let cols = self.cols;
        let mut i = 0;
        while i < cps.len() {
            // Write narrow and wide characters straight into the row until
            // something needs the general path: a wrap, a wide character at
            // the margin, a zero-width character.
            let (tpl, y, start, first) = (self.tpl, self.y, self.x, i);
            let mut x = start;
            if !self.pending_wrap {
                let (row, meta) = self.grid.row_meta_mut(y);
                let left_tail = row[start].width() == Width::SpacerTail;
                let mut last = 0;
                while i < cps.len() {
                    let cp = cps[i];
                    match char_width(cp) {
                        1 => {
                            row[x] = Cell(tpl | cp as u64);
                            x += 1;
                            last = cp;
                        }
                        2 if x + 1 < cols => {
                            row[x] = Cell(tpl | WIDE | cp as u64);
                            row[x + 1] = Cell(tpl | SPACER_TAIL);
                            x += 2;
                            last = cp;
                        }
                        // A mark on a cell written in this run.
                        0 if x > start => {
                            let mut b = x - 1;
                            if row[b].width() == Width::SpacerTail {
                                b -= 1;
                            }
                            let base = row[b];
                            if attach_mark(row, meta, b, cp) && widens(cp, base) && b + 1 == x && x < cols {
                                widen(row, b);
                                x += 1;
                            }
                        }
                        _ => break,
                    }
                    i += 1;
                    if x == cols {
                        break;
                    }
                }
                if i != first {
                    if left_tail && start > 0 {
                        row[start - 1] = blankify(row[start - 1]);
                    }
                    if x < cols && row[x].width() == Width::SpacerTail {
                        row[x] = blankify(row[x]);
                    }
                    meta.touch(x);
                    mark(&mut self.dirty, self.all_dirty, self.rows, self.view_offset, y);
                    if last != 0 {
                        self.last_char = last;
                    }
                    if x == cols {
                        self.x = cols - 1;
                        self.pending_wrap = self.autowrap;
                    } else {
                        self.x = x;
                    }
                    continue;
                }
            }
            self.print_char(cps[i]);
            i += 1;
        }
    }

    #[inline]
    fn print_char(&mut self, cp: u32) {
        let cp = if self.slow { self.map_charset(cp) } else { cp };
        match char_width(cp) {
            1 => self.put(cp, false),
            2 => self.put(cp, true),
            0 => self.combine(cp),
            _ => {}
        }
    }

    fn map_charset(&mut self, cp: u32) -> u32 {
        let set = if self.single_shift != 0 {
            let s = self.charsets[self.single_shift as usize];
            self.single_shift = 0;
            self.update_slow();
            s
        } else {
            self.charsets[self.gl as usize]
        };
        if set == Charset::DecSpecial && (0x5F..=0x7E).contains(&cp) { DEC_SPECIAL[(cp - 0x5F) as usize] } else { cp }
    }

    /// Moves to the start of the next row, marking the current row as wrapped.
    fn wrap(&mut self) {
        self.grid.meta_mut(self.y).wrapped = true;
        self.index();
        self.x = 0;
    }

    /// Writes one narrow or wide character at the cursor (general path).
    fn put(&mut self, cp: u32, wide: bool) {
        let cols = self.cols;
        if self.pending_wrap {
            if self.autowrap {
                self.wrap();
            } else {
                self.pending_wrap = false;
            }
        }
        let w = 1 + wide as usize;
        if wide && self.x + 1 >= cols {
            if !self.autowrap || cols < 2 {
                return;
            }
            let (x, y, tpl) = (self.x, self.y, self.tpl);
            let (row, meta) = self.grid.row_meta_mut(y);
            if row[x].width() == Width::SpacerTail && x > 0 {
                row[x - 1] = blankify(row[x - 1]);
            }
            row[x] = Cell(tpl | SPACER_HEAD);
            meta.touch(x + 1);
            mark(&mut self.dirty, self.all_dirty, self.rows, self.view_offset, y);
            self.wrap();
        }
        let (x, y, tpl) = (self.x, self.y, self.tpl);
        let end = x + w;
        let (row, meta) = self.grid.row_meta_mut(y);
        if self.insert && end < cols {
            row.copy_within(x..cols - w, end);
            if row[cols - 1].width() == Width::Wide {
                row[cols - 1] = blankify(row[cols - 1]);
            }
            meta.occ = (meta.occ as usize + w).min(cols) as u16;
            meta.shift_graphemes_right(x, w, cols);
        }
        if row[x].width() == Width::SpacerTail && x > 0 {
            row[x - 1] = blankify(row[x - 1]);
        }
        if end < cols && row[end].width() == Width::SpacerTail {
            row[end] = blankify(row[end]);
        }
        if wide {
            row[x] = Cell(tpl | WIDE | cp as u64);
            row[x + 1] = Cell(tpl | SPACER_TAIL);
        } else {
            row[x] = Cell(tpl | cp as u64);
        }
        meta.touch(end);
        mark(&mut self.dirty, self.all_dirty, self.rows, self.view_offset, y);
        self.last_char = cp;
        if end >= cols {
            self.x = cols - 1;
            self.pending_wrap = self.autowrap;
        } else {
            self.x = end;
        }
    }

    /// Attaches a zero-width codepoint to the previous cell's grapheme.
    fn combine(&mut self, cp: u32) {
        let cols = self.cols;
        let mut x = if self.pending_wrap {
            self.x
        } else if self.x > 0 {
            self.x - 1
        } else {
            return;
        };
        let y = self.y;
        let (row, meta) = self.grid.row_meta_mut(y);
        if row[x].width() == Width::SpacerTail && x > 0 {
            x -= 1;
        }
        let base = row[x];
        if !attach_mark(row, meta, x, cp) {
            return;
        }
        if widens(cp, base) && !self.pending_wrap && self.x == x + 1 && x + 1 < cols {
            widen(row, x);
            meta.touch(x + 2);
            if x + 2 >= cols {
                self.x = cols - 1;
                self.pending_wrap = self.autowrap;
            } else {
                self.x = x + 2;
            }
        }
        mark(&mut self.dirty, self.all_dirty, self.rows, self.view_offset, y);
    }

    /// REP: repeats the last printed character.
    fn repeat(&mut self, n: usize) {
        let c = self.last_char;
        if c == 0 {
            return;
        }
        if c < 0x80 && !self.slow {
            let buf = [c as u8; 256];
            let mut n = n;
            while n > 0 {
                let k = n.min(buf.len());
                self.print_ascii(&buf[..k]);
                n -= k;
            }
            return;
        }
        self.resolve_pen();
        let wide = char_width(c) == 2;
        for _ in 0..n {
            self.put(c, wide);
        }
    }

    // ---- Cursor movement and scrolling ---------------------------------

    #[inline]
    pub fn carriage_return(&mut self) {
        self.x = 0;
        self.pending_wrap = false;
    }

    #[inline]
    pub fn linefeed(&mut self) {
        self.index();
        if self.lnm {
            self.x = 0;
        }
    }

    #[inline(always)]
    fn index(&mut self) {
        self.pending_wrap = false;
        if self.y == self.bottom {
            let blank = self.blank();
            let row0 = self.grid.pushed();
            let saved = self.grid.scroll_up_one(self.top, self.bottom, blank, !self.alt_active);
            if saved && self.view_offset != 0 {
                self.view_offset = (self.view_offset + 1).min(self.grid.hist_len());
            }
            if self.selection.is_some() {
                self.scroll_selection(row0, self.top, self.bottom, 1, true, usize::from(saved));
            }
            self.mark_all_dirty();
        } else if self.y + 1 < self.rows {
            self.y += 1;
        }
        if self.semantic != Semantic::Output {
            self.semantic_line_end();
        }
    }

    /// ghostty's index for a prompt or input line: input that ends at the
    /// end of the line becomes output, otherwise the new row continues the
    /// prompt.
    #[cold]
    fn semantic_line_end(&mut self) {
        if self.semantic_clear_eol {
            self.set_semantic(Semantic::Output, false);
        } else {
            self.grid.meta_mut(self.y).prompt = PromptMark::Continuation;
        }
    }

    fn semantic_bits(&self) -> u64 {
        (self.semantic as u64) << Cell::SEMANTIC_SHIFT
    }

    fn set_semantic(&mut self, semantic: Semantic, clear_eol: bool) {
        self.semantic = semantic;
        self.semantic_clear_eol = clear_eol;
        self.tpl = (self.tpl & !Cell::SEMANTIC_MASK) | self.semantic_bits();
    }

    /// ghostty's semanticPromptFreshLine: a new line unless at its start.
    fn fresh_line(&mut self) {
        if self.x != 0 {
            self.carriage_return();
            self.index();
        }
    }

    /// OSC 7 (ghostty's reportPwd): an empty URL forgets the directory; a
    /// file or kitty-shell-cwd URL of this host sets it; while no program
    /// set a title, the directory is the title.
    fn report_pwd(&mut self, url: &[u8]) {
        let path = if url.is_empty() {
            None
        } else {
            match parse_cwd(url, local_hostname()) {
                Some(path) => Some(path),
                None => return,
            }
        };
        if !self.seen_title {
            self.title.clear();
            self.title.push_str(path.as_deref().unwrap_or(""));
        }
        self.cwd = path;
    }

    /// OSC 133 (ghostty's Terminal.semanticPrompt).
    fn semantic_prompt(&mut self, rest: &[u8]) {
        let (&action, options) = match rest.split_first() {
            Some((a, [])) => (a, &[][..]),
            Some((a, [b';', options @ ..])) => (a, options),
            _ => return,
        };
        let option = |key: &[u8]| {
            options.split(|&b| b == b';').find_map(|kv| kv.strip_prefix(key).and_then(|v| v.strip_prefix(b"=")))
        };
        let prompt = |t: &mut Self| {
            let continuation = matches!(option(b"k"), Some(b"c" | b"s"));
            t.set_semantic(Semantic::Prompt, false);
            t.grid.meta_mut(t.y).prompt = if continuation { PromptMark::Continuation } else { PromptMark::Prompt };
        };
        match action {
            b'A' | b'N' => {
                self.fresh_line();
                prompt(self);
                // ghostty reads click_events first (1 absolute, 2 relative),
                // then cl (line, m, v, w); invalid values are ignored.
                let events = option(b"click_events").and_then(|v| match v {
                    b"1" => Some(PromptClick::Absolute),
                    b"2" => Some(PromptClick::Relative),
                    _ => None,
                });
                let cl = option(b"cl").filter(|v| matches!(*v, b"line" | b"m" | b"v" | b"w")).map(|_| PromptClick::Cl);
                if let Some(click) = events.or(cl) {
                    self.prompt_click = Some(click);
                }
                let redraw = option(b"redraw").and_then(|v| match v {
                    b"0" => Some(Redraw::None),
                    b"1" => Some(Redraw::All),
                    b"last" => Some(Redraw::Last),
                    _ => None,
                });
                if let Some(redraw) = redraw {
                    self.redraw = redraw;
                }
            }
            b'P' => prompt(self),
            b'B' => self.set_semantic(Semantic::Input, false),
            b'I' => self.set_semantic(Semantic::Input, true),
            b'C' => {
                self.messages.push(Message::StartCommand);
                self.set_semantic(Semantic::Output, false);
                let meta = self.grid.meta_mut(self.y);
                if meta.prompt != PromptMark::None && self.x == 0 {
                    meta.prompt = PromptMark::None;
                }
            }
            b'D' => {
                // ghostty's StreamHandler.semanticPrompt: the first option
                // is the exit code (0 if absent or invalid, 1 if not a byte).
                let code =
                    options.split(|&b| b == b';').next().and_then(|v| std::str::from_utf8(v).ok()?.parse::<i32>().ok());
                self.messages.push(Message::StopCommand(code.map_or(0, |c| u8::try_from(c).unwrap_or(1))));
                self.set_semantic(Semantic::Output, false);
            }
            b'L' if options.is_empty() => self.fresh_line(),
            _ => {}
        }
    }

    fn reverse_index(&mut self) {
        self.pending_wrap = false;
        if self.y == self.top {
            self.scroll_down(1);
        } else if self.y > 0 {
            self.y -= 1;
        }
    }

    fn scroll_up(&mut self, n: usize) {
        let blank = self.blank();
        let row0 = self.grid.pushed();
        let pushed = self.grid.scroll_up(self.top, self.bottom, n, blank, !self.alt_active);
        if pushed != 0 && self.view_offset != 0 {
            self.view_offset = (self.view_offset + pushed).min(self.grid.hist_len());
        }
        self.scroll_selection(row0, self.top, self.bottom, n, true, pushed);
        self.mark_all_dirty();
    }

    /// Keeps the selection on its text after rows `top..=bottom` of the
    /// screen whose first row was line `row0` scrolled `n` rows up (`up`) or
    /// down in place, `saved` of them into the scrollback. Ends move with
    /// their cells; an end on discarded text moves to the nearest kept cell
    /// inside the selection, and a selection with no text left is cleared.
    fn scroll_selection(&mut self, row0: u64, top: usize, bottom: usize, n: usize, up: bool, saved: usize) {
        let Some(sel) = self.selection else { return };
        let (mut first, mut last) = sel.ordered();
        let (n, height) = (n.min(bottom + 1 - top) as u64, (bottom + 1 - top) as u64);
        let (top, bottom) = (row0 + top as u64, row0 + bottom as u64);
        if saved > 0 {
            // Saved rows keep their numbers, but the rows below the region
            // stay where they are while the screen's numbering moves on.
            for p in [&mut first, &mut last].into_iter().filter(|p| p.line > bottom) {
                p.line += saved as u64;
            }
            self.selection = Some(Selection { start: first, end: last, rect: sel.rect });
            return;
        }
        // The discarded lines, and the lines where the kept text just before
        // and just after them is now.
        let (gone, before, after) = if up {
            (top..top + n, top.checked_sub(1), if n < height { top } else { bottom + 1 })
        } else {
            (bottom + 1 - n..bottom + 1, if n < height { Some(bottom) } else { top.checked_sub(1) }, bottom + 1)
        };
        let before = before.filter(|&l| l >= self.grid.first_line());
        let after = (after < row0 + self.rows as u64).then_some(after);
        let last_col = self.cols as u16 - 1;
        let moved = |p: Point| {
            if !(top..=bottom).contains(&p.line) {
                Some(p)
            } else if gone.contains(&p.line) {
                None
            } else if up {
                Some(Point { line: p.line - n, ..p })
            } else {
                Some(Point { line: p.line + n, ..p })
            }
        };
        let first = moved(first).or_else(|| after.map(|line| Point { line, col: if sel.rect { first.col } else { 0 } }));
        let last = moved(last).or_else(|| before.map(|line| Point { line, col: if sel.rect { last.col } else { last_col } }));
        self.selection = match (first, last) {
            (Some(start), Some(end)) if start.line < end.line || start.line == end.line && start.col <= end.col => {
                Some(Selection { start, end, rect: sel.rect })
            }
            _ => None,
        };
    }

    fn scroll_down(&mut self, n: usize) {
        let blank = self.blank();
        self.grid.scroll_down(self.top, self.bottom, n, blank);
        self.scroll_selection(self.grid.pushed(), self.top, self.bottom, n, false, 0);
        self.mark_all_dirty();
    }

    /// CUP: `row` and `col` are 0-based and relative to the origin.
    fn goto(&mut self, row: usize, col: usize) {
        let (top, max) = if self.origin { (self.top, self.bottom) } else { (0, self.rows - 1) };
        self.y = (row + top).min(max);
        self.x = col.min(self.cols - 1);
        self.pending_wrap = false;
    }

    fn set_row(&mut self, row: usize) {
        let x = self.x;
        self.goto(row, x);
    }

    fn origin_row(&self) -> usize {
        if self.origin { self.y.saturating_sub(self.top) } else { self.y }
    }

    fn cursor_up(&mut self, n: usize) {
        let min = if self.y >= self.top { self.top } else { 0 };
        self.y = self.y.saturating_sub(n).max(min);
        self.pending_wrap = false;
    }

    fn cursor_down(&mut self, n: usize) {
        let max = if self.y <= self.bottom { self.bottom } else { self.rows - 1 };
        self.y = (self.y + n).min(max);
        self.pending_wrap = false;
    }

    fn set_col(&mut self, x: usize) {
        self.x = x.min(self.cols - 1);
        self.pending_wrap = false;
    }

    fn tab_forward(&mut self, n: usize) {
        let last = self.cols - 1;
        for _ in 0..n {
            if self.x >= last {
                break;
            }
            self.x += 1;
            while self.x < last && !self.tabs[self.x] {
                self.x += 1;
            }
        }
    }

    fn tab_back(&mut self, n: usize) {
        self.pending_wrap = false;
        for _ in 0..n {
            if self.x == 0 {
                break;
            }
            self.x -= 1;
            while self.x > 0 && !self.tabs[self.x] {
                self.x -= 1;
            }
        }
    }

    fn set_margins(&mut self, top: usize, bottom: usize) {
        let top = top.max(1);
        let bottom = if bottom == 0 { self.rows } else { bottom.min(self.rows) };
        if top < bottom {
            self.top = top - 1;
            self.bottom = bottom - 1;
            self.goto(0, 0);
        }
    }

    // ---- Erasing and editing -------------------------------------------

    /// Fills `a..b` of the cursor row with blanks, widened so no wide
    /// character is split.
    fn erase_cols(&mut self, mut a: usize, mut b: usize) {
        let blank = self.blank();
        let cols = self.cols;
        let y = self.y;
        let (row, meta) = self.grid.row_meta_mut(y);
        if a > 0 && row[a].width() == Width::SpacerTail {
            a -= 1;
        }
        if b < cols && row[b].width() == Width::SpacerTail {
            b += 1;
        }
        row[a..b].fill(blank);
        if blank != Cell::EMPTY {
            meta.touch(b);
        } else if b >= meta.occ as usize && (a as u16) < meta.occ {
            meta.occ = a as u16;
        }
        mark(&mut self.dirty, self.all_dirty, self.rows, self.view_offset, y);
    }

    fn erase_line(&mut self, mode: u16) {
        self.pending_wrap = false;
        match mode {
            0 => self.erase_cols(self.x, self.cols),
            1 => self.erase_cols(0, self.x + 1),
            2 => self.erase_cols(0, self.cols),
            _ => return,
        }
        if mode != 1 {
            self.grid.meta_mut(self.y).wrapped = false;
        }
    }

    fn erase_display(&mut self, mode: u16) {
        match mode {
            0 => {
                self.erase_line(0);
                let blank = self.blank();
                for y in self.y + 1..self.rows {
                    self.grid.clear_row(y, blank);
                }
            }
            1 => {
                self.erase_line(1);
                let blank = self.blank();
                for y in 0..self.y {
                    self.grid.clear_row(y, blank);
                }
            }
            2 => {
                self.pending_wrap = false;
                let blank = self.blank();
                for y in 0..self.rows {
                    self.grid.clear_row(y, blank);
                }
            }
            3 => {
                if !self.alt_active {
                    self.grid.clear_history();
                    self.view_offset = 0;
                }
            }
            _ => return,
        }
        self.mark_all_dirty();
    }

    fn erase_chars(&mut self, n: usize) {
        self.pending_wrap = false;
        let end = (self.x + n).min(self.cols);
        self.erase_cols(self.x, end);
    }

    fn insert_chars(&mut self, n: usize) {
        self.pending_wrap = false;
        let blank = self.blank();
        let (cols, x, y) = (self.cols, self.x, self.y);
        let n = n.min(cols - x);
        let (row, meta) = self.grid.row_meta_mut(y);
        if row[x].width() == Width::SpacerTail && x > 0 {
            row[x - 1] = blankify(row[x - 1]);
            row[x] = blankify(row[x]);
        }
        row.copy_within(x..cols - n, x + n);
        if row[cols - 1].width() == Width::Wide {
            row[cols - 1] = blankify(row[cols - 1]);
        }
        row[x..x + n].fill(blank);
        meta.occ = (meta.occ as usize + n).min(cols) as u16;
        meta.shift_graphemes_right(x, n, cols);
        if blank != Cell::EMPTY {
            meta.touch(x + n);
        }
        mark(&mut self.dirty, self.all_dirty, self.rows, self.view_offset, y);
    }

    fn delete_chars(&mut self, n: usize) {
        self.pending_wrap = false;
        let blank = self.blank();
        let (cols, x, y) = (self.cols, self.x, self.y);
        let n = n.min(cols - x);
        let (row, meta) = self.grid.row_meta_mut(y);
        if row[x].width() == Width::SpacerTail && x > 0 {
            row[x - 1] = blankify(row[x - 1]);
        }
        if x + n < cols && row[x + n].width() == Width::SpacerTail {
            row[x + n] = blankify(row[x + n]);
        }
        row.copy_within(x + n..cols, x);
        row[cols - n..].fill(blank);
        meta.shift_graphemes_left(x, n);
        if blank != Cell::EMPTY {
            meta.touch(cols);
        }
        mark(&mut self.dirty, self.all_dirty, self.rows, self.view_offset, y);
    }

    fn insert_lines(&mut self, n: usize) {
        if self.y < self.top || self.y > self.bottom {
            return;
        }
        let blank = self.blank();
        self.grid.scroll_down(self.y, self.bottom, n, blank);
        self.scroll_selection(self.grid.pushed(), self.y, self.bottom, n, false, 0);
        self.x = 0;
        self.pending_wrap = false;
        self.mark_all_dirty();
    }

    fn delete_lines(&mut self, n: usize) {
        if self.y < self.top || self.y > self.bottom {
            return;
        }
        let blank = self.blank();
        self.grid.scroll_up(self.y, self.bottom, n, blank, false);
        self.scroll_selection(self.grid.pushed(), self.y, self.bottom, n, true, 0);
        self.x = 0;
        self.pending_wrap = false;
        self.mark_all_dirty();
    }

    fn decaln(&mut self) {
        self.top = 0;
        self.bottom = self.rows - 1;
        let e = Cell::new('E' as u32, Width::Narrow, StyleId::DEFAULT);
        for y in 0..self.rows {
            self.grid.clear_row(y, Cell::EMPTY);
            let (row, meta) = self.grid.row_meta_mut(y);
            row.fill(e);
            meta.touch(row.len());
        }
        self.goto(0, 0);
        self.mark_all_dirty();
    }

    // ---- Cursor save/restore, screens and resets -----------------------

    fn save_cursor(&mut self) {
        self.saved[self.alt_active as usize] = Some(Saved {
            x: self.x,
            y: self.y,
            pending_wrap: self.pending_wrap,
            pen: self.pen,
            charsets: self.charsets,
            gl: self.gl,
            origin: self.origin,
        });
    }

    fn restore_cursor(&mut self) {
        let s = self.saved[self.alt_active as usize].clone().unwrap_or_default();
        self.x = s.x.min(self.cols - 1);
        self.y = s.y.min(self.rows - 1);
        self.pending_wrap = s.pending_wrap && self.x == self.cols - 1;
        self.set_pen(s.pen);
        self.charsets = s.charsets;
        self.gl = s.gl;
        self.origin = s.origin;
        self.update_slow();
    }

    fn screen_cursor(&self) -> ScreenCursor {
        ScreenCursor { x: self.x, y: self.y, pending_wrap: self.pending_wrap, semantic: self.semantic }
    }

    fn switch_screen(&mut self, alt: bool) {
        if alt == self.alt_active {
            return;
        }
        self.other_cursor = self.screen_cursor();
        std::mem::swap(&mut self.grid, &mut self.other);
        self.alt_active = alt;
        self.selection = None;
        self.view_offset = 0;
        self.mark_all_dirty();
    }

    fn clear_screen(&mut self) {
        for y in 0..self.rows {
            self.grid.clear_row(y, Cell::EMPTY);
        }
        self.mark_all_dirty();
    }

    /// DECSTR.
    fn soft_reset(&mut self) {
        self.cursor_visible = true;
        self.insert = false;
        self.origin = false;
        self.autowrap = true;
        self.modes.app_cursor = false;
        self.modes.app_keypad = false;
        self.top = 0;
        self.bottom = self.rows - 1;
        self.set_pen(Pen::DEFAULT);
        self.charsets = [Charset::Ascii; 4];
        self.gl = 0;
        self.single_shift = 0;
        self.saved[self.alt_active as usize] = None;
        self.pending_wrap = false;
        self.update_slow();
    }

    /// RIS.
    pub fn full_reset(&mut self) {
        let hist = self.primary().hist_cap();
        let replies = std::mem::take(&mut self.replies);
        let messages = std::mem::take(&mut self.messages);
        let cwd = self.cwd.take();
        let bell = self.bell;
        let base = std::mem::take(&mut self.base_palette);
        *self = Term::new(self.cols, self.rows, hist);
        self.palette = base.clone();
        self.base_palette = base;
        self.replies = replies;
        self.messages = messages;
        self.cwd = cwd;
        self.bell = bell;
    }

    // ---- Modes ---------------------------------------------------------

    fn set_ansi_mode(&mut self, mode: u16, on: bool) {
        match mode {
            4 => {
                self.insert = on;
                self.update_slow();
            }
            20 => self.lnm = on,
            _ => {}
        }
    }

    fn set_dec_mode(&mut self, mode: u16, on: bool) {
        let m = &mut self.modes;
        match mode {
            1 => m.app_cursor = on,
            5 => {
                if m.reverse_video != on {
                    m.reverse_video = on;
                    self.mark_all_dirty();
                }
            }
            6 => {
                self.origin = on;
                self.goto(0, 0);
            }
            7 => {
                self.autowrap = on;
                self.update_slow();
            }
            12 => self.cursor_blink = on,
            25 => self.cursor_visible = on,
            9 | 1000 | 1002 | 1003 => {
                m.mouse_mode = match (on, mode) {
                    (false, _) => MouseMode::None,
                    (true, 9) => MouseMode::X10,
                    (true, 1000) => MouseMode::Normal,
                    (true, 1002) => MouseMode::ButtonMotion,
                    _ => MouseMode::AnyMotion,
                };
                // ghostty points with an arrow while reporting the mouse.
                self.mouse_shape = if on { "default" } else { "text" };
            }
            1005 | 1006 | 1015 | 1016 => {
                let enc = match mode {
                    1005 => MouseEncoding::Utf8,
                    1006 => MouseEncoding::Sgr,
                    1016 => MouseEncoding::SgrPixels,
                    _ => MouseEncoding::Urxvt,
                };
                if on {
                    m.mouse_encoding = enc;
                } else if m.mouse_encoding == enc {
                    m.mouse_encoding = MouseEncoding::Default;
                }
            }
            1004 => {
                m.focus_events = on;
                // ghostty reports the current focus as soon as it is asked.
                if on {
                    let report: &[u8] = if self.focused { b"\x1b[I" } else { b"\x1b[O" };
                    self.replies.extend_from_slice(report);
                }
            }
            1007 => m.alternate_scroll = on,
            2004 => m.bracketed_paste = on,
            2026 => m.sync_update = on,
            47 => self.switch_screen(on),
            1047 => {
                if !on && self.alt_active {
                    self.clear_screen();
                }
                self.switch_screen(on);
            }
            1048 => {
                if on {
                    self.save_cursor()
                } else {
                    self.restore_cursor()
                }
            }
            1049 => {
                if on && !self.alt_active {
                    self.save_cursor();
                    self.switch_screen(true);
                    self.clear_screen();
                } else if !on && self.alt_active {
                    self.switch_screen(false);
                    self.restore_cursor();
                }
            }
            _ => {}
        }
    }

    fn dec_mode_state(&self, mode: u16) -> Option<bool> {
        let m = &self.modes;
        Some(match mode {
            1 => m.app_cursor,
            5 => m.reverse_video,
            6 => self.origin,
            7 => self.autowrap,
            12 => self.cursor_blink,
            25 => self.cursor_visible,
            9 => m.mouse_mode == MouseMode::X10,
            1000 => m.mouse_mode == MouseMode::Normal,
            1002 => m.mouse_mode == MouseMode::ButtonMotion,
            1003 => m.mouse_mode == MouseMode::AnyMotion,
            1004 => m.focus_events,
            1005 => m.mouse_encoding == MouseEncoding::Utf8,
            1006 => m.mouse_encoding == MouseEncoding::Sgr,
            1015 => m.mouse_encoding == MouseEncoding::Urxvt,
            1016 => m.mouse_encoding == MouseEncoding::SgrPixels,
            1007 => m.alternate_scroll,
            2004 => m.bracketed_paste,
            2026 => m.sync_update,
            47 | 1047 | 1049 => self.alt_active,
            1048 => false,
            _ => return None,
        })
    }

    // ---- Dispatch ------------------------------------------------------

    pub fn execute(&mut self, b: u8) {
        match b {
            0x07 => self.bell = true,
            0x08 => {
                self.x = self.x.saturating_sub(1);
                self.pending_wrap = false;
            }
            0x09 => self.tab_forward(1),
            0x0A..=0x0C => self.linefeed(),
            0x0D => self.carriage_return(),
            0x0E => {
                self.gl = 1;
                self.update_slow();
            }
            0x0F => {
                self.gl = 0;
                self.update_slow();
            }
            _ => {}
        }
    }

    pub fn esc_dispatch(&mut self, inter: u8, fin: u8) {
        match (inter, fin) {
            (0, b'7') => self.save_cursor(),
            (0, b'8') => self.restore_cursor(),
            (0, b'D') => self.index(),
            (0, b'E') => {
                self.index();
                self.x = 0;
            }
            (0, b'M') => self.reverse_index(),
            (0, b'H') => self.tabs[self.x] = true,
            (0, b'c') => {
                self.full_reset();
                // ghostty's StreamHandler.fullReset also clears the progress bar.
                self.messages
                    .push(Message::ProgressReport(ProgressReport { state: ProgressState::Remove, progress: None }));
            }
            (0, b'=') => self.modes.app_keypad = true,
            (0, b'>') => self.modes.app_keypad = false,
            (0, b'N') => {
                self.single_shift = 2;
                self.update_slow();
            }
            (0, b'O') => {
                self.single_shift = 3;
                self.update_slow();
            }
            (0, b'Z') => self.replies.extend_from_slice(b"\x1b[?62;22c"),
            (b'#', b'8') => self.decaln(),
            (b'(' | b')' | b'*' | b'+', b'B' | b'0') => {
                let set = if fin == b'0' { Charset::DecSpecial } else { Charset::Ascii };
                self.charsets[(inter - b'(') as usize] = set;
                self.update_slow();
            }
            _ => {}
        }
    }

    /// Dispatches a CSI sequence; SGR and CUP, the most frequent ones, take
    /// a short path.
    #[inline(always)]
    pub fn csi_dispatch(&mut self, p: &Params, private: u8, inter: u8, fin: u8) {
        if private | inter == 0 {
            match fin {
                b'm' => {
                    let mut pen = self.pen;
                    apply_sgr(&mut pen, p);
                    self.set_pen(pen);
                    return;
                }
                b'H' => return self.goto(p.count(0) - 1, p.count(1) - 1),
                _ => {}
            }
        }
        self.csi_dispatch_other(p, private, inter, fin);
    }

    fn csi_dispatch_other(&mut self, p: &Params, private: u8, inter: u8, fin: u8) {
        match (private, inter, fin) {
            (0, 0, b'H' | b'f') => self.goto(p.count(0) - 1, p.count(1) - 1),
            (0, 0, b'A') => self.cursor_up(p.count(0)),
            (0, 0, b'B') => self.cursor_down(p.count(0)),
            (0, 0, b'C' | b'a') => self.set_col(self.x + p.count(0)),
            (0, 0, b'D') => self.set_col(self.x.saturating_sub(p.count(0))),
            (0, 0, b'E') => {
                self.cursor_down(p.count(0));
                self.x = 0;
            }
            (0, 0, b'F') => {
                self.cursor_up(p.count(0));
                self.x = 0;
            }
            (0, 0, b'G' | b'`') => self.set_col(p.count(0) - 1),
            (0, 0, b'd') => self.set_row(p.count(0) - 1),
            (0, 0, b'e') => self.set_row(self.origin_row() + p.count(0)),
            (0, 0, b'I') => self.tab_forward(p.count(0)),
            (0, 0, b'Z') => self.tab_back(p.count(0)),
            (0, 0, b'J') => self.erase_display(p.get(0)),
            (b'?', 0, b'J') => self.erase_display(p.get(0)),
            (0, 0, b'K') | (b'?', 0, b'K') => self.erase_line(p.get(0)),
            (0, 0, b'@') => self.insert_chars(p.count(0)),
            (0, 0, b'P') => self.delete_chars(p.count(0)),
            (0, 0, b'X') => self.erase_chars(p.count(0)),
            (0, 0, b'L') => self.insert_lines(p.count(0)),
            (0, 0, b'M') => self.delete_lines(p.count(0)),
            (0, 0, b'S') => self.scroll_up(p.count(0)),
            (0, 0, b'T') if p.len() <= 1 => self.scroll_down(p.count(0)),
            (0, 0, b'b') => self.repeat(p.count(0)),
            (0, 0, b'c') if p.get(0) == 0 => self.replies.extend_from_slice(b"\x1b[?62;22c"),
            (b'>', 0, b'c') if p.get(0) == 0 => self.replies.extend_from_slice(b"\x1b[>1;10;0c"),
            // XTVERSION: the terminal's name and version.
            (b'>', 0, b'q') if p.get(0) == 0 => {
                let _ = write!(self.replies, "\x1bP>|tapust {}\x1b\\", env!("CARGO_PKG_VERSION"));
            }
            (0, 0, b'g') => match p.get(0) {
                0 => self.tabs[self.x] = false,
                3 => self.tabs.fill(false),
                _ => {}
            },
            (0, 0, b'h' | b'l') => {
                for m in p.iter() {
                    self.set_ansi_mode(m, fin == b'h');
                }
            }
            (b'?', 0, b'h' | b'l') => {
                for m in p.iter() {
                    self.set_dec_mode(m, fin == b'h');
                }
            }
            (0, 0, b'n') => match p.get(0) {
                5 => self.replies.extend_from_slice(b"\x1b[0n"),
                6 => {
                    let _ = write!(self.replies, "\x1b[{};{}R", self.origin_row() + 1, self.x + 1);
                }
                _ => {}
            },
            (0, 0, b'r') => self.set_margins(p.get(0) as usize, p.get(1) as usize),
            (0, 0, b's') => self.save_cursor(),
            (0, 0, b'u') => self.restore_cursor(),
            (0, b' ', b'q') => {
                let (shape, blink) = match p.get(0) {
                    0 | 1 => (CursorShape::Block, true),
                    2 => (CursorShape::Block, false),
                    3 => (CursorShape::Underline, true),
                    4 => (CursorShape::Underline, false),
                    5 => (CursorShape::Bar, true),
                    6 => (CursorShape::Bar, false),
                    _ => return,
                };
                self.cursor_shape = shape;
                self.cursor_blink = blink;
            }
            (0, b'!', b'p') => self.soft_reset(),
            (0, b'$', b'p') => {
                let mode = p.get(0);
                let state = match mode {
                    4 => Some(self.insert),
                    20 => Some(self.lnm),
                    _ => None,
                };
                let _ = write!(self.replies, "\x1b[{};{}$y", mode, state.map_or(0, |s| 2 - s as u8));
            }
            (b'?', b'$', b'p') => {
                let mode = p.get(0);
                let state = self.dec_mode_state(mode).map_or(0, |s| 2 - s as u8);
                let _ = write!(self.replies, "\x1b[?{};{}$y", mode, state);
            }
            (0, 0, b't') => match p.get(0) {
                18 => {
                    let _ = write!(self.replies, "\x1b[8;{};{}t", self.rows, self.cols);
                }
                19 => {
                    let _ = write!(self.replies, "\x1b[9;{};{}t", self.rows, self.cols);
                }
                _ => {}
            },
            _ => {}
        }
    }

    /// XTGETTCAP: answers each hex-encoded terminfo capability name with its
    /// value, or as unknown, one reply per name.
    pub fn xtgettcap(&mut self, data: &[u8]) {
        for hex in data.split(|&b| b == b';') {
            let name = decode_hex(hex);
            match name.as_deref().and_then(capability) {
                Some(value) => {
                    self.replies.extend_from_slice(b"\x1bP1+r");
                    self.replies.extend_from_slice(hex);
                    if let Some(value) = value {
                        self.replies.push(b'=');
                        for b in value.bytes() {
                            let _ = write!(self.replies, "{b:02X}");
                        }
                    }
                }
                None => {
                    self.replies.extend_from_slice(b"\x1bP0+r");
                    self.replies.extend_from_slice(hex);
                }
            }
            self.replies.extend_from_slice(b"\x1b\\");
        }
    }

    pub fn osc_dispatch(&mut self, data: &[u8], bel: bool) {
        let (cmd, rest) = match data.iter().position(|&b| b == b';') {
            Some(i) => (&data[..i], &data[i + 1..]),
            None => (data, &data[data.len()..]),
        };
        // ghostty only captures what follows the number's `;`.
        let has_rest = cmd.len() < data.len();
        let Some(cmd) = std::str::from_utf8(cmd).ok().and_then(|s| s.parse::<u16>().ok()) else {
            return;
        };
        let st: &[u8] = if bel { b"\x07" } else { b"\x1b\\" };
        match cmd {
            0 | 2 => {
                self.title.clear();
                self.title.push_str(&String::from_utf8_lossy(rest));
                self.seen_title = true;
            }
            4 => {
                let mut it = rest.split(|&b| b == b';');
                while let (Some(idx), Some(spec)) = (it.next(), it.next()) {
                    let Some(idx) = std::str::from_utf8(idx).ok().and_then(|s| s.parse::<u8>().ok()) else {
                        break;
                    };
                    if spec == b"?" {
                        let c = self.palette.indexed[idx as usize];
                        self.color_reply(&format!("4;{idx}"), c, st);
                    } else if let Some(c) = parse_color(spec) {
                        self.palette.indexed[idx as usize] = c;
                        self.mark_all_dirty();
                    }
                }
            }
            104 => {
                let def = self.base_palette.clone();
                if rest.is_empty() {
                    self.palette.indexed = def.indexed;
                } else {
                    for idx in rest.split(|&b| b == b';') {
                        if let Some(i) = std::str::from_utf8(idx).ok().and_then(|s| s.parse::<u8>().ok()) {
                            self.palette.indexed[i as usize] = def.indexed[i as usize];
                        }
                    }
                }
                self.mark_all_dirty();
            }
            7 => self.report_pwd(rest),
            9 if has_rest => match parse_osc9(rest) {
                Osc9::Notification(body) => self.desktop_notification(b"", body),
                Osc9::Progress(report) => self.messages.push(Message::ProgressReport(report)),
                Osc9::Pwd(url) => self.report_pwd(url),
                Osc9::Prompt => self.semantic_prompt(b"A"),
                Osc9::Ignored => {}
            },
            777 if has_rest => {
                if let Some((title, body)) = parse_osc777(rest) {
                    self.desktop_notification(title, body);
                }
            }
            133 => self.semantic_prompt(rest),
            22 => {
                if rest.is_empty() {
                    self.mouse_shape = if self.modes.mouse_mode == MouseMode::None { "text" } else { "default" };
                } else if let Some(shape) = std::str::from_utf8(rest).ok().and_then(mouse_shape) {
                    self.mouse_shape = shape;
                }
            }
            10..=12 => {
                for (k, spec) in rest.split(|&b| b == b';').enumerate() {
                    let target = cmd as usize + k;
                    if target > 12 {
                        break;
                    }
                    if spec == b"?" {
                        let c = match target {
                            10 => self.palette.foreground,
                            11 => self.palette.background,
                            _ => self.palette.cursor.unwrap_or(self.palette.foreground),
                        };
                        self.color_reply(&target.to_string(), c, st);
                    } else if let Some(c) = parse_color(spec) {
                        match target {
                            10 => self.palette.foreground = c,
                            11 => self.palette.background = c,
                            _ => self.palette.cursor = Some(c),
                        }
                        self.mark_all_dirty();
                    }
                }
            }
            110 => self.palette.foreground = self.base_palette.foreground,
            111 => self.palette.background = self.base_palette.background,
            112 => self.palette.cursor = None,
            _ => {}
        }
        if matches!(cmd, 110 | 111) {
            self.mark_all_dirty();
        }
    }

    /// ghostty's DesktopNotification.init: the title and the body cut to
    /// fit its buffers, never inside a UTF-8 sequence.
    fn desktop_notification(&mut self, title: &[u8], body: &[u8]) {
        fn cut(s: &[u8], cap: usize) -> String {
            let mut len = s.len().min(cap);
            while len > 0 && len < s.len() && s[len] & 0xC0 == 0x80 {
                len -= 1;
            }
            String::from_utf8_lossy(&s[..len]).into_owned()
        }
        self.messages.push(Message::DesktopNotification { title: cut(title, 63), body: cut(body, 255) });
    }

    fn color_reply(&mut self, prefix: &str, c: Rgb, st: &[u8]) {
        let (r, g, b) = (c.r as u16 * 257, c.g as u16 * 257, c.b as u16 * 257);
        let _ = write!(self.replies, "\x1b]{prefix};rgb:{r:04x}/{g:04x}/{b:04x}");
        self.replies.extend_from_slice(st);
    }

    // ---- Resize --------------------------------------------------------

    /// ghostty's Terminal.resize: the primary screen reflows while DECAWM is
    /// set, the alternate screen never does.
    pub fn resize(&mut self, cols: usize, rows: usize) {
        self.modes.sync_update = false;
        if cols == self.cols && rows == self.rows {
            self.mark_all_dirty();
            return;
        }
        let alt = self.alt_active;
        let (mut active, mut inactive) = (self.screen_cursor(), self.other_cursor);
        let (mut no_selection, mut no_view) = (None, 0);
        let on = (&mut active, &mut self.selection, &mut self.view_offset);
        let off = (&mut inactive, &mut no_selection, &mut no_view);
        let ((cursor, selection, view_offset), (alt_cursor, alt_selection, alt_view_offset)) =
            if alt { (off, on) } else { (on, off) };
        let [saved, alt_saved] = &mut self.saved;
        let tracked = Tracked { cursor, saved, selection, view_offset };
        let alt_tracked =
            Tracked { cursor: alt_cursor, saved: alt_saved, selection: alt_selection, view_offset: alt_view_offset };
        let (grid, alt_grid) = if alt { (&self.other, &self.grid) } else { (&self.grid, &self.other) };
        let primary = resize_screen(grid, cols, rows, self.autowrap, self.redraw, tracked);
        let alternate = resize_screen(alt_grid, cols, rows, false, Redraw::None, alt_tracked);
        (self.grid, self.other) = if alt { (alternate, primary) } else { (primary, alternate) };
        (self.x, self.y, self.pending_wrap) = (active.x, active.y, active.pending_wrap);
        self.other_cursor = inactive;
        // Unlike ghostty, which resets them, the tab stops in the remaining columns stay.
        let old = std::mem::replace(&mut self.tabs, default_tabs(cols));
        let keep = old.len().min(cols);
        self.tabs[..keep].copy_from_slice(&old[..keep]);
        self.cols = cols;
        self.rows = rows;
        self.top = 0;
        self.bottom = rows - 1;
        self.dirty.rows = vec![0; rows.div_ceil(64)];
        self.set_all_dirty();
    }
}

/// How the shell wants clicks at its prompt (OSC 133;A click_events or cl).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PromptClick {
    /// click_events=1: an SGR click with the absolute row.
    Absolute,
    /// click_events=2: an SGR click with the row from the prompt's.
    Relative,
    /// cl=line, m, v or w: arrow keys (ghostty moves along the line for all).
    Cl,
}

/// ghostty's OSC 22 names: CSS cursor names and their X11 aliases, as CSS.
fn mouse_shape(name: &str) -> Option<&'static str> {
    const CSS: [&str; 34] = [
        "default", "context-menu", "help", "pointer", "progress", "wait", "cell", "crosshair", "text",
        "vertical-text", "alias", "copy", "move", "no-drop", "not-allowed", "grab", "grabbing", "all-scroll",
        "col-resize", "row-resize", "n-resize", "e-resize", "s-resize", "w-resize", "ne-resize", "nw-resize",
        "se-resize", "sw-resize", "ew-resize", "ns-resize", "nesw-resize", "nwse-resize", "zoom-in", "zoom-out",
    ];
    if let Some(css) = CSS.iter().find(|&&c| c == name) {
        return Some(css);
    }
    Some(match name {
        "left_ptr" => "default",
        "question_arrow" => "help",
        "hand" => "pointer",
        "left_ptr_watch" => "progress",
        "watch" => "wait",
        "cross" => "crosshair",
        "xterm" => "text",
        "dnd-link" => "alias",
        "dnd-copy" => "copy",
        "dnd-move" => "move",
        "dnd-no-drop" => "no-drop",
        "crossed_circle" => "not-allowed",
        "hand1" => "grab",
        "right_side" => "e-resize",
        "top_side" => "n-resize",
        "top_right_corner" => "ne-resize",
        "top_left_corner" => "nw-resize",
        "bottom_side" => "s-resize",
        "bottom_right_corner" => "se-resize",
        "bottom_left_corner" => "sw-resize",
        "left_side" => "w-resize",
        "fleur" => "all-scroll",
        _ => return None,
    })
}

/// ghostty's osc.Parser.MAX_BUF: the fixed buffer that holds what follows
/// `9;` or `777;`. Parsers that terminate their text need one more byte.
const OSC_BUF: usize = 2048;

/// What ghostty's OSC 9 parser makes of the text after `9;`.
enum Osc9<'a> {
    /// An iTerm2 notification with this body.
    Notification(&'a [u8]),
    Progress(ProgressReport),
    /// ConEmu's 9;9, handled as OSC 7.
    Pwd(&'a [u8]),
    /// ConEmu's 9;12, handled as OSC 133;A.
    Prompt,
    /// Invalid, or a ConEmu command ghostty does not implement.
    Ignored,
}

/// ghostty's osc9.parse: ConEmu's commands when the text matches one,
/// otherwise an iTerm2 notification.
fn parse_osc9(data: &[u8]) -> Osc9<'_> {
    if data.len() > OSC_BUF {
        return Osc9::Ignored;
    }
    let terminated = |cmd| {
        if data.len() < OSC_BUF { cmd } else { Osc9::Ignored }
    };
    match data {
        // Sleep, xterm emulation, comment.
        [b'1', b';', ..] | [b'1', b'0'] | [b'1', b'0', b';', b'0'..=b'3', ..] | [b'1', b'1', b';', ..] => Osc9::Ignored,
        [b'1', b'2', ..] => Osc9::Prompt,
        // Message box, tab title, GUI macro, run process, environment variable.
        [b'2' | b'3' | b'6' | b'7' | b'8', b';', ..] => Osc9::Ignored,
        [b'4', b';', state @ b'0'..=b'4', rest @ ..] => {
            let state = match state {
                b'0' => ProgressState::Remove,
                b'1' => ProgressState::Set,
                b'2' => ProgressState::Error,
                b'3' => ProgressState::Indeterminate,
                _ => ProgressState::Pause,
            };
            let mut progress = (state == ProgressState::Set).then_some(0);
            if matches!(state, ProgressState::Set | ProgressState::Error | ProgressState::Pause)
                && let [b';', value @ ..] = rest
            {
                // ghostty's lib.parseInt takes no sign for an unsigned type.
                progress = std::str::from_utf8(value)
                    .ok()
                    .filter(|v| !v.starts_with('+'))
                    .and_then(|v| v.parse::<usize>().ok())
                    .map(|p| p.min(100) as u8);
            }
            Osc9::Progress(ProgressReport { state, progress })
        }
        // Wait for input.
        [b'5', ..] => Osc9::Ignored,
        [b'9', b';', url @ ..] => terminated(Osc9::Pwd(url)),
        _ => terminated(Osc9::Notification(data)),
    }
}

/// ghostty's rxvt_extension.parse: `notify;TITLE;BODY` after `777;`.
fn parse_osc777(data: &[u8]) -> Option<(&[u8], &[u8])> {
    if data.len() >= OSC_BUF {
        return None;
    }
    let rest = data.strip_prefix(b"notify;")?;
    let t = rest.iter().position(|&b| b == b';')?;
    Some((&rest[..t], &rest[t + 1..]))
}

/// The path of an OSC 7 URL: `file://host/path` (percent-decoded) or
/// `kitty-shell-cwd://host/path` (raw), only for this host or localhost.
fn parse_cwd(s: &[u8], hostname: &str) -> Option<String> {
    let (rest, raw) = match (s.strip_prefix(b"file://"), s.strip_prefix(b"kitty-shell-cwd://")) {
        (Some(rest), _) => (rest, false),
        (_, Some(rest)) => (rest, true),
        _ => return None,
    };
    let slash = rest.iter().position(|&b| b == b'/')?;
    let host = String::from_utf8(percent_decode(&rest[..slash])).ok()?;
    if host != "localhost" && host != hostname {
        return None;
    }
    let path = &rest[slash..];
    let path = if raw { path.to_vec() } else { percent_decode(path) };
    Some(String::from_utf8_lossy(&path).into_owned())
}

fn percent_decode(s: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(s.len());
    let mut i = 0;
    while i < s.len() {
        let hex = |c: u8| (c as char).to_digit(16);
        if s[i] == b'%'
            && let (Some(h), Some(l)) = (s.get(i + 1).and_then(|&c| hex(c)), s.get(i + 2).and_then(|&c| hex(c)))
        {
            out.push((h * 16 + l) as u8);
            i += 3;
            continue;
        }
        out.push(s[i]);
        i += 1;
    }
    out
}

/// gethostname(2), as ghostty's isLocal compares OSC 7 hosts with it.
fn local_hostname() -> &'static str {
    static NAME: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    NAME.get_or_init(|| {
        std::fs::read_to_string("/proc/sys/kernel/hostname").map(|s| s.trim_end().to_owned()).unwrap_or_default()
    })
}

/// The terminfo capabilities tapust reports through XTGETTCAP, so that
/// programs learn about features xterm-256color's entry lacks: the value of a
/// string or number, or None for a boolean.
fn capability(name: &[u8]) -> Option<Option<&'static str>> {
    Some(match name {
        b"TN" | b"name" => Some("xterm-256color"),
        b"Co" | b"colors" => Some("256"),
        b"RGB" | b"Tc" => None,
        b"setrgbf" => Some("\x1b[38;2;%p1%d;%p2%d;%p3%dm"),
        b"setrgbb" => Some("\x1b[48;2;%p1%d;%p2%d;%p3%dm"),
        b"Ss" => Some("\x1b[%p1%d q"),
        b"Se" => Some("\x1b[0 q"),
        b"Sync" => Some("\x1b[?2026%?%p1%{1}%-%tl%eh%;"),
        b"Smulx" => Some("\x1b[4:%p1%dm"),
        b"Setulc" => Some("\x1b[58:2::%p1%{65536}%/%d:%p1%{256}%/%{255}%&%d:%p1%{255}%&%dm"),
        _ => return None,
    })
}

/// Hex digits to bytes, or None if they are not pairs of hex digits.
fn decode_hex(hex: &[u8]) -> Option<Vec<u8>> {
    if hex.is_empty() || !hex.len().is_multiple_of(2) {
        return None;
    }
    hex.chunks(2).map(|pair| u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok()).collect()
}
