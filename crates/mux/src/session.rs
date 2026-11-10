//! Session, winlink and command logic, ported from tmux 3.6 `session.c`,
//! `server-fn.c`, `resize.c`, `spawn.c` and the `cmd-*.c` files.

use std::collections::{BTreeMap, BTreeSet};

use crate::keys::KeyState;
use crate::layout::{LayoutType, WINDOW_MAXIMUM};
use crate::window::{Pane, Window};
use crate::{format, Command, Direction, Effect, PaneId, Session};

/// The error of a `split-window -f` that tmux 3.6 would carry out with
/// wrapped unsigned sizes (layout_resize_child_cells), making the server
/// allocate a pane of billions of cells. The model refuses it instead.
pub(crate) const SPLIT_OVERFLOW: &str =
    "no space for new pane (tmux 3.6 would overflow the layout)";

/// The error of a `select-layout -E` whose layout_spread_cell asks a cell to
/// shrink below its minimum: tmux 3.6 then loops forever in
/// layout_resize_adjust or wraps a pane size. The model refuses it.
pub(crate) const SPREAD_OVERFLOW: &str =
    "can't spread out the layout (tmux 3.6 would hang or overflow the layout)";

pub(crate) fn new_session(cols: u16, rows: u16, status: bool) -> (Session, Vec<Effect>) {
    let mut s = Session {
        name: "0".to_owned(),
        cols,
        rows,
        status,
        winlinks: BTreeMap::new(),
        curw: 0,
        lastw: Vec::new(),
        next_pane_id: 0,
        next_window_id: 0,
        next_active_point: 0,
        keys: KeyState::default(),
        prompt: None,
        message: None,
        history: Vec::new(),
        generation: 0,
        dead: false,
        last_active: 0,
        spawned: Vec::new(),
        writes: Vec::new(),
    };
    let effects = s.op(|s| {
        let idx = s.spawn_window(None);
        s.curw = idx;
    });
    (s, effects)
}

impl Session {
    /// Runs one operation the way the tmux server loop would, then turns the
    /// changes into effects.
    pub(crate) fn op(&mut self, f: impl FnOnce(&mut Session)) -> Vec<Effect> {
        if self.dead {
            return Vec::new();
        }
        let before = self.size_map();
        f(self);
        self.server_loop();
        debug_assert!(self.winlinks.values().all(Window::layout_valid), "inconsistent layout");
        self.generation += 1;
        self.collect_effects(&before)
    }

    fn size_map(&self) -> BTreeMap<PaneId, (u32, u32)> {
        self.winlinks
            .values()
            .flat_map(|w| w.panes.iter())
            .map(|p| (p.id, (p.sx, p.sy)))
            .collect()
    }

    fn collect_effects(&mut self, before: &BTreeMap<PaneId, (u32, u32)>) -> Vec<Effect> {
        let after = self.size_map();
        let spawned: Vec<_> = std::mem::take(&mut self.spawned);
        let new: BTreeSet<PaneId> = spawned.iter().map(|s| s.0).collect();
        let mut effects = Vec::new();
        for pane in before.keys().filter(|p| !after.contains_key(p)) {
            effects.push(Effect::Destroy { pane: *pane });
        }
        for (pane, &(sx, sy)) in &after {
            if !new.contains(pane) && before.get(pane).is_some_and(|&b| b != (sx, sy)) {
                effects.push(Effect::Resize { pane: *pane, cols: clamp(sx), rows: clamp(sy) });
            }
        }
        for (pane, cwd_of) in spawned {
            if let Some(&(sx, sy)) = after.get(&pane) {
                effects.push(Effect::Spawn { pane, cols: clamp(sx), rows: clamp(sy), cwd_of });
            }
        }
        effects.append(&mut self.writes);
        if self.dead {
            effects.push(Effect::Exit);
        } else {
            self.last_active = self.cur().active;
        }
        effects
    }

    pub(crate) fn cur(&self) -> &Window {
        &self.winlinks[&self.curw]
    }

    pub(crate) fn cur_mut(&mut self) -> &mut Window {
        self.winlinks.get_mut(&self.curw).expect("current window")
    }

