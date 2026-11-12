//! Copies terminal rows into pane grids, resolving styles to colors.

use term::{Attrs, Cell, Color, CursorShape, Palette, Point, Rgb, Selection, Style, StyleId, Terminal, Underline, Width};

use crate::config::{Config, TerminalColor};
use crate::frame::{BOLD, CursorStyle, FrameCell, FrameCursor, Grid, ITALIC, STRIKE, WIDE};
use crate::links::Link;
use crate::theme;

fn rgba(c: Rgb) -> [u8; 4] {
    [c.r, c.g, c.b, 0xff]
}

pub fn resolve(color: Color, palette: &Palette, default: Rgb) -> [u8; 4] {
    match color {
        Color::Default => rgba(default),
        Color::Indexed(i) => rgba(palette.indexed[i as usize]),
        Color::Rgb(r, g, b) => [r, g, b, 0xff],
    }
}

/// Halfway from `c` towards `bg`: faint (SGR 2), like most terminals.
fn dim(mut c: [u8; 4], bg: [u8; 4]) -> [u8; 4] {
    for i in 0..3 {
        c[i] = ((c[i] as u16 + bg[i] as u16) / 2) as u8;
    }
    c
}

fn style_colors(style: &Style, palette: &Palette, reverse_video: bool) -> ([u8; 4], [u8; 4]) {
    let mut fg = resolve(style.fg, palette, palette.foreground);
    let mut bg = resolve(style.bg, palette, palette.background);
    if style.attrs.contains(Attrs::INVERSE) != reverse_video {
        std::mem::swap(&mut fg, &mut bg);
    }
    if style.attrs.contains(Attrs::DIM) {
        fg = dim(fg, bg);
    }
    if style.attrs.contains(Attrs::HIDDEN) {
        fg = bg;
    }
    (fg, bg)
}

/// A cell in `style` without its character, which `frame_cell` adds.
fn styled(style: &Style, palette: &Palette, reverse_video: bool) -> FrameCell {
    let (fg, bg) = style_colors(style, palette, reverse_video);
    let attrs = style.attrs;
    // ghostty's underline color stays when the colors are inverted or selected.
    let underline_color = match style.underline_color {
        Color::Default => None,
        c if attrs.contains(Attrs::DIM) => Some(dim(resolve(c, palette, palette.foreground), bg)),
        c => Some(resolve(c, palette, palette.foreground)),
    };
    // Like xterm and ghostty, hidden text has no decorations.
    let underline = if attrs.contains(Attrs::HIDDEN) { Underline::None } else { style.underline };
    let mut flags = 0;
    if attrs.contains(Attrs::BOLD) {
        flags |= BOLD;
    }
    if attrs.contains(Attrs::ITALIC) {
        flags |= ITALIC;
    }
    if attrs.contains(Attrs::STRIKE) {
        flags |= STRIKE;
    }
    FrameCell { ch: 0, fg, bg, underline_color, underline, flags }
}

fn frame_cell(cell: Cell, styled: FrameCell) -> FrameCell {
    match cell.width() {
        Width::Narrow => FrameCell { ch: cell.codepoint(), ..styled },
        Width::Wide => FrameCell { ch: cell.codepoint(), flags: styled.flags | WIDE, ..styled },
        Width::SpacerTail | Width::SpacerHead => styled,
    }
}

/// Gives the cells of `sel` on `line` the colors `colors` picks for them.
fn paint(
    out: &mut [FrameCell],
    sel: &Selection,
    line: u64,
    cols: usize,
    colors: impl Fn(&FrameCell) -> ([u8; 4], [u8; 4]),
) {
    if let Some((left, right)) = sel.columns(line, cols as u16)
        && let Some(cells) = out.get_mut(left as usize..=(right as usize).min(cols - 1))
    {
        for cell in cells {
            (cell.fg, cell.bg) = colors(cell);
        }
    }
}

