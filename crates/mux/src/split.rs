//! The panes of a window as ghostty's split tree (`src/datastruct/split_tree.zig`):
//! a binary tree whose splits keep only an orientation and a ratio. Pixel
//! sizes come from laying the tree out like ghostty's nested GtkPaned
//! widgets, so resizing the window keeps every ratio.

use crate::{Metrics, PaneId, Rect};

#[derive(Clone, Debug)]
pub(crate) enum Node {
    Leaf(PaneId),
    Split(Box<Split>),
}

#[derive(Clone, Debug)]
pub(crate) struct Split {
    /// Children side by side (ghostty's `horizontal` layout) or stacked.
    pub horizontal: bool,
    /// The first child's share of the space between the children.
    pub ratio: f64,
    pub first: Node,
    pub second: Node,
}

/// Where a new pane goes relative to the one it splits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Side {
    pub horizontal: bool,
    /// Left of or above the split pane.
    pub before: bool,
}

/// Result of searching for the split a resize moves.
enum Resize {
    NotHere,
    /// The pane is below, but no ancestor with the orientation was found yet.
    Pending,
    Done,
}

fn extent(rect: Rect, horizontal: bool) -> u32 {
    if horizontal { rect.w } else { rect.h }
}

impl Split {
    /// The first child's size along the split: GtkPaned's position for the
    /// ratio, kept between the children's minimum sizes (`shrink-*-child: false`).
    /// Also returns the position the ratio asks for.
    fn position(&self, rect: Rect, m: &Metrics) -> (u32, u32) {
        let avail = extent(rect, self.horizontal).saturating_sub(m.divider);
        let desired = (f64::from(avail) * self.ratio).round() as u32;
        let min = self.first.min_size(self.horizontal, m);
        let max = avail.saturating_sub(self.second.min_size(self.horizontal, m));
        (desired.clamp(min, min.max(max)).min(avail), desired)
    }

    /// The children's rectangles and the divider between them.
    fn children(&self, rect: Rect, m: &Metrics) -> (Rect, Rect, Rect) {
        let (pos, _) = self.position(rect, m);
        let total = extent(rect, self.horizontal);
        let divider = m.divider.min(total - pos);
        let rest = total - pos - divider;
        if self.horizontal {
            (
                Rect { w: pos, ..rect },
                Rect { x: rect.x + pos, w: divider, ..rect },
                Rect { x: rect.x + pos + divider, w: rest, ..rect },
            )
        } else {
            (
                Rect { h: pos, ..rect },
                Rect { y: rect.y + pos, h: divider, ..rect },
                Rect { y: rect.y + pos + divider, h: rest, ..rect },
            )
        }
    }
}

impl Node {
    fn split_node(horizontal: bool, first: Node, second: Node) -> Node {
        Node::Split(Box::new(Split { horizontal, ratio: 0.5, first, second }))
    }

    /// The panes in order: left to right and top to bottom within every split.
    pub(crate) fn leaves(&self, out: &mut Vec<PaneId>) {
        match self {
            Node::Leaf(wp) => out.push(*wp),
            Node::Split(s) => {
                s.first.leaves(out);
                s.second.leaves(out);
            }
        }
    }

    /// Wraps this node in a split with `new` on `side`, halving its space
    /// (ghostty's new_split always uses the ratio 0.5).
    pub(crate) fn split(&mut self, new: PaneId, side: Side) {
        let old = std::mem::replace(self, Node::Leaf(new));
        let new = Node::Leaf(new);
        *self = if side.before {
            Node::split_node(side.horizontal, new, old)
        } else {
            Node::split_node(side.horizontal, old, new)
        };
    }

    /// The leaf of pane `wp`.
    pub(crate) fn find_mut(&mut self, wp: PaneId) -> Option<&mut Node> {
        match self {
            Node::Leaf(p) if *p == wp => Some(self),
            Node::Leaf(_) => None,
            Node::Split(s) => match s.first.find_mut(wp) {
                Some(found) => Some(found),
                None => s.second.find_mut(wp),
            },
        }
    }

    /// Removes pane `wp`; its sibling takes the place of their parent. The
    /// last pane cannot be removed.
    pub(crate) fn remove(&mut self, wp: PaneId) -> bool {
        let Node::Split(s) = self else { return false };
        let keep = if matches!(s.first, Node::Leaf(p) if p == wp) {
            std::mem::replace(&mut s.second, Node::Leaf(wp))
        } else if matches!(s.second, Node::Leaf(p) if p == wp) {
            std::mem::replace(&mut s.first, Node::Leaf(wp))
        } else {
            return s.first.remove(wp) || s.second.remove(wp);
        };
        *self = keep;
        true
    }