    /// The current window and tmux's `next_active_point` counter.
    fn cur_point(&mut self) -> (&mut Window, &mut u32) {
        let w = self.winlinks.get_mut(&self.curw).expect("current window");
        (w, &mut self.next_active_point)
    }

    pub(crate) fn find_pane_mut(&mut self, wp: PaneId) -> Option<&mut Pane> {
        self.winlinks.values_mut().find_map(|w| w.pane_mut_opt(wp))
    }

    pub(crate) fn window_of(&self, wp: PaneId) -> Option<u32> {
        self.winlinks.iter().find(|(_, w)| w.has_pane(wp)).map(|(&i, _)| i)
    }

    pub(crate) fn window_by_id(&self, id: u32) -> Option<u32> {
        self.winlinks.iter().find(|(_, w)| w.id == id).map(|(&i, _)| i)
    }

    /// Whether the client has room for the status line (`CLIENT_STATUSOFF`).
    pub(crate) fn status_lines(&self) -> u32 {
        u32::from(self.status && self.rows > 1)
    }

    /// The window size the client asks for (clients_calculate_size and
    /// default_window_size for the single client).
    pub(crate) fn area(&self) -> (u32, u32) {
        let sx = u32::from(self.cols);
        let sy = u32::from(self.rows) - self.status_lines().min(u32::from(self.rows));
        (sx.clamp(1, WINDOW_MAXIMUM), sy.clamp(1, WINDOW_MAXIMUM))
    }

    fn new_pane(&mut self, sx: u32, sy: u32) -> Pane {
        let id = self.next_pane_id;
        self.next_pane_id += 1;
        Pane::new(id, sx, sy)
    }

    /// winlink_add(-1 - base-index): the lowest free index.
    fn free_index(&self) -> u32 {
        (0..).find(|i| !self.winlinks.contains_key(i)).expect("free index")
    }

    /// spawn_window for a new window at the lowest free index, sized for the
    /// client. Returns the index; the caller selects it.
    fn spawn_window(&mut self, cwd_of: Option<PaneId>) -> u32 {
        let idx = self.free_index();
        let (sx, sy) = self.area();
        let pane = self.new_pane(sx, sy);
        let id = pane.id;
        let mut w = Window::new(self.next_window_id, sx, sy, id);
        self.next_window_id += 1;
        w.panes.push(pane);
        w.layout_init(id);
        // window_set_active_pane with no previous active pane.
        w.panes[0].active_point = self.next_active_point;
        self.next_active_point = self.next_active_point.wrapping_add(1);
        self.winlinks.insert(idx, w);
        self.spawned.push((id, cwd_of));
        idx
    }

    /// session_set_current
    fn set_current(&mut self, idx: u32) -> i32 {
        if !self.winlinks.contains_key(&idx) {
            return -1;
        }
        if idx == self.curw {
            return 1;
        }
        let old = self.curw;
        self.lastw.retain(|&i| i != idx && i != old);
        if self.winlinks.contains_key(&old) {
            self.lastw.insert(0, old);
        }
        self.curw = idx;
        0
    }

    /// session_next
    fn session_next(&mut self) -> i32 {
        let next = self.winlinks.range(self.curw + 1..).next().map(|(&i, _)| i);
        let idx = next.or_else(|| self.winlinks.keys().next().copied());
        idx.map_or(-1, |i| self.set_current(i))
    }

    /// session_previous
    fn session_previous(&mut self) -> i32 {
        let prev = self.winlinks.range(..self.curw).next_back().map(|(&i, _)| i);
        let idx = prev.or_else(|| self.winlinks.keys().next_back().copied());
        idx.map_or(-1, |i| self.set_current(i))
    }

    /// session_last
    fn session_last(&mut self) -> i32 {
        match self.lastw.first().copied() {
            None => -1,
            Some(idx) if idx == self.curw => 1,
            Some(idx) => self.set_current(idx),
        }
    }

    /// session_detach: returns whether the session is now empty.
    fn session_detach(&mut self, idx: u32) -> bool {
        if self.curw == idx && self.session_last() != 0 && self.session_previous() != 0 {
            self.session_next();
        }
        self.lastw.retain(|&i| i != idx);
        self.winlinks.remove(&idx);
        self.winlinks.is_empty()
    }

