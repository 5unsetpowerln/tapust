//! Selections on a terminal's absolute lines, after ghostty's
//! SelectionGesture (drag rules), Screen.selectWord and selectLine, and the
//! plain formatter it copies with.

use term::{Cell, Terminal, Width};

/// A cell: an absolute line and a column.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Point {
    pub line: u64,
    pub col: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Selection {
    pub start: Point,
    pub end: Point,
    pub rect: bool,
}

/// ghostty's default selection-word-chars.
const WORD_BOUNDARIES: [char; 20] =
    ['\0', ' ', '\t', '\'', '"', '\u{2502}', '`', '|', ':', ';', ',', '(', ')', '[', ']', '{', '}', '<', '>', '$'];
/// ghostty's default_line_whitespace.
const LINE_WHITESPACE: [char; 3] = ['\0', ' ', '\t'];

impl Selection {
    /// The first and last cell in reading order (for a rectangle: its
    /// top-left and bottom-right corners).
    pub fn ordered(&self) -> (Point, Point) {
        if self.rect {
            let (top, bottom) = (self.start.line.min(self.end.line), self.start.line.max(self.end.line));
            let (left, right) = (self.start.col.min(self.end.col), self.start.col.max(self.end.col));
            (Point { line: top, col: left }, Point { line: bottom, col: right })
        } else if self.start <= self.end {
            (self.start, self.end)
        } else {
            (self.end, self.start)
        }
    }

    /// The selected columns of `line` as an inclusive range, if any.
    pub fn columns(&self, line: u64, cols: u16) -> Option<(u16, u16)> {
        let (first, last) = self.ordered();
        if line < first.line || line > last.line {
            return None;
        }
        if self.rect {
            return Some((first.col, last.col));
        }
        let left = if line == first.line { first.col } else { 0 };
        let right = if line == last.line { last.col } else { cols - 1 };
        Some((left, right))
    }

    pub fn contains(&self, p: Point, cols: u16) -> bool {
        self.columns(p.line, cols).is_some_and(|(l, r)| (l..=r).contains(&p.col))
    }
}

/// A cell's text as ghostty sees it: the character, or nothing for an
/// unwritten cell.
fn char_at(cell: Cell) -> Option<char> {
    cell.char().filter(|&c| c != '\0')
}

/// selectWordCodepoint: a spacer reads as the wide character it belongs to.
fn word_char(term: &Terminal, p: Point) -> Option<char> {
    let row = term.line(p.line)?;
    let cell = row[p.col as usize];
    match cell.width() {
        Width::SpacerTail if p.col > 0 => char_at(row[p.col as usize - 1]),
        Width::SpacerHead => {
            let next = term.line(p.line + 1)?[0];
            (next.width() == Width::Wide).then(|| char_at(next)).flatten()
        }
        _ => char_at(cell),
    }
}

/// The next cell in reading order across all lines (Pin.rightWrap).
fn right_wrap(term: &Terminal, p: Point) -> Option<Point> {
    if p.col + 1 < term.cols() {
        Some(Point { line: p.line, col: p.col + 1 })
    } else {
        (p.line + 1 < term.end_line()).then_some(Point { line: p.line + 1, col: 0 })
    }
}

/// The previous cell in reading order across all lines (Pin.leftWrap).
fn left_wrap(term: &Terminal, p: Point) -> Option<Point> {
    if p.col > 0 {
        Some(Point { line: p.line, col: p.col - 1 })
    } else {
        (p.line > term.first_line()).then(|| Point { line: p.line - 1, col: term.cols() - 1 })
    }
}

/// Screen.selectWord: the run of boundary or non-boundary characters around
/// `p`, continuing onto the next line only from a soft-wrapped one.
pub fn word(term: &Terminal, p: Point) -> Option<Selection> {
    let boundary = WORD_BOUNDARIES.contains(&word_char(term, p)?);
    let same = |q: Point| word_char(term, q).is_some_and(|c| WORD_BOUNDARIES.contains(&c) == boundary);
    let last_col = term.cols() - 1;
    let mut end = p;
    while let Some(next) = right_wrap(term, end) {
        if end.col == last_col && !term.line_wrapped(end.line) {
            break;
        }
        if !same(next) {
            break;
        }
        end = next;
    }
    let mut start = p;
    while let Some(prev) = left_wrap(term, start) {
        if prev.col == last_col && !term.line_wrapped(prev.line) {
            break;
        }
        if !same(prev) {
            break;
        }
        start = prev;
    }
    Some(Selection { start, end, rect: false })
}

