//! Windows and panes: tmux 3.6's `window.c` bookkeeping (pane order, the
//! last-pane stack, zoom, directional selection) over ghostty's split tree.

use crate::layout::{self, Layout};
use crate::split::{Node, Side};
use crate::{PaneId, Rect};

#[derive(Clone, Debug)]
pub(crate) struct Pane {
    pub id: PaneId,
    /// Where GTK last allocated the pane, in logical pixels of the tree's
    /// area; hidden panes of a zoomed window keep the one they had.
    pub rect: Rect,
    /// The terminal grid GTK's allocation holds.
    pub cols: u16,
    pub rows: u16,
    pub active_point: u32,
    pub title: String,
    /// The foreground process name reported by the GUI (automatic-rename).
    pub auto_name: String,
}

impl Pane {
    pub(crate) fn new(id: PaneId) -> Pane {
        Pane {
            id,
            rect: Rect::default(),
            cols: 0,
            rows: 0,
            active_point: 0,
            title: String::new(),
            auto_name: String::new(),
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct Window {
    /// tmux `@N`, never reused within a session.
    pub id: u32,
    /// The size GTK gives the window's split tree, in logical pixels.
    pub area: Rect,
    /// tmux's window size (`w->sx`, `w->sy`), which the layouts divide: the
    /// grid the tree's area holds in the active pane's cells, tmux's
    /// default-size until GTK reports one.
    pub size: (u32, u32),
    /// `w->lastlayout`.
    pub last_layout: Option<Layout>,
    /// `w->panes`; a pane's index is its position. Splits keep it in the
    /// tree's order, but the mirrored layouts and rotate-window do not.
    pub panes: Vec<Pane>,
    /// `w->last_panes`, most recent first.
    pub last_panes: Vec<PaneId>,
    pub active: PaneId,
    pub tree: Node,
    pub zoomed: bool,
    pub was_zoomed: bool,
    /// Set by rename-window, which turns automatic-rename off.
    pub fixed_name: Option<String>,
}

impl Window {
    pub(crate) fn new(id: u32, pane: Pane) -> Window {
        let active = pane.id;
        Window {
            id,
            area: Rect::default(),
            size: (80, 24),
            last_layout: None,
            panes: vec![pane],
            last_panes: Vec::new(),
            active,
            tree: Node::Leaf(active),
            zoomed: false,
            was_zoomed: false,
            fixed_name: None,
        }
    }

    pub(crate) fn pos(&self, wp: PaneId) -> Option<usize> {
        self.panes.iter().position(|p| p.id == wp)
    }

    pub(crate) fn pane(&self, wp: PaneId) -> &Pane {
        &self.panes[self.pos(wp).expect("pane in window")]
    }

    pub(crate) fn pane_mut(&mut self, wp: PaneId) -> &mut Pane {
        let pos = self.pos(wp).expect("pane in window");
        &mut self.panes[pos]
    }

    pub(crate) fn pane_mut_opt(&mut self, wp: PaneId) -> Option<&mut Pane> {
        self.panes.iter_mut().find(|p| p.id == wp)
    }

    pub(crate) fn has_pane(&self, wp: PaneId) -> bool {
        self.pos(wp).is_some()
    }

    /// The window name: the fixed one, or the active pane's process name.
    pub(crate) fn name(&self) -> &str {
        match &self.fixed_name {
            Some(name) => name,
            None => &self.pane(self.active).auto_name,
        }
    }

    pub(crate) fn next_of(&self, wp: PaneId) -> Option<PaneId> {
        let pos = self.pos(wp)?;
        self.panes.get(pos + 1).map(|p| p.id)
    }

    pub(crate) fn prev_of(&self, wp: PaneId) -> Option<PaneId> {
        let pos = self.pos(wp)?;
        pos.checked_sub(1).map(|i| self.panes[i].id)
    }

    /// window_add_pane and layout_split_pane: `pane` splits `at` (or the
    /// whole window when `full`) on `side`.
    pub(crate) fn add_pane(&mut self, pane: Pane, at: PaneId, side: Side, full: bool) {
        let new = pane.id;
        let pos = match (full, side.before) {
            (true, true) => 0,
            (true, false) => self.panes.len(),
            (false, before) => self.pos(at).expect("pane in window") + usize::from(!before),
        };
        self.panes.insert(pos, pane);
        let node = if full { &mut self.tree } else { self.tree.find_mut(at).expect("pane in tree") };
        node.split(new, side);
    }

    /// Exchanges the places of two panes, in the tree and in the order.
    pub(crate) fn swap_panes(&mut self, a: PaneId, b: PaneId) {
        self.tree.relabel(&|p| if p == a { b } else if p == b { a } else { p });
        let (i, j) = (self.pos(a).expect("pane in window"), self.pos(b).expect("pane in window"));
        self.panes.swap(i, j);
    }

    /// rotate-window: the panes move one place along the order, up (to the
    /// end for the first) or `down`, and each takes the place in the tree
    /// of the pane that had its position in the order.
    pub(crate) fn rotate(&mut self, down: bool) {
        let old: Vec<PaneId> = self.panes.iter().map(|p| p.id).collect();
        if down {
            self.panes.rotate_right(1);
        } else {
            self.panes.rotate_left(1);
        }
        let new: Vec<PaneId> = self.panes.iter().map(|p| p.id).collect();
        self.tree.relabel(&|p| new[old.iter().position(|&q| q == p).expect("pane in window")]);
    }

    /// layout_set_select: the preset for the panes in their order.
    pub(crate) fn select_layout(&mut self, layout: Layout) {
        let order: Vec<PaneId> = self.panes.iter().map(|p| p.id).collect();
        if let Some(tree) = layout.arrange(&order, self.size) {
            self.tree = tree;
        }
        self.last_layout = Some(layout);
    }

    /// layout_spread_out for the active pane.
    pub(crate) fn spread_out(&mut self) -> Result<(), String> {
        layout::spread_out(&mut self.tree, self.active, self.size)
    }

    /// ghostty's resize_split for pane `wp`, by `delta` of the tree's extent.
    pub(crate) fn resize_split(&mut self, wp: PaneId, horizontal: bool, delta: f64) -> bool {
        self.tree.resize(wp, horizontal, delta)
    }

    /// ghostty's moveSplit: `source` leaves its place (its sibling takes it)
    /// and splits `target` on `side` with the ratio 0.5. In the order it
    /// goes next to `target`, as join-pane puts it.
    pub(crate) fn move_pane(&mut self, source: PaneId, target: PaneId, side: Side) {
        self.tree.remove(source);
        self.tree.find_mut(target).expect("pane in tree").split(source, side);
        let pane = self.panes.remove(self.pos(source).expect("pane in window"));
        let pos = self.pos(target).expect("pane in window") + usize::from(!side.before);
        self.panes.insert(pos, pane);
    }

    /// window_pane_stack_remove
    pub(crate) fn stack_remove(&mut self, wp: PaneId) {
        self.last_panes.retain(|&p| p != wp);
    }

    /// window_pane_stack_push
    fn stack_push(&mut self, wp: PaneId) {
        self.stack_remove(wp);
        self.last_panes.insert(0, wp);
    }

    /// window_set_active_pane; `next_point` is tmux's global `next_active_point`.
    pub(crate) fn set_active(&mut self, wp: PaneId, next_point: &mut u32) -> bool {
        if wp == self.active {
            return false;
        }
        let last = self.active;
        self.stack_remove(wp);
        if self.has_pane(last) {
            self.stack_push(last);
        }
        self.active = wp;
        self.pane_mut(wp).active_point = *next_point;
        *next_point = next_point.wrapping_add(1);
        true
    }

    /// window_lost_pane; `prev`/`next` are the pane's neighbours in the list.
    fn lost_pane(&mut self, wp: PaneId, prev: Option<PaneId>, next: Option<PaneId>) {
        self.stack_remove(wp);
        if wp == self.active
            && let Some(new) = self.last_panes.first().copied().or(prev).or(next)
        {
            self.active = new;
            self.stack_remove(new);
        }
    }

    /// window_remove_pane: its sibling in the tree takes its place.
    pub(crate) fn remove_pane(&mut self, wp: PaneId) -> Pane {
        let (prev, next) = (self.prev_of(wp), self.next_of(wp));
        self.lost_pane(wp, prev, next);
        self.tree.remove(wp);
        let pos = self.pos(wp).expect("pane in window");
        self.panes.remove(pos)
    }

    /// window_zoom
    pub(crate) fn zoom(&mut self, wp: PaneId, next_point: &mut u32) -> bool {
        if self.zoomed || self.panes.len() == 1 {
            return false;
        }
        if self.active != wp {
            self.set_active(wp, next_point);
        }
        self.zoomed = true;
        true
    }

    /// window_unzoom
    pub(crate) fn unzoom(&mut self) -> bool {
        if !self.zoomed {
            return false;
        }
        self.zoomed = false;
        true
    }

    /// window_push_zoom
    pub(crate) fn push_zoom(&mut self, always: bool, flag: bool) -> bool {
        self.was_zoomed = flag && (always || self.zoomed);
        self.unzoom()
    }

    /// window_pop_zoom
    pub(crate) fn pop_zoom(&mut self, next_point: &mut u32) -> bool {
        if self.was_zoomed {
            let active = self.active;
            return self.zoom(active, next_point);
        }
        false
    }

    /// window_pane_next_by_number
    pub(crate) fn next_by_number(&self, wp: PaneId, n: u32) -> PaneId {
        let mut pos = self.pos(wp).expect("pane in window");
        for _ in 0..n {
            pos = (pos + 1) % self.panes.len();
        }
        self.panes[pos].id
    }

    /// window_pane_choose_best: the most recently active, first on ties.
    fn choose_best(&self, list: &[usize]) -> Option<PaneId> {
        let mut best = *list.first()?;
        for &next in &list[1..] {
            if self.panes[next].active_point > self.panes[best].active_point {
                best = next;
            }
        }
        Some(self.panes[best].id)
    }

    /// Whether `[start, end]` touches the span `[low, high]` as in tmux's
    /// window_pane_find_* functions.
    fn overlaps(start: u32, end: u32, low: u32, high: u32) -> bool {
        (start < low && end > high) || (start >= low && start <= high) || (end >= low && end <= high)
    }

    /// The panes passing `filter`, as tmux's window_pane_find_* collect them.
    fn candidates(&self, wp: PaneId, filter: impl Fn(&Rect) -> bool) -> Option<PaneId> {
        let list: Vec<usize> =
            (0..self.panes.len()).filter(|&i| self.panes[i].id != wp && filter(&self.panes[i].rect)).collect();
        self.choose_best(&list)
    }

    /// window_pane_find_*: the panes across the divider on that side that
    /// overlap the pane, wrapping around at the window's edge. tmux's
    /// one-cell border is the divider's width here.
    pub(crate) fn find(&self, wp: PaneId, dir: crate::Direction) -> Option<PaneId> {
        use crate::Direction::{Down, Left, Right, Up};
        let p = self.pane(wp).rect;
        // GtkPaned's separator is one logical pixel.
        let (d, area) = (1, self.area);
        let across = |n: &Rect| Self::overlaps(n.y, n.y + n.h.saturating_sub(1), p.y, p.y + p.h);
        let along = |n: &Rect| Self::overlaps(n.x, n.x + n.w.saturating_sub(1), p.x, p.x + p.w);
        match dir {
            Left => {
                let edge = if p.x == 0 { area.w + d } else { p.x };
                self.candidates(wp, |n| n.x + n.w + d == edge && across(n))
            }
            Right => {
                let edge = if p.x + p.w + d >= area.w { 0 } else { p.x + p.w + d };
                self.candidates(wp, |n| n.x == edge && across(n))
            }
            Up => {
                let edge = if p.y == 0 { area.h + d } else { p.y };
                self.candidates(wp, |n| n.y + n.h + d == edge && along(n))
            }
            Down => {
                let edge = if p.y + p.h + d >= area.h { 0 } else { p.y + p.h + d };
                self.candidates(wp, |n| n.y == edge && along(n))
            }
        }
    }
}