    /// winlink_next_by_number / winlink_previous_by_number from the current window.
    fn winlink_by_offset(&self, offset: i32) -> u32 {
        let keys: Vec<u32> = self.winlinks.keys().copied().collect();
        let mut pos = keys.iter().position(|&i| i == self.curw).expect("current winlink");
        for _ in 0..offset.unsigned_abs() {
            pos = if offset > 0 {
                (pos + 1) % keys.len()
            } else {
                (pos + keys.len() - 1) % keys.len()
            };
        }
        keys[pos]
    }

    /// server_kill_window(w, 1)
    fn kill_window(&mut self, idx: u32) {
        if let Some(w) = self.winlinks.get_mut(&idx) {
            w.unzoom();
        }
        if self.session_detach(idx) {
            self.dead = true;
            return;
        }
        self.recalculate_sizes(false);
    }

    /// server_kill_pane
    fn kill_pane(&mut self, idx: u32, wp: PaneId) {
        let w = self.winlinks.get_mut(&idx).expect("window");
        if w.panes.len() == 1 {
            self.kill_window(idx);
            if !self.dead {
                self.recalculate_sizes(false);
            }
        } else {
            w.unzoom();
            w.close_pane(wp);
            w.remove_pane(wp);
        }
    }

    /// server_destroy_pane with remain-on-exit off.
    pub(crate) fn destroy_pane(&mut self, wp: PaneId) {
        let Some(idx) = self.window_of(wp) else { return };
        let w = self.winlinks.get_mut(&idx).expect("window");
        w.unzoom();
        w.close_pane(wp);
        w.remove_pane(wp);
        if w.panes.is_empty() {
            self.kill_window(idx);
        }
    }

    /// recalculate_sizes / recalculate_sizes_now for the single client.
    pub(crate) fn recalculate_sizes(&mut self, now: bool) {
        let (sx, sy) = self.area();
        for w in self.winlinks.values_mut() {
            let unchanged = match w.pending_resize {
                Some(pending) => pending == (sx, sy),
                None => (w.sx, w.sy) == (sx, sy),
            };
            if !now && unchanged {
                continue;
            }
            if now {
                resize_window(w, sx, sy, &mut self.next_active_point);
            } else {
                w.pending_resize = Some((sx, sy));
            }
        }
    }

    /// server_client_check_window_resize: only the current window of the
    /// attached session is resized by the server loop.
    fn server_loop(&mut self) {
        if self.dead {
            return;
        }
        let (w, point) = self.cur_point();
        if let Some((sx, sy)) = w.pending_resize {
            resize_window(w, sx, sy, point);
        }
    }

    pub(crate) fn exec(&mut self, cmd: &Command) -> Result<(), String> {
        match *cmd {
            Command::SplitWindow { horizontal, before, full, size, cwd_of_active } => {
                self.split_window(horizontal, before, full, size, cwd_of_active)
            }
            Command::NewWindow { cwd_of_active } => {
                let cwd_of = cwd_of_active.then(|| self.cur().active);
                let idx = self.spawn_window(cwd_of);
                self.set_current(idx);
                Ok(())
            }
            Command::SelectPane(dir) => {
                self.select_pane_dir(dir);
                Ok(())
            }
            Command::LastPane => self.last_pane(),
            Command::NextPane => {
                let (w, point) = self.cur_point();
                let wp = w.next_by_number(w.active, 1);
                if wp != w.active {
                    w.push_zoom(false, false);
                    w.set_active(wp, point);
                    w.pop_zoom(point);
                }
                Ok(())
            }
            Command::ResizePane(dir, adjust) => self.resize_pane(dir, adjust),
            Command::ZoomPane => {
                let (w, point) = self.cur_point();
                if w.zoomed {
                    w.unzoom();
                } else {
                    let wp = w.active;
                    w.zoom(wp, point);
                }
                Ok(())
            }
            Command::KillPane => {
                let wp = self.cur().active;
                self.kill_pane(self.curw, wp);
                Ok(())
            }
            Command::KillWindow => {
                self.kill_window(self.curw);
                Ok(())
            }
            Command::BreakPane => {
                self.break_pane();
                Ok(())
            }
            Command::SwapPane { up } => {
                self.swap_pane(up);
                Ok(())
            }
            Command::RotateWindow { down } => {
                self.rotate_window(down);
                Ok(())
            }
            Command::SelectWindow(index) => self.select_window(index),
            Command::NextWindow => self.step_window(Self::session_next, "no next window"),
            Command::PreviousWindow => {
                self.step_window(Self::session_previous, "no previous window")
            }
            Command::LastWindow => self.step_window(Self::session_last, "no last window"),
            Command::SwapWindow(offset) => {
                self.swap_window(offset);
                Ok(())
            }
            Command::SpreadLayout => {
                let w = self.cur_mut();
                let saved = w.clone();
                w.unzoom();
                w.stuck = false;
                let wp = w.active;
                w.spread_out(wp);
                if w.stuck || !w.layout_valid() {
                    *w = saved;
                    return Err(SPREAD_OVERFLOW.to_owned());
                }
                self.recalculate_sizes(false);
                Ok(())
            }
            Command::RenameWindow(ref name) => {
                let id = self.cur().id;
                self.rename_window(id, name);
                Ok(())
            }
            Command::RenameSession(ref name) => self.rename_session(name),
            Command::SendPrefix => {
                let pane = self.cur().active;
                self.writes.push(Effect::Write { pane, bytes: vec![0x02] });
                Ok(())
            }
        }
    }

