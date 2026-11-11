//! Terminal state and the actions dispatched by the parser.

use std::io::Write as _;

use unicode_width::UnicodeWidthChar;

use crate::cell::{Cell, StyleId, Width};
use crate::grid::{Grid, ReflowCursor, attach_mark, blankify};
use crate::palette::{default_palette, parse_color};
use crate::parser::Params;
use crate::style::{Pen, StyleTable, apply_sgr};
use crate::{CursorShape, Dirty, Modes, MouseEncoding, MouseMode, Palette, Rgb};

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
    top: usize,
    bottom: usize,
    tabs: Vec<bool>,
    /// DECSC slots of the primary and alternate screens.
    saved: [Option<Saved>; 2],
    pub dirty: Dirty,
    /// Every row is marked in `dirty`.
    all_dirty: bool,
    pub view_offset: usize,
    pub replies: Vec<u8>,
    pub title: String,
    pub cwd: Option<String>,
    pub bell: bool,
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
            modes: Modes::default(),
            cursor_visible: true,
            cursor_shape: CursorShape::Block,
            // ghostty's default for DEC mode 12 (cursor-style-blink unset).
            cursor_blink: true,
            top: 0,
            bottom: rows - 1,
            tabs: default_tabs(cols),
            saved: [None, None],
            dirty: Dirty { rows: vec![0; rows.div_ceil(64)] },
            all_dirty: false,
            view_offset: 0,
            replies: Vec::new(),
            title: String::new(),
            cwd: None,
            bell: false,
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
        self.tpl = (id.0 as u64) << 32;
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
                self.tpl = (self.intern(pen).0 as u64) << 32;
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
            if self.grid.scroll_up_one(self.top, self.bottom, blank, !self.alt_active) && self.view_offset != 0 {
                self.view_offset = (self.view_offset + 1).min(self.grid.hist_len());
            }
            self.mark_all_dirty();
        } else if self.y + 1 < self.rows {
            self.y += 1;
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
        let pushed = self.grid.scroll_up(self.top, self.bottom, n, blank, !self.alt_active);
        if pushed != 0 && self.view_offset != 0 {
            self.view_offset = (self.view_offset + pushed).min(self.grid.hist_len());
        }
        self.mark_all_dirty();
    }

    fn scroll_down(&mut self, n: usize) {
        let blank = self.blank();
        self.grid.scroll_down(self.top, self.bottom, n, blank);
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

    fn switch_screen(&mut self, alt: bool) {
        if alt == self.alt_active {
            return;
        }
        std::mem::swap(&mut self.grid, &mut self.other);
        self.alt_active = alt;
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
    fn full_reset(&mut self) {
        let hist = self.primary().hist_cap();
        let replies = std::mem::take(&mut self.replies);
        let cwd = self.cwd.take();
        let bell = self.bell;
        let base = std::mem::take(&mut self.base_palette);
        *self = Term::new(self.cols, self.rows, hist);
        self.palette = base.clone();
        self.base_palette = base;
        self.replies = replies;
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
                }
            }
            1005 | 1006 | 1015 => {
                let enc = match mode {
                    1005 => MouseEncoding::Utf8,
                    1006 => MouseEncoding::Sgr,
                    _ => MouseEncoding::Urxvt,
                };
                if on {
                    m.mouse_encoding = enc;
                } else if m.mouse_encoding == enc {
                    m.mouse_encoding = MouseEncoding::Default;
                }
            }
            1004 => m.focus_events = on,
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
            (0, b'c') => self.full_reset(),
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

    pub fn osc_dispatch(&mut self, data: &[u8], bel: bool) {
        let (cmd, rest) = match data.iter().position(|&b| b == b';') {
            Some(i) => (&data[..i], &data[i + 1..]),
            None => (data, &data[data.len()..]),
        };
        let Some(cmd) = std::str::from_utf8(cmd).ok().and_then(|s| s.parse::<u16>().ok()) else {
            return;
        };
        let st: &[u8] = if bel { b"\x07" } else { b"\x1b\\" };
        match cmd {
            0 | 2 => {
                self.title.clear();
                self.title.push_str(&String::from_utf8_lossy(rest));
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
            7 => self.cwd = parse_cwd(rest),
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

    fn color_reply(&mut self, prefix: &str, c: Rgb, st: &[u8]) {
        let (r, g, b) = (c.r as u16 * 257, c.g as u16 * 257, c.b as u16 * 257);
        let _ = write!(self.replies, "\x1b]{prefix};rgb:{r:04x}/{g:04x}/{b:04x}");
        self.replies.extend_from_slice(st);
    }

    // ---- Resize --------------------------------------------------------

    pub fn resize(&mut self, cols: usize, rows: usize) {
        if cols == self.cols && rows == self.rows {
            self.mark_all_dirty();
            return;
        }
        let pcur = if self.alt_active {
            let s = self.saved[0].clone().unwrap_or_default();
            ReflowCursor { x: s.x, y: s.y, pending: s.pending_wrap }
        } else {
            ReflowCursor { x: self.x, y: self.y, pending: self.pending_wrap }
        };
        let (primary, pcur) = self.primary().reflowed(cols, rows, pcur);
        if self.alt_active {
            let (alt, shift) = self.grid.resized(cols, rows, self.y);
            self.grid = alt;
            self.other = primary;
            self.y -= shift;
            if let Some(s) = &mut self.saved[0] {
                (s.x, s.y, s.pending_wrap) = (pcur.x, pcur.y, pcur.pending);
            }
        } else {
            self.other = self.other.resized(cols, rows, 0).0;
            self.grid = primary;
            (self.x, self.y, self.pending_wrap) = (pcur.x, pcur.y, pcur.pending);
        }
        self.cols = cols;
        self.rows = rows;
        self.x = self.x.min(cols - 1);
        self.y = self.y.min(rows - 1);
        if self.pending_wrap && self.x != cols - 1 {
            self.pending_wrap = false;
        }
        for s in self.saved.iter_mut().flatten() {
            s.x = s.x.min(cols - 1);
            s.y = s.y.min(rows - 1);
        }
        self.top = 0;
        self.bottom = rows - 1;
        let old = std::mem::take(&mut self.tabs);
        self.tabs = default_tabs(cols);
        let keep = old.len().min(cols);
        self.tabs[..keep].copy_from_slice(&old[..keep]);
        self.view_offset = 0;
        self.dirty.rows = vec![0; rows.div_ceil(64)];
        self.set_all_dirty();
    }
}

/// The path of an OSC 7 `file://host/path` URL, percent-decoded.
fn parse_cwd(s: &[u8]) -> Option<String> {
    let path = match s.strip_prefix(b"file://") {
        Some(rest) => &rest[rest.iter().position(|&b| b == b'/')?..],
        None if s.first() == Some(&b'/') => s,
        None => return None,
    };
    let mut out = Vec::with_capacity(path.len());
    let mut i = 0;
    while i < path.len() {
        let b = path[i];
        let hex = |c: u8| (c as char).to_digit(16);
        if b == b'%'
            && let (Some(h), Some(l)) = (path.get(i + 1).and_then(|&c| hex(c)), path.get(i + 2).and_then(|&c| hex(c)))
        {
            out.push((h * 16 + l) as u8);
            i += 3;
            continue;
        }
        out.push(b);
        i += 1;
    }
    Some(String::from_utf8_lossy(&out).into_owned())
}
