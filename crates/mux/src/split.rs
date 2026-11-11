//! The panes of a window as ghostty's split tree (`src/datastruct/split_tree.zig`):
//! a binary tree whose splits keep an orientation and a ratio. Pixel sizes
//! come from laying the tree out like ghostty's nested GtkPaned widgets: each
//! split also keeps the state of its GtkPaned (`gtk/gtkpaned.c` in GTK 4.22),
//! which places the divider from its previous size, and ghostty moves the
//! divider back to the ratio only when it strays more than 0.001.

use crate::{Divider, Metrics, PaneId, Rect};

#[derive(Clone, Debug)]
pub(crate) enum Node {
    Leaf(PaneId),
    Split(Box<Split>),
}

#[derive(Clone, Debug)]
pub(crate) struct Split {
    /// Children side by side (ghostty's `horizontal` layout) or stacked.
    pub horizontal: bool,
    /// The first child's share of the space between the children; always a
    /// binary16 value, since ghostty keeps ratios as `f16`.
    pub ratio: f64,
    pub first: Node,
    pub second: Node,
    paned: Paned,
}

/// What a GtkPaned keeps between allocations. ghostty creates new ones
/// whenever the tree changes, so [`Node::rebuild`] starts them over.
#[derive(Clone, Copy, Debug)]
struct Paned {
    /// Set by `gtk_paned_set_position`; until then the position follows the
    /// children's minimum sizes.
    position_set: bool,
    /// `start_child_size`: the first child's size.
    position: i32,
    last_allocation: i32,
    min: i32,
    max: i32,
    /// The ratio ghostty's split widget aims for (`priv.ratio`): the tree's
    /// when built, then what it last wrote to the tree, unrounded.
    ratio: f64,
    /// The position changed by a drag, for ghostty's idle sync.
    moved: bool,
    /// The last allocation: the split's rectangle and its divider's.
    rect: Rect,
    handle: Rect,
}

impl Paned {
    /// `gtk_paned_init`, for a split widget built from a tree with `ratio`.
    fn new(ratio: f64) -> Paned {
        Paned {
            position_set: false,
            position: 0,
            last_allocation: -1,
            min: 0,
            max: i32::MAX,
            ratio,
            moved: false,
            rect: Rect::default(),
            handle: Rect::default(),
        }
    }
}

/// A divider grabbed by the pointer: the split's place among the dividers
/// (as [`crate::View::dividers`] lists them) and where in it the pointer is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Grab {
    pub index: usize,
    /// `drag_pos`: the pointer's distance from the divider's start.
    offset: i32,
}

/// What the pointer is over, in the order GTK picks widgets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hit {
    /// A divider's handle (6 pixels around it on the first pane's side).
    Divider { horizontal: bool },
    /// The drag handle on top of a pane of a split window.
    DragHandle(PaneId),
    Pane(PaneId),
}

/// ghostty's drag handle is a band this many logical pixels high (its 16px icon).
pub const DRAG_HANDLE_HEIGHT: u32 = 16;
/// GtkPaned's `HANDLE_EXTRA_SIZE`.
const HANDLE_EXTRA: f64 = 6.0;

/// A pane at `rect` under `point`; with `split`, its top band is the drag handle.
pub(crate) fn leaf_hit(wp: PaneId, rect: Rect, point: (f64, f64), split: bool) -> Option<Hit> {
    if !contains(rect, point, 0.0) {
        return None;
    }
    let band = Rect { h: DRAG_HANDLE_HEIGHT.min(rect.h), ..rect };
    Some(if split && contains(band, point, 0.0) { Hit::DragHandle(wp) } else { Hit::Pane(wp) })
}