    /// cmd_split_window_exec
    fn split_window(
        &mut self,
        horizontal: bool,
        before: bool,
        full: bool,
        size: Option<u32>,
        cwd_of_active: bool,
    ) -> Result<(), String> {
        let ty = if horizontal { LayoutType::LeftRight } else { LayoutType::TopBottom };
        let size = size.map_or(-1, |n| i32::try_from(n).unwrap_or(i32::MAX));
        let w = self.cur_mut();
        let wp = w.active;
        // A refused overflow leaves the window as it was, zoom included.
        let saved = full.then(|| w.clone());
        w.push_zoom(true, false);
        let Some(lc) = w.split_pane(wp, ty, size, before, full) else {
            return Err("no space for new pane".to_owned());
        };
        if let Some(saved) = saved
            && !w.layout_valid()
        {
            *w = saved;
            return Err(SPLIT_OVERFLOW.to_owned());
        }
        let (sx, sy) = (w.sx, w.sy);
        let pane = self.new_pane(sx, sy);
        let id = pane.id;
        let (w, point) = self.cur_point();
        w.add_pane(pane, wp, before, full);
        w.assign_pane(lc, id, false);
        w.set_active(id, point);
        w.pop_zoom(point);
        self.spawned.push((id, cwd_of_active.then_some(wp)));
        Ok(())
    }

    /// cmd_select_pane_exec with -L/-R/-U/-D
    fn select_pane_dir(&mut self, dir: Direction) {
        let (w, point) = self.cur_point();
        let wp = w.active;
        w.push_zoom(false, true);
        let found = match dir {
            Direction::Left => w.find_left(wp),
            Direction::Right => w.find_right(wp),
            Direction::Up => w.find_up(wp),
            Direction::Down => w.find_down(wp),
        };
        w.pop_zoom(point);
        let Some(found) = found else { return };
        if found == w.active {
            return;
        }
        w.push_zoom(false, false);
        w.set_active(found, point);
        w.pop_zoom(point);
    }

    /// cmd_select_pane_exec for last-pane
    fn last_pane(&mut self) -> Result<(), String> {
        let (w, point) = self.cur_point();
        let mut last = w.last_panes.first().copied();
        if last.is_none() && w.panes.len() == 2 {
            last = w.prev_of(w.active).or_else(|| w.next_of(w.active));
        }
        let Some(last) = last else { return Err("no last pane".to_owned()) };
        w.push_zoom(false, false);
        w.set_active(last, point);
        w.pop_zoom(point);
        Ok(())
    }

    /// cmd_resize_pane_exec with -L/-R/-U/-D
    fn resize_pane(&mut self, dir: Direction, adjust: u32) -> Result<(), String> {
        let w = self.cur_mut();
        w.unzoom();
        if adjust == 0 {
            return Err("adjustment too small".to_owned());
        }
        let Ok(adjust) = i32::try_from(adjust) else {
            return Err("adjustment too large".to_owned());
        };
        let wp = w.active;
        match dir {
            Direction::Left => w.resize_pane(wp, LayoutType::LeftRight, -adjust, true),
            Direction::Right => w.resize_pane(wp, LayoutType::LeftRight, adjust, true),
            Direction::Up => w.resize_pane(wp, LayoutType::TopBottom, -adjust, true),
            Direction::Down => w.resize_pane(wp, LayoutType::TopBottom, adjust, true),
        }
        Ok(())
    }

