//! Row storage.
//!
//! Every row lives in a fixed-stride slot (`cols` cells) of one slab. The
//! screen is a ring of `rows` slot indices and the scrollback is a ring of at
//! most `hist_cap` slot indices, so scrolling moves indices instead of cells.

use crate::cell::{Cell, StyleId, Width};

/// Column of a grapheme entry whose cell was deleted.
const FREE: u16 = u16::MAX;
/// Upper bound of a grapheme cluster's UTF-8 length.
pub(crate) const MAX_GRAPHEME: usize = 256;

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

    /// Frees the entries of columns `from..`.
    fn free_graphemes_from(&mut self, from: usize) {
        let live = self.glen as usize;
        for g in &mut self.graphemes[..live] {
            if g.0 != FREE && g.0 as usize >= from {
                g.0 = FREE;
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

#[derive(Clone)]
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
    /// the absolute number of screen row 0. A resize counts the rows above
    /// the screen, so the oldest stored line keeps its number.
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

    /// The absolute number of the oldest stored line.
    pub fn first_line(&self) -> u64 {
        self.pushed - self.hist.len() as u64
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

    /// Resizes as ghostty's PageList.resize does, except that the cursor's
    /// row stays on the screen, then drops the history the scrollback limit
    /// does not allow. `pins` (line from the oldest stored line, column),
    /// the cursor's first, follow their cells as ghostty's tracked pins.
    /// Those on rows dropped from the history go to the first cell of the
    /// oldest remaining row, those on rows cut below the screen to the line
    /// after the screen.
    pub fn resize(&self, cols: usize, rows: usize, reflow: bool, pins: &mut [(usize, usize)]) -> Grid {
        let mut s = Resize::new(self);
        let cursor_y = pins[0].0 - self.hist.len();
        // More rows come from the history only for a cursor on the last row.
        let pull = cursor_y + 1 >= self.rows;
        if !reflow {
            s.set_cols(cols, pins);
            s.set_rows(rows, pull, pins);
        } else if cols > self.cols {
            s.reflow(cols, cursor_y, pins);
            s.set_rows(rows, pull, pins);
        } else {
            s.set_rows(rows, pull, pins);
            if cols < self.cols {
                s.reflow(cols, pins[0].0 - s.top(), pins);
            }
        }
        let hist = s.list.len() - rows;
        let drop = hist.saturating_sub(self.hist_cap);
        s.list.drain(..drop);
        let end = s.list.len();
        for p in pins.iter_mut() {
            *p = match p.0 {
                CUT => (end, p.1),
                line => line.checked_sub(drop).map_or((0, 0), |line| (line, p.1)),
            };
        }
        let screen = s.list.split_off(hist - drop);
        Grid::from_rows(cols, rows, self.hist_cap, s.list, screen).with_pushed(self.first_line() + hist as u64)
    }

    /// ghostty's clearPromptForRedraw for a cursor on line `line`: clears
    /// the cursor's row, or with `all` every row from the start of its
    /// prompt to the bottom of the screen. The rows keep their marks.
    pub fn clear_prompt(&mut self, line: usize, all: bool) {
        let end = if all { self.hist.len() + self.rows } else { line + 1 };
        let Some(start) = (if all { self.prompt_start(line) } else { Some(line) }) else { return };
        for l in start..end {
            let slot = self.line_slot(l);
            let (wrapped, prompt) = (self.meta[slot].wrapped, self.meta[slot].prompt);
            self.clear_slot(slot, Cell::EMPTY);
            (self.meta[slot].wrapped, self.meta[slot].prompt) = (wrapped, prompt);
        }
    }

    /// ghostty's promptIterator going up from line `line`: the nearest
    /// prompt row, or the top of the nearest continuation rows. Continuation
    /// rows that reach the oldest line give the lowest one, as in ghostty.
    fn prompt_start(&self, line: usize) -> Option<usize> {
        let mark = |l: usize| self.meta[self.line_slot(l)].prompt;
        let mut l = line;
        loop {
            match mark(l) {
                PromptMark::Prompt => return Some(l),
                PromptMark::Continuation => break,
                PromptMark::None => l = l.checked_sub(1)?,
            }
        }
        let mut top = l;
        while top > 0 {
            match mark(top - 1) {
                PromptMark::None => return Some(top),
                PromptMark::Prompt => return Some(top - 1),
                PromptMark::Continuation => top -= 1,
            }
        }
        Some(l)
    }
}

/// ghostty's Cell.isEmpty: no text, no background, not part of a wide character.
fn is_empty(c: Cell) -> bool {
    c.codepoint() == 0 && c.width() == Width::Narrow && c.style() == StyleId::DEFAULT
}

struct NewRow {
    cells: Vec<Cell>,
    meta: RowMeta,
    /// ghostty's wrap_continuation: the row continues a wrapped row above.
    cont: bool,
}

impl NewRow {
    fn blank(cols: usize) -> NewRow {
        NewRow { cells: vec![Cell::EMPTY; cols], meta: RowMeta::default(), cont: false }
    }

    fn has_text(&self) -> bool {
        self.cells[..self.meta.occ as usize].iter().any(|c| c.codepoint() != 0)
    }
}

/// The line of a pin whose row a resize cut below the screen.
const CUT: usize = usize::MAX;

/// A screen being resized as ghostty's PageList: every stored row, oldest
/// first, of which the last `rows` are the screen.
struct Resize {
    cols: usize,
    rows: usize,
    list: Vec<NewRow>,
}

impl Resize {
    fn new(g: &Grid) -> Resize {
        let total = g.hist.len() + g.rows;
        let mut list: Vec<NewRow> = Vec::with_capacity(total);
        for line in 0..total {
            let slot = g.line_slot(line);
            let (cells, m) = (g.slot_cells(slot), &g.meta[slot]);
            let mut meta = RowMeta { occ: m.occ, wrapped: m.wrapped, prompt: m.prompt, ..RowMeta::default() };
            for (x, s) in m.graphemes().filter(|g| cells[g.0].has_grapheme()) {
                meta.push_grapheme(x, s);
            }
            let cont = list.last().is_some_and(|r| r.meta.wrapped);
            list.push(NewRow { cells: cells.to_vec(), meta, cont });
        }
        Resize { cols: g.cols, rows: g.rows, list }
    }

    /// The first screen row.
    fn top(&self) -> usize {
        self.list.len() - self.rows
    }

    fn push_blank(&mut self, n: usize) {
        let cols = self.cols;
        self.list.extend((0..n).map(|_| NewRow::blank(cols)));
    }

    /// ghostty's resizeWithoutReflow for the rows, but fewer rows keep the
    /// cursor's row: trailing rows without text below it are dropped, then
    /// top rows move to the history down to the cursor's row, then rows
    /// below it are cut. More rows come from the history with `pull`, else
    /// are blank.
    fn set_rows(&mut self, rows: usize, pull: bool, pins: &mut [(usize, usize)]) {
        if rows < self.rows {
            let (top, cursor) = (self.top(), pins[0].0);
            let below = self.list[cursor + 1..].iter().rev().take(self.rows - rows);
            let screen = self.list.len() - below.take_while(|r| !r.has_text()).count() - top;
            let up = (screen - rows).min(cursor - top);
            self.cut(top + up + rows, pins);
        } else if pull {
            self.push_blank(rows.saturating_sub(self.list.len()));
        } else {
            self.push_blank(rows - self.rows);
        }
        self.rows = rows;
    }

    /// Drops the rows from `len` on; their pins go to `CUT`.
    fn cut(&mut self, len: usize, pins: &mut [(usize, usize)]) {
        self.list.truncate(len);
        for p in pins.iter_mut().filter(|p| p.0 >= len) {
            p.0 = CUT;
        }
    }

    /// ghostty's resizeWithoutReflow for the columns. Cut cells are cleared
    /// along with a wide character cut in half, and pins move left into
    /// the row. Rows copied into wider ones lose their wrap marks, and a
    /// spacer head at the old last column becomes a blank.
    fn set_cols(&mut self, cols: usize, pins: &mut [(usize, usize)]) {
        let old = self.cols;
        if cols < old {
            for r in &mut self.list {
                let start = if r.cells[cols - 1].width() == Width::Wide { cols - 1 } else { cols };
                r.cells.truncate(start);
                r.cells.resize(cols, Cell::EMPTY);
                r.meta.occ = r.meta.occ.min(start as u16);
                r.meta.free_graphemes_from(start);
            }
            for p in pins {
                p.1 = p.1.min(cols - 1);
            }
        } else if cols > old {
            for r in &mut self.list {
                if r.cells[old - 1].width() == Width::SpacerHead {
                    r.cells[old - 1] = blankify(r.cells[old - 1]);
                }
                r.cells.resize(cols, Cell::EMPTY);
                r.meta.wrapped = false;
                r.cont = false;
            }
        }
        self.cols = cols;
    }

    /// ghostty's resizeCols: rewraps every row at `cols` columns. Blank rows
    /// are added below so that the cursor, `cursor_y` rows from the top of
    /// the screen, keeps the rows it had under it, less the rows its line
    /// gained from wrapping. Unlike ghostty, rows that would push the
    /// cursor's row into the history are cut below it instead.
    fn reflow(&mut self, cols: usize, cursor_y: usize, pins: &mut [(usize, usize)]) {
        let remaining = self.rows.saturating_sub(cursor_y + 1);
        let wrapped = self.continued_rows(pins[0].0);
        let mut w = Reflow { cols, out: vec![NewRow::blank(cols)], x: 0, pending_wrap: false, new_rows: 0 };
        let mut moved = vec![false; pins.len()];
        for (line, row) in std::mem::take(&mut self.list).into_iter().enumerate() {
            w.row(&row, line, pins, &mut moved);
        }
        self.cols = cols;
        self.list = w.out;
        self.push_blank(self.rows.saturating_sub(self.list.len()));
        let (top, line) = (self.top(), pins[0].0);
        if line < top {
            self.cut(line + self.rows, pins);
        } else {
            let below = self.rows - (line - top + 1);
            let gained = self.continued_rows(line).saturating_sub(wrapped);
            self.push_blank(remaining.saturating_sub(gained).saturating_sub(below));
        }
    }

    /// The rows from the top of the screen down to `line` that continue a
    /// wrapped row.
    fn continued_rows(&self, line: usize) -> usize {
        let top = self.top();
        if line < top {
            return 0;
        }
        self.list[top..=line].iter().filter(|r| r.cont).count()
    }
}

/// ghostty's PageList.ReflowCursor: where the next reflowed cell goes.
struct Reflow {
    cols: usize,
    out: Vec<NewRow>,
    x: usize,
    pending_wrap: bool,
    /// Line breaks not written yet: blank rows are written only before text.
    new_rows: usize,
}

impl Reflow {
    fn last(&mut self) -> &mut NewRow {
        self.out.last_mut().unwrap()
    }

    fn scroll(&mut self) {
        self.out.push(NewRow::blank(self.cols));
        self.x = 0;
        self.pending_wrap = false;
    }

    /// ghostty's reflowRow of line `line`. The pins not `moved` yet that are
    /// on the row follow their cells; those in its trailing blanks, except
    /// the cursor's, first move left to fit the destination row. Unlike
    /// ghostty, a row takes the prompt mark of the source row it starts at,
    /// and a row that starts inside a marked source row continues it.
    fn row(&mut self, src: &NewRow, line: usize, pins: &mut [(usize, usize)], moved: &mut [bool]) {
        let mut len = src.cells.len();
        if !src.meta.wrapped {
            len = src.cells[..src.meta.occ as usize].iter().rposition(|&c| !is_empty(c)).map_or(0, |x| x + 1);
            if len == 0 && src.meta.prompt != PromptMark::None {
                len = 1;
            }
        }
        let dst_x = if self.new_rows > 0 || self.pending_wrap { 0 } else { self.x };
        let mut has_pins = false;
        for (i, p) in pins.iter_mut().enumerate().filter(|(i, p)| !moved[*i] && p.0 == line) {
            has_pins = true;
            if i > 0 {
                if p.1 >= len {
                    p.1 = p.1.min(self.cols - 1 - dst_x);
                }
                len = len.max(p.1 + 1);
            }
        }
        if !moved[0] && pins[0].0 == line {
            len = len.max(pins[0].1 + 1);
        }
        if len == 0 {
            if !src.cont {
                self.new_rows += 1;
            }
            return;
        }
        for _ in 0..std::mem::take(&mut self.new_rows) {
            self.scroll();
        }
        let mut x = 0;
        while x < len {
            if self.pending_wrap {
                self.last().meta.wrapped = true;
                self.scroll();
                self.last().cont = true;
            }
            // A wide character goes to the next row after a spacer head, and
            // its pins with it (ghostty leaves them on the spacer head).
            if src.cells[x].width() == Width::Wide && self.cols > 1 && self.x == self.cols - 1 {
                self.put(Cell::new(0, Width::SpacerHead, StyleId::DEFAULT), None);
                continue;
            }
            if has_pins {
                self.move_pins(line, x, pins, moved);
            }
            let mark = match src.meta.prompt {
                _ if x == 0 => src.meta.prompt,
                PromptMark::None => PromptMark::None,
                _ => PromptMark::Continuation,
            };
            if self.write(src, x, mark) {
                x += 1;
            } else {
                if has_pins {
                    self.move_pins(line, x + 1, pins, moved);
                }
                x += 2;
            }
        }
        if !src.meta.wrapped {
            self.new_rows += 1;
        }
    }

    fn move_pins(&self, line: usize, x: usize, pins: &mut [(usize, usize)], moved: &mut [bool]) {
        let to = (self.out.len() - 1, self.x);
        for (p, m) in pins.iter_mut().zip(moved) {
            if !*m && *p == (line, x) {
                (*p, *m) = (to, true);
            }
        }
    }

    /// ghostty's writeCell for column `x` of `src`, but for the spacer head
    /// before a wide character; `mark` is the prompt mark of a row the cell
    /// starts. False if the next column is skipped: one column cannot hold
    /// a wide character, which becomes a blank without its tail.
    fn write(&mut self, src: &NewRow, x: usize, mark: PromptMark) -> bool {
        let c = src.cells[x];
        let fits = match c.width() {
            Width::SpacerHead => return true,
            Width::SpacerTail if self.cols == 1 => return true,
            Width::Wide => self.cols > 1,
            _ => true,
        };
        if self.x == 0 {
            self.last().meta.prompt = mark;
        }
        if fits {
            let grapheme = if c.has_grapheme() { src.meta.grapheme(x as u16) } else { None };
            self.put(c, grapheme);
        } else {
            self.put(Cell::EMPTY, None);
        }
        fits
    }

    fn put(&mut self, c: Cell, grapheme: Option<&str>) {
        let (x, cols) = (self.x, self.cols);
        let r = self.last();
        r.cells[x] = c;
        r.meta.touch(x + 1);
        if let Some(g) = grapheme {
            r.meta.push_grapheme(x, g);
        }
        if x == cols - 1 {
            self.pending_wrap = true;
        } else {
            self.x += 1;
        }
    }
}
