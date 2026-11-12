//! tmux 3.6's layout presets and `select-layout -E` (layout-set.c and the
//! spreading in layout.c). They are computed as tmux computes them, on
//! tmux's layout cells: an n-ary tree sized in cells over the window's size,
//! with a one-cell border between siblings. The split tree then takes them
//! as ratios, with which GtkPaned gives the panes the same rectangles on a
//! grid of the window's size ([`Node::layout`]) and the same proportions in
//! pixels.

use std::collections::BTreeMap;

use crate::split::{Node, Split};
use crate::{PaneId, Rect};

/// tmux's PANE_MINIMUM.
const PANE_MINIMUM: u32 = 1;
/// The main-pane-width and main-pane-height options. other-pane-width and
/// other-pane-height are 0, so the other panes take the rest.
const MAIN_PANE_WIDTH: u32 = 80;
const MAIN_PANE_HEIGHT: u32 = 24;

/// select-layout -E is refused where tmux 3.6 would loop forever (a cell
/// cannot shrink as far as spreading asks) and where a pane gets no cell
/// of the window's grid, which never happens in tmux.
const SPREAD_REFUSED: &str = "can't spread out the layout";

/// A preset of tmux's layout_sets, in the order next-layout steps through them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Layout {
    EvenHorizontal,
    EvenVertical,
    MainHorizontal,
    MainHorizontalMirrored,
    MainVertical,
    MainVerticalMirrored,
    Tiled,
}

impl Layout {
    pub const ALL: [Layout; 7] = [
        Layout::EvenHorizontal,
        Layout::EvenVertical,
        Layout::MainHorizontal,
        Layout::MainHorizontalMirrored,
        Layout::MainVertical,
        Layout::MainVerticalMirrored,
        Layout::Tiled,
    ];

    /// The name select-layout takes.
    pub fn name(self) -> &'static str {
        match self {
            Layout::EvenHorizontal => "even-horizontal",
            Layout::EvenVertical => "even-vertical",
            Layout::MainHorizontal => "main-horizontal",
            Layout::MainHorizontalMirrored => "main-horizontal-mirrored",
            Layout::MainVertical => "main-vertical",
            Layout::MainVerticalMirrored => "main-vertical-mirrored",
            Layout::Tiled => "tiled",
        }
    }

    /// layout_set_next after `last` (`w->lastlayout`).
    pub(crate) fn next(last: Option<Layout>) -> Layout {
        last.map_or(Layout::ALL[0], |l| Layout::ALL[(l as usize + 1) % Layout::ALL.len()])
    }

    /// layout_set_previous before `last`.
    pub(crate) fn previous(last: Option<Layout>) -> Layout {
        let n = Layout::ALL.len();
        last.map_or(Layout::ALL[n - 1], |l| Layout::ALL[(l as usize + n - 1) % n])
    }

    /// The preset's tree for `panes`, in tmux's pane order, on a `size`
    /// grid; None for a single pane, which tmux leaves as it is.
    pub(crate) fn arrange(self, panes: &[PaneId], size: (u32, u32)) -> Option<Node> {
        if panes.len() <= 1 {
            return None;
        }
        let cell = match self {
            Layout::EvenHorizontal => Cell::even(panes, size, true),
            Layout::EvenVertical => Cell::even(panes, size, false),
            Layout::MainHorizontal => Cell::main(panes, size, false, false),
            Layout::MainHorizontalMirrored => Cell::main(panes, size, false, true),
            Layout::MainVertical => Cell::main(panes, size, true, false),
            Layout::MainVerticalMirrored => Cell::main(panes, size, true, true),
            Layout::Tiled => Cell::tiled(panes, size),
        };
        let mut tree = cell.to_node();
        tree.fit(&cell.rects(), false);
        Some(tree)
    }
}

/// select-layout -E: layout_spread_out for `pane`, on the tree as GtkPaned
/// lays it out on a `size` grid.
pub(crate) fn spread_out(tree: &mut Node, pane: PaneId, (sx, sy): (u32, u32)) -> Result<(), String> {
    let refused = || SPREAD_REFUSED.to_owned();
    let mut root = Cell::from_node(tree, Rect { x: 0, y: 0, w: sx, h: sy }).ok_or_else(refused)?;
    match root.spread_out(pane).expect("pane in tree") {
        Ok(true) => {
            tree.fit(&root.rects(), true);
            Ok(())
        }
        Ok(false) => Ok(()),
        Err(Stuck) => Err(refused()),
    }
}

/// tmux's layout_cell.
struct Cell {
    sx: u32,
    sy: u32,
    kind: Kind,
}

enum Kind {
    Pane(PaneId),
    /// LAYOUT_LEFTRIGHT (`horizontal`) or LAYOUT_TOPBOTTOM.
    Node { horizontal: bool, cells: Vec<Cell> },
}

/// tmux 3.6 would loop forever in layout_resize_adjust: the cells cannot
/// shrink as far as the change asks.
#[derive(Debug)]
struct Stuck;