    /// cmd_break_pane_exec without -a/-b/-d/-n/-t
    fn break_pane(&mut self) {
        let idx = self.curw;
        let w = self.cur_mut();
        w.unzoom();
        if w.panes.len() == 1 {
            // server_link_window to the lowest free index, then
            // server_unlink_window of the old winlink.
            let new = self.free_index();
            let w = self.winlinks.remove(&idx).expect("window");
            self.winlinks.insert(new, w);
            self.lastw.retain(|&i| i != new && i != idx);
            self.curw = new;
            return;
        }
        let wp = w.active;
        w.close_pane(wp);
        let pane = w.remove_pane(wp);
        let (sx, sy) = (w.sx, w.sy);
        let mut nw = Window::new(self.next_window_id, sx, sy, wp);
        self.next_window_id += 1;
        nw.panes.push(pane);
        nw.layout_init(wp);
        let new = self.free_index();
        self.winlinks.insert(new, nw);
        self.set_current(new);
    }

    /// cmd_swap_pane_exec with -U/-D on the active pane
    fn swap_pane(&mut self, up: bool) {
        let (w, point) = self.cur_point();
        let dst = w.active;
        w.push_zoom(false, false);
        let src = if up {
            w.prev_of(dst).unwrap_or_else(|| w.panes[w.panes.len() - 1].id)
        } else {
            w.next_of(dst).unwrap_or_else(|| w.panes[0].id)
        };
        if src != dst {
            let (ps, pd) = (w.pos(src).expect("pane"), w.pos(dst).expect("pane"));
            w.panes.swap(ps, pd);
            let (src_lc, dst_lc) = (w.pane(src).cell, w.pane(dst).cell);
            w.pane_mut(dst).cell = src_lc;
            w.pane_mut(src).cell = dst_lc;
            w.retarget_cell(src_lc, dst);
            w.retarget_cell(dst_lc, src);
            let s = w.pane(src).clone();
            let d = w.pane(dst).clone();
            w.pane_mut(src).set_geometry(d.xoff, d.yoff, d.sx, d.sy);
            w.pane_mut(dst).set_geometry(s.xoff, s.yoff, s.sx, s.sy);
            w.set_active(dst, point);
            w.fix_panes(None);
        }
        w.pop_zoom(point);
    }

    /// cmd_rotate_window_exec
    fn rotate_window(&mut self, down: bool) {
        let (w, point) = self.cur_point();
        w.push_zoom(false, false);
        let n = w.panes.len();
        let new_active = if down {
            let last = w.panes.pop().expect("pane");
            w.panes.insert(0, last);
            let first = w.panes[0].clone();
            for i in 0..n - 1 {
                let next = w.panes[i + 1].clone();
                w.take_place_of(i, &next);
            }
            w.take_place_of(n - 1, &first);
            w.prev_of(w.active).unwrap_or(w.panes[n - 1].id)
        } else {
            let first = w.panes.remove(0);
            w.panes.push(first);
            let last = w.panes[n - 1].clone();
            for i in (1..n).rev() {
                let prev = w.panes[i - 1].clone();
                w.take_place_of(i, &prev);
            }
            w.take_place_of(0, &last);
            w.next_of(w.active).unwrap_or(w.panes[0].id)
        };
        w.set_active(new_active, point);
        w.pop_zoom(point);
    }

    /// cmd_select_window_exec with -t :=index
    fn select_window(&mut self, index: u32) -> Result<(), String> {
        let target = if self.winlinks.contains_key(&index) {
            Some(index)
        } else {
            // An exact window name match (CMD_FIND_EXACT_WINDOW).
            let name = index.to_string();
            let mut found = self.winlinks.iter().filter(|(_, w)| w.name() == name);
            match (found.next(), found.next()) {
                (Some((&i, _)), None) => Some(i),
                _ => None,
            }
        };
        let Some(target) = target else {
            return Err(format!("can't find window: {index}"));
        };
        self.set_current(target);
        self.recalculate_sizes(false);
        Ok(())
    }