/// Screen.selectWordBetween: the first word found walking from `from`
/// toward `to`.
fn word_between(term: &Terminal, from: Point, to: Point) -> Option<Selection> {
    let mut p = from;
    loop {
        if let Some(sel) = word(term, p) {
            return Some(sel);
        }
        if p == to {
            return None;
        }
        p = if from < to { right_wrap(term, p)? } else { left_wrap(term, p)? };
    }
}

/// Screen.selectLine: the soft-wrapped logical line around `p`, with the
/// whitespace at both ends left out when `trim` (none if it is all
/// whitespace).
pub fn line(term: &Terminal, p: Point, trim: bool) -> Option<Selection> {
    let mut first = p.line;
    while first > term.first_line() && term.line_wrapped(first - 1) {
        first -= 1;
    }
    let mut last = p.line;
    while term.line_wrapped(last) {
        if last + 1 >= term.end_line() {
            return None;
        }
        last += 1;
    }
    let mut start = Point { line: first, col: 0 };
    let mut end = Point { line: last, col: term.cols() - 1 };
    if trim {
        let text = |q: Point| term.line(q.line).and_then(|row| char_at(row[q.col as usize]));
        let solid = |q: Point| text(q).is_some_and(|c| !LINE_WHITESPACE.contains(&c));
        while !solid(start) {
            if start == end {
                return None;
            }
            start = right_wrap(term, start)?;
        }
        while !solid(end) {
            end = left_wrap(term, end)?;
        }
    }
    Some(Selection { start, end, rect: false })
}

/// Where a drag happened: a cell and the pointer's x in device pixels from
/// the grid's left edge.
#[derive(Clone, Copy)]
pub struct DragPoint {
    pub cell: Point,
    pub x: f64,
}

/// SelectionGesture.dragSelection: whether the clicked and the current cell
/// are part of the selection depends on which side of 60% of the cell the
/// pointer is on.
pub fn drag_cells(term: &Terminal, click: DragPoint, drag: DragPoint, rect: bool, cell_width: u32) -> Option<Selection> {
    let cols = u32::from(term.cols());
    if cols == 0 || cell_width == 0 {
        return None;
    }
    let threshold = (f64::from(cell_width) * 0.6).round() as u32;
    let max_x = cols * cell_width - 1;
    let frac = |x: f64| (x.max(0.0) as u32).min(max_x) % cell_width;
    let (click_frac, drag_frac) = (frac(click.x), frac(drag.x));
    let (c, d) = (click.cell, drag.cell);
    let same = c == d;
    let end_before_start = if same {
        drag_frac < click_frac
    } else if rect {
        match d.col.cmp(&c.col) {
            std::cmp::Ordering::Equal => drag_frac < click_frac,
            std::cmp::Ordering::Less => true,
            std::cmp::Ordering::Greater => false,
        }
    } else {
        d < c
    };
    let include_click = if end_before_start { click_frac >= threshold } else { click_frac < threshold };
    let include_drag = if end_before_start { drag_frac < threshold } else { drag_frac >= threshold };
    let left_clamp = |p: Point| Point { line: p.line, col: p.col.saturating_sub(1) };
    let right_clamp = |p: Point| Point { line: p.line, col: (p.col + 1).min(term.cols() - 1) };
    let start = if include_click {
        c
    } else if end_before_start {
        if rect { left_clamp(c) } else { left_wrap(term, c).unwrap_or(c) }
    } else if rect {
        right_clamp(c)
    } else {
        right_wrap(term, c).unwrap_or(c)
    };
    let end = if include_drag {
        d
    } else if end_before_start {
        if rect { right_clamp(d) } else { right_wrap(term, d).unwrap_or(d) }
    } else if rect {
        left_clamp(d)
    } else {
        left_wrap(term, d).unwrap_or(d)
    };
    if (!include_click && same)
        || (!include_click && rect && c.col == d.col)
        || (!include_click && end == c)
        || (!include_click && rect && end.col == c.col)
        || (!include_drag && start == d)
        || (!include_drag && rect && start.col == d.col)
    {
        return None;
    }
    Some(Selection { start, end, rect })
}

/// SelectionGesture.dragSelectionWord: whole words from the clicked one to
/// the one under the pointer.
pub fn drag_words(term: &Terminal, click: Point, drag: Point) -> Option<Selection> {
    let at_click = word_between(term, click, drag)?;
    let at_drag = word_between(term, drag, click)?;
    let (start, end) = if drag < click { (at_drag.start, at_click.end) } else { (at_click.start, at_drag.end) };
    Some(Selection { start, end, rect: false })
}