    /// Exchanges the places of panes `a` and `b`.
    pub(crate) fn swap(&mut self, a: PaneId, b: PaneId) {
        match self {
            Node::Leaf(p) if *p == a => *p = b,
            Node::Leaf(p) if *p == b => *p = a,
            Node::Leaf(_) => {}
            Node::Split(s) => {
                s.first.swap(a, b);
                s.second.swap(a, b);
            }
        }
    }

    /// Puts `order` into the leaves, keeping the shape and ratios.
    pub(crate) fn assign(&mut self, order: &mut impl Iterator<Item = PaneId>) {
        match self {
            Node::Leaf(p) => *p = order.next().expect("one pane per leaf"),
            Node::Split(s) => {
                s.first.assign(order);
                s.second.assign(order);
            }
        }
    }

    /// ghostty's equalize: every split's ratio becomes its first child's
    /// share of the leaves counted along the split.
    pub(crate) fn equalize(&mut self) {
        if let Node::Split(s) = self {
            let first = s.first.weight(s.horizontal);
            let second = s.second.weight(s.horizontal);
            s.ratio = f64::from(first) / f64::from(first + second);
            s.first.equalize();
            s.second.equalize();
        }
    }

    fn weight(&self, horizontal: bool) -> u32 {
        match self {
            Node::Split(s) if s.horizontal == horizontal => s.first.weight(horizontal) + s.second.weight(horizontal),
            _ => 1,
        }
    }

    /// The smallest size along an axis: a pane keeps one cell inside its
    /// padding, like ghostty's split minimum size.
    pub(crate) fn min_size(&self, horizontal: bool, m: &Metrics) -> u32 {
        match self {
            Node::Leaf(_) if horizontal => m.cell_width + 2 * m.padding_x,
            Node::Leaf(_) => m.cell_height + 2 * m.padding_y,
            Node::Split(s) => {
                let (a, b) = (s.first.min_size(horizontal, m), s.second.min_size(horizontal, m));
                if s.horizontal == horizontal { a + m.divider + b } else { a.max(b) }
            }
        }
    }

    /// Lays the tree out in `rect`, appending each pane's rectangle and each divider.
    pub(crate) fn layout(&self, rect: Rect, m: &Metrics, panes: &mut Vec<(PaneId, Rect)>, dividers: &mut Vec<Rect>) {
        match self {
            Node::Leaf(wp) => panes.push((*wp, rect)),
            Node::Split(s) => {
                let (first, divider, second) = s.children(rect, m);
                s.first.layout(first, m, panes, dividers);
                dividers.push(divider);
                s.second.layout(second, m, panes, dividers);
            }
        }
    }

    /// ghostty's resize_split: moves the divider of the nearest split around
    /// `wp` with the orientation by `amount` pixels (positive is right or
    /// down). `rect` is the tree's area.
    pub(crate) fn resize(&mut self, wp: PaneId, horizontal: bool, amount: i32, rect: Rect, m: &Metrics) {
        let total = f64::from(extent(rect, horizontal));
        if total > 0.0 {
            let delta = (f64::from(amount) / total).clamp(-1.0, 1.0);
            self.resize_in(wp, horizontal, delta, rect, 1.0, m);
        }
    }