fn contains(r: Rect, (x, y): (f64, f64), pad: f64) -> bool {
    x >= f64::from(r.x) - pad && x < f64::from(r.x + r.w) + pad && y >= f64::from(r.y) - pad && y < f64::from(r.y + r.h) + pad
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

/// Rounds to the nearest IEEE binary16 value, ties to even. ghostty computes
/// ratios in `f16`, where every operation rounds its result this way; an
/// operation done in `f64` and then rounded gives the same value.
pub(crate) fn f16(x: f64) -> f64 {
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

impl Split {
    /// `gtk_paned_size_allocate`: places the divider for `rect` and returns
    /// the children's rectangles and the divider's.
    fn allocate(&mut self, rect: Rect, m: &Metrics) -> (Rect, Rect, Rect) {
        let h = self.horizontal;
        self.paned.rect = rect;
        let (start_min, end_min) = (self.first.min_size(h, m) as i32, self.second.min_size(h, m) as i32);
        let (total, handle) = (extent(rect, h) as i32, m.divider as i32);
        self.calc_position((total - handle).max(1), start_min, end_min);
        let pos = self.paned.position;
        // The position never drops below the first child's minimum (it is
        // the minimum position), so only the second child can overflow; GTK
        // then gives it its minimum anyway and lets it stick out.
        let first = pos.max(1) as u32;
        let second = (total - pos - handle).max(1).max(end_min) as u32;
        let (pos, handle) = (pos as u32, m.divider);
        let children = if h {
            (
                Rect { w: first, ..rect },
                Rect { x: rect.x + pos, w: handle, ..rect },
                Rect { x: rect.x + pos + handle, w: second, ..rect },
            )
        } else {
            (
                Rect { h: first, ..rect },
                Rect { y: rect.y + pos, h: handle, ..rect },
                Rect { y: rect.y + pos + handle, h: second, ..rect },
            )
        };
        self.paned.handle = children.1;
        children
    }

    /// `gtk_paned_calc_position` with `shrink-*-child` false and both
    /// `resize-*-child` true, followed by ghostty's tree-to-widget sync when
    /// the minimum or maximum position changed.
    fn calc_position(&mut self, allocation: i32, start_min: i32, end_min: i32) {
        let p = &mut self.paned;
        let min = start_min;
        let max = (allocation - end_min).max(1).max(min);
        // C converts the double to int by truncation, as `as` does.
        let pos = if !p.position_set {
            let share = if start_min + end_min != 0 { f64::from(start_min) / f64::from(start_min + end_min) } else { 0.5 };
            (f64::from(allocation) * share + 0.5) as i32
        } else if p.last_allocation > 0 {
            (f64::from(allocation) * (f64::from(p.position) / f64::from(p.last_allocation)) + 0.5) as i32
        } else {
            p.position
        };
        let limits_changed = (min, max) != (p.min, p.max);
        (p.position, p.min, p.max) = (pos.clamp(min, max), min, max);
        if limits_changed && let Some((_, clamped, _)) = self.sync(end_min) {
            // gtk_paned_set_position
            self.paned.position = clamped;
            self.paned.position_set = true;
        }
        self.paned.last_allocation = allocation;
    }

    /// ghostty's syncSplitRatio: when the shown position strays from the
    /// ratio by more than 0.001, the position the ratio asks for, that
    /// position clamped to the GtkPaned's limits, and the size the ratio
    /// applies to.
    fn sync(&self, end_min: i32) -> Option<(i32, i32, i32)> {
        let p = &self.paned;
        if p.max == i32::MAX {
            return None;
        }
        let size = p.max + end_min;
        if size == 0 || (f64::from(p.position) / f64::from(size) - p.ratio).abs() <= 0.001 {
            return None;
        }
        let desired = (f64::from(size) * p.ratio).round() as i32;
        Some((desired, desired.clamp(p.min, p.min.max(p.max)), size))
    }

    /// ghostty's setTreeRatio: the tree keeps the ratio as f16, the widget unrounded.
    fn set_tree_ratio(&mut self, ratio: f64) {
        self.ratio = f16(ratio);
        self.paned.ratio = ratio;
    }
}

impl Node {
    fn split_node(horizontal: bool, first: Node, second: Node) -> Node {
        Node::Split(Box::new(Split { horizontal, ratio: 0.5, first, second, paned: Paned::new(0.5) }))
    }

    /// Starts every GtkPaned over, as ghostty's rebuild of the widget tree
    /// after each change of the tree does.
    pub(crate) fn rebuild(&mut self) {
        if let Node::Split(s) = self {
            s.paned = Paned::new(s.ratio);
            s.first.rebuild();
            s.second.rebuild();
        }
    }

    fn split_count(&self) -> usize {
        match self {
            Node::Leaf(_) => 0,
            Node::Split(s) => 1 + s.first.split_count() + s.second.split_count(),
        }
    }

    /// GtkPaned's pointer gesture works in the capture phase, so the
    /// outermost split whose handle area (6 pixels around the divider)
    /// holds a press takes it, on either side of the divider.
    pub(crate) fn grab(&self, point: (f64, f64)) -> Option<Grab> {
        self.grab_in(point, 0)
    }

    fn grab_in(&self, point: (f64, f64), before: usize) -> Option<Grab> {
        let Node::Split(s) = self else { return None };
        let index = before + s.first.split_count();
        let (p, h) = (&s.paned, s.paned.handle);
        if contains(p.rect, point, 0.0) && contains(h, point, HANDLE_EXTRA) {
            let start = if s.horizontal { point.0 - f64::from(h.x) } else { point.1 - f64::from(h.y) };
            // drag_pos is an int measured from the widened area's start.
            let offset = (start + HANDLE_EXTRA) as i32 - HANDLE_EXTRA as i32;
            return Some(Grab { index, offset });
        }
        match s.first.grab_in(point, before) {
            Some(grab) => Some(grab),
            None => s.second.grab_in(point, index + 1),
        }
    }

    fn split_at(&mut self, index: usize) -> Option<&mut Split> {
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

    /// GtkPaned's update_drag for the pointer at `point`: the divider keeps
    /// the grab offset and stays between the children's minimums.
    pub(crate) fn drag(&mut self, grab: Grab, point: (f64, f64)) {
        let Some(s) = self.split_at(grab.index) else { return };
        let p = &mut s.paned;
        // The pointer in the paned's coordinates, converted to int by C.
        let xpos = if s.horizontal { point.0 - f64::from(p.rect.x) } else { point.1 - f64::from(p.rect.y) };
        let size = (xpos as i32 - grab.offset).clamp(p.min, p.max);
        if size != p.position {
            p.position = size;
            p.position_set = true;
            p.moved = true;
        }
    }

    /// ghostty's idle widget-to-tree sync after drags: a divider that ended
    /// more than 0.001 away from its ratio writes its ratio to the tree.
    pub(crate) fn sync_dragged(&mut self, m: &Metrics) {
        if let Node::Split(s) = self {
            if std::mem::take(&mut s.paned.moved)
                && let Some((_, _, size)) = s.sync(s.second.min_size(s.horizontal, m) as i32)
            {
                s.set_tree_ratio(f64::from(s.paned.position) / f64::from(size));
            }
            s.first.sync_dragged(m);
            s.second.sync_dragged(m);
        }
    }

    /// The widget GTK picks at `point`: a split's second child lies above its
    /// divider handle, which lies above its first child; the drag handle of a
    /// pane in a split window lies above the pane.
    pub(crate) fn hit(&self, point: (f64, f64), split: bool, rect_of: &impl Fn(PaneId) -> Rect) -> Option<Hit> {
        match self {
            Node::Leaf(wp) => leaf_hit(*wp, rect_of(*wp), point, split),
            Node::Split(s) => {
                let p = &s.paned;
                if !contains(p.rect, point, 0.0) {
                    return None;
                }
                if let Some(hit) = s.second.hit(point, split, rect_of) {
                    return Some(hit);
                }
                if contains(p.handle, point, HANDLE_EXTRA) {
                    return Some(Hit::Divider { horizontal: s.horizontal });
                }
                s.first.hit(point, split, rect_of)
            }
        }
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

    fn weight(&self, horizontal: bool) -> u32 {
        match self {
            Node::Split(s) if s.horizontal == horizontal => s.first.weight(horizontal) + s.second.weight(horizontal),
            _ => 1,
        }
    }

    /// The smallest size along an axis, as GtkPaned measures it.
    pub(crate) fn min_size(&self, horizontal: bool, m: &Metrics) -> u32 {
        match self {
            Node::Leaf(_) if horizontal => m.min_width,
            Node::Leaf(_) => m.min_height,
            Node::Split(s) => {
                let (a, b) = (s.first.min_size(horizontal, m), s.second.min_size(horizontal, m));
                if s.horizontal == horizontal { a + m.divider + b } else { a.max(b) }
            }
        }
    }

    /// Allocates `rect` to the tree, appending each pane's rectangle and each divider.
    pub(crate) fn layout(&mut self, rect: Rect, m: &Metrics, panes: &mut Vec<(PaneId, Rect)>, dividers: &mut Vec<Divider>) {
        match self {
            Node::Leaf(wp) => panes.push((*wp, rect)),
            Node::Split(s) => {
                let (first, divider, second) = s.allocate(rect, m);
                s.first.layout(first, m, panes, dividers);
                dividers.push(Divider { rect: divider, after: panes.len(), horizontal: s.horizontal });
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
            // GTK computes the delta in f64; the tree takes it as f16.
            let delta = f16((f64::from(amount) / total).clamp(-1.0, 1.0));
            let root = f16(f64::from(self.units(horizontal)));
            self.resize_in(wp, horizontal, delta, root, root, m);
        }
    }

    /// `size` is this node's extent in ghostty's spatial representation: the
    /// root spans `root` leaf units and every split hands its children
    /// `size * ratio` and `size * (1 - ratio)`, each rounded to f16.
    fn resize_in(&mut self, wp: PaneId, horizontal: bool, delta: f64, size: f64, root: f64, m: &Metrics) -> Resize {
        let s = match self {
            Node::Leaf(p) => return if *p == wp { Resize::Pending } else { Resize::NotHere },
            Node::Split(s) => s,
        };
        let along = s.horizontal == horizontal;
        let (first_size, second_size) =
            if along { (f16(size * s.ratio), f16(size * f16(1.0 - s.ratio))) } else { (size, size) };
        let found = match s.first.resize_in(wp, horizontal, delta, first_size, root, m) {
            Resize::NotHere => s.second.resize_in(wp, horizontal, delta, second_size, root, m),
            found => found,
        };
        if !matches!(found, Resize::Pending) || !along {
            return found;
        }
        // A split held at a child's minimum size resizes from the ratio it
        // shows, not the one it keeps for when it grows again (ghostty's
        // clamped_to_tree sync).
        if let Some((desired, clamped, size)) = s.sync(s.second.min_size(horizontal, m) as i32)
            && clamped != desired
        {
            s.set_tree_ratio(f64::from(clamped) / f64::from(size));
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

    const M: Metrics =
        Metrics {
        width: 1276,
        height: 796,
        top: 0,
        divider: 1,
        min_width: 14,
        min_height: 24,
        scale: 1.0,
        cell_width: 10,
        cell_height: 20,
        padding_x: 2,
        padding_y: 2,
    };
    const AREA: Rect = Rect { x: 0, y: 0, w: 1276, h: 796 };
    const RIGHT: Side = Side { horizontal: true, before: false };
    const DOWN: Side = Side { horizontal: false, before: false };

    /// A layout by new GtkPaned widgets, as after each change of the tree.
    fn layout(tree: &mut Node) -> (Vec<(PaneId, Rect)>, Vec<Divider>) {
        tree.rebuild();
        let (mut panes, mut dividers) = (Vec::new(), Vec::new());
        tree.layout(AREA, &M, &mut panes, &mut dividers);
        (panes, dividers)
    }

    fn rect(x: u32, y: u32, w: u32, h: u32) -> Rect {
        Rect { x, y, w, h }
    }

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
    fn halves_around_a_one_pixel_divider() {
        let mut tree = Node::Leaf(0);
        tree.split(1, RIGHT);
        let (panes, dividers) = layout(&mut tree);
        // GtkPaned: round((1276 - 1) * 0.5) = 638 for the first child.
        assert_eq!(panes, vec![(0, rect(0, 0, 638, 796)), (1, rect(639, 0, 637, 796))]);
        assert_eq!(dividers, vec![Divider { rect: rect(638, 0, 1, 796), after: 1, horizontal: true }]);
    }

    #[test]
    fn removing_gives_the_parent_to_the_sibling_subtree() {
        let mut tree = Node::Leaf(0);
        tree.split(1, RIGHT);
        tree.find_mut(1).unwrap().split(2, DOWN);
        assert!(tree.remove(0));
        // The stacked pair now fills the window.
        let (panes, _) = layout(&mut tree);
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
        let (panes, _) = layout(&mut tree);
        assert_eq!(panes[0].1.w, 588);
        // The vertical split moves by 30 pixels of the whole height.
        tree.resize(2, false, 30, AREA, &M);
        let (panes, _) = layout(&mut tree);
        assert_eq!(panes[1].1.h, 427);
    }

    #[test]
    fn minimum_sizes_hold_and_resize_starts_from_the_shown_ratio() {
        let mut tree = Node::Leaf(0);
        tree.split(1, RIGHT);
        tree.resize(0, true, -2000, AREA, &M);
        let (panes, _) = layout(&mut tree);
        // One cell plus the padding on both sides.
        assert_eq!(panes[0].1.w, 14);
        tree.resize(0, true, 10, AREA, &M);
        let (panes, _) = layout(&mut tree);
        assert_eq!(panes[0].1.w, 24);
    }

    #[test]
    fn equalize_weighs_leaves_along_the_split() {
        let mut tree = Node::Leaf(0);
        tree.split(1, RIGHT);
        tree.find_mut(1).unwrap().split(2, RIGHT);
        tree.equalize();
        let (panes, _) = layout(&mut tree);
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
