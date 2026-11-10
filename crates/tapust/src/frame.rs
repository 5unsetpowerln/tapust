//! The window-sized grid the renderer draws.
//!
//! Like tmux, the whole window is one character grid: visible panes are
//! copied into it at their offsets, followed by the pane borders and the
//! status line. Colors are already resolved, so the renderer knows nothing
//! about palettes, styles or panes.

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

/// Where and how to draw the cursor, in frame coordinates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FrameCursor {
    pub x: u16,
    pub y: u16,
    pub shape: term::CursorShape,
    pub color: [u8; 4],
    /// Color of the glyph under a block cursor.
    pub text_color: [u8; 4],
    pub wide: bool,
}

pub struct Frame {
    cols: u16,
    rows: u16,
    cells: Vec<FrameCell>,
    dirty: Vec<bool>,
    pub cursor: Option<FrameCursor>,
    pub default_bg: [u8; 4],
}

impl Frame {
    pub fn new(cols: u16, rows: u16, default_bg: [u8; 4]) -> Frame {
        Frame {
            cols,
            rows,
            cells: vec![FrameCell::blank(default_bg); cols as usize * rows as usize],
            dirty: vec![true; rows as usize],
            cursor: None,
            default_bg,
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

    pub fn clear_dirty(&mut self) {
        self.dirty.fill(false);
    }
}