impl Cell {
    fn pane(pane: PaneId, (sx, sy): (u32, u32)) -> Cell {
        Cell { sx, sy, kind: Kind::Pane(pane) }
    }

    fn node(horizontal: bool, (sx, sy): (u32, u32), cells: Vec<Cell>) -> Cell {
        Cell { sx, sy, kind: Kind::Node { horizontal, cells } }
    }

    fn size(&self, horizontal: bool) -> u32 {
        if horizontal { self.sx } else { self.sy }
    }

    /// layout_set_even: the panes side by side (`horizontal`) or stacked,
    /// spread out evenly.
    fn even(panes: &[PaneId], (wx, wy): (u32, u32), horizontal: bool) -> Cell {
        let along = panes.len() as u32 * (PANE_MINIMUM + 1) - 1;
        let size = if horizontal { (along.max(wx), wy) } else { (wx, along.max(wy)) };
        let cells = panes.iter().map(|&p| Cell::pane(p, (wx, wy))).collect();
        let mut root = Cell::node(horizontal, size, cells);
        root.spread().expect("panes resize freely");
        root
    }

    /// layout_set_main_v (`horizontal`: the first pane left of the others)
    /// and layout_set_main_h, mirrored when the first pane goes last.
    fn main(panes: &[PaneId], (wx, wy): (u32, u32), horizontal: bool, mirrored: bool) -> Cell {
        // Sizes as (along the root, across it) and back.
        let xy = |along, across| if horizontal { (along, across) } else { (across, along) };
        let (along, across) = xy(wx, wy);
        let wanted = if horizontal { MAIN_PANE_WIDTH } else { MAIN_PANE_HEIGHT };
        // One cell of the window is the border.
        let space = along - 1;
        let (main, other) = if wanted + PANE_MINIMUM >= space {
            let main = if space <= PANE_MINIMUM + PANE_MINIMUM { PANE_MINIMUM } else { space - PANE_MINIMUM };
            (main, PANE_MINIMUM)
        } else {
            (wanted, space - wanted)
        };
        let (first, rest) = panes.split_first().expect("two panes or more");
        let across = (rest.len() as u32 * (PANE_MINIMUM + 1) - 1).max(across);
        let main_cell = Cell::pane(*first, xy(main, across));
        let other_cell = match rest {
            [only] => Cell::pane(*only, xy(other, across)),
            _ => {
                let cells = rest.iter().map(|&p| Cell::pane(p, xy(other, PANE_MINIMUM))).collect();
                let mut others = Cell::node(!horizontal, xy(other, across), cells);
                others.spread().expect("panes resize freely");
                others
            }
        };
        let cells = if mirrored { vec![other_cell, main_cell] } else { vec![main_cell, other_cell] };
        Cell::node(horizontal, xy(main + other + 1, across), cells)
    }

    /// layout_set_tiled: rows of columns filled in pane order, the last
    /// column and the last row taking what is left of the window.
    fn tiled(panes: &[PaneId], (wx, wy): (u32, u32)) -> Cell {
        let n = panes.len() as u32;
        // tiled-layout-max-columns is 0: no limit.
        let (mut rows, mut columns) = (1, 1);
        while rows * columns < n {
            rows += 1;
            if rows * columns < n {
                columns += 1;
            }
        }
        // tmux's u_int wraps around in a window narrower than its borders.
        let width = (wx.saturating_sub(columns - 1) / columns).max(PANE_MINIMUM);
        let height = (wy.saturating_sub(rows - 1) / rows).max(PANE_MINIMUM);
        let size = (((width + 1) * columns - 1).max(wx), ((height + 1) * rows - 1).max(wy));
        let mut cells: Vec<Cell> = panes
            .chunks(columns as usize)
            .map(|chunk| {
                if let [only] = chunk {
                    return Cell::pane(*only, (wx, height));
                }
                let mut cells: Vec<Cell> = chunk.iter().map(|&p| Cell::pane(p, (width, height))).collect();
                let used = chunk.len() as u32 * (width + 1) - 1;
                if wx > used {
                    let last = cells.last_mut().expect("a column");
                    last.resize_adjust(true, (wx - used) as i32).expect("panes resize freely");
                }
                Cell::node(true, (wx, height), cells)
            })
            .collect();
        let used = rows * height + rows - 1;
        if wy > used {
            let last = cells.last_mut().expect("a row");
            last.resize_adjust(false, (wy - used) as i32).expect("growing never sticks");
        }
        Cell::node(false, size, cells)
    }

    /// layout_resize_check: how far the cell can shrink along the axis.
    fn resize_check(&self, horizontal: bool) -> u32 {
        match &self.kind {
            Kind::Pane(_) => self.size(horizontal).saturating_sub(PANE_MINIMUM),
            Kind::Node { horizontal: h, cells } if *h == horizontal => {
                cells.iter().map(|c| c.resize_check(horizontal)).sum()
            }
            Kind::Node { cells, .. } => cells.iter().map(|c| c.resize_check(horizontal)).min().unwrap_or(u32::MAX),
        }
    }

