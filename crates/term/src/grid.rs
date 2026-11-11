//! Row storage.
//!
//! Every row lives in a fixed-stride slot (`cols` cells) of one slab. The
//! screen is a ring of `rows` slot indices and the scrollback is a ring of at
//! most `hist_cap` slot indices, so scrolling moves indices instead of cells.

use crate::cell::{Cell, StyleId, Width};

/// Column of a grapheme entry whose cell was deleted.
const FREE: u16 = u16::MAX;
/// Upper bound of a grapheme cluster's UTF-8 length.
const MAX_GRAPHEME: usize = 256;

/// ghostty's row semantic_prompt values.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PromptMark {
    #[default]
    None,
    Prompt,
    Continuation,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct RowMeta {
    /// Upper bound of the written columns: cells at `occ..` are `Cell::EMPTY`.
    pub occ: u16,
    /// The row was soft-wrapped and its logical line continues on the next row.
    pub wrapped: bool,
    /// ghostty's row semantic_prompt: the row starts a prompt or continues one.
    pub prompt: PromptMark,
    /// Entries of `graphemes` in use, oldest first.
    glen: u32,
    /// Combining codepoints: (column, base character followed by its marks).
    /// Entries past `glen` are spares whose strings are reused, so recycled
    /// rows do not allocate. An entry may be stale or shadowed by a newer one
    /// for the same column; the cell's grapheme bit and recency decide.
    graphemes: Vec<(u16, String)>,
}

impl RowMeta {
    #[inline(always)]
    pub fn touch(&mut self, end: usize) {
        if (self.occ as usize) < end {
            self.occ = end as u16;
        }
    }

    fn live(&self) -> &[(u16, String)] {
        &self.graphemes[..self.glen as usize]
    }

    pub fn grapheme(&self, x: u16) -> Option<&str> {
        self.live().iter().rev().find(|g| g.0 == x).map(|g| g.1.as_str())
    }

    /// The newest entry of each column, newest first; may include stale ones.
    pub fn graphemes(&self) -> impl Iterator<Item = (usize, &str)> {
        let live = self.live();
        live.iter()
            .enumerate()
            .rev()
            .filter(|&(i, g)| g.0 != FREE && !live[i + 1..].iter().any(|h| h.0 == g.0))
            .map(|(_, g)| (g.0 as usize, g.1.as_str()))
    }

    /// Adds an entry for column `x`, shadowing any older one.
    fn new_grapheme(&mut self, x: usize) -> &mut String {
        let i = self.glen as usize;
        if i == self.graphemes.len() {
            self.graphemes.push((x as u16, String::new()));
        } else {
            self.graphemes[i].0 = x as u16;
        }
        self.glen += 1;
        let s = &mut self.graphemes[i].1;
        s.clear();
        s
    }

    pub fn push_grapheme(&mut self, x: usize, s: &str) {
        self.new_grapheme(x).push_str(s);
    }

    /// Drops stale and shadowed entries.
    fn compact_graphemes(&mut self, row: &mut [Cell]) {
        let live = self.glen as usize;
        let mut keep = Vec::with_capacity(live);
        // The grapheme bit marks columns still to be claimed, newest entry first.
        for i in (0..live).rev() {
            let x = self.graphemes[i].0 as usize;
            if self.graphemes[i].0 != FREE && row[x].has_grapheme() {
                row[x] = row[x].with_grapheme(false);
                keep.push(i);
            }
        }
        keep.reverse();
        for (n, &i) in keep.iter().enumerate() {
            self.graphemes.swap(n, i);
            let x = self.graphemes[n].0 as usize;
            row[x] = row[x].with_grapheme(true);
        }
        self.glen = keep.len() as u32;
    }

    #[inline]
    pub fn clear_graphemes(&mut self) {
        self.glen = 0;
    }

