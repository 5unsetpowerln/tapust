//! Copies terminal rows into pane grids, resolving styles to colors.

use term::{Attrs, Cell, Color, CursorShape, Palette, Rgb, Style, Terminal, Underline, Width};

use crate::frame::{BOLD, CursorStyle, FrameCell, FrameCursor, Grid, ITALIC, STRIKE, UNDERLINE, WIDE};

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

fn style_colors(style: &Style, palette: &Palette, reverse_video: bool) -> ([u8; 4], [u8; 4]) {
    let mut fg = resolve(style.fg, palette, palette.foreground);
    let mut bg = resolve(style.bg, palette, palette.background);
    if style.attrs.contains(Attrs::INVERSE) != reverse_video {
        std::mem::swap(&mut fg, &mut bg);
    }
    if style.attrs.contains(Attrs::DIM) {
        // Halfway towards the background, like most terminals.
        for i in 0..3 {
            fg[i] = ((fg[i] as u16 + bg[i] as u16) / 2) as u8;
        }
    }
    if style.attrs.contains(Attrs::HIDDEN) {
        fg = bg;
    }
    (fg, bg)
}

fn frame_cell(cell: Cell, term: &Terminal, palette: &Palette, reverse_video: bool) -> FrameCell {
    let style = term.style(cell.style());
    let (fg, bg) = style_colors(style, palette, reverse_video);
    let attrs = style.attrs;
    let mut flags = 0;
    if attrs.contains(Attrs::BOLD) {
        flags |= BOLD;
    }
    if attrs.contains(Attrs::ITALIC) {
        flags |= ITALIC;
    }
    if style.underline != Underline::None {
        flags |= UNDERLINE;
    }
    if attrs.contains(Attrs::STRIKE) {
        flags |= STRIKE;
    }
    let ch = match cell.width() {
        Width::Narrow => cell.codepoint(),
        Width::Wide => {
            flags |= WIDE;
            cell.codepoint()
        }
        Width::SpacerTail | Width::SpacerHead => 0,
    };
    FrameCell { ch, fg, bg, flags }
}

/// Copies the terminal's dirty rows (all rows when `full`) into its pane's
/// grid and clears the terminal's dirty marks. Rows and columns the terminal
/// does not have yet (it is resized after the grid) are blank.
pub fn copy_pane(grid: &mut Grid, term: &mut Terminal, full: bool) {
    let (rows, cols) = (term.rows().min(grid.rows()), term.cols().min(grid.cols()) as usize);
    let palette = term.palette();
    let reverse_video = term.modes().reverse_video;
    let blank = FrameCell::blank(style_colors(&Style::DEFAULT, palette, reverse_video).1);
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
            *dst = if cell == Cell::EMPTY && !reverse_video { blank } else { frame_cell(cell, term, palette, reverse_video) };
        }
        out[cols..].fill(blank);
    }
    term.clear_dirty();
}

/// The cursor of the pane drawn in `frame.grids[grid]`, if visible. A pane
/// that does not have the keyboard focus gets ghostty's hollow block.
/// ghostty's lock cursor at a password prompt: drawn even when the cursor
/// is hidden, unfocused or blinked off.
pub fn lock_cursor(term: &Terminal, grid: usize) -> Option<FrameCursor> {
    let cursor = term.cursor();
    (cursor.x < term.cols() && cursor.y < term.rows()).then(|| cursor_at(term, grid, CursorStyle::Lock))
}

pub fn pane_cursor(term: &Terminal, grid: usize, focused: bool) -> Option<FrameCursor> {
    let cursor = term.cursor();
    if !cursor.visible || cursor.x >= term.cols() || cursor.y >= term.rows() {
        return None;
    }
    let style = match (focused, cursor.shape) {
        (false, _) => CursorStyle::Hollow,
        (true, CursorShape::Block) => CursorStyle::Block,
        (true, CursorShape::Underline) => CursorStyle::Underline,
        (true, CursorShape::Bar) => CursorStyle::Bar,
    };
    Some(cursor_at(term, grid, style))
}

fn cursor_at(term: &Terminal, grid: usize, style: CursorStyle) -> FrameCursor {
    let cursor = term.cursor();
    let palette = term.palette();
    let cell = term.row(cursor.y)[cursor.x as usize];
    let (fg, bg) = style_colors(term.style(cell.style()), palette, term.modes().reverse_video);
    FrameCursor {
        grid,
        x: cursor.x,
        y: cursor.y,
        style,
        color: palette.cursor.map(rgba).unwrap_or(fg),
        text_color: bg,
        wide: cell.width() == Width::Wide,
    }
}
