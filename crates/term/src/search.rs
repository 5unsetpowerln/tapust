//! Literal, case-insensitive search of the active screen's text.

use crate::{Point, Selection, Terminal, Width};

/// Appends `c` lowercased with Unicode's mapping (`char::to_lowercase`).
#[inline]
fn push_folded(out: &mut String, c: char) {
    if c.is_ascii() {
        out.push(c.to_ascii_lowercase());
    } else {
        out.extend(c.to_lowercase());
    }
}

impl Terminal {
    /// Every occurrence of `query` in the active screen's stored lines (its
    /// scrollback and screen), top to bottom. Both sides are lowercased with
    /// `char::to_lowercase` and compared literally; rows soft-wrapped into
    /// the next one are searched as one line, and the search resumes after
    /// each match, so occurrences never overlap. Blank cells read as spaces,
    /// except those that end a line. A match covers whole cells: a wide
    /// character's spacer and the cell of a grapheme cluster. An empty query
    /// matches nothing.
    pub fn search(&self, query: &str) -> Vec<Selection> {
        let mut needle = String::new();
        for c in query.chars() {
            push_folded(&mut needle, c);
        }
        let mut found = Vec::new();
        if needle.is_empty() {
            return found;
        }
        let cols = usize::from(self.cols());
        // A logical line's folded text and, for each of its bytes, the cell it
        // came from as row * cols + column from the line's first row.
        let (mut text, mut cells) = (String::new(), Vec::<u32>::new());
        let (mut line, end) = (self.first_line(), self.end_line());
        while line < end {
            let start = line;
            text.clear();
            cells.clear();
            // The text's length up to its last non-blank cell.
            let mut kept = 0;
            loop {
                let row = self.line(line).expect("a stored line");
                let base = (line - start) as usize * cols;
                for (x, &cell) in row.iter().enumerate() {
                    let cp = cell.codepoint();
                    // Most cells hold one ASCII character, or nothing.
                    if cp < 0x80 && cell.width() == Width::Narrow && !cell.has_grapheme() {
                        text.push(if cp == 0 { ' ' } else { (cp as u8).to_ascii_lowercase() as char });
                        cells.push((base + x) as u32);
                        if cp > 0x20 {
                            kept = text.len();
                        }
                        continue;
                    }
                    if matches!(cell.width(), Width::SpacerTail | Width::SpacerHead) {
                        continue;
                    }
                    match self.line_grapheme(line, x as u16) {
                        Some(cluster) => cluster.chars().for_each(|c| push_folded(&mut text, c)),
                        None => push_folded(&mut text, cell.char().unwrap_or(' ')),
                    }
                    cells.resize(text.len(), (base + x) as u32);
                    kept = text.len();
                }
                line += 1;
                if line == end || !self.line_wrapped(line - 1) {
                    break;
                }
            }
            text.truncate(kept);
            let point =
                |cell: u32| Point { line: start + (cell as usize / cols) as u64, col: (cell as usize % cols) as u16 };
            for (i, m) in text.match_indices(needle.as_str()) {
                let (first, mut last) = (point(cells[i]), point(cells[i + m.len() - 1]));
                if self.line(last.line).is_some_and(|row| row[last.col as usize].width() == Width::Wide) {
                    last.col += 1;
                }
                found.push(Selection { start: first, end: last, rect: false });
            }
        }
        found
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn term(cols: u16, rows: u16, text: &str) -> Terminal {
        let mut t = Terminal::new(cols, rows, 100);
        t.feed(text.as_bytes());
        t
    }

    /// The matches as (line from the top of the screen, column) pairs.
    fn spans(t: &Terminal, query: &str) -> Vec<((u64, u16), (u64, u16))> {
        let top = t.view_top();
        t.search(query)
            .into_iter()
            .map(|s| {
                assert!(!s.rect);
                ((s.start.line - top, s.start.col), (s.end.line - top, s.end.col))
            })
            .collect()
    }

    #[test]
    fn folds_case_with_unicode_lowercase() {
        let t = term(30, 3, "Hello WORLD \u{C4}PFEL \u{3A3}\u{39F}\u{3A3}");
        assert_eq!(spans(&t, "world"), [((0, 6), (0, 10))]);
        assert_eq!(spans(&t, "wOrLd"), [((0, 6), (0, 10))]);
        // A with diaeresis, written in either case.
        assert_eq!(spans(&t, "\u{E4}pfel"), [((0, 12), (0, 16))]);
        assert_eq!(spans(&t, "\u{C4}PFEL"), [((0, 12), (0, 16))]);
        // Capital sigma lowercases to the medial sigma on both sides.
        assert_eq!(spans(&t, "\u{3C3}\u{3BF}\u{3C3}"), [((0, 18), (0, 20))]);
        assert!(spans(&t, "worlds").is_empty());
    }

    #[test]
    fn empty_query_matches_nothing() {
        let t = term(10, 2, "abc");
        assert!(t.search("").is_empty());
    }

    #[test]
    fn matches_across_a_soft_wrap_but_not_a_hard_line_end() {
        let t = term(10, 3, "0123456789abc\r\nxyz");
        assert_eq!(spans(&t, "89AB"), [((0, 8), (1, 1))]);
        // "c" ends its line and "x" starts the next one.
        assert!(spans(&t, "cx").is_empty());
        assert!(spans(&t, "c x").is_empty());
    }

    #[test]
    fn wide_characters_cover_their_spacers() {
        // a, two wide characters (columns 1-2 and 3-4), b.
        let t = term(10, 2, "a\u{65E5}\u{672C}b");
        assert_eq!(spans(&t, "\u{65E5}"), [((0, 1), (0, 2))]);
        assert_eq!(spans(&t, "\u{672C}b"), [((0, 3), (0, 5))]);
        assert_eq!(spans(&t, "a\u{65E5}\u{672C}b"), [((0, 0), (0, 5))]);
        // A wide character that did not fit leaves a spacer head at the end
        // of the row, which the text skips.
        let t = term(4, 3, "abc\u{65E5}d");
        assert_eq!(spans(&t, "c\u{65E5}d"), [((0, 2), (1, 2))]);
    }

    #[test]
    fn grapheme_clusters_map_to_their_cell() {
        let t = term(10, 2, "ae\u{301}b E\u{301}");
        assert_eq!(spans(&t, "e\u{301}b"), [((0, 1), (0, 2))]);
        assert_eq!(spans(&t, "b"), [((0, 2), (0, 2))]);
        // The cluster folds too.
        assert_eq!(spans(&t, "e\u{301}"), [((0, 1), (0, 1)), ((0, 4), (0, 4))]);
    }

    #[test]
    fn occurrences_do_not_overlap() {
        let t = term(10, 3, "aaaa\r\naaa\r\naAaAa");
        assert_eq!(
            spans(&t, "aa"),
            [((0, 0), (0, 1)), ((0, 2), (0, 3)), ((1, 0), (1, 1)), ((2, 0), (2, 1)), ((2, 2), (2, 3))]
        );
    }

    #[test]
    fn blanks_read_as_spaces_except_at_the_end_of_a_line() {
        // The cursor moves over two unwritten cells between "a" and "b".
        let t = term(10, 2, "a\x1b[2Cb");
        assert_eq!(spans(&t, "a  b"), [((0, 0), (0, 3))]);
        assert!(spans(&t, "b ").is_empty());
    }

    #[test]
    fn searches_the_scrollback_top_to_bottom() {
        let mut t = Terminal::new(10, 2, 100);
        for i in 0..5 {
            t.feed(format!("line {i}\r\n").as_bytes());
        }
        let found = t.search("LINE");
        let lines: Vec<u64> = found.iter().map(|s| s.start.line - t.first_line()).collect();
        assert_eq!(lines, [0, 1, 2, 3, 4]);
        assert_eq!(t.history_len(), 4);
    }
}
