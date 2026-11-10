//! Copies terminal rows into the frame, resolving styles to colors.

use term::{Attrs, Cell, Color, CursorShape, Palette, Rgb, Style, Terminal, Underline, Width};

use crate::frame::{BOLD, CursorStyle, Frame, FrameCell, FrameCursor, ITALIC, STRIKE, UNDERLINE, WIDE};

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

/// Copies the terminal's dirty rows (all rows when `full`) into the frame at
/// (`x0`, `y0`), clipped to `w x h`, and clears the terminal's dirty marks.
pub fn copy_pane(frame: &mut Frame, term: &mut Terminal, x0: u16, y0: u16, w: u16, h: u16, full: bool) {
    copy_rows(frame, term, x0, y0, w, h, full);
    term.clear_dirty();
}

fn copy_rows(frame: &mut Frame, term: &Terminal, x0: u16, y0: u16, w: u16, h: u16, full: bool) {
    let (rows, cols) = (term.rows().min(h), term.cols().min(w) as usize);
    let palette = term.palette();
    let reverse_video = term.modes().reverse_video;
    let blank = FrameCell::blank(style_colors(&Style::DEFAULT, palette, reverse_video).1);
    for y in 0..h.min(frame.rows().saturating_sub(y0)) {
        if !full && (y >= rows || !term.dirty().is_dirty(y)) {
            continue;
        }
        let out = &mut frame.row_mut(y0 + y)[x0 as usize..(x0 + w) as usize];
        if y >= rows {
            out.fill(blank);
            continue;
        }
        for (dst, &cell) in out.iter_mut().zip(&term.row(y)[..cols]) {
            *dst = if cell == Cell::EMPTY && !reverse_video { blank } else { frame_cell(cell, term, palette, reverse_video) };
        }
        out[cols..].fill(blank);
    }
}

/// The cursor of a pane drawn at (`x0`, `y0`), if visible. A pane that does
/// not have the keyboard focus gets ghostty's hollow block.
pub fn pane_cursor(term: &Terminal, x0: u16, y0: u16, focused: bool) -> Option<FrameCursor> {
    let cursor = term.cursor();
    if !cursor.visible || cursor.x >= term.cols() || cursor.y >= term.rows() {
        return None;
    }
    let palette = term.palette();
    let cell = term.row(cursor.y)[cursor.x as usize];
    let (fg, bg) = style_colors(term.style(cell.style()), palette, term.modes().reverse_video);
    let style = match (focused, cursor.shape) {
        (false, _) => CursorStyle::Hollow,
        (true, CursorShape::Block) => CursorStyle::Block,
        (true, CursorShape::Underline) => CursorStyle::Underline,
        (true, CursorShape::Bar) => CursorStyle::Bar,
    };
    Some(FrameCursor {
        x: x0 + cursor.x,
        y: y0 + cursor.y,
        style,
        color: palette.cursor.map(rgba).unwrap_or(fg),
        text_color: bg,
        wide: cell.width() == Width::Wide,
    })
}
