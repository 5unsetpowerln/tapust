//! Windows and panes: tmux 3.6's `window.c` bookkeeping (pane order, the
//! last-pane stack, zoom, directional selection) over ghostty's split tree.

use crate::split::{Node, Side};
use crate::{Divider, Metrics, PaneId, Rect};

#[derive(Clone, Debug)]
pub(crate) struct Pane {
    pub id: PaneId,
    /// The last laid-out position in pixels, relative to the window area;
    /// hidden panes of a zoomed window keep the one they had before the zoom.
    pub rect: Rect,
    /// The terminal grid inside the padding.
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
    pub metrics: Metrics,
    /// What the tab view allocates to the window's tree: the pane area, or
    /// more when the window cannot be that small.
    pub area: Rect,
    /// `w->panes`, kept in the tree's order; a pane's index is its position.
    pub panes: Vec<Pane>,
    /// `w->last_panes`, most recent first.
    pub last_panes: Vec<PaneId>,
    pub active: PaneId,
    pub tree: Node,
    /// The dividers of the last layout.
    pub dividers: Vec<Divider>,
    pub zoomed: bool,
    pub was_zoomed: bool,
    /// Set by rename-window, which turns automatic-rename off.
    pub fixed_name: Option<String>,
}

impl Window {
    pub(crate) fn new(id: u32, metrics: Metrics, pane: Pane) -> Window {
        let active = pane.id;
        let mut w = Window {
            id,
            metrics,
            area: metrics.area(),
            panes: vec![pane],
            last_panes: Vec::new(),
            active,
            tree: Node::Leaf(active),
            dividers: Vec::new(),
            zoomed: false,
            was_zoomed: false,
            fixed_name: None,
        };
        w.relayout();
        w
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

    /// Lays the panes out for the window's metrics: the whole area for the
    /// zoomed pane, else the split tree. Only the current window is laid out,
    /// as libadwaita allocates only the selected tab.
    pub(crate) fn relayout(&mut self) {
        let (m, area) = (self.metrics, self.area);
        let mut placed = Vec::new();
        self.dividers.clear();
        if self.zoomed {
            placed.push((self.active, area));
        } else {
            self.tree.layout(area, &m, &mut placed, &mut self.dividers);
        }
        for (wp, rect) in placed {
            let (cols, rows) = m.grid(rect);
            let p = self.pane_mut(wp);
            p.rect = rect;
            p.cols = cols;
            p.rows = rows;
        }
    }

    /// After every change of the tree: ghostty builds new GtkPaned widgets,
    /// and `panes` goes back to the tree's order.
    fn tree_changed(&mut self) {
        self.tree.rebuild();
        let mut order = Vec::with_capacity(self.panes.len());
        self.tree.leaves(&mut order);
        self.panes.sort_by_key(|p| order.iter().position(|&wp| wp == p.id));
    }

    /// window_add_pane and layout_split_pane: `pane` splits `at` (or the
    /// whole window when `full`) on `side`.
    pub(crate) fn add_pane(&mut self, pane: Pane, at: PaneId, side: Side, full: bool) {
        let new = pane.id;
        self.panes.push(pane);
        let node = if full { &mut self.tree } else { self.tree.find_mut(at).expect("pane in tree") };
        node.split(new, side);
        self.tree_changed();
    }

    /// Exchanges the places of two panes, in the tree and in the order.
    pub(crate) fn swap_panes(&mut self, a: PaneId, b: PaneId) {
        self.tree.swap(a, b);
        self.tree_changed();
    }

    /// Gives the tree's places to the panes in their current order (rotate-window).
    pub(crate) fn place_in_order(&mut self) {
        let order: Vec<PaneId> = self.panes.iter().map(|p| p.id).collect();
        self.tree.assign(&mut order.into_iter());
        self.tree_changed();
    }

    /// ghostty's equalize_splits.
    pub(crate) fn equalize(&mut self) {
        self.tree.equalize();
        self.tree_changed();
    }

    /// ghostty's resize_split for the active pane, by `amount` logical pixels.
    pub(crate) fn resize_split(&mut self, horizontal: bool, amount: i32) {
        let (m, area) = (self.metrics, self.area);
        let wp = self.active;
        self.tree.resize(wp, horizontal, amount, area, &m);
        self.tree_changed();
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
        let pane = self.panes.remove(pos);
        self.tree_changed();
        pane
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
        self.tree_changed();
        self.relayout();
        true
    }

    /// window_unzoom
    pub(crate) fn unzoom(&mut self) -> bool {
        if !self.zoomed {
            return false;
        }
        self.zoomed = false;
        self.tree_changed();
        self.relayout();
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

    /// window_pane_visible
    pub(crate) fn visible(&self, wp: PaneId) -> bool {
        !self.zoomed || wp == self.active
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
        let (d, area) = (self.metrics.divider, self.area);
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
