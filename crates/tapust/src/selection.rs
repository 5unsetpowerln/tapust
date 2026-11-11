//! Selections on a terminal's absolute lines, after ghostty's
//! SelectionGesture (drag rules), Screen.selectWord and selectLine, and the
//! plain formatter it copies with.

use term::{Cell, Point, PromptMark, Selection, Semantic, Terminal, Width};

/// ghostty's default selection-word-chars.
const WORD_BOUNDARIES: [char; 20] =
    ['\0', ' ', '\t', '\'', '"', '\u{2502}', '`', '|', ':', ';', ',', '(', ')', '[', ']', '{', '}', '<', '>', '$'];
/// ghostty's default_line_whitespace.
const LINE_WHITESPACE: [char; 3] = ['\0', ' ', '\t'];

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

/// Screen.selectLine: the soft-wrapped logical line around `p`, cut where
/// the shell's semantic content (prompt, input, output) changes, with the
/// whitespace at both ends left out when `trim` (none if it is all
/// whitespace).
pub fn line(term: &Terminal, p: Point, trim: bool) -> Option<Selection> {
    let cols = term.cols();
    let row = |l: u64| term.line(l);
    let v = row(p.line)?[p.col as usize].semantic();
    let start = 'start: {
        let cells = row(p.line)?;
        if let Some(x) = (0..=p.col).rev().find(|&x| cells[x as usize].semantic() != v) {
            break 'start Point { line: p.line, col: x + 1 };
        }
        let mut prev = Point { line: p.line, col: 0 };
        let mut l = p.line;
        while l > term.first_line() {
            l -= 1;
            if !term.line_wrapped(l) {
                break 'start Point { line: prev.line, col: 0 };
            }
            let cells = row(l)?;
            for x in (0..cols).rev() {
                if cells[x as usize].semantic() != v {
                    break 'start prev;
                }
                prev = Point { line: l, col: x };
            }
        }
        Point { line: prev.line, col: 0 }
    };
    let end = 'end: {
        let mut l = p.line;
        loop {
            let cells = row(l)?;
            let first = if l == p.line { p.col } else { 0 };
            if first == 0 && cells[0].semantic() != v {
                break 'end Point { line: l - 1, col: cols - 1 };
            }
            if let Some(x) = (first..cols).find(|&x| cells[x as usize].semantic() != v) {
                break 'end Point { line: l, col: x - 1 };
            }
            if !term.line_wrapped(l) {
                break 'end Point { line: l, col: cols - 1 };
            }
            l += 1;
            if l >= term.end_line() {
                return None;
            }
        }
    };
    let (mut start, mut end) = (start, end);
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

