//! A selection on a terminal's absolute lines (see
//! [`Terminal::first_line`](crate::Terminal::first_line)).

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