    /// Follows cells `from..` moving right by `n` (freeing those pushed past `cols`).
    pub fn shift_graphemes_right(&mut self, from: usize, n: usize, cols: usize) {
        let live = self.glen as usize;
        for g in &mut self.graphemes[..live] {
            let x = g.0 as usize;
            if g.0 != FREE && x >= from {
                g.0 = if x + n < cols { (x + n) as u16 } else { FREE };
            }
        }
    }

    /// Follows cells `from + n..` moving left by `n`; cells `from..from + n` are deleted.
    pub fn shift_graphemes_left(&mut self, from: usize, n: usize) {
        let live = self.glen as usize;
        for g in &mut self.graphemes[..live] {
            let x = g.0 as usize;
            if g.0 != FREE && x >= from {
                g.0 = if x >= from + n { (x - n) as u16 } else { FREE };
            }
        }
    }
}

/// Appends the zero-width `cp` to the grapheme of cell `x`; false if the
/// cell is empty.
#[inline]
pub(crate) fn attach_mark(row: &mut [Cell], meta: &mut RowMeta, x: usize, cp: u32) -> bool {
    let base = row[x];
    let (Some(bc), Some(ch)) = (base.char(), char::from_u32(cp)) else {
        return false;
    };
    if base.has_grapheme() {
        let live = meta.glen as usize;
        if let Some(g) = meta.graphemes[..live].iter_mut().rev().find(|g| g.0 as usize == x) {
            if g.1.len() < MAX_GRAPHEME {
                g.1.push(ch);
            }
            return true;
        }
    }
    if meta.glen as usize > row.len() {
        meta.compact_graphemes(row);
    }
    let s = meta.new_grapheme(x);
    s.push(bc);
    s.push(ch);
    row[x] = base.with_grapheme(true);
    true
}

/// A cell with the content removed but the style (background) kept.
#[inline(always)]
pub(crate) fn blankify(c: Cell) -> Cell {
    Cell::new(0, Width::Narrow, c.style())
}

/// Fixed rows outside a scroll region that are cheaper to move one by one
/// than moving the region.
const FEW_ROWS: usize = 2;

pub(crate) struct Grid {
    cols: usize,
    rows: usize,
    cells: Vec<Cell>,
    meta: Vec<RowMeta>,
    screen: Vec<u32>,
    head: usize,
    hist: Vec<u32>,
    hist_start: usize,
    hist_cap: usize,
    free: Vec<u32>,
    /// Lines ever moved into the scrollback, including those dropped since:
    /// the absolute number of screen row 0.
    pushed: u64,
}

impl Grid {
    pub fn new(cols: usize, rows: usize, hist_cap: usize) -> Grid {
        Grid {
            cols,
            rows,
            cells: vec![Cell::EMPTY; cols * rows],
            meta: vec![RowMeta::default(); rows],
            screen: (0..rows as u32).collect(),
            head: 0,
            hist: Vec::new(),
            hist_start: 0,
            hist_cap,
            free: Vec::new(),
            pushed: 0,
        }
    }

    pub fn hist_cap(&self) -> usize {
        self.hist_cap
    }

    #[inline(always)]
    fn ring(&self, y: usize) -> usize {
        let i = self.head + y;
        if i >= self.rows { i - self.rows } else { i }
    }

    #[inline(always)]
    pub fn slot(&self, y: usize) -> usize {
        self.screen[self.ring(y)] as usize
    }

    #[inline(always)]
    pub fn slot_cells(&self, slot: usize) -> &[Cell] {
        let s = slot * self.cols;
        &self.cells[s..s + self.cols]
    }

    #[inline(always)]
    pub fn slot_meta(&self, slot: usize) -> &RowMeta {
        &self.meta[slot]
    }

    #[inline(always)]
    pub fn row(&self, y: usize) -> &[Cell] {
        self.slot_cells(self.slot(y))
    }

    #[inline(always)]
    pub fn meta(&self, y: usize) -> &RowMeta {
        &self.meta[self.slot(y)]
    }

    #[inline(always)]
    pub fn meta_mut(&mut self, y: usize) -> &mut RowMeta {
        let slot = self.slot(y);
        &mut self.meta[slot]
    }

