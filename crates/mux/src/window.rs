//! Windows and panes, ported from tmux 3.6 `window.c`.

use crate::layout::{Cell, CellId};
use crate::PaneId;

#[derive(Clone, Debug)]
pub(crate) struct Pane {
    pub id: PaneId,
    pub xoff: u32,
    pub yoff: u32,
    pub sx: u32,
    pub sy: u32,
    pub cell: Option<CellId>,
    pub saved_cell: Option<CellId>,
    pub active_point: u32,
    pub title: String,
    /// The foreground process name reported by the GUI (automatic-rename).
    pub auto_name: String,
}

impl Pane {
    pub(crate) fn new(id: PaneId, sx: u32, sy: u32) -> Pane {
        Pane {
            id,
            xoff: 0,
            yoff: 0,
            sx,
            sy,
            cell: None,
            saved_cell: None,
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
    pub sx: u32,
    pub sy: u32,
    /// `w->panes` in order; a pane's index is its position.
    pub panes: Vec<Pane>,
    /// `w->last_panes`, most recent first.
    pub last_panes: Vec<PaneId>,
    pub active: PaneId,
    pub cells: Vec<Option<Cell>>,
    pub root: Option<CellId>,
    pub saved_root: Option<CellId>,
    pub zoomed: bool,
    pub was_zoomed: bool,
    /// Set by rename-window, which turns automatic-rename off.
    pub fixed_name: Option<String>,
    /// `WINDOW_RESIZE` with `new_sx`/`new_sy`: applied when the window is current.
    pub pending_resize: Option<(u32, u32)>,
    /// Set when layout_resize_adjust could not shrink as asked, where tmux
    /// would loop forever.
    pub stuck: bool,
}

impl Window {
    pub(crate) fn new(id: u32, sx: u32, sy: u32, active: PaneId) -> Window {
        Window {
            id,
            sx,
            sy,
            panes: Vec::new(),
            last_panes: Vec::new(),
            active,
            cells: Vec::new(),
            root: None,
            saved_root: None,
            zoomed: false,
            was_zoomed: false,
            fixed_name: None,
            pending_resize: None,
            stuck: false,
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
    pub(crate) fn lost_pane(&mut self, wp: PaneId, prev: Option<PaneId>, next: Option<PaneId>) {
        self.stack_remove(wp);
        if wp == self.active
            && let Some(new) = self.last_panes.first().copied().or(prev).or(next)
        {
            self.active = new;
            self.stack_remove(new);
        }
    }

    /// window_remove_pane (the layout cell must already be closed).
    pub(crate) fn remove_pane(&mut self, wp: PaneId) -> Pane {
        let (prev, next) = (self.prev_of(wp), self.next_of(wp));
        self.lost_pane(wp, prev, next);
        let pos = self.pos(wp).expect("pane in window");
        self.panes.remove(pos)
    }

    /// window_add_pane: insert a new pane next to `other`.
    pub(crate) fn add_pane(&mut self, pane: Pane, other: PaneId, before: bool, full: bool) {
        if self.panes.is_empty() {
            self.panes.push(pane);
            return;
        }
        let pos = self.pos(other).expect("pane in window");
        let at = match (before, full) {
            (true, true) => 0,
            (true, false) => pos,
            (false, true) => self.panes.len(),
            (false, false) => pos + 1,
        };
        self.panes.insert(at, pane);
    }

    /// window_zoom
    pub(crate) fn zoom(&mut self, wp: PaneId, next_point: &mut u32) -> bool {
        if self.zoomed || self.panes.len() == 1 {
            return false;
        }
        if self.active != wp {
            self.set_active(wp, next_point);
        }
        for p in &mut self.panes {
            p.saved_cell = p.cell.take();
        }
        self.saved_root = self.root.take();
        self.layout_init(wp);
        self.zoomed = true;
        true
    }

    /// window_unzoom
    pub(crate) fn unzoom(&mut self) -> bool {
        if !self.zoomed {
            return false;
        }
        self.zoomed = false;
        self.layout_free();
        self.root = self.saved_root.take();
        for p in &mut self.panes {
            p.cell = p.saved_cell.take();
        }
        self.fix_panes(None);
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

    /// window_pane_find_up
    pub(crate) fn find_up(&self, wp: PaneId) -> Option<PaneId> {
        let p = self.pane(wp);
        let mut edge = p.yoff;
        if edge == 0 {
            edge = self.sy + 1;
        }
        let (left, right) = (p.xoff, p.xoff + p.sx);
        let list: Vec<usize> = (0..self.panes.len())
            .filter(|&i| {
                let n = &self.panes[i];
                n.id != wp
                    && n.yoff + n.sy + 1 == edge
                    && Self::overlaps(n.xoff, n.xoff + n.sx - 1, left, right)
            })
            .collect();
        self.choose_best(&list)
    }

    /// window_pane_find_down
    pub(crate) fn find_down(&self, wp: PaneId) -> Option<PaneId> {
        let p = self.pane(wp);
        let mut edge = p.yoff + p.sy + 1;
        if edge >= self.sy {
            edge = 0;
        }
        let (left, right) = (p.xoff, p.xoff + p.sx);
        let list: Vec<usize> = (0..self.panes.len())
            .filter(|&i| {
                let n = &self.panes[i];
                n.id != wp
                    && n.yoff == edge
                    && Self::overlaps(n.xoff, n.xoff + n.sx - 1, left, right)
            })
            .collect();
        self.choose_best(&list)
    }

    /// window_pane_find_left
    pub(crate) fn find_left(&self, wp: PaneId) -> Option<PaneId> {
        let p = self.pane(wp);
        let mut edge = p.xoff;
        if edge == 0 {
            edge = self.sx + 1;
        }
        let (top, bottom) = (p.yoff, p.yoff + p.sy);
        let list: Vec<usize> = (0..self.panes.len())
            .filter(|&i| {
                let n = &self.panes[i];
                n.id != wp
                    && n.xoff + n.sx + 1 == edge
                    && Self::overlaps(n.yoff, n.yoff + n.sy - 1, top, bottom)
            })
            .collect();
        self.choose_best(&list)
    }

    /// window_pane_find_right
    pub(crate) fn find_right(&self, wp: PaneId) -> Option<PaneId> {
        let p = self.pane(wp);
        let mut edge = p.xoff + p.sx + 1;
        if edge >= self.sx {
            edge = 0;
        }
        let (top, bottom) = (p.yoff, p.yoff + p.sy);
        let list: Vec<usize> = (0..self.panes.len())
            .filter(|&i| {
                let n = &self.panes[i];
                n.id != wp
                    && n.xoff == edge
                    && Self::overlaps(n.yoff, n.yoff + n.sy - 1, top, bottom)
            })
            .collect();
        self.choose_best(&list)
    }
}
