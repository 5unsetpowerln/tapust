//! Pane borders, ported from tmux 3.6 screen-redraw.c with the default
//! options (pane-border-lines single, pane-border-indicators colour, no pane
//! status lines or scrollbars).

use crate::window::{Pane, Window};
use crate::{BorderCell, PaneId, Session};

/// screen_redraw_border_type
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Border {
    Outside,
    Inside,
    Left,
    Right,
    Top,
    Bottom,
}

/// tmux CELL_* types.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CellType {
    Inside,
    TopBottom,
    LeftRight,
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
    TopJoin,
    BottomJoin,
    LeftJoin,
    RightJoin,
    Join,
    Outside,
}

impl CellType {
    /// CELL_BORDERS through the ACS table as drawn on a UTF-8 terminal.
    fn glyph(self) -> char {
        match self {
            CellType::Inside => ' ',
            CellType::TopBottom => '\u{2502}',
            CellType::LeftRight => '\u{2500}',
            CellType::TopLeft => '\u{250c}',
            CellType::TopRight => '\u{2510}',
            CellType::BottomLeft => '\u{2514}',
            CellType::BottomRight => '\u{2518}',
            CellType::TopJoin => '\u{252c}',
            CellType::BottomJoin => '\u{2534}',
            CellType::LeftJoin => '\u{251c}',
            CellType::RightJoin => '\u{2524}',
            CellType::Join => '\u{253c}',
            CellType::Outside => '\u{b7}',
        }
    }
}

/// screen_redraw_two_panes
fn two_panes(w: &Window, direction: u8) -> bool {
    if w.panes.len() != 2 {
        return false;
    }
    let second = &w.panes[1];
    !((direction == 0 && second.xoff == 0) || (direction == 1 && second.yoff == 0))
}

/// screen_redraw_pane_border
fn pane_border(w: &Window, wp: &Pane, px: u32, py: u32) -> Border {
    let ex = wp.xoff + wp.sx;
    let ey = wp.yoff + wp.sy;
    if px >= wp.xoff && px < ex && py >= wp.yoff && py < ey {
        return Border::Inside;
    }
    let hsplit = two_panes(w, 0);
    let vsplit = two_panes(w, 1);

    // The wp->sy / 2 tests colour only half the border of two panes.
    if (wp.yoff == 0 || py >= wp.yoff - 1) && py <= ey {
        if wp.xoff == 0 && px == wp.sx && (!hsplit || py <= wp.sy / 2) {
            return Border::Right;
        }
        if wp.xoff != 0 {
            if px == wp.xoff - 1 && (!hsplit || py > wp.sy / 2) {
                return Border::Left;
            }
            if px == wp.xoff + wp.sx {
                return Border::Right;
            }
        }
    }

    if vsplit {
        if wp.yoff == 0 && py == wp.sy && px <= wp.sx / 2 {
            return Border::Bottom;
        }
        if wp.yoff != 0 && py == wp.yoff - 1 && px > wp.sx / 2 {
            return Border::Top;
        }
    } else if (wp.xoff == 0 || px >= wp.xoff) && px <= ex {
        if wp.yoff != 0 && py == wp.yoff - 1 {
            return Border::Top;
        }
        if py == ey {
            return Border::Bottom;
        }
    }
    Border::Outside
}

/// The visible panes starting at the active one, as screen_redraw_check_cell
/// walks them.
fn from_active(w: &Window) -> impl Iterator<Item = &Pane> {
    let start = w.pos(w.active).unwrap_or(0);
    let n = w.panes.len();
    (0..n).map(move |i| &w.panes[(start + i) % n]).filter(|p| w.visible(p.id))
}

/// screen_redraw_cell_border
fn cell_border(w: &Window, px: u32, py: u32) -> bool {
    if px > w.sx || py > w.sy {
        return false;
    }
    if px == w.sx || py == w.sy {
        return true;
    }
    for wp in w.panes.iter().filter(|p| w.visible(p.id)) {
        match pane_border(w, wp, px, py) {
            Border::Inside => return false,
            Border::Outside => {}
            _ => return true,
        }
    }
    false
}

/// screen_redraw_type_of_cell
fn type_of_cell(w: &Window, px: u32, py: u32) -> CellType {
    if px > w.sx || py > w.sy {
        return CellType::Outside;
    }
    let mut borders = 0;
    if px == 0 || cell_border(w, px - 1, py) {
        borders |= 8;
    }
    if px <= w.sx && cell_border(w, px + 1, py) {
        borders |= 4;
    }
    if py == 0 || cell_border(w, px, py - 1) {
        borders |= 2;
    }
    if cell_border(w, px, py + 1) {
        borders |= 1;
    }
    match borders {
        15 => CellType::Join,
        14 => CellType::BottomJoin,
        13 => CellType::TopJoin,
        12 => CellType::LeftRight,
        11 => CellType::RightJoin,
        10 => CellType::BottomRight,
        9 => CellType::TopRight,
        7 => CellType::LeftJoin,
        6 => CellType::BottomLeft,
        5 => CellType::TopLeft,
        3 => CellType::TopBottom,
        _ => CellType::Outside,
    }
}

/// screen_redraw_check_cell: the cell type and the pane whose border it is.
fn check_cell(w: &Window, px: u32, py: u32) -> (CellType, Option<PaneId>) {
    if px > w.sx || py > w.sy {
        return (CellType::Outside, None);
    }
    if px == w.sx || py == w.sy {
        return (type_of_cell(w, px, py), None);
    }
    let mut owner = None;
    for wp in from_active(w) {
        owner = Some(wp.id);
        match pane_border(w, wp, px, py) {
            Border::Inside => return (CellType::Inside, owner),
            Border::Outside => {}
            _ => return (type_of_cell(w, px, py), owner),
        }
    }
    (CellType::Outside, owner)
}

impl Session {
    /// screen_redraw_draw_borders: every border cell of the client's window
    /// area. A pane's border cells all use one style, chosen at its first
    /// cell (screen_redraw_draw_borders_style); since the active pane owns
    /// every cell of its own border, that is the active style exactly for the
    /// cells owned by the active pane.
    pub(crate) fn borders(&self) -> Vec<BorderCell> {
        let w = self.cur();
        let (cols, rows) = (u32::from(self.cols), self.area().1);
        let mut out = Vec::new();
        for y in 0..rows {
            for x in 0..cols {
                let (ty, owner) = check_cell(w, x, y);
                if ty == CellType::Inside {
                    continue;
                }
                out.push(BorderCell {
                    x: x as u16,
                    y: y as u16,
                    glyph: ty.glyph(),
                    active: owner == Some(w.active),
                });
            }
        }
        out
    }
}
