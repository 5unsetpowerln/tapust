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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CursorStyle {
    Block,
    Underline,
    Bar,
    /// The outline ghostty draws in unfocused panes and windows.
    Hollow,
}

/// Where and how to draw a cursor, in frame coordinates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FrameCursor {
    pub x: u16,
    pub y: u16,
    pub style: CursorStyle,
    pub color: [u8; 4],
    /// Color of the glyph under a block cursor.
    pub text_color: [u8; 4],
    pub wide: bool,
}

/// A translucent rectangle over a range of cells (unfocused panes are dimmed with one).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Dim {
    pub x: u16,
    pub y: u16,
    pub w: u16,
    pub h: u16,
    pub color: [u8; 4],
}

/// Text drawn on a panel, in pixels from the panel's top-left corner.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PanelText {
    pub x: i32,
    pub y: i32,
    pub text: String,
    pub color: [u8; 4],
    pub bold: bool,
}

/// A box drawn above everything else (dialogs, messages), in pixels from the grid origin.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Panel {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
    pub fill: [u8; 4],
    pub outline: Option<[u8; 4]>,
    pub texts: Vec<PanelText>,
}

pub struct Frame {
    cols: u16,
    rows: u16,
    cells: Vec<FrameCell>,
    dirty: Vec<bool>,
    pub cursors: Vec<FrameCursor>,
    pub dims: Vec<Dim>,
    pub panels: Vec<Panel>,
    pub default_bg: [u8; 4],
}

impl Frame {
    pub fn new(cols: u16, rows: u16, default_bg: [u8; 4]) -> Frame {
        Frame {
            cols,
            rows,
            cells: vec![FrameCell::blank(default_bg); cols as usize * rows as usize],
            dirty: vec![true; rows as usize],
            cursors: Vec::new(),
            dims: Vec::new(),
            panels: Vec::new(),
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