/// A configured color for a cell drawn as (`fg`, `bg`): its own color,
/// the cell's foreground or background, or `default` when unset.
fn pick(color: Option<TerminalColor>, (fg, bg): ([u8; 4], [u8; 4]), default: [u8; 4]) -> [u8; 4] {
    match color {
        None => default,
        Some(TerminalColor::Rgb(c)) => rgba(c),
        Some(TerminalColor::CellForeground) => fg,
        Some(TerminalColor::CellBackground) => bg,
    }
}

/// ghostty's selection colors for a cell drawn as `cell`: the configured
/// selection-foreground and -background, by default the terminal's default
/// foreground and background swapped.
fn selected(config: &Config, palette: &Palette, cell: ([u8; 4], [u8; 4])) -> ([u8; 4], [u8; 4]) {
    let (fg, bg) = style_colors(&Style::DEFAULT, palette, false);
    (pick(config.selection_foreground, cell, bg), pick(config.selection_background, cell, fg))
}

/// Copies the terminal's dirty viewport rows (all of them when `full`) into
/// `grid` and clears the terminal's dirty marks. Rows and columns the
/// terminal does not have yet (it is resized after the grid) are blank.
/// The search's `matches` (ordered and not overlapping) get ghostty's search
/// colors, the `current` one its selected-match colors, and the cells of
/// `selection` over them the selection colors of `config`.
pub fn copy_pane(
    grid: &mut Grid,
    term: &mut Terminal,
    full: bool,
    matches: &[Selection],
    current: Option<usize>,
    selection: Option<&Selection>,
    config: &Config,
) {
    let (rows, cols) = (term.rows().min(grid.rows()), term.cols().min(grid.cols()) as usize);
    let palette = term.palette();
    let reverse_video = term.modes().reverse_video;
    // Blank cells draw nothing in the foreground color, which a selection
    // of the cells' foreground still needs.
    let (blank_fg, blank_bg) = style_colors(&Style::DEFAULT, palette, reverse_video);
    let blank = FrameCell { fg: blank_fg, ..FrameCell::blank(blank_bg) };
    let colors = |(fg, bg): (Rgb, Rgb)| (rgba(fg), rgba(bg));
    // Cells in a row mostly share their style: resolve it once per run.
    let mut run = (StyleId::DEFAULT, styled(&Style::DEFAULT, palette, reverse_video));
    for y in 0..grid.rows() {
        if !full && (y >= rows || !term.dirty().is_dirty(y)) {
            continue;
        }
        let out = grid.row_mut(y);
        if y >= rows {
            out.fill(blank);
            continue;
        }
        for (dst, &cell) in out.iter_mut().zip(&term.row(y)[..cols]) {
            if cell == Cell::EMPTY && !reverse_video {
                *dst = blank;
                continue;
            }
            if cell.style() != run.0 {
                run = (cell.style(), styled(term.style(cell.style()), palette, reverse_video));
            }
            *dst = frame_cell(cell, run.1);
        }
        out[cols..].fill(blank);
        let line = term.view_top() + u64::from(y);
        // Ordered matches that do not overlap end in order too.
        let first = matches.partition_point(|m| m.end.line < line);
        for (i, m) in matches.iter().enumerate().skip(first).take_while(|(_, m)| m.start.line <= line) {
            let pair = colors(if Some(i) == current { theme::SEARCH_CURRENT } else { theme::SEARCH_MATCH });
            paint(out, m, line, cols, |_| pair);
        }
        if let Some(sel) = selection {
            paint(out, sel, line, cols, |cell| selected(config, palette, (cell.fg, cell.bg)));
        }
    }
    term.clear_dirty();
}

