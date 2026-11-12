//! Selections on a terminal's absolute lines, after ghostty's
//! SelectionGesture (drag rules), Screen.selectWord and selectLine, and the
//! plain and HTML formatters it copies with.

use std::fmt::Write;

use term::{
    Attrs, Cell, Color, LinkId, Palette, Point, PromptMark, Rgb, Selection, Semantic, Style, StyleId, Terminal,
    Underline, Width,
};

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
/// `p`.
pub fn word(term: &Terminal, p: Point) -> Option<Selection> {
    let boundary = WORD_BOUNDARIES.contains(&word_char(term, p)?);
    Some(run(term, p, |q| word_char(term, q).is_some_and(|c| WORD_BOUNDARIES.contains(&c) == boundary)))
}

/// The cells around `p` for which `same` holds, continuing onto the next
/// line only from a soft-wrapped one.
pub fn run(term: &Terminal, p: Point, same: impl Fn(Point) -> bool) -> Selection {
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
    Selection { start, end, rect: false }
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
pub fn prompt_above(term: &Terminal, line: u64) -> Option<u64> {
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
pub fn prompt_below(term: &Terminal, line: u64) -> Option<(u64, u64)> {
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

/// Screen.selectAll: the scrollback and the screen from the first to the
/// last cell with text other than whitespace.
pub fn all(term: &Terminal) -> Option<Selection> {
    let cols = term.cols();
    let lines = term.first_line()..term.end_line();
    let mut cells = lines.flat_map(|line| (0..cols).map(move |col| Point { line, col }));
    let text = |p: &Point| {
        term.line(p.line).and_then(|row| char_at(row[p.col as usize])).is_some_and(|c| !LINE_WHITESPACE.contains(&c))
    };
    let start = cells.find(text)?;
    // Only the cells after `start` are left, and none of them may hold text.
    let end = cells.rfind(text).unwrap_or(start);
    Some(Selection { start, end, rect: false })
}

/// What comes before a cell that is written: line breaks, then blank cells.
#[derive(Default)]
struct Gap {
    lines: usize,
    blanks: usize,
}

/// The selected cells as ghostty's formatter goes through them with
/// unwrap: `visit` gets each cell that is not `blank` (a wide character
/// once) with the gap before it. Every cell of a row without text is blank;
/// soft-wrapped lines join (not in a rectangle), and the blank cells at the
/// end of a line and the blank lines at the end are left out.
fn walk(term: &Terminal, sel: &Selection, blank: impl Fn(Cell) -> bool, mut visit: impl FnMut(Gap, u64, u16, Cell)) {
    let (first, last) = sel.ordered();
    let cols = term.cols();
    let mut gap = Gap::default();
    for line in first.line..=last.line {
        let Some(row) = term.line(line) else { continue };
        let Some((left, right)) = sel.columns(line, cols) else { continue };
        let cells = &row[left as usize..=right as usize];
        let has_text = cells.iter().any(|c| c.codepoint() != 0);
        for (x, &cell) in (left..).zip(cells) {
            if matches!(cell.width(), Width::SpacerTail | Width::SpacerHead) {
                continue;
            }
            if !has_text || blank(cell) {
                gap.blanks += 1;
                continue;
            }
            visit(std::mem::take(&mut gap), line, x, cell);
        }
        let joined = !sel.rect && term.line_wrapped(line) && line != last.line;
        if !joined {
            gap.blanks = 0;
            gap.lines += 1;
        }
    }
}

/// The selected text as ghostty copies it (plain, unwrapped, trimmed):
/// soft-wrapped lines join, blank and space cells become spaces only when
/// text follows on the line, and trailing blank lines are dropped.
pub fn text(term: &Terminal, sel: &Selection) -> String {
    let mut out = String::new();
    walk(term, sel, |cell| matches!(char_at(cell), None | Some(' ')), |gap, line, x, cell| {
        out.extend(std::iter::repeat_n('\n', gap.lines));
        out.extend(std::iter::repeat_n(' ', gap.blanks));
        match term.line_grapheme(line, x) {
            Some(cluster) => out.push_str(cluster),
            None => out.extend(char_at(cell)),
        }
    });
    out
}

/// The selected cells as ghostty's HTML formatter writes them for
/// copy_to_clipboard = .mixed: a monospace block that keeps its spaces,
/// without the default colors so that the text takes those of where it is
/// pasted; inline CSS for each run of a style; an anchor for each OSC 8
/// hyperlink; text escaped, all but ASCII as numeric references; and "\n"
/// between lines. Unlike ghostty, the runs are spans, not inline divs that
/// importers which ignore CSS take for blocks; anchors and spans nest, and
/// a URI is escaped by character rather than by byte; inverse swaps the
/// colors as on screen rather than inverting them with a CSS filter; and
/// the blank cells written before text are unstyled and unlinked, as on
/// screen, rather than in the style of the text before.
pub fn html(term: &Terminal, sel: &Selection) -> String {
    let mut html = Html { out: r#"<div style="font-family: monospace; white-space: pre;">"#.into(), ..Html::default() };
    walk(term, sel, |cell| cell.codepoint() == 0 && cell.style() == StyleId::DEFAULT, |gap, line, x, cell| {
        html.cell(term, gap, line, x, cell)
    });
    html.close_link();
    html.out.push_str("</div>");
    html.out
}

#[derive(Default)]
struct Html {
    out: String,
    /// The CSS of the open span; empty when none is open.
    span: String,
    /// The link of the open anchor.
    link: Option<LinkId>,
    /// The CSS of the last style id seen.
    style: (StyleId, String),
}

impl Html {
    fn close_span(&mut self) {
        if !self.span.is_empty() {
            self.out.push_str("</span>");
            self.span.clear();
        }
    }

    /// Closes the anchor and the span in it.
    fn close_link(&mut self) {
        self.close_span();
        if self.link.take().is_some() {
            self.out.push_str("</a>");
        }
    }

    fn cell(&mut self, term: &Terminal, gap: Gap, line: u64, x: u16, cell: Cell) {
        if gap.lines > 0 {
            self.close_span();
            self.out.extend(std::iter::repeat_n('\n', gap.lines));
        }
        if gap.blanks > 0 {
            self.close_link();
            self.out.extend(std::iter::repeat_n(' ', gap.blanks));
        }
        let link = cell.link();
        if link != self.link {
            self.close_link();
            if let Some(uri) = link.and_then(|id| term.link_uri(id)) {
                self.out.push_str("<a href=\"");
                uri.chars().for_each(|c| escape(&mut self.out, c));
                self.out.push_str("\">");
                self.link = link;
            }
        }
        if cell.style() != self.style.0 {
            self.style = (cell.style(), css(term.style(cell.style()), term.palette()));
        }
        if self.span != self.style.1 {
            self.close_span();
            if !self.style.1.is_empty() {
                let _ = write!(self.out, "<span style=\"{}\">", self.style.1);
                self.span.clone_from(&self.style.1);
            }
        }
        match term.line_grapheme(line, x) {
            Some(cluster) => cluster.chars().for_each(|c| escape(&mut self.out, c)),
            // A cell without text that has a style shows it as a space.
            None => escape(&mut self.out, char_at(cell).unwrap_or(' ')),
        }
    }
}

/// ghostty's HTML escaping: the characters HTML gives a meaning, and all
/// but ASCII as numeric references, since the HTML has no charset.
fn escape(out: &mut String, c: char) {
    match c {
        '<' => out.push_str("&lt;"),
        '>' => out.push_str("&gt;"),
        '&' => out.push_str("&amp;"),
        '"' => out.push_str("&quot;"),
        '\'' => out.push_str("&#39;"),
        c if c.is_ascii() => out.push(c),
        c => {
            let _ = write!(out, "&#{};", u32::from(c));
        }
    }
}

/// ghostty's Style.formatterHtml with the palette: the style as inline CSS
/// properties, colors as rgb(). Colors left at the default are not set;
/// inverse swaps the colors, taking the terminal's default for one left at
/// the default.
fn css(style: &Style, palette: &Palette) -> String {
    let resolve = |c: Color| match c {
        Color::Default => None,
        Color::Indexed(i) => Some(palette.indexed[i as usize]),
        Color::Rgb(r, g, b) => Some(Rgb { r, g, b }),
    };
    let attrs = style.attrs;
    let (mut fg, mut bg) = (resolve(style.fg), resolve(style.bg));
    if attrs.contains(Attrs::INVERSE) {
        (fg, bg) = (Some(bg.unwrap_or(palette.background)), Some(fg.unwrap_or(palette.foreground)));
    }
    let mut css = String::new();
    let colors = [("color", fg), ("background-color", bg), ("text-decoration-color", resolve(style.underline_color))];
    for (property, color) in colors {
        if let Some(Rgb { r, g, b }) = color {
            let _ = write!(css, "{property}: rgb({r}, {g}, {b});");
        }
    }
    let lines = [
        (style.underline != Underline::None, " underline"),
        (attrs.contains(Attrs::STRIKE), " line-through"),
        (attrs.contains(Attrs::OVERLINE), " overline"),
        (attrs.contains(Attrs::BLINK), " blink"),
    ];
    if lines.iter().any(|&(on, _)| on) {
        css.push_str("text-decoration-line:");
        css.extend(lines.iter().filter(|&&(on, _)| on).map(|&(_, line)| line));
        css.push(';');
    }
    let underline = match style.underline {
        Underline::None => "",
        Underline::Single => "solid",
        Underline::Double => "double",
        Underline::Curly => "wavy",
        Underline::Dotted => "dotted",
        Underline::Dashed => "dashed",
    };
    if !underline.is_empty() {
        let _ = write!(css, "text-decoration-style: {underline};");
    }
    for (flag, property) in [
        (Attrs::BOLD, "font-weight: bold;"),
        (Attrs::ITALIC, "font-style: italic;"),
        (Attrs::DIM, "opacity: 0.5;"),
        (Attrs::HIDDEN, "visibility: hidden;"),
    ] {
        if attrs.contains(flag) {
            css.push_str(property);
        }
    }
    css
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
    fn all_spans_the_scrollback_without_the_whitespace_around() {
        let mut t = term(10, 3, "\r\n  first\r\n");
        for i in 0..5 {
            t.feed(format!("{i}\r\n").as_bytes());
        }
        t.feed(b"last \t \r\n");
        let s = all(&t).unwrap();
        assert_eq!((s.start, s.end), (Point { line: 1, col: 2 }, Point { line: 7, col: 3 }));
        assert!(t.view_top() > 1);
        assert_eq!(text(&t, &s), "first\n0\n1\n2\n3\n4\nlast");
        assert!(all(&term(10, 3, " \t ")).is_none());
        let t = term(10, 3, "  x");
        let s = all(&t).unwrap();
        assert_eq!((s.start, s.end), (at(&t, 0, 2), at(&t, 0, 2)));
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

    /// The HTML of `sel` inside its monospace block.
    fn html_of(t: &Terminal, sel: Selection) -> String {
        let html = html(t, &sel);
        let inner = html.strip_prefix(r#"<div style="font-family: monospace; white-space: pre;">"#).unwrap();
        inner.strip_suffix("</div>").unwrap().to_owned()
    }

    fn line_of(t: &Terminal, row: u64) -> Selection {
        Selection { start: at(t, row, 0), end: at(t, row, t.cols() - 1), rect: false }
    }

    #[test]
    fn html_escapes_text_and_keeps_written_spaces() {
        // A wide character, an accent combined with 'e', and the characters
        // HTML gives a meaning; the spaces at the end were written.
        let t = term(20, 2, "<a> & \"b\" 'c'  \r\n\u{65e5}e\u{301} x");
        let s = Selection { start: at(&t, 0, 0), end: at(&t, 1, 19), rect: false };
        assert_eq!(html_of(&t, s), "&lt;a&gt; &amp; &quot;b&quot; &#39;c&#39;  \n&#26085;e&#769; x");
        assert_eq!(text(&t, &s), "<a> & \"b\" 'c'\n\u{65e5}e\u{301} x");
    }

    #[test]
    fn html_runs_of_a_style_are_spans_in_the_current_palette() {
        let t = term(
            30,
            3,
            "\x1b[31mred\x1b[0m \x1b[1;44mbold on blue\x1b[0m <&>\r\n\
             \x1b]4;2;rgb:12/34/56\x1b\\\x1b[32;48;2;1;2;3mgreen\x1b[0m\r\n",
        );
        let s = Selection { start: at(&t, 0, 0), end: at(&t, 2, 29), rect: false };
        assert_eq!(
            html_of(&t, s),
            "<span style=\"color: rgb(205, 0, 0);\">red</span> \
             <span style=\"background-color: rgb(0, 0, 238);font-weight: bold;\">bold on blue</span> &lt;&amp;&gt;\n\
             <span style=\"color: rgb(18, 52, 86);background-color: rgb(1, 2, 3);\">green</span>"
        );
        assert_eq!(text(&t, &s), "red bold on blue <&>\ngreen");
    }

    #[test]
    fn html_swaps_inverse_colors_and_writes_attributes_as_ghostty() {
        let t = term(
            20,
            1,
            "\x1b[7mi\x1b[31mr\x1b[0m\x1b[2mf\x1b[22;8mh\x1b[28;3mi\x1b[0m\x1b[4:3;58;5;2mc\x1b[0m\x1b[4;9;53mu\x1b[0m",
        );
        assert_eq!(
            html_of(&t, line_of(&t, 0)),
            "<span style=\"color: rgb(0, 0, 0);background-color: rgb(229, 229, 229);\">i</span>\
             <span style=\"color: rgb(0, 0, 0);background-color: rgb(205, 0, 0);\">r</span>\
             <span style=\"opacity: 0.5;\">f</span>\
             <span style=\"visibility: hidden;\">h</span>\
             <span style=\"font-style: italic;\">i</span>\
             <span style=\"text-decoration-color: rgb(0, 205, 0);text-decoration-line: underline;\
             text-decoration-style: wavy;\">c</span>\
             <span style=\"text-decoration-line: underline line-through overline;\
             text-decoration-style: solid;\">u</span>"
        );
    }

    #[test]
    fn html_links_are_anchors_around_their_spans() {
        let t = term(30, 1, "\x1b]8;;https://e.com/?a=1&b=\u{e9}\x1b\\li\x1b[1mnk\x1b[0m\x1b]8;;\x1b\\ x");
        assert_eq!(
            html_of(&t, line_of(&t, 0)),
            "<a href=\"https://e.com/?a=1&amp;b=&#233;\">li<span style=\"font-weight: bold;\">nk</span></a> x"
        );
    }

    #[test]
    fn html_writes_blank_cells_unstyled_and_rows_without_text_as_blank_lines() {
        // A tab leaves cells unwritten; an erase gives them the background.
        let t = term(12, 4, "\x1b[41mA\x1b[0m\tB\r\n\x1b[44m\x1b[K\x1b[0m\r\n\x1b[44mX\x1b[K\x1b[0m");
        let s = Selection { start: at(&t, 0, 0), end: at(&t, 2, 4), rect: false };
        assert_eq!(
            html_of(&t, s),
            "<span style=\"background-color: rgb(205, 0, 0);\">A</span>       B\n\n\
             <span style=\"background-color: rgb(0, 0, 238);\">X    </span>"
        );
        assert_eq!(text(&t, &s), "A       B\n\nX");
    }

    #[test]
    fn html_joins_soft_wraps_except_in_a_rectangle() {
        // "abcd" wraps into "ef".
        let t = term(4, 3, "ab\x1b[32mcdef\x1b[0m\r\nxyz");
        let green = "<span style=\"color: rgb(0, 205, 0);\">";
        let s = Selection { start: at(&t, 0, 1), end: at(&t, 2, 2), rect: false };
        assert_eq!(html_of(&t, s), format!("b{green}cdef</span>\nxyz"));
        assert_eq!(text(&t, &s), "bcdef\nxyz");
        let s = Selection { start: at(&t, 0, 1), end: at(&t, 2, 2), rect: true };
        assert_eq!(html_of(&t, s), format!("b{green}c</span>\n{green}f</span>\nyz"));
        assert_eq!(text(&t, &s), "bc\nf\nyz");
    }
}