    #[inline(always)]
    pub fn row_meta_mut(&mut self, y: usize) -> (&mut [Cell], &mut RowMeta) {
        let slot = self.slot(y);
        let s = slot * self.cols;
        (&mut self.cells[s..s + self.cols], &mut self.meta[slot])
    }

    pub fn pushed(&self) -> u64 {
        self.pushed
    }

    pub fn hist_len(&self) -> usize {
        self.hist.len()
    }

    /// Slot of history line `i` (0 = oldest).
    #[inline]
    fn hist_slot(&self, i: usize) -> usize {
        let j = self.hist_start + i;
        let n = self.hist.len();
        self.hist[if j >= n { j - n } else { j }] as usize
    }

    /// Slot of line `line`, counting from the oldest history line through the screen.
    #[inline]
    pub fn line_slot(&self, line: usize) -> usize {
        let h = self.hist.len();
        if line < h { self.hist_slot(line) } else { self.slot(line - h) }
    }

    /// All cells of the slab, including free slots (which are always empty).
    pub fn cells(&self) -> &[Cell] {
        &self.cells
    }

    #[inline]
    fn clear_slot(&mut self, slot: usize, blank: Cell) {
        let cols = self.cols;
        let m = &mut self.meta[slot];
        let s = slot * cols;
        if blank == Cell::EMPTY {
            let occ = m.occ as usize;
            // Cells past `occ` are already empty, so a short row can be
            // cleared with a fixed-size store instead of a memset call.
            if occ == 0 {
            } else if occ <= 8 && cols >= 8 {
                self.cells[s..s + 8].fill(Cell::EMPTY);
            } else {
                self.cells[s..s + occ].fill(Cell::EMPTY);
            }
            m.occ = 0;
        } else {
            self.cells[s..s + cols].fill(blank);
            m.occ = cols as u16;
        }
        m.wrapped = false;
        m.prompt = PromptMark::None;
        m.clear_graphemes();
    }

    pub fn clear_row(&mut self, y: usize, blank: Cell) {
        let slot = self.slot(y);
        self.clear_slot(slot, blank);
    }

    /// A free slot, growing the slab geometrically up to `rows + hist_cap` slots.
    fn alloc_slot(&mut self) -> u32 {
        if let Some(s) = self.free.pop() {
            return s;
        }
        let total = self.meta.len();
        let max = self.rows + self.hist_cap;
        let grow = total.max(64).min(max - total);
        debug_assert!(grow > 0);
        self.cells.reserve_exact(grow * self.cols);
        self.cells.resize((total + grow) * self.cols, Cell::EMPTY);
        self.meta.resize_with(total + grow, RowMeta::default);
        self.free.reserve_exact(grow);
        self.free.extend((total as u32 + 1..(total + grow) as u32).rev());
        total as u32
    }

    /// Moves `slot` into the scrollback and returns a slot to reuse for a new row.
    #[inline]
    fn push_history(&mut self, slot: u32) -> u32 {
        self.pushed += 1;
        if self.hist.len() < self.hist_cap {
            // The ring starts at 0 until it is full.
            self.hist.push(slot);
            self.alloc_slot()
        } else {
            let old = self.hist[self.hist_start];
            self.hist[self.hist_start] = slot;
            self.hist_start += 1;
            if self.hist_start == self.hist_cap {
                self.hist_start = 0;
            }
            old
        }
    }

    pub fn clear_history(&mut self) {
        for i in 0..self.hist.len() {
            let slot = self.hist_slot(i);
            self.clear_slot(slot, Cell::EMPTY);
            self.free.push(slot as u32);
        }
        self.hist.clear();
        self.hist_start = 0;
    }