/// Underlines the visible cells of a hovered link, as ghostty does: every
/// cell of a hyperlink, or the cells of a match; with a single underline, or
/// a double one where the text has a single underline. The kind comes from
/// the terminal, so underlining again changes nothing.
pub fn underline(grid: &mut Grid, term: &Terminal, link: &Link) {
    // The terminal reuses a hyperlink's id once no cell has it.
    if link.id.is_some_and(|id| term.link_uri(id) != Some(link.target.as_str())) {
        return;
    }
    let top = term.view_top();
    let (rows, cols) = (term.rows().min(grid.rows()), term.cols().min(grid.cols()));
    for y in 0..rows {
        let span = link.cells.columns(top + u64::from(y), cols);
        for (x, (dst, cell)) in grid.row_mut(y).iter_mut().zip(term.row(y)).take(cols as usize).enumerate() {
            let hovered = match link.id {
                Some(id) => cell.link() == Some(id),
                None => span.is_some_and(|(left, right)| (left as usize..=right as usize).contains(&x)),
            };
            let style = term.style(cell.style());
            if hovered && !style.attrs.contains(Attrs::HIDDEN) {
                let single = style.underline == Underline::Single;
                dst.underline = if single { Underline::Double } else { Underline::Single };
            }
        }
    }
}

/// ghostty's preedit cells: the input method's text from the cursor on,
/// in the default foreground on the default background, underlined, shifted
/// left (and cut from the start) to stay on the row.
pub fn preedit(grid: &mut Grid, term: &Terminal, text: &str) {
    let cursor = term.cursor();
    let cols = grid.cols();
    if cursor.y >= grid.rows() || cursor.x >= cols {
        return;
    }
    let palette = term.palette();
    let (fg, bg) = style_colors(&Style::DEFAULT, palette, term.modes().reverse_video);
    let chars: Vec<(char, usize)> = text.chars().map(|c| (c, term::char_width(c))).filter(|&(_, w)| w > 0).collect();
    // renderer.State.Preedit.range: measure from the end; past the row's
    // width, start at the character that overflows and shift left.
    let (start, max) = (usize::from(cursor.x), usize::from(cols) - 1);
    let max_width = max - start + 1;
    let (mut width, mut offset) = (0, 0);
    for (i, &(_, w)) in chars.iter().enumerate().rev() {
        width += w;
        if width > max_width {
            offset = i;
            break;
        }
    }
    let end = if width > 0 { start + width - 1 } else { start };
    let mut x = start.saturating_sub(end.saturating_sub(max));
    let row = grid.row_mut(cursor.y);
    let blank = FrameCell { fg, underline: Underline::Single, ..FrameCell::blank(bg) };
    for &(c, w) in &chars[offset..] {
        if x + w > row.len() {
            break;
        }
        row[x] = FrameCell { ch: c as u32, flags: if w == 2 { WIDE } else { 0 }, ..blank };
        if w == 2 {
            row[x + 1] = blank;
        }
        x += w;
    }
}

/// The cursor of the pane drawn in `frame.grids[grid]`, if visible. A pane
/// that does not have the keyboard focus gets ghostty's hollow block.
/// ghostty's lock cursor at a password prompt: drawn even when the cursor
/// is hidden, unfocused or blinked off.
pub fn lock_cursor(term: &Terminal, grid: usize, config: &Config) -> Option<FrameCursor> {
    let cursor = term.cursor();
    let on_screen = cursor.x < term.cols() && cursor.y < term.rows();
    on_screen.then(|| cursor_at(term, grid, cursor.x, cursor.y, CursorStyle::Lock, config))
}

pub fn pane_cursor(term: &Terminal, grid: usize, focused: bool, config: &Config) -> Option<FrameCursor> {
    let cursor = term.cursor();
    if !cursor.visible || cursor.x >= term.cols() || cursor.y >= term.rows() {
        return None;
    }
    let style = match (focused, cursor.shape) {
        (false, _) | (true, CursorShape::BlockHollow) => CursorStyle::Hollow,
        (true, CursorShape::Block) => CursorStyle::Block,
        (true, CursorShape::Underline) => CursorStyle::Underline,
        (true, CursorShape::Bar) => CursorStyle::Bar,
    };
    Some(cursor_at(term, grid, cursor.x, cursor.y, style, config))
}

