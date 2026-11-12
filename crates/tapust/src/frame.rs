//! What the renderer draws: one grid of resolved cells per visible pane at
//! the pane's pixel origin, then the cursors. Colors are already resolved, so
//! the renderer knows nothing about palettes, styles or the multiplexer.

use term::Underline;

pub const BOLD: u16 = 1 << 0;
pub const ITALIC: u16 = 1 << 1;
pub const STRIKE: u16 = 1 << 2;
/// The cell holds the left half of a double-width glyph.
pub const WIDE: u16 = 1 << 3;
/// The cell's background takes the background opacity: the terminal's
/// default background, or with background-opacity-cells any but a selected
/// or inverted cell's (ghostty's bg_alpha).
pub const TRANSLUCENT: u16 = 1 << 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(C)]
pub struct FrameCell {
    /// Unicode scalar value, 0 for blank.
    pub ch: u32,
    pub fg: [u8; 4],
    pub bg: [u8; 4],
    /// The underline's own color (SGR 58); without one it takes `fg`.
    pub underline_color: Option<[u8; 4]>,
    pub underline: Underline,
    pub flags: u16,
}

impl FrameCell {
    pub const fn blank(bg: [u8; 4]) -> FrameCell {
        FrameCell { ch: 0, fg: [0; 4], bg, underline_color: None, underline: Underline::None, flags: 0 }
    }
}

/// The sides of a grid whose padding shows the nearest cells' backgrounds
/// (ghostty's padding_extend).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Extend {
    pub left: bool,
    pub right: bool,
    pub up: bool,
    pub down: bool,
}

/// A pane's cells, drawn with cell (0, 0) at (`x`, `y`) in window pixels,
/// inside the pane's rectangle `pane` (x, y, width, height), which shows `bg`
/// around the cells (ghostty's padding), or the cells' own on the sides of
/// `extend`.
pub struct Grid {
    /// Identifies the pane across frames, so the renderer can keep its work.
    pub id: u32,
    pub x: i32,
    pub y: i32,
    pub pane: (i32, i32, u32, u32),
    pub bg: [u8; 4],
    pub extend: Extend,
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
            extend: Extend::default(),
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
    /// The lock glyph ghostty draws while a password is typed.
    Lock,
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

pub struct Frame {
    pub grids: Vec<Grid>,
    pub cursors: Vec<FrameCursor>,
    /// The window background, wherever no pane is.
    pub window_bg: [u8; 4],
    /// background-opacity, for the window background, the padding and the
    /// translucent cells.
    pub opacity: u8,
}

impl Frame {
    pub fn new(window_bg: [u8; 4], opacity: u8) -> Frame {
        Frame { grids: Vec::new(), cursors: Vec::new(), window_bg, opacity }
    }
}