    /// Scrolls rows `top..=bottom` up by `n`, filling the new rows with
    /// `blank`. With `keep_history`, rows leaving a region that starts at the
    /// top of the screen move into the scrollback; returns how many did.
    #[inline]
    pub fn scroll_up(&mut self, top: usize, bottom: usize, n: usize, blank: Cell, keep_history: bool) -> usize {
        let rows = self.rows;
        let n = n.min(bottom + 1 - top);
        if top == 0 && bottom == rows - 1 {
            let save = keep_history && self.hist_cap > 0;
            for _ in 0..n {
                let old = self.screen[self.head];
                let new = if save { self.push_history(old) } else { old };
                self.screen[self.head] = new;
                self.head += 1;
                if self.head == rows {
                    self.head = 0;
                }
                self.clear_slot(new as usize, blank);
            }
            return if save { n } else { 0 };
        }
        if top == 0 && keep_history && self.hist_cap > 0 {
            for _ in 0..n {
                self.scroll_up_one(top, bottom, blank, true);
            }
            return n;
        }
        let (a, b) = self.contiguous(top, bottom);
        self.screen[a..=b].rotate_left(n);
        for i in b + 1 - n..=b {
            self.clear_slot(self.screen[i] as usize, blank);
        }
        0
    }

    /// `scroll_up` by one row, the case of every linefeed at the bottom margin.
    #[inline]
    pub fn scroll_up_one(&mut self, top: usize, bottom: usize, blank: Cell, keep_history: bool) -> bool {
        let rows = self.rows;
        // As in xterm, rows leaving a region at the top of the screen are saved.
        let saved = top == 0 && keep_history && self.hist_cap > 0;
        let slot = if top == 0 && bottom == rows - 1 {
            let old = self.screen[self.head];
            let new = if saved { self.push_history(old) } else { old };
            self.screen[self.head] = new;
            self.head += 1;
            if self.head == rows {
                self.head = 0;
            }
            new
        } else if top == 0 && rows - 1 - bottom <= FEW_ROWS {
            // Rotate the ring and move the fixed rows below the region back.
            let old = self.screen[self.head];
            let mut carry = if saved { self.push_history(old) } else { old };
            self.head = self.ring(1);
            for y in bottom..rows {
                let i = self.ring(y);
                std::mem::swap(&mut self.screen[i], &mut carry);
            }
            self.screen[self.ring(bottom)]
        } else if bottom == rows - 1 && top <= FEW_ROWS {
            // Rotate the ring and move the fixed rows above the region back.
            self.head = self.ring(1);
            let last = self.ring(rows - 1);
            let mut carry = self.screen[last];
            for y in 0..top {
                let i = self.ring(y);
                std::mem::swap(&mut self.screen[i], &mut carry);
            }
            self.screen[last] = carry;
            carry
        } else {
            let (a, b) = self.contiguous(top, bottom);
            let old = self.screen[a];
            let new = if saved { self.push_history(old) } else { old };
            let region = &mut self.screen[a..=b];
            region.copy_within(1.., 0);
            region[b - a] = new;
            new
        };
        self.clear_slot(slot as usize, blank);
        saved
    }

    /// Scrolls rows `top..=bottom` down by `n`, filling the new rows with `blank`.
    pub fn scroll_down(&mut self, top: usize, bottom: usize, n: usize, blank: Cell) {
        let rows = self.rows;
        let n = n.min(bottom + 1 - top);
        if top == 0 && bottom == rows - 1 {
            self.head = (self.head + rows - n) % rows;
            for y in 0..n {
                self.clear_row(y, blank);
            }
            return;
        }
        let (a, b) = self.contiguous(top, bottom);
        self.screen[a..=b].rotate_right(n);
        for i in a..a + n {
            self.clear_slot(self.screen[i] as usize, blank);
        }
    }

    /// The physical range of screen rows `top..=bottom` in the ring, first
    /// unrotating the ring if the rows wrap around its end.
    #[inline]
    fn contiguous(&mut self, top: usize, bottom: usize) -> (usize, usize) {
        let (a, b) = (self.ring(top), self.ring(bottom));
        if a <= b {
            return (a, b);
        }
        self.screen.rotate_left(self.head);
        self.head = 0;
        (top, bottom)
    }