/// tmux's copy-mode cursor at (`x`, `y`) of the view, drawn as the pane's
/// block cursor (hollow without the keyboard focus). On a selected cell it
/// takes the selection's colors inverted, where ghostty's cursor, in the
/// foreground color, would not show on the selection it moves.
pub fn copy_cursor(
    term: &Terminal,
    grid: usize,
    x: u16,
    y: u16,
    focused: bool,
    config: &Config,
) -> Option<FrameCursor> {
    if x >= term.cols() || y >= term.rows() {
        return None;
    }
    let style = if focused { CursorStyle::Block } else { CursorStyle::Hollow };
    let mut cursor = cursor_at(term, grid, x, y, style, config);
    let cell = Point { line: term.view_top() + u64::from(y), col: x };
    let colored = term.palette().cursor.is_some() || config.cursor_color.is_some();
    if !colored && term.selection().is_some_and(|s| s.contains(cell, term.cols())) {
        let palette = term.palette();
        let shown = style_colors(term.style(term.row(y)[x as usize].style()), palette, term.modes().reverse_video);
        (cursor.color, cursor.text_color) = selected(config, palette, shown);
    }
    Some(cursor)
}

/// ghostty's cursor-color and cursor-text for the cell under the cursor;
/// unset, the cell's foreground and background.
fn cursor_at(term: &Terminal, grid: usize, x: u16, y: u16, style: CursorStyle, config: &Config) -> FrameCursor {
    let palette = term.palette();
    let cell = term.row(y)[x as usize];
    let (fg, bg) = style_colors(term.style(cell.style()), palette, term.modes().reverse_video);
    FrameCursor {
        grid,
        x,
        y,
        style,
        color: palette.cursor.map(rgba).unwrap_or_else(|| pick(config.cursor_color, (fg, bg), fg)),
        text_color: pick(config.cursor_text, (fg, bg), bg),
        wide: cell.width() == Width::Wide,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_matches_are_highlighted_under_the_selection() {
        let mut t = Terminal::new(12, 2, 10);
        t.feed(b"abc abc abcabc");
        let matches = t.search("abc");
        assert_eq!(matches.len(), 4);
        let top = t.view_top();
        let selection = Selection { start: Point { line: top, col: 5 }, end: Point { line: top, col: 5 }, rect: false };
        let new_grid = || Grid::new(0, (0, 0, 120, 40), (0, 0), (12, 2), [0; 4]);
        let (mut plain, mut grid) = (new_grid(), new_grid());
        copy_pane(&mut plain, &mut t, true, &[], None, None, &Config::default());
        copy_pane(&mut grid, &mut t, true, &matches, Some(3), Some(&selection), &Config::default());
        let colors = |grid: &Grid, y: u16, x: usize| (grid.row(y)[x].fg, grid.row(y)[x].bg);
        let pair = |(fg, bg): (Rgb, Rgb)| (rgba(fg), rgba(bg));
        let (found, current) = (pair(theme::SEARCH_MATCH), pair(theme::SEARCH_CURRENT));
        let palette = t.palette();
        assert_eq!(colors(&grid, 0, 0), found);
        assert_eq!(colors(&grid, 0, 3), colors(&plain, 0, 3));
        assert_eq!(colors(&grid, 0, 5), (rgba(palette.background), rgba(palette.foreground)));
        assert_eq!(colors(&grid, 0, 6), found);
        // The last match wrapped: columns 11 and 0-1.
        assert_eq!(colors(&grid, 0, 11), current);
        assert_eq!(colors(&grid, 1, 1), current);
        assert_eq!(colors(&grid, 1, 2), colors(&plain, 1, 2));
    }

    fn new_grid() -> Grid {
        Grid::new(0, (0, 0, 120, 40), (0, 0), (12, 2), [0; 4])
    }

    #[test]
    fn the_copy_cursor_shows_on_the_selection() {
        let mut t = Terminal::new(12, 2, 10);
        t.feed(b"abc");
        let palette = t.palette().clone();
        let (fg, bg) = (rgba(palette.foreground), rgba(palette.background));
        let cursor = copy_cursor(&t, 0, 1, 0, true, &Config::default()).unwrap();
        assert_eq!((cursor.style, cursor.color, cursor.text_color), (CursorStyle::Block, fg, bg));
        let top = t.view_top();
        let (start, end) = (Point { line: top, col: 0 }, Point { line: top, col: 2 });
        t.set_selection(Some(Selection { start, end, rect: false }));
        let cursor = copy_cursor(&t, 0, 1, 0, true, &Config::default()).unwrap();
        assert_eq!((cursor.color, cursor.text_color), (bg, fg));
        assert_eq!(copy_cursor(&t, 0, 1, 0, false, &Config::default()).unwrap().style, CursorStyle::Hollow);
        assert!(copy_cursor(&t, 0, 12, 0, true, &Config::default()).is_none());
    }

    /// ghostty's selection-foreground and -background, and cursor-color and
    /// cursor-text: a color, or the cell's own foreground or background.
    #[test]
    fn configured_selection_and_cursor_colors() {
        let mut t = Terminal::new(12, 2, 10);
        // A red on blue cell, then an inverse one.
        t.feed(b"\x1b[31;44ma\x1b[7mb\x1b[0m");
        let top = t.view_top();
        let selection = Selection { start: Point { line: top, col: 0 }, end: Point { line: top, col: 2 }, rect: false };
        let config = Config {
            selection_foreground: Some(TerminalColor::CellBackground),
            selection_background: Some(TerminalColor::Rgb(Rgb { r: 1, g: 2, b: 3 })),
            cursor_color: Some(TerminalColor::CellBackground),
            cursor_text: Some(TerminalColor::CellForeground),
            ..Config::default()
        };
        let mut grid = new_grid();
        copy_pane(&mut grid, &mut t, true, &[], None, Some(&selection), &config);
        let p = t.palette().clone();
        let (red, blue) = (rgba(p.indexed[1]), rgba(p.indexed[4]));
        let row = grid.row(0);
        assert_eq!((row[0].fg, row[0].bg), (blue, [1, 2, 3, 0xff]));
        // The inverse cell shows blue on red, and the blank one the defaults.
        assert_eq!(row[1].fg, red);
        assert_eq!(row[2].fg, rgba(p.background));
        t.feed(b"\x1b[H");
        let cursor = pane_cursor(&t, 0, true, &config).unwrap();
        assert_eq!((cursor.color, cursor.text_color), (blue, red));
        // An OSC 12 color outranks the configured one.
        t.feed(b"\x1b]12;#fff\x07");
        assert_eq!(pane_cursor(&t, 0, true, &config).unwrap().color, [0xf0, 0xf0, 0xf0, 0xff]);
        let hollow = Config { cursor_style: CursorShape::BlockHollow, ..Config::default() };
        t.set_default_cursor(CursorShape::BlockHollow, None);
        assert_eq!(pane_cursor(&t, 0, true, &hollow).unwrap().style, CursorStyle::Hollow);
    }

    #[test]
    fn cells_carry_the_underline_kind_and_color() {
        let mut t = Terminal::new(12, 2, 10);
        // Curly in palette color 1, dotted in the foreground, faint and
        // hidden ones, and a wide character with a double underline.
        t.feed(b"\x1b[4:3;58;5;1ma\x1b[0m\x1b[4:4mb\x1b[0m\x1b[2;4;58:2::200:100:0mc\x1b[0m\x1b[8;4;58;5;2md");
        t.feed("\x1b[0m\x1b[21;7;58;5;3m\u{4e00}".as_bytes());
        let mut grid = new_grid();
        copy_pane(&mut grid, &mut t, true, &[], None, None, &Config::default());
        let palette = t.palette();
        let (fg, bg) = (rgba(palette.foreground), rgba(palette.background));
        let row = grid.row(0);
        let kinds = |cells: &[FrameCell]| cells.iter().map(|c| (c.underline, c.underline_color)).collect::<Vec<_>>();
        assert_eq!(
            kinds(&row[..6]),
            [
                (Underline::Curly, Some(rgba(palette.indexed[1]))),
                (Underline::Dotted, None),
                (Underline::Single, Some(dim([200, 100, 0, 0xff], bg))),
                (Underline::None, Some(rgba(palette.indexed[2]))),
                (Underline::Double, Some(rgba(palette.indexed[3]))),
                (Underline::Double, Some(rgba(palette.indexed[3]))),
            ]
        );
        // Inverse swaps the text colors but keeps the underline color.
        assert_eq!((row[4].fg, row[4].bg, row[4].flags & WIDE), (bg, fg, WIDE));
    }

    #[test]
    fn hovered_links_get_a_single_or_a_double_underline() {
        let mut t = Terminal::new(12, 2, 10);
        t.feed(b"ab\x1b[4mcd\x1b[4:3mef\x1b[8mg");
        let top = t.view_top();
        let cells = Selection { start: Point { line: top, col: 0 }, end: Point { line: top, col: 6 }, rect: false };
        let link = Link { cells, id: None, target: String::new() };
        let mut grid = new_grid();
        copy_pane(&mut grid, &mut t, true, &[], None, None, &Config::default());
        let kinds = |grid: &Grid| grid.row(0)[..8].iter().map(|c| c.underline).collect::<Vec<_>>();
        let (none, single, double) = (Underline::None, Underline::Single, Underline::Double);
        // Plain text and other kinds get a single underline, a single one
        // becomes double, hidden text stays bare; again, nothing changes.
        let hovered = [single, single, double, double, single, single, none, none];
        for _ in 0..2 {
            underline(&mut grid, &t, &link);
            assert_eq!(kinds(&grid), hovered);
        }
        copy_pane(&mut grid, &mut t, true, &[], None, None, &Config::default());
        assert_eq!(kinds(&grid)[..6], [none, none, single, single, Underline::Curly, Underline::Curly]);
    }

    #[test]
    fn a_hovered_hyperlink_underlines_all_its_visible_cells() {
        let mut t = Terminal::new(12, 2, 10);
        // The same link (same id and URI) twice, apart; another link with the
        // same URI but no id; a single underline under the link.
        t.feed(b"\x1b]8;id=a;gemini://a\x1b\\ab\x1b]8;;\x1b\\c\x1b]8;;gemini://a\x1b\\d\x1b]8;;\x1b\\");
        t.feed(b"\x1b]8;id=a;gemini://a\x1b\\\x1b[4me\x1b[0m\x1b]8;;\x1b\\");
        let top = t.view_top();
        let link = crate::links::link_at(&t, Point { line: top, col: 0 }).unwrap();
        assert_eq!((link.cells.end, link.target.as_str()), (Point { line: top, col: 1 }, "gemini://a"));
        let mut grid = new_grid();
        let kinds = |grid: &Grid| grid.row(0)[..6].iter().map(|c| c.underline).collect::<Vec<_>>();
        let (none, single, double) = (Underline::None, Underline::Single, Underline::Double);
        copy_pane(&mut grid, &mut t, true, &[], None, None, &Config::default());
        underline(&mut grid, &t, &link);
        assert_eq!(kinds(&grid), [single, single, none, none, double, none]);
        // A link whose id now belongs to another URI underlines nothing.
        let stale = Link { target: "gemini://b".to_owned(), ..link };
        copy_pane(&mut grid, &mut t, true, &[], None, None, &Config::default());
        underline(&mut grid, &t, &stale);
        assert_eq!(kinds(&grid), [none, none, none, none, single, none]);
    }
}