/// ghostty's Screen.promptClickLine: the arrow presses (left, right) that
/// move a cursor at the shell's input to the input cell `click`, counting
/// only input cells along soft-wrapped and continued prompt rows.
pub fn prompt_click_move(term: &Terminal, cursor: Point, click: Point) -> (usize, usize) {
    let input = |c: Cell| c.semantic() == Semantic::Input;
    let cursor_on_input = term.line(cursor.line).is_some_and(|row| input(row[cursor.col as usize]));
    if term.cursor_semantic() != Semantic::Input && !cursor_on_input || cursor == click {
        return (0, 0);
    }
    let mut count = 0;
    if cursor < click {
        for l in cursor.line..=click.line {
            let Some(row) = term.line(l) else { break };
            let is_cursor_row = l == cursor.line;
            if !is_cursor_row && term.line_prompt(l) != PromptMark::Continuation {
                break;
            }
            let start = if is_cursor_row { cursor.col as usize + 1 } else { row.iter().position(|&c| input(c)).unwrap_or(row.len()) };
            for (x, &cell) in row.iter().enumerate().skip(start) {
                if !input(cell) {
                    continue;
                }
                count += 1;
                if l == click.line && x == click.col as usize {
                    return (0, count);
                }
            }
            if !term.line_wrapped(l) {
                if cursor_on_input {
                    count += 1;
                }
                break;
            }
        }
        return (0, count);
    }
    for l in (click.line..=cursor.line).rev() {
        let Some(row) = term.line(l) else { break };
        let end = if l == cursor.line { cursor.col as usize } else { row.len() };
        for x in (0..end).rev() {
            if !input(row[x]) {
                continue;
            }
            count += 1;
            if l == click.line && x == click.col as usize {
                return (count, 0);
            }
        }
        // Keep going up only into the row this one wraps from.
        if l == term.first_line() || !term.line_wrapped(l - 1) {
            break;
        }
    }
    (count, 0)
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

/// PromptIterator(.left_up): the first line of the prompt at or above
/// `line`, walking a run of continuation lines up to its prompt line.
fn prompt_above(term: &Terminal, line: u64) -> Option<u64> {
    let first = term.first_line();
    let mut l = line;
    loop {
        match term.line_prompt(l) {
            PromptMark::Prompt => return Some(l),
            PromptMark::Continuation => {
                let mut top = l;
                while top > first {
                    match term.line_prompt(top - 1) {
                        PromptMark::None => return Some(top),
                        PromptMark::Continuation => top -= 1,
                        PromptMark::Prompt => return Some(top - 1),
                    }
                }
                // The scrollback was cut inside the prompt.
                return Some(l);
            }
            PromptMark::None if l > first => l -= 1,
            PromptMark::None => return None,
        }
    }
}

/// PromptIterator(.right_down): the first line of the first prompt at or
/// below `line`, and the line after its run of continuation lines.
fn prompt_below(term: &Terminal, line: u64) -> Option<(u64, u64)> {
    let start = (line..term.end_line()).find(|&l| term.line_prompt(l) != PromptMark::None)?;
    let after = (start + 1..term.end_line()).find(|&l| term.line_prompt(l) != PromptMark::Continuation);
    Some((start, after.unwrap_or(term.end_line())))
}

/// Screen.selectOutput: the output of the command whose prompt is at or
/// above `p`, from its first to its last cell with text; nothing unless
/// `p` is on output.
pub fn output(term: &Terminal, p: Point) -> Option<Selection> {
    let cell = |q: Point| term.line(q.line).map(|r| r[q.col as usize]);
    let has_text = |c: Cell| char_at(c).is_some();
    if cell(p)?.semantic() != Semantic::Output {
        return None;
    }
    let last_col = term.cols() - 1;
    let Some(prompt) = prompt_above(term, p.line) else {
        // Without a prompt above, the output runs from the top of the
        // scrollback to the last text before the next prompt.
        let (next, _) = prompt_below(term, p.line)?;
        let start = Point { line: term.first_line(), col: 0 };
        if next <= start.line {
            return None;
        }
        let mut end = Point { line: next - 1, col: last_col };
        while end != start && !has_text(cell(end)?) {
            end = left_wrap(term, end)?;
        }
        return Some(Selection { start, end, rect: false });
    };
    // PageList.highlightSemanticContent(.output) up to the next prompt.
    let (_, after) = prompt_below(term, prompt)?;
    let zone_end = match prompt_below(term, after) {
        Some((next, _)) => Point { line: next - 1, col: last_col },
        None => Point { line: term.end_line() - 1, col: last_col },
    };
    let mut found: Option<(Point, Point)> = None;
    let mut q = Point { line: prompt, col: 0 };
    loop {
        let c = cell(q)?;
        match (c.semantic(), &mut found) {
            (Semantic::Output, None) if has_text(c) => found = Some((q, q)),
            (Semantic::Output, Some(range)) if has_text(c) => range.1 = q,
            (Semantic::Prompt | Semantic::Input, Some(_)) => break,
            _ => {}
        }
        if q == zone_end {
            break;
        }
        q = right_wrap(term, q)?;
    }
    let (start, end) = found?;
    Some(Selection { start, end, rect: false })
}

/// SelectionGesture.dragSelectionOutput: from the output under the click
/// to the output under the pointer, or the clicked output alone.
pub fn drag_output(term: &Terminal, click: Point, drag: Point) -> Option<Selection> {
    let mut sel = output(term, click)?;
    let Some(current) = output(term, drag) else { return Some(sel) };
    if drag < click {
        sel.start = current.start;
    } else {
        sel.end = current.end;
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
    fn lines_stop_at_semantic_boundaries_and_clicks_move_along_input() {
        let t = term(20, 3, "\x1b]133;A\x07$ \x1b]133;B\x07echo hi");
        let l = line(&t, at(&t, 0, 4), true).unwrap();
        assert_eq!(text(&t, &l), "echo hi");
        // The cursor is after "echo hi"; a click on 'e' is 7 cells left.
        let cursor = Point { line: t.view_top(), col: 9 };
        assert_eq!(prompt_click_move(&t, cursor, at(&t, 0, 2)), (7, 0));
        // Past the end of the input there is nothing to the right to reach.
        assert_eq!(prompt_click_move(&t, at(&t, 0, 3), at(&t, 0, 5)), (0, 2));
    }

    #[test]
    fn a_reflow_keeps_a_split_input_one_prompt() {
        // "$ echo abcdefghij" splits into "$ echo abc" and "defghij": 'e' to
        // 'g' is still 11 input cells, as on one row.
        let mut t = term(20, 4, "\x1b]133;A;redraw=0\x07$ \x1b]133;B\x07echo abcdefghij");
        assert_eq!(prompt_click_move(&t, at(&t, 0, 2), at(&t, 0, 13)), (0, 11));
        t.resize(10, 4);
        assert_eq!(prompt_click_move(&t, at(&t, 0, 2), at(&t, 1, 3)), (0, 11));
        let mut t = term(20, 5, "\x1b]133;A\x07$ \x1b]133;B\x07echo abcdefghij\r\n\x1b]133;C\x07out1\r\nout2");
        t.resize(10, 5);
        let s = output(&t, at(&t, 2, 1)).unwrap();
        assert_eq!(text(&t, &s), "out1\nout2");
    }

    /// ghostty's "Screen: selectOutput" test.
    #[test]
    fn output_selects_the_command_output_around_the_click() {
        let t = term(
            10,
            15,
            "output1\r\noutput1\r\n\x1b]133;A\x07prompt2\r\n\x1b]133;B\x07input2\r\n\x1b]133;C\x07\
             output2output2output2output2\r\noutput2\r\n\x1b]133;A\x07$ \x1b]133;B\x07input3\r\n\x1b]133;C\x07\
             output3\r\noutput3\r\noutput3",
        );
        let s = output(&t, at(&t, 1, 1)).unwrap();
        assert_eq!(text(&t, &s), "output1\noutput1");
        let s = output(&t, at(&t, 7, 3)).unwrap();
        assert_eq!(text(&t, &s), "output2output2output2output2\noutput2");
        let s = output(&t, at(&t, 10, 2)).unwrap();
        assert_eq!((s.start, s.end), (at(&t, 9, 0), at(&t, 11, 6)));
        assert!(output(&t, at(&t, 8, 1)).is_none());
        assert!(output(&t, at(&t, 8, 5)).is_none());
        // A drag extends by whole outputs.
        let s = drag_output(&t, at(&t, 10, 2), at(&t, 4, 0)).unwrap();
        assert_eq!((s.start, s.end), (at(&t, 4, 0), at(&t, 11, 6)));
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
