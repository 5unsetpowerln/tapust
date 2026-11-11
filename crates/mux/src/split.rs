//! The panes of a window as ghostty's split tree (`src/datastruct/split_tree.zig`):
//! a binary tree whose splits keep an orientation and a ratio. GTK lays the
//! tree out (nested GtkPaned widgets); this module only keeps the data model
//! and its arithmetic, which ghostty does in `f16`.

use crate::PaneId;

#[derive(Clone, Debug, PartialEq)]
pub enum Node {
    Leaf(PaneId),
    Split(Box<Split>),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Split {
    /// Children side by side (ghostty's `horizontal` layout) or stacked.
    pub horizontal: bool,
    /// The first child's share of the space between the children; always a
    /// binary16 value, since ghostty keeps ratios as `f16`.
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

/// Rounds to the nearest IEEE binary16 value, ties to even. ghostty computes
/// ratios in `f16`, where every operation rounds its result this way; an
/// operation done in `f64` and then rounded gives the same value.
pub fn f16(x: f64) -> f64 {
    const MAX: f64 = 65504.0;
    if x == 0.0 || !x.is_finite() {
        return x;
    }
    let a = x.abs();
    let exponent = ((a.to_bits() >> 52) & 0x7ff) as i32 - 1023;
    // Ten fraction bits; below the normal range the spacing stays 2^-24.
    let ulp = 2f64.powi(exponent.max(-14) - 10);
    let rounded = (a / ulp).round_ties_even() * ulp;
    let rounded = if rounded > MAX { f64::INFINITY } else { rounded };
    rounded.copysign(x)
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
            s.ratio = f16(f64::from(first) / f64::from(first + second));
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

    /// The extent along an axis counting every leaf as one unit, from which
    /// ghostty's spatial representation starts.
    fn units(&self, horizontal: bool) -> u32 {
        match self {
            Node::Leaf(_) => 1,
            Node::Split(s) => {
                let (a, b) = (s.first.units(horizontal), s.second.units(horizontal));
                if s.horizontal == horizontal { a + b } else { a.max(b) }
            }
        }
    }

    /// The split with in-order index `index` (as the GUI numbers the split
    /// widgets it builds from the tree).
    pub(crate) fn split_at(&mut self, index: usize) -> Option<&mut Split> {
        let Node::Split(s) = self else { return None };
        let own = s.first.split_count();
        if index < own {
            s.first.split_at(index)
        } else if index == own {
            Some(s)
        } else {
            s.second.split_at(index - own - 1)
        }
    }

    fn split_count(&self) -> usize {
        match self {
            Node::Leaf(_) => 0,
            Node::Split(s) => 1 + s.first.split_count() + s.second.split_count(),
        }
    }

    /// ghostty's SplitTree.resize: moves the nearest split around `wp` with
    /// the orientation by `delta`, a fraction of the whole tree's extent
    /// (positive is right or down). Returns whether a split moved.
    pub(crate) fn resize(&mut self, wp: PaneId, horizontal: bool, delta: f64) -> bool {
        // GTK computes the delta in f64; the tree takes it as f16.
        let delta = f16(delta.clamp(-1.0, 1.0));
        let root = f16(f64::from(self.units(horizontal)));
        matches!(self.resize_in(wp, horizontal, delta, root, root), Resize::Done)
    }

    /// `size` is this node's extent in ghostty's spatial representation: the
    /// root spans `root` leaf units and every split hands its children
    /// `size * ratio` and `size * (1 - ratio)`, each rounded to f16.
    fn resize_in(&mut self, wp: PaneId, horizontal: bool, delta: f64, size: f64, root: f64) -> Resize {
        let s = match self {
            Node::Leaf(p) => return if *p == wp { Resize::Pending } else { Resize::NotHere },
            Node::Split(s) => s,
        };
        let along = s.horizontal == horizontal;
        let (first_size, second_size) =
            if along { (f16(size * s.ratio), f16(size * f16(1.0 - s.ratio))) } else { (size, size) };
        let found = match s.first.resize_in(wp, horizontal, delta, first_size, root) {
            Resize::NotHere => s.second.resize_in(wp, horizontal, delta, second_size, root),
            found => found,
        };
        if !matches!(found, Resize::Pending) || !along {
            return found;
        }
        // The split's width relative to the tree's, as spatial normalizes it.
        let scale = f16(size / root);
        if scale > 0.0 {
            s.ratio = f16(s.ratio + f16(delta / scale)).clamp(0.0, 1.0);
        }
        Resize::Done
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RIGHT: Side = Side { horizontal: true, before: false };
    const DOWN: Side = Side { horizontal: false, before: false };

    #[test]
    fn rounds_like_ieee_binary16() {
        // Expected values from Python's struct format 'e' (IEEE binary16).
        let table = [
            (0.5, 0.5),
            (1.0 / 3.0, 0.333251953125),
            (2.0 / 3.0, 0.66650390625),
            (0.1, 0.0999755859375),
            (50.0 / 1276.0, 0.0391845703125),
            (0.9999, 1.0),
            (1.00048828125, 1.0),
            (1.00146484375, 1.001953125),
            (2.9802322387695312e-08, 0.0),
            (4.470348358154297e-08, 5.960464477539063e-08),
            (1e-07, 1.1920928955078125e-07),
            (6.1e-05, 6.097555160522461e-05),
            (65519.0, 65504.0),
            (65520.0, f64::INFINITY),
            (-0.35233447033367526, -0.352294921875),
            (0.3018689460797075, 0.3017578125),
            (-0.8551274266649145, -0.85498046875),
            (0.0717640086133784, 0.07177734375),
        ];
        for (x, expected) in table {
            assert_eq!(f16(x), expected, "{x}");
        }
    }

    #[test]
    fn removing_gives_the_parent_to_the_sibling_subtree() {
        let mut tree = Node::Leaf(0);
        tree.split(1, RIGHT);
        tree.find_mut(1).unwrap().split(2, DOWN);
        assert!(tree.remove(0));
        let mut order = Vec::new();
        tree.leaves(&mut order);
        assert_eq!(order, [1, 2]);
        assert!(matches!(&tree, Node::Split(s) if !s.horizontal));
        assert!(!Node::Leaf(3).remove(3));
    }

    #[test]
    fn resize_moves_the_nearest_split_of_the_orientation() {
        // [0 | [1 / 2]]: pane 2's nearest horizontal split is the root.
        let mut tree = Node::Leaf(0);
        tree.split(1, RIGHT);
        tree.find_mut(1).unwrap().split(2, DOWN);
        assert!(tree.resize(2, true, -50.0 / 1276.0));
        let Node::Split(root) = &tree else { panic!() };
        assert_eq!(root.ratio, f16(0.5 + f16(-50.0 / 1276.0)));
        // The vertical split spans the whole height: scale 1.
        assert!(tree.resize(2, false, 30.0 / 796.0));
        let Node::Split(root) = &tree else { panic!() };
        let Node::Split(inner) = &root.second else { panic!() };
        assert_eq!(inner.ratio, f16(0.5 + f16(30.0 / 796.0)));
        // No vertical split above pane 0.
        assert!(!tree.resize(0, false, 0.1));
    }

    #[test]
    fn resize_scales_by_the_splits_share_of_the_tree() {
        // [[0 | 2] | 1]: the inner split spans half the width, so the same
        // delta moves its ratio twice as far.
        let mut tree = Node::Leaf(0);
        tree.split(1, RIGHT);
        tree.find_mut(0).unwrap().split(2, RIGHT);
        assert!(tree.resize(2, true, 50.0 / 1276.0));
        let Node::Split(root) = &tree else { panic!() };
        let Node::Split(inner) = &root.first else { panic!() };
        assert_eq!(inner.ratio, f16(0.5 + f16(f16(50.0 / 1276.0) / 0.5)));
    }

    #[test]
    fn equalize_weighs_leaves_along_the_split() {
        let mut tree = Node::Leaf(0);
        tree.split(1, RIGHT);
        tree.find_mut(1).unwrap().split(2, RIGHT);
        tree.equalize();
        let Node::Split(root) = &tree else { panic!() };
        assert_eq!(root.ratio, f16(1.0 / 3.0));
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
        assert_eq!(tree.split_at(1).map(|s| s.horizontal), Some(false));
    }
}