    /// next-window, previous-window and last-window
    fn step_window(&mut self, step: fn(&mut Session) -> i32, error: &str) -> Result<(), String> {
        if step(self) != 0 {
            return Err(error.to_owned());
        }
        self.recalculate_sizes(false);
        Ok(())
    }

    /// cmd_swap_window_exec with -d and a -t offset from the current window
    fn swap_window(&mut self, offset: i32) {
        let src = self.curw;
        let dst = self.winlink_by_offset(offset);
        if src == dst {
            return;
        }
        let a = self.winlinks.remove(&src).expect("window");
        let b = self.winlinks.remove(&dst).expect("window");
        self.winlinks.insert(src, b);
        self.winlinks.insert(dst, a);
        self.set_current(dst);
        self.recalculate_sizes(false);
    }

    /// cmd_rename_window_exec for the window with id `id`.
    pub(crate) fn rename_window(&mut self, id: u32, name: &str) {
        let Some(idx) = self.window_by_id(id) else { return };
        let ctx = format::Context { window: idx, pane: self.winlinks[&idx].active };
        let name = format::vis(&self.expand(name, ctx));
        self.winlinks.get_mut(&idx).expect("window").fixed_name = Some(name);
    }

    /// cmd_rename_session_exec
    fn rename_session(&mut self, name: &str) -> Result<(), String> {
        let ctx = format::Context { window: self.curw, pane: self.cur().active };
        let expanded = self.expand(name, ctx);
        // session_check_name
        if expanded.is_empty() {
            return Err(format!("invalid session: {expanded}"));
        }
        self.name = format::vis(&expanded.replace([':', '.'], "_"));
        Ok(())
    }

    /// kill-pane run by confirm-before for the pane that was current then.
    pub(crate) fn kill_pane_target(&mut self, wp: PaneId) {
        let (idx, wp) = match self.window_of(wp) {
            Some(idx) => (idx, wp),
            None => (self.curw, self.cur().active),
        };
        self.kill_pane(idx, wp);
    }

    /// kill-window run by confirm-before for the window that was current then.
    pub(crate) fn kill_window_target(&mut self, id: u32) {
        let idx = self.window_by_id(id).unwrap_or(self.curw);
        self.kill_window(idx);
    }
}

/// resize_window
fn resize_window(w: &mut Window, sx: u32, sy: u32, point: &mut u32) {
    let sx = sx.clamp(1, WINDOW_MAXIMUM);
    let sy = sy.clamp(1, WINDOW_MAXIMUM);
    let zoomed = w.zoomed;
    if zoomed {
        w.unzoom();
    }
    w.layout_resize(sx, sy);
    let (root_sx, root_sy) = {
        let root = w.cell(w.root.expect("layout root"));
        (root.sx, root.sy)
    };
    w.sx = sx.max(root_sx);
    w.sy = sy.max(root_sy);
    if zoomed {
        let active = w.active;
        w.zoom(active, point);
    }
    w.pending_resize = None;
}

fn clamp(v: u32) -> u16 {
    u16::try_from(v).unwrap_or(u16::MAX)
}

impl Pane {
    fn set_geometry(&mut self, xoff: u32, yoff: u32, sx: u32, sy: u32) {
        self.xoff = xoff;
        self.yoff = yoff;
        self.sx = sx;
        self.sy = sy;
    }
}

impl Window {
    /// Points layout cell `lc` (if any) at pane `wp`.
    fn retarget_cell(&mut self, lc: Option<crate::layout::CellId>, wp: PaneId) {
        if let Some(lc) = lc {
            self.set_cell_pane(lc, wp);
        }
    }

    /// Gives the pane at `pos` the layout cell and geometry of `other` (rotate-window).
    fn take_place_of(&mut self, pos: usize, other: &Pane) {
        let id = self.panes[pos].id;
        self.panes[pos].cell = other.cell;
        self.retarget_cell(other.cell, id);
        self.panes[pos].set_geometry(other.xoff, other.yoff, other.sx, other.sy);
    }
}