    /// layout_resize_adjust: changes the size along the axis, the children
    /// of the same orientation one cell at a time in turn.
    fn resize_adjust(&mut self, horizontal: bool, change: i32) -> Result<(), Stuck> {
        let size = if horizontal { &mut self.sx } else { &mut self.sy };
        *size = size.checked_add_signed(change).expect("a cell never shrinks past zero");
        let Kind::Node { horizontal: h, cells } = &mut self.kind else { return Ok(()) };
        if *h != horizontal {
            return cells.iter_mut().try_for_each(|c| c.resize_adjust(horizontal, change));
        }
        let mut change = change;
        while change != 0 {
            let before = change;
            for c in cells.iter_mut() {
                if change == 0 {
                    break;
                }
                if change > 0 {
                    c.resize_adjust(horizontal, 1)?;
                    change -= 1;
                } else if c.resize_check(horizontal) > 0 {
                    c.resize_adjust(horizontal, -1)?;
                    change += 1;
                }
            }
            if change == before {
                return Err(Stuck);
            }
        }
        Ok(())
    }

    /// layout_spread_cell: the children get the same size, the first ones a
    /// cell more until the remainder is used up. Returns whether one changed.
    fn spread(&mut self) -> Result<bool, Stuck> {
        let (sx, sy) = (self.sx, self.sy);
        let Kind::Node { horizontal, cells } = &mut self.kind else { return Ok(false) };
        let horizontal = *horizontal;
        let number = cells.len() as u32;
        let size = if horizontal { sx } else { sy };
        if number <= 1 || size < number - 1 {
            return Ok(false);
        }
        let each = (size - (number - 1)) / number;
        if each == 0 {
            return Ok(false);
        }
        let mut remainder = size + 1 - number * (each + 1);
        let mut changed = false;
        for c in cells {
            let mut this = each;
            if remainder > 0 {
                this += 1;
                remainder -= 1;
            }
            let change = this as i32 - c.size(horizontal) as i32;
            c.resize_adjust(horizontal, change)?;
            changed |= change != 0;
        }
        Ok(changed)
    }

    /// layout_spread_out: spreads the cells from the pane's parent up to the
    /// first ancestor that changes. None when the pane is not in this cell.
    fn spread_out(&mut self, pane: PaneId) -> Option<Result<bool, Stuck>> {
        let cells = match &mut self.kind {
            Kind::Pane(p) => return (*p == pane).then_some(Ok(false)),
            Kind::Node { cells, .. } => cells,
        };
        let below = cells.iter_mut().find_map(|c| c.spread_out(pane))?;
        Some(match below {
            Ok(false) => self.spread(),
            done => done,
        })
    }

    /// The cells of `node` as GtkPaned lays it out in `r`: a split and the
    /// splits of the same orientation below it are one cell. None when a
    /// pane gets no cell.
    fn from_node(node: &Node, r: Rect) -> Option<Cell> {
        let s = match node {
            Node::Leaf(p) => return (r.w > 0 && r.h > 0).then(|| Cell::pane(*p, (r.w, r.h))),
            Node::Split(s) => s,
        };
        let (a, b) = s.divide(r);
        let mut cells = Vec::new();
        for (child, r) in [(&s.first, a), (&s.second, b)] {
            match Cell::from_node(child, r)? {
                Cell { kind: Kind::Node { horizontal, cells: inner }, .. } if horizontal == s.horizontal => {
                    cells.extend(inner)
                }
                cell => cells.push(cell),
            }
        }
        Some(Cell::node(s.horizontal, (r.w, r.h), cells))
    }

    /// The split tree of the cells, every cell's children nested to the
    /// right as splitting the last pane again and again nests them. The
    /// ratios are fit to the cells' rectangles afterwards ([`Node::fit`]).
    fn to_node(&self) -> Node {
        match &self.kind {
            Kind::Pane(p) => Node::Leaf(*p),
            Kind::Node { horizontal, cells } => {
                let mut nodes = cells.iter().rev().map(Cell::to_node);
                let last = nodes.next().expect("a node has children");
                nodes.fold(last, |second, first| {
                    Node::Split(Box::new(Split { horizontal: *horizontal, ratio: 0.5, first, second }))
                })
            }
        }
    }

    /// layout_fix_offsets and layout_fix_panes: the panes' rectangles.
    fn rects(&self) -> BTreeMap<PaneId, Rect> {
        fn place(c: &Cell, x: u32, y: u32, out: &mut BTreeMap<PaneId, Rect>) {
            match &c.kind {
                Kind::Pane(p) => {
                    out.insert(*p, Rect { x, y, w: c.sx, h: c.sy });
                }
                Kind::Node { horizontal, cells } => {
                    let (mut x, mut y) = (x, y);
                    for child in cells {
                        place(child, x, y, out);
                        if *horizontal {
                            x += child.sx + 1;
                        } else {
                            y += child.sy + 1;
                        }
                    }
                }
            }
        }
        let mut out = BTreeMap::new();
        place(self, 0, 0, &mut out);
        out
    }
}