    /// `share` is this node's extent as a fraction of the tree's, from the ratios alone.
    fn resize_in(&mut self, wp: PaneId, horizontal: bool, delta: f64, rect: Rect, share: f64, m: &Metrics) -> Resize {
        let s = match self {
            Node::Leaf(p) => return if *p == wp { Resize::Pending } else { Resize::NotHere },
            Node::Split(s) => s,
        };
        let (first, _, second) = s.children(rect, m);
        let along = s.horizontal == horizontal;
        let (first_share, second_share) = if along { (share * s.ratio, share * (1.0 - s.ratio)) } else { (share, share) };
        let found = match s.first.resize_in(wp, horizontal, delta, first, first_share, m) {
            Resize::NotHere => s.second.resize_in(wp, horizontal, delta, second, second_share, m),
            found => found,
        };
        if !matches!(found, Resize::Pending) || !along {
            return found;
        }
        // A split held at a child's minimum size resizes from the ratio it
        // shows, not the one it keeps for when it grows again.
        let (shown, desired) = s.position(rect, m);
        let avail = extent(rect, horizontal).saturating_sub(m.divider);
        if shown != desired && avail > 0 {
            s.ratio = f64::from(shown) / f64::from(avail);
        }
        if share > 0.0 {
            s.ratio = (s.ratio + delta / share).clamp(0.0, 1.0);
        }
        Resize::Done
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const M: Metrics =
        Metrics { width: 1276, height: 796, cell_width: 10, cell_height: 20, padding_x: 2, padding_y: 2, divider: 1 };
    const AREA: Rect = Rect { x: 0, y: 0, w: 1276, h: 796 };
    const RIGHT: Side = Side { horizontal: true, before: false };
    const DOWN: Side = Side { horizontal: false, before: false };

    fn layout(tree: &Node) -> (Vec<(PaneId, Rect)>, Vec<Rect>) {
        let (mut panes, mut dividers) = (Vec::new(), Vec::new());
        tree.layout(AREA, &M, &mut panes, &mut dividers);
        (panes, dividers)
    }

    fn rect(x: u32, y: u32, w: u32, h: u32) -> Rect {
        Rect { x, y, w, h }
    }

    #[test]
    fn halves_around_a_one_pixel_divider() {
        let mut tree = Node::Leaf(0);
        tree.split(1, RIGHT);
        let (panes, dividers) = layout(&tree);
        // GtkPaned: round((1276 - 1) * 0.5) = 638 for the first child.
        assert_eq!(panes, vec![(0, rect(0, 0, 638, 796)), (1, rect(639, 0, 637, 796))]);
        assert_eq!(dividers, vec![rect(638, 0, 1, 796)]);
    }

    #[test]
    fn removing_gives_the_parent_to_the_sibling_subtree() {
        let mut tree = Node::Leaf(0);
        tree.split(1, RIGHT);
        tree.find_mut(1).unwrap().split(2, DOWN);
        assert!(tree.remove(0));
        // The stacked pair now fills the window.
        let (panes, _) = layout(&tree);
        assert_eq!(panes, vec![(1, rect(0, 0, 1276, 398)), (2, rect(0, 399, 1276, 397))]);
        assert!(!Node::Leaf(3).remove(3));
    }

    #[test]
    fn resize_moves_the_nearest_divider_of_the_orientation() {
        let mut tree = Node::Leaf(0);
        tree.split(1, RIGHT);
        tree.find_mut(1).unwrap().split(2, DOWN);
        // Pane 2 sits under the vertical split; its nearest horizontal split is the root.
        tree.resize(2, true, -50, AREA, &M);
        let (panes, _) = layout(&tree);
        assert_eq!(panes[0].1.w, 588);
        // The vertical split moves by 30 pixels of the whole height.
        tree.resize(2, false, 30, AREA, &M);
        let (panes, _) = layout(&tree);
        assert_eq!(panes[1].1.h, 427);
    }

    #[test]
    fn minimum_sizes_hold_and_resize_starts_from_the_shown_ratio() {
        let mut tree = Node::Leaf(0);
        tree.split(1, RIGHT);
        tree.resize(0, true, -2000, AREA, &M);
        let (panes, _) = layout(&tree);
        // One cell plus the padding on both sides.
        assert_eq!(panes[0].1.w, 14);
        tree.resize(0, true, 10, AREA, &M);
        let (panes, _) = layout(&tree);
        assert_eq!(panes[0].1.w, 24);
    }

    #[test]
    fn equalize_weighs_leaves_along_the_split() {
        let mut tree = Node::Leaf(0);
        tree.split(1, RIGHT);
        tree.find_mut(1).unwrap().split(2, RIGHT);
        tree.equalize();
        let (panes, _) = layout(&tree);
        let widths: Vec<u32> = panes.iter().map(|p| p.1.w).collect();
        assert_eq!(widths, vec![425, 425, 424]);
    }

    #[test]
    fn leaves_follow_the_tree_and_swap_and_assign_keep_the_shape() {
        let mut tree = Node::Leaf(0);
        tree.split(1, Side { horizontal: true, before: true });
        tree.find_mut(0).unwrap().split(2, DOWN);
        let mut order = Vec::new();
        tree.leaves(&mut order);
        assert_eq!(order, vec![1, 0, 2]);
        tree.swap(1, 2);
        tree.assign(&mut [5, 6, 7].into_iter());
        order.clear();
        tree.leaves(&mut order);
        assert_eq!(order, vec![5, 6, 7]);
    }
}
