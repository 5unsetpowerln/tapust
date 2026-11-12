//! Session, winlink and command logic, ported from tmux 3.6 `session.c`,
//! `server-fn.c`, `resize.c`, `spawn.c` and the `cmd-*.c` files.

use std::collections::{BTreeMap, BTreeSet};

use crate::keys::KeyState;
use crate::split::Side;
use crate::window::{Pane, Window};
use crate::{format, Command, Direction, Effect, PaneId, Session};

pub(crate) fn new_session() -> (Session, Vec<Effect>) {
    let mut s = Session {
        name: "0".to_owned(),
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
        let idx = s.free_index();
        s.spawn_window(idx, None, false);
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
        let before = self.pane_ids();
        f(self);
        self.generation += 1;
        self.collect_effects(&before)
    }

    fn pane_ids(&self) -> BTreeSet<PaneId> {
        self.winlinks.values().flat_map(|w| w.panes.iter()).map(|p| p.id).collect()
    }

    fn collect_effects(&mut self, before: &BTreeSet<PaneId>) -> Vec<Effect> {
        let after = self.pane_ids();
        let spawned: Vec<_> = std::mem::take(&mut self.spawned);
        let mut effects = Vec::new();
        for pane in before.difference(&after) {
            effects.push(Effect::Destroy { pane: *pane });
        }
        for (pane, parent, inherit_cwd) in spawned {
            if after.contains(&pane) {
                effects.push(Effect::Spawn { pane, parent, inherit_cwd });
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

    fn new_pane(&mut self) -> Pane {
        let id = self.next_pane_id;
        self.next_pane_id += 1;
        Pane::new(id)
    }

    /// winlink_add(-1 - base-index): the lowest free index.
    fn free_index(&self) -> u32 {
        (0..).find(|i| !self.winlinks.contains_key(i)).expect("free index")
    }

    /// spawn_window for a new window at `idx`, sized for the client; the
    /// caller selects it.
    fn spawn_window(&mut self, idx: u32, parent: Option<PaneId>, inherit_cwd: bool) {
        let pane = self.new_pane();
        let id = pane.id;
        let mut w = Window::new(self.next_window_id, pane);
        self.next_window_id += 1;
        // window_set_active_pane with no previous active pane.
        w.panes[0].active_point = self.next_active_point;
        self.next_active_point = self.next_active_point.wrapping_add(1);
        self.winlinks.insert(idx, w);
        self.spawned.push((id, parent, inherit_cwd));
    }

    /// winlink_shuffle_up without `before`: frees the index after the
    /// current window by moving the windows from there up by one.
    fn shuffle_up(&mut self) -> u32 {
        let idx = self.curw + 1;
        let mut last = idx;
        while self.winlinks.contains_key(&last) {
            last += 1;
        }
        for from in (idx..last).rev() {
            let w = self.winlinks.remove(&from).expect("window");
            self.winlinks.insert(from + 1, w);
            for i in &mut self.lastw {
                if *i == from {
                    *i = from + 1;
                }
            }
        }
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
    pub(crate) fn kill_window(&mut self, idx: u32) {
        if let Some(w) = self.winlinks.get_mut(&idx) {
            w.unzoom();
        }
        if self.session_detach(idx) {
            self.dead = true;
        }
    }

    /// server_kill_pane
    fn kill_pane(&mut self, idx: u32, wp: PaneId) {
        let w = self.winlinks.get_mut(&idx).expect("window");
        if w.panes.len() == 1 {
            self.kill_window(idx);
        } else {
            w.unzoom();
            w.remove_pane(wp);
        }
    }

    /// server_destroy_pane with remain-on-exit off.
    pub(crate) fn destroy_pane(&mut self, wp: PaneId) {
        let Some(idx) = self.window_of(wp) else { return };
        let w = self.winlinks.get_mut(&idx).expect("window");
        w.unzoom();
        w.remove_pane(wp);
        if w.panes.is_empty() {
            self.kill_window(idx);
        }
    }

    pub(crate) fn exec(&mut self, cmd: &Command) -> Result<(), String> {
        match *cmd {
            Command::SplitWindow { horizontal, before, full, cwd_of_active } => {
                self.split_window(Side { horizontal, before }, full, cwd_of_active)
            }
            Command::NewWindow { cwd_of_active, after } => {
                let parent = self.cur().active;
                let idx = if after { self.shuffle_up() } else { self.free_index() };
                self.spawn_window(idx, Some(parent), cwd_of_active);
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
            Command::Equalize => {
                self.cur_mut().equalize();
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

    /// cmd_split_window_exec: the new pane takes half of the active pane (or
    /// of the window when `full`), as ghostty's new_split does.
    fn split_window(&mut self, side: Side, full: bool, cwd_of_active: bool) -> Result<(), String> {
        let w = self.cur_mut();
        let wp = w.active;
        w.push_zoom(true, false);
        // ghostty always splits; GTK raises the window's minimum size instead.
        let pane = self.new_pane();
        let id = pane.id;
        let (w, point) = self.cur_point();
        w.add_pane(pane, wp, side, full);
        w.set_active(id, point);
        w.pop_zoom(point);
        self.spawned.push((id, Some(wp), cwd_of_active));
        Ok(())
    }

    /// cmd_select_pane_exec with -L/-R/-U/-D
    fn select_pane_dir(&mut self, dir: Direction) {
        let (w, point) = self.cur_point();
        let wp = w.active;
        w.push_zoom(false, true);
        let found = w.find(wp, dir);
        w.pop_zoom(point);
        let Some(found) = found else { return };
        if found == w.active {
            return;
        }
        w.push_zoom(false, false);
        w.set_active(found, point);
        w.pop_zoom(point);
    }

    /// select-pane -t for a pane of the current window.
    pub(crate) fn focus(&mut self, wp: PaneId) {
        let (w, point) = self.cur_point();
        if !w.has_pane(wp) || wp == w.active {
            return;
        }
        w.push_zoom(false, false);
        w.set_active(wp, point);
        w.pop_zoom(point);
    }

    /// ghostty's moveSplit within the current window.
    pub(crate) fn drop_pane(&mut self, source: PaneId, target: PaneId, dir: Direction) {
        let w = self.cur_mut();
        if source == target || !w.has_pane(source) || !w.has_pane(target) {
            return;
        }
        let side = match dir {
            Direction::Left => Side { horizontal: true, before: true },
            Direction::Right => Side { horizontal: true, before: false },
            Direction::Up => Side { horizontal: false, before: true },
            Direction::Down => Side { horizontal: false, before: false },
        };
        w.move_pane(source, target, side);
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
        // The GUI turns cells into a share of the tree, as ghostty's
        // resize_split turns pixels into one, and calls resize_split.
        let pane = w.active;
        let cells = if matches!(dir, Direction::Left | Direction::Up) { -adjust } else { adjust };
        self.writes.push(Effect::ResizeSplit { pane, horizontal: matches!(dir, Direction::Left | Direction::Right), cells });
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
        let pane = w.remove_pane(wp);
        let nw = Window::new(self.next_window_id, pane);
        self.next_window_id += 1;
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
            w.swap_panes(src, dst);
                w.set_active(dst, point);
        }
        w.pop_zoom(point);
    }

    /// cmd_rotate_window_exec
    fn rotate_window(&mut self, down: bool) {
        let (w, point) = self.cur_point();
        w.push_zoom(false, false);
        let n = w.panes.len();
        // The panes move one place along the order; the places stay.
        let new_active = if down {
            w.panes.rotate_right(1);
            w.place_in_order();
            w.prev_of(w.active).unwrap_or(w.panes[n - 1].id)
        } else {
            w.panes.rotate_left(1);
            w.place_in_order();
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
        Ok(())
    }

    /// next-window, previous-window and last-window
    fn step_window(&mut self, step: fn(&mut Session) -> i32, error: &str) -> Result<(), String> {
        if step(self) != 0 {
            return Err(error.to_owned());
        }
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