    /// Replaces every style id in the slab through `remap`.
    pub fn remap_styles(&mut self, remap: &[u16]) {
        for c in &mut self.cells {
            let id = remap[c.style().0 as usize];
            *c = Cell((c.0 & !(0xFFFF << 32)) | ((id as u64) << 32));
        }
    }

    fn with_pushed(mut self, pushed: u64) -> Grid {
        self.pushed = pushed;
        self
    }

    /// A grid of a new size holding the given history and screen rows.
    fn from_rows(cols: usize, rows: usize, hist_cap: usize, hist: Vec<NewRow>, screen: Vec<NewRow>) -> Grid {
        let mut g = Grid::new(cols, rows, hist_cap);
        let nh = hist.len();
        g.cells = Vec::with_capacity((nh + rows) * cols);
        g.meta = Vec::with_capacity(nh + rows);
        for r in hist.into_iter().chain(screen) {
            debug_assert_eq!(r.cells.len(), cols);
            g.cells.extend_from_slice(&r.cells);
            g.meta.push(r.meta);
        }
        g.hist = (0..nh as u32).collect();
        g.screen = (nh as u32..(nh + rows) as u32).collect();
        g
    }

    /// Resizes without reflow (alternate screen): rows are truncated or padded
    /// at the right and the bottom; rows are dropped from the top if needed to
    /// keep row `keep_y` on screen. Returns the grid and the shift applied to y.
    pub fn resized(&self, cols: usize, rows: usize, keep_y: usize) -> (Grid, usize) {
        let shift = (keep_y + 1).saturating_sub(rows);
        let mut screen = Vec::with_capacity(rows);
        for y in 0..rows {
            let mut r = NewRow::blank(cols);
            let oy = y + shift;
            if oy < self.rows {
                let old = self.row(oy);
                let m = self.meta(oy);
                let n = cols.min(self.cols);
                r.cells[..n].copy_from_slice(&old[..n]);
                if n < self.cols && r.cells[n - 1].width() == Width::Wide {
                    r.cells[n - 1] = blankify(r.cells[n - 1]);
                }
                r.meta.occ = (m.occ as usize).min(cols) as u16;
                r.meta.wrapped = m.wrapped && cols == self.cols;
                for (x, s) in m.graphemes().filter(|g| g.0 < n) {
                    r.meta.push_grapheme(x, s);
                }
            }
            screen.push(r);
        }
        (Grid::from_rows(cols, rows, self.hist_cap, Vec::new(), screen).with_pushed(self.pushed), shift)
    }

    /// Resizes with reflow: logical lines joined by soft-wrap marks are
    /// rewrapped at the new width and the cursor stays on its content.
    pub fn reflowed(&self, cols: usize, rows: usize, cur: ReflowCursor) -> (Grid, ReflowCursor) {
        let lines = self.logical_lines(cur);
        let (out, pos) = rewrap(&lines.lines, cols, lines.cursor);
        let (mut cr, mut cc) = pos;
        let mut out = out;
        let mut pending = false;
        if cur.pending {
            if cc + 1 < cols {
                cc += 1;
            } else {
                pending = true;
            }
        }
        let total = out.len();
        let start = total.saturating_sub(rows).min(cr);
        out.truncate(start + rows);
        let mut screen = out.split_off(start);
        screen.resize_with(rows, || NewRow::blank(cols));
        let hist_drop = start.saturating_sub(self.hist_cap);
        let hist: Vec<NewRow> = out.into_iter().skip(hist_drop).collect();
        cr -= start;
        let grid = Grid::from_rows(cols, rows, self.hist_cap, hist, screen).with_pushed(self.pushed);
        (grid, ReflowCursor { x: cc.min(cols - 1), y: cr, pending })
    }