/// SelectionGesture.dragSelectionLine: whole lines from the clicked one to
/// the one under the pointer.
pub fn drag_lines(term: &Terminal, click: Point, drag: Point) -> Option<Selection> {
    let at_drag = line(term, drag, true).or_else(|| line(term, drag, false))?;
    let mut sel = line(term, click, true).or_else(|| line(term, click, false))?;
    if drag < click {
        sel.start = at_drag.start;
    } else {
        sel.end = at_drag.end;
    }
    Some(sel)
}

/// The selected text as ghostty copies it (plain, unwrapped, trimmed):
/// soft-wrapped lines join, blank and space cells become spaces only when
/// text follows on the line, and trailing blank lines are dropped.
pub fn text(term: &Terminal, sel: &Selection) -> String {
    let (first, last) = sel.ordered();
    let cols = term.cols();
    let mut out = String::new();
    let mut blanks = 0;
    let mut newlines = 0;
    for line in first.line..=last.line {
        let Some(row) = term.line(line) else { continue };
        let Some((left, right)) = sel.columns(line, cols) else { continue };
        for x in left..=right {
            let cell = row[x as usize];
            if matches!(cell.width(), Width::SpacerTail | Width::SpacerHead) {
                continue;
            }
            match char_at(cell) {
                None | Some(' ') => blanks += 1,
                Some(c) => {
                    out.extend(std::iter::repeat_n('\n', newlines));
                    out.extend(std::iter::repeat_n(' ', blanks));
                    (newlines, blanks) = (0, 0);
                    match term.line_grapheme(line, x) {
                        Some(cluster) => out.push_str(cluster),
                        None => out.push(c),
                    }
                }
            }
        }
        let joined = !sel.rect && term.line_wrapped(line) && line != last.line;
        if !joined {
            blanks = 0;
            newlines += 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn term(cols: u16, rows: u16, text: &str) -> Terminal {
        let mut t = Terminal::new(cols, rows, 100);
        t.feed(text.as_bytes());
        t
    }

    fn at(t: &Terminal, row: u64, col: u16) -> Point {
        Point { line: t.view_top() + row, col }
    }

    #[test]
    fn words_stop_at_boundaries_and_hard_line_ends() {
        let t = term(10, 3, "foo.bar baz\r\nx(y)");
        let w = word(&t, at(&t, 0, 1)).unwrap();
        assert_eq!(text(&t, &w), "foo.bar");
        // "baz" was soft-wrapped onto the next row: "foo.bar ba" + "z".
        let w = word(&t, at(&t, 1, 0)).unwrap();
        assert_eq!(text(&t, &w), "baz");
        let w = word(&t, at(&t, 2, 2)).unwrap();
        assert_eq!(text(&t, &w), "y");
        assert!(word(&t, at(&t, 2, 8)).is_none());
    }

    #[test]
    fn lines_join_soft_wraps_and_trim_whitespace() {
        let t = term(6, 3, "  abcdefg  \r\n   ");
        let l = line(&t, at(&t, 1, 0), true).unwrap();
        assert_eq!(text(&t, &l), "abcdefg");
        assert!(line(&t, at(&t, 2, 0), true).is_none());
    }

    #[test]
    fn drags_include_cells_past_sixty_percent() {
        let t = term(10, 2, "abcdefghij");
        let p = |col, x| DragPoint { cell: at(&t, 0, col), x };
        // Click in the left half of 'b', drag into 'd' past 60%: b..=d.
        let s = drag_cells(&t, p(1, 12.0), p(3, 37.0), false, 10).unwrap();
        assert_eq!(text(&t, &s), "bcd");
        // The same drag ending before 60% of 'd' stops at 'c'.
        let s = drag_cells(&t, p(1, 12.0), p(3, 33.0), false, 10).unwrap();
        assert_eq!(text(&t, &s), "bc");
        // Clicking past 60% of 'b' leaves it out.
        let s = drag_cells(&t, p(1, 17.0), p(3, 37.0), false, 10).unwrap();
        assert_eq!(text(&t, &s), "cd");
        // Inside one cell, crossing 60% selects it; staying before does not.
        assert!(drag_cells(&t, p(1, 11.0), p(1, 15.0), false, 10).is_none());
        assert!(drag_cells(&t, p(1, 11.0), p(1, 17.0), false, 10).is_some());
    }

    #[test]
    fn text_keeps_inner_blanks_and_drops_trailing_ones() {
        let t = term(10, 3, "a  b   \r\n\r\n");
        let s = Selection { start: at(&t, 0, 0), end: at(&t, 2, 9), rect: false };
        assert_eq!(text(&t, &s), "a  b");
        let t = term(10, 3, "abc\r\ndef");
        let s = Selection { start: at(&t, 0, 1), end: at(&t, 1, 1), rect: true };
        assert_eq!(text(&t, &s), "b\ne");
    }
}
