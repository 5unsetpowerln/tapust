//! What the renderer draws: one grid of resolved cells per visible pane at
//! the pane's pixel origin, the cursors, then shapes in window pixels
//! (dividers, dimming, the tab bar, dialogs). Colors are already resolved, so
//! the renderer knows nothing about palettes, styles or the multiplexer.

pub const BOLD: u16 = 1 << 0;
pub const ITALIC: u16 = 1 << 1;
pub const UNDERLINE: u16 = 1 << 2;
pub const STRIKE: u16 = 1 << 3;
/// The cell holds the left half of a double-width glyph.
pub const WIDE: u16 = 1 << 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(C)]
pub struct FrameCell {
    /// Unicode scalar value, 0 for blank.
    pub ch: u32,
    pub fg: [u8; 4],
    pub bg: [u8; 4],
    pub flags: u16,
}

impl FrameCell {
    pub const fn blank(bg: [u8; 4]) -> FrameCell {
        FrameCell { ch: 0, fg: [0; 4], bg, flags: 0 }
    }
}

/// A pane's cells, drawn with cell (0, 0) at (`x`, `y`) in window pixels,
/// inside the pane's rectangle `pane` (x, y, width, height), which shows `bg`
/// around the cells (ghostty's padding).
pub struct Grid {
    /// Identifies the pane across frames, so the renderer can keep its work.
    pub id: u32,
    pub x: i32,
    pub y: i32,
    pub pane: (i32, i32, u32, u32),
    pub bg: [u8; 4],
    cols: u16,
    rows: u16,
    cells: Vec<FrameCell>,
    dirty: Vec<bool>,
}

impl Grid {
    pub fn new(id: u32, pane: (i32, i32, u32, u32), (x, y): (i32, i32), (cols, rows): (u16, u16), bg: [u8; 4]) -> Grid {
        Grid {
            id,
            x,
            y,
            pane,
            bg,
            cols,
            rows,
            cells: vec![FrameCell::blank(bg); cols as usize * rows as usize],
            dirty: vec![true; rows as usize],
        }
    }

    pub fn cols(&self) -> u16 {
        self.cols
    }

    pub fn rows(&self) -> u16 {
        self.rows
    }

    #[inline]
    pub fn row(&self, y: u16) -> &[FrameCell] {
        let start = y as usize * self.cols as usize;
        &self.cells[start..start + self.cols as usize]
    }

    /// Mutable access to row `y`; marks it dirty.
    #[inline]
    pub fn row_mut(&mut self, y: u16) -> &mut [FrameCell] {
        self.dirty[y as usize] = true;
        let start = y as usize * self.cols as usize;
        &mut self.cells[start..start + self.cols as usize]
    }

    pub fn is_dirty(&self, y: u16) -> bool {
        self.dirty[y as usize]
    }

    pub fn mark_dirty(&mut self) {
        self.dirty.fill(true);
    }

    pub fn clear_dirty(&mut self) {
        self.dirty.fill(false);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CursorStyle {
    Block,
    Underline,
    Bar,
    /// The outline ghostty draws in unfocused panes and windows.
    Hollow,
}

/// Where and how to draw a cursor: a cell of `frame.grids[grid]`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FrameCursor {
    pub grid: usize,
    pub x: u16,
    pub y: u16,
    pub style: CursorStyle,
    pub color: [u8; 4],
    /// Color of the glyph under a block cursor.
    pub text_color: [u8; 4],
    pub wide: bool,
}

/// Drawn above the grids, in window pixels.
#[derive(Clone, Debug, PartialEq)]
pub enum Shape {
    /// A rectangle; `radius` rounds its corners and `ring` keeps only a 1px outline.
    Rect { x: i32, y: i32, w: i32, h: i32, color: [u8; 4], radius: u16, ring: bool },
    /// One line of text whose line box starts at (`x`, `y`).
    Text { x: i32, y: i32, text: String, color: [u8; 4], face: crate::font::Face },
    /// A rectangle with fractional edges, `color` at `alpha`; a pixel it
    /// covers partly gets that part of the alpha, as GTK's renderer draws
    /// widgets at fractional scales.
    Area { x0: f64, y0: f64, x1: f64, y1: f64, color: [u8; 3], alpha: f64 },
}

pub struct Frame {
    pub grids: Vec<Grid>,
    pub cursors: Vec<FrameCursor>,
    pub shapes: Vec<Shape>,
    /// The window background, wherever no pane is.
    pub window_bg: [u8; 4],
    /// Device pixels (x, y, width, height) to keep as the drag preview once
    /// the grids are drawn.
    pub capture: Option<(i32, i32, u32, u32)>,
    /// Where to draw the kept pixels, scaled, above everything (x, y, width, height).
    pub preview: Option<(f64, f64, f64, f64)>,
}

impl Frame {
    pub fn new(window_bg: [u8; 4]) -> Frame {
        Frame { grids: Vec::new(), cursors: Vec::new(), shapes: Vec::new(), window_bg, capture: None, preview: None }
    }
}