    fn logical_lines(&self, cur: ReflowCursor) -> Lines {
        let total = self.hist.len() + self.rows;
        let cursor_abs = self.hist.len() + cur.y;
        let mut lines = Vec::new();
        let mut line = Line::default();
        let mut cursor = (0, 0);
        for abs in 0..total {
            let slot = self.line_slot(abs);
            let row = self.slot_cells(slot);
            let meta = &self.meta[slot];
            let mut len = if meta.wrapped {
                if row[self.cols - 1].width() == Width::SpacerHead { self.cols - 1 } else { self.cols }
            } else {
                row[..meta.occ as usize].iter().rposition(|c| *c != Cell::EMPTY).map_or(0, |p| p + 1)
            };
            if abs == cursor_abs {
                cursor = (lines.len(), line.cells.len() + cur.x);
                len = len.max(cur.x + 1);
            }
            let base = line.cells.len();
            line.cells.extend_from_slice(&row[..len]);
            for (col, s) in meta.graphemes() {
                if col < len && row[col].has_grapheme() {
                    line.graphemes.push((base + col, s.to_owned()));
                }
            }
            if !meta.wrapped {
                line.graphemes.sort_unstable_by_key(|g| g.0);
                lines.push(std::mem::take(&mut line));
            }
        }
        if !line.cells.is_empty() {
            line.graphemes.sort_unstable_by_key(|g| g.0);
            lines.push(line);
        }
        // Blank lines below the cursor are not content.
        while lines.len() > cursor.0 + 1 && lines.last().is_some_and(|l| l.cells.is_empty()) {
            lines.pop();
        }
        Lines { lines, cursor }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ReflowCursor {
    pub x: usize,
    pub y: usize,
    pub pending: bool,
}

#[derive(Default)]
struct Line {
    cells: Vec<Cell>,
    graphemes: Vec<(usize, String)>,
}

struct Lines {
    lines: Vec<Line>,
    /// (line index, cell offset) of the cursor.
    cursor: (usize, usize),
}

struct NewRow {
    cells: Vec<Cell>,
    meta: RowMeta,
}

impl NewRow {
    fn blank(cols: usize) -> NewRow {
        NewRow { cells: vec![Cell::EMPTY; cols], meta: RowMeta::default() }
    }
}

/// Splits logical lines into rows of `cols` cells; returns the rows and the
/// (row, column) of the cell at the cursor offset.
fn rewrap(lines: &[Line], cols: usize, cursor: (usize, usize)) -> (Vec<NewRow>, (usize, usize)) {
    let mut out: Vec<NewRow> = Vec::new();
    let mut pos = (0, 0);
    for (li, line) in lines.iter().enumerate() {
        let mut graphemes = line.graphemes.iter().peekable();
        out.push(NewRow::blank(cols));
        let mut col = 0;
        let mut i = 0;
        while i < line.cells.len() {
            let mut c = line.cells[i];
            let mut w = 1;
            match c.width() {
                Width::Wide if cols >= 2 => w = 2,
                Width::Narrow => {}
                _ => c = blankify(c),
            }
            if col + w > cols {
                let r = out.last_mut().unwrap();
                if w == 2 {
                    r.cells[col] = Cell::new(0, Width::SpacerHead, StyleId::DEFAULT);
                    r.meta.touch(col + 1);
                }
                r.meta.wrapped = true;
                out.push(NewRow::blank(cols));
                col = 0;
            }
            let ri = out.len() - 1;
            let r = &mut out[ri];
            r.cells[col] = c;
            if w == 2 {
                r.cells[col + 1] = Cell::new(0, Width::SpacerTail, c.style());
            }
            r.meta.touch(col + w);
            while let Some((gi, s)) = graphemes.next_if(|g| g.0 <= i) {
                if *gi == i {
                    r.meta.push_grapheme(col, s);
                }
            }
            if li == cursor.0 {
                if cursor.1 == i {
                    pos = (ri, col);
                } else if w == 2 && cursor.1 == i + 1 {
                    pos = (ri, col + 1);
                }
            }
            col += w;
            i += if w == 2 && line.cells.get(i + 1).is_some_and(|n| n.width() == Width::SpacerTail) { 2 } else { 1 };
        }
    }
    (out, pos)
}
