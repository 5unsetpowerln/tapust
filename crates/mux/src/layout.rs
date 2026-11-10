//! The layout cell tree of a window, ported from tmux 3.6 `layout.c`.
//!
//! Sizes use `u32` with the wrapping arithmetic of tmux's `u_int`, so the
//! model reproduces tmux's rounding and its quirks; [`Window::layout_valid`]
//! detects the cases where that arithmetic would leave tmux with a broken
//! tree.

use crate::window::Window;
use crate::PaneId;

pub(crate) type CellId = usize;

/// tmux `PANE_MINIMUM`.
pub(crate) const PANE_MINIMUM: u32 = 1;
/// tmux `WINDOW_MAXIMUM`.
pub(crate) const WINDOW_MAXIMUM: u32 = 10000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LayoutType {
    LeftRight,
    TopBottom,
    Pane,
}

#[derive(Clone, Debug)]
pub(crate) struct Cell {
    pub ty: LayoutType,
    pub parent: Option<CellId>,
    pub children: Vec<CellId>,
    pub sx: u32,
    pub sy: u32,
    pub xoff: u32,
    pub yoff: u32,
    pub wp: Option<PaneId>,
}

impl Window {
    pub(crate) fn cell(&self, lc: CellId) -> &Cell {
        self.cells[lc].as_ref().expect("live layout cell")
    }

    fn cell_mut(&mut self, lc: CellId) -> &mut Cell {
        self.cells[lc].as_mut().expect("live layout cell")
    }

    fn size_of(&self, lc: CellId, ty: LayoutType) -> u32 {
        let c = self.cell(lc);
        if ty == LayoutType::LeftRight { c.sx } else { c.sy }
    }

    fn sibling(&self, lc: CellId, delta: isize) -> Option<CellId> {
        let parent = self.cell(lc).parent?;
        let children = &self.cell(parent).children;
        let pos = children.iter().position(|&c| c == lc)?;
        let pos = pos.checked_add_signed(delta)?;
        children.get(pos).copied()
    }

    fn next_sibling(&self, lc: CellId) -> Option<CellId> {
        self.sibling(lc, 1)
    }

    fn prev_sibling(&self, lc: CellId) -> Option<CellId> {
        self.sibling(lc, -1)
    }

    fn is_last_child(&self, lc: CellId) -> bool {
        self.cell(lc)
            .parent
            .is_some_and(|p| self.cell(p).children.last() == Some(&lc))
    }

    /// layout_create_cell
    fn create_cell(&mut self, parent: Option<CellId>) -> CellId {
        let cell = Cell {
            ty: LayoutType::Pane,
            parent,
            children: Vec::new(),
            sx: u32::MAX,
            sy: u32::MAX,
            xoff: u32::MAX,
            yoff: u32::MAX,
            wp: None,
        };
        match self.cells.iter().position(Option::is_none) {
            Some(i) => {
                self.cells[i] = Some(cell);
                i
            }
            None => {
                self.cells.push(Some(cell));
                self.cells.len() - 1
            }
        }
    }

    /// layout_free_cell
    fn free_cell(&mut self, lc: CellId) {
        let cell = self.cells[lc].take().expect("live layout cell");
        match cell.ty {
            LayoutType::LeftRight | LayoutType::TopBottom => {
                for child in cell.children {
                    self.free_cell(child);
                }
            }
            LayoutType::Pane => {
                if let Some(p) = cell.wp.and_then(|wp| self.pane_mut_opt(wp)) {
                    p.cell = None;
                }
            }
        }
    }

    fn set_size(&mut self, lc: CellId, sx: u32, sy: u32, xoff: u32, yoff: u32) {
        let c = self.cell_mut(lc);
        c.sx = sx;
        c.sy = sy;
        c.xoff = xoff;
        c.yoff = yoff;
    }

    /// Points the leaf cell `lc` at pane `wp` (swap-pane, rotate-window).
    pub(crate) fn set_cell_pane(&mut self, lc: CellId, wp: PaneId) {
        self.cell_mut(lc).wp = Some(wp);
    }

    /// layout_make_leaf
    fn make_leaf(&mut self, lc: CellId, wp: PaneId) {
        let c = self.cell_mut(lc);
        c.ty = LayoutType::Pane;
        c.children.clear();
        c.wp = Some(wp);
        self.pane_mut(wp).cell = Some(lc);
    }

    /// layout_make_node
    fn make_node(&mut self, lc: CellId, ty: LayoutType) {
        let c = self.cell_mut(lc);
        c.ty = ty;
        c.children.clear();
        if let Some(wp) = c.wp.take() {
            self.pane_mut(wp).cell = None;
        }
    }

    /// layout_fix_offsets1
    fn fix_offsets1(&mut self, lc: CellId) {
        let (ty, xoff, yoff, children) = {
            let c = self.cell(lc);
            (c.ty, c.xoff, c.yoff, c.children.clone())
        };
        let mut off = if ty == LayoutType::LeftRight { xoff } else { yoff };
        for child in children {
            let c = self.cell_mut(child);
            if ty == LayoutType::LeftRight {
                c.xoff = off;
                c.yoff = yoff;
                off = off.wrapping_add(c.sx).wrapping_add(1);
            } else {
                c.xoff = xoff;
                c.yoff = off;
                off = off.wrapping_add(c.sy).wrapping_add(1);
            }
            if self.cell(child).ty != LayoutType::Pane {
                self.fix_offsets1(child);
            }
        }
    }

    /// layout_fix_offsets
    pub(crate) fn fix_offsets(&mut self) {
        let root = self.root.expect("layout root");
        let c = self.cell_mut(root);
        c.xoff = 0;
        c.yoff = 0;
        self.fix_offsets1(root);
    }

    /// layout_fix_panes (no pane status lines or scrollbars in this model)
    pub(crate) fn fix_panes(&mut self, skip: Option<PaneId>) {
        for i in 0..self.panes.len() {
            let Some(lc) = self.panes[i].cell else { continue };
            if Some(self.panes[i].id) == skip {
                continue;
            }
            let (xoff, yoff, sx, sy) = {
                let c = self.cell(lc);
                (c.xoff, c.yoff, c.sx, c.sy)
            };
            let p = &mut self.panes[i];
            p.xoff = xoff;
            p.yoff = yoff;
            p.sx = sx;
            p.sy = sy;
        }
    }

    /// layout_resize_check: how much a cell can shrink.
    pub(crate) fn resize_check(&self, lc: CellId, ty: LayoutType) -> u32 {
        let c = self.cell(lc);
        if c.ty == LayoutType::Pane {
            let available = if ty == LayoutType::LeftRight { c.sx } else { c.sy };
            available.saturating_sub(PANE_MINIMUM)
        } else if c.ty == ty {
            c.children
                .iter()
                .fold(0u32, |acc, &ch| acc.wrapping_add(self.resize_check(ch, ty)))
        } else {
            c.children
                .iter()
                .map(|&ch| self.resize_check(ch, ty))
                .min()
                .unwrap_or(u32::MAX)
        }
    }

    /// layout_resize_adjust
    pub(crate) fn resize_adjust(&mut self, lc: CellId, ty: LayoutType, change: i32) {
        {
            let c = self.cell_mut(lc);
            if ty == LayoutType::LeftRight {
                c.sx = c.sx.wrapping_add(change as u32);
            } else {
                c.sy = c.sy.wrapping_add(change as u32);
            }
        }
        let children = self.cell(lc).children.clone();
        if self.cell(lc).ty != ty {
            for child in children {
                self.resize_adjust(child, ty, change);
            }
            return;
        }
        let mut change = change;
        while change != 0 {
            let mut progressed = false;
            for &child in &children {
                if change == 0 {
                    break;
                }
                if change > 0 {
                    self.resize_adjust(child, ty, 1);
                    change -= 1;
                    progressed = true;
                    continue;
                }
                if self.resize_check(child, ty) > 0 {
                    self.resize_adjust(child, ty, -1);
                    change += 1;
                    progressed = true;
                }
            }
            // tmux loops forever here; only layout_spread_cell does not
            // bound the change first.
            if !progressed {
                self.stuck = true;
                break;
            }
        }
    }

    /// layout_destroy_cell
    fn destroy_cell(&mut self, lc: CellId) {
        let Some(parent) = self.cell(lc).parent else {
            self.free_cell(lc);
            self.root = None;
            return;
        };
        let other = if self.cell(parent).children.first() == Some(&lc) {
            self.next_sibling(lc)
        } else {
            self.prev_sibling(lc)
        };
        if let Some(other) = other {
            let ty = self.cell(parent).ty;
            let size = self.size_of(lc, ty);
            self.resize_adjust(other, ty, size.wrapping_add(1) as i32);
        }
        self.cell_mut(parent).children.retain(|&c| c != lc);
        self.free_cell(lc);

        if self.cell(parent).children.len() == 1 {
            let only = self.cell_mut(parent).children.remove(0);
            let grandparent = self.cell(parent).parent;
            self.cell_mut(only).parent = grandparent;
            match grandparent {
                None => {
                    let c = self.cell_mut(only);
                    c.xoff = 0;
                    c.yoff = 0;
                    self.root = Some(only);
                }
                Some(gp) => {
                    for c in &mut self.cell_mut(gp).children {
                        if *c == parent {
                            *c = only;
                        }
                    }
                }
            }
            self.free_cell(parent);
        }
    }

    /// layout_init
    pub(crate) fn layout_init(&mut self, wp: PaneId) {
        let lc = self.create_cell(None);
        self.root = Some(lc);
        let (sx, sy) = (self.sx, self.sy);
        self.set_size(lc, sx, sy, 0, 0);
        self.make_leaf(lc, wp);
        self.fix_panes(None);
    }

    /// layout_free
    pub(crate) fn layout_free(&mut self) {
        if let Some(root) = self.root.take() {
            self.free_cell(root);
        }
    }

    /// layout_resize: resize the whole layout after the window was resized.
    pub(crate) fn layout_resize(&mut self, sx: u32, sy: u32) {
        let root = self.root.expect("layout root");
        for ty in [LayoutType::LeftRight, LayoutType::TopBottom] {
            let (want, cur) = if ty == LayoutType::LeftRight {
                (sx, self.cell(root).sx)
            } else {
                (sy, self.cell(root).sy)
            };
            let mut change = want.wrapping_sub(cur) as i32;
            let limit = self.resize_check(root, ty) as i32;
            if change < 0 && change < limit.wrapping_neg() {
                change = limit.wrapping_neg();
            }
            if limit == 0 {
                change = if want <= cur { 0 } else { want.wrapping_sub(cur) as i32 };
            }
            if change != 0 {
                self.resize_adjust(root, ty, change);
            }
        }
        self.fix_offsets();
        self.fix_panes(None);
    }

    /// layout_resize_layout
    fn resize_layout(&mut self, lc: CellId, ty: LayoutType, change: i32, opposite: bool) {
        let mut needed = change;
        while needed != 0 {
            let size = if change > 0 {
                let size = self.resize_pane_grow(lc, ty, needed, opposite);
                needed -= size;
                size
            } else {
                let size = self.resize_pane_shrink(lc, ty, needed);
                needed += size;
                size
            };
            if size == 0 {
                break;
            }
        }
        self.fix_offsets();
        self.fix_panes(None);
    }

    /// layout_resize_pane: resize a pane by `change` cells in direction `ty`.
    pub(crate) fn resize_pane(&mut self, wp: PaneId, ty: LayoutType, change: i32, opposite: bool) {
        let Some(mut lc) = self.pane(wp).cell else { return };
        let mut parent = self.cell(lc).parent;
        while let Some(p) = parent {
            if self.cell(p).ty == ty {
                break;
            }
            lc = p;
            parent = self.cell(p).parent;
        }
        if parent.is_none() {
            return;
        }
        if self.is_last_child(lc) {
            lc = self.prev_sibling(lc).expect("a node has two children");
        }
        self.resize_layout(lc, ty, change, opposite);
    }

    /// layout_resize_pane_grow
    fn resize_pane_grow(&mut self, lc: CellId, ty: LayoutType, needed: i32, opposite: bool) -> i32 {
        let mut size = 0u32;
        let mut remove = self.next_sibling(lc);
        while let Some(r) = remove {
            size = self.resize_check(r, ty);
            if size > 0 {
                break;
            }
            remove = self.next_sibling(r);
        }
        if opposite && remove.is_none() {
            remove = self.prev_sibling(lc);
            while let Some(r) = remove {
                size = self.resize_check(r, ty);
                if size > 0 {
                    break;
                }
                remove = self.prev_sibling(r);
            }
        }
        let Some(remove) = remove else { return 0 };
        if size > needed as u32 {
            size = needed as u32;
        }
        self.resize_adjust(lc, ty, size as i32);
        self.resize_adjust(remove, ty, (size as i32).wrapping_neg());
        size as i32
    }

    /// layout_resize_pane_shrink
    fn resize_pane_shrink(&mut self, lc: CellId, ty: LayoutType, needed: i32) -> i32 {
        let mut remove = Some(lc);
        let mut size = 0u32;
        while let Some(r) = remove {
            size = self.resize_check(r, ty);
            if size != 0 {
                break;
            }
            remove = self.prev_sibling(r);
        }
        let Some(remove) = remove else { return 0 };
        let Some(add) = self.next_sibling(lc) else { return 0 };
        if size > needed.wrapping_neg() as u32 {
            size = needed.wrapping_neg() as u32;
        }
        self.resize_adjust(add, ty, size as i32);
        self.resize_adjust(remove, ty, (size as i32).wrapping_neg());
        size as i32
    }

    /// layout_assign_pane
    pub(crate) fn assign_pane(&mut self, lc: CellId, wp: PaneId, do_not_resize: bool) {
        self.make_leaf(lc, wp);
        self.fix_panes(do_not_resize.then_some(wp));
    }

    /// layout_new_pane_size
    fn new_pane_size(
        &self,
        previous: u32,
        lc: CellId,
        ty: LayoutType,
        size: u32,
        count_left: u32,
        size_left: u32,
    ) -> u32 {
        if count_left == 1 {
            return size_left;
        }
        let available = self.resize_check(lc, ty);
        let mut min = (PANE_MINIMUM + 1).wrapping_mul(count_left - 1);
        let cur = self.size_of(lc, ty);
        if cur.wrapping_sub(available) > min {
            min = cur.wrapping_sub(available);
        }
        let mut new_size = cur.wrapping_mul(size).checked_div(previous).unwrap_or(0);
        let max = size_left.wrapping_sub(min);
        if new_size > max {
            new_size = max;
        }
        if new_size < PANE_MINIMUM {
            new_size = PANE_MINIMUM;
        }
        new_size
    }

    /// layout_set_size_check
    fn set_size_check(&self, lc: CellId, ty: LayoutType, size: i32) -> bool {
        let c = self.cell(lc);
        if c.ty == LayoutType::Pane {
            return size >= PANE_MINIMUM as i32;
        }
        let mut available = size as u32;
        let count = c.children.len() as u32;
        if c.ty == ty {
            if available < (count * 2) - 1 {
                return false;
            }
            let previous = self.size_of(lc, ty);
            for (idx, &child) in c.children.iter().enumerate() {
                let idx = idx as u32;
                let new_size =
                    self.new_pane_size(previous, child, ty, size as u32, count - idx, available);
                if idx == count - 1 {
                    if new_size > available {
                        return false;
                    }
                    available -= new_size;
                } else {
                    if new_size.wrapping_add(1) > available {
                        return false;
                    }
                    available = available.wrapping_sub(new_size.wrapping_add(1));
                }
                if !self.set_size_check(child, ty, new_size as i32) {
                    return false;
                }
            }
        } else {
            for &child in &c.children {
                if self.cell(child).ty == LayoutType::Pane {
                    continue;
                }
                if !self.set_size_check(child, ty, size) {
                    return false;
                }
            }
        }
        true
    }

    /// layout_resize_child_cells
    fn resize_child_cells(&mut self, lc: CellId) {
        let (ty, sx, sy, xoff, children) = {
            let c = self.cell(lc);
            (c.ty, c.sx, c.sy, c.xoff, c.children.clone())
        };
        if ty == LayoutType::Pane {
            return;
        }
        let count = children.len() as u32;
        let mut previous = 0u32;
        for &child in &children {
            previous = previous.wrapping_add(self.size_of(child, ty));
        }
        previous = previous.wrapping_add(count - 1);

        let mut available = if ty == LayoutType::LeftRight { sx } else { sy };
        for (idx, &child) in children.iter().enumerate() {
            let left = count - idx as u32;
            if ty == LayoutType::TopBottom {
                let c = self.cell_mut(child);
                c.sx = sx;
                c.xoff = xoff;
            } else {
                let new = self.new_pane_size(previous, child, ty, sx, left, available);
                self.cell_mut(child).sx = new;
                available = available.wrapping_sub(new.wrapping_add(1));
            }
            if ty == LayoutType::LeftRight {
                self.cell_mut(child).sy = sy;
            } else {
                let new = self.new_pane_size(previous, child, ty, sy, left, available);
                self.cell_mut(child).sy = new;
                available = available.wrapping_sub(new.wrapping_add(1));
            }
            self.resize_child_cells(child);
        }
    }

    /// layout_split_pane: split `wp`'s cell (or the whole window if `full`).
    /// `size` is the new pane's size or -1 for half. Returns the new empty
    /// cell, which must be given to [`Window::assign_pane`].
    pub(crate) fn split_pane(
        &mut self,
        wp: PaneId,
        ty: LayoutType,
        size: i32,
        before: bool,
        full: bool,
    ) -> Option<CellId> {
        let lc = if full { self.root? } else { self.pane(wp).cell? };
        let (sx, sy, xoff, yoff) = {
            let c = self.cell(lc);
            (c.sx, c.sy, c.xoff, c.yoff)
        };

        let minimum = PANE_MINIMUM * 2 + 1;
        let saved_size = if ty == LayoutType::LeftRight { sx } else { sy };
        if saved_size < minimum {
            return None;
        }

        let mut size2 = if size < 0 {
            saved_size.div_ceil(2) - 1
        } else if before {
            saved_size.wrapping_sub(size as u32).wrapping_sub(1)
        } else {
            size as u32
        };
        if size2 < PANE_MINIMUM {
            size2 = PANE_MINIMUM;
        } else if size2 > saved_size - 2 {
            size2 = saved_size - 2;
        }
        let size1 = saved_size - 1 - size2;
        let new_size = if before { size2 } else { size1 };

        if full && !self.set_size_check(lc, ty, new_size as i32) {
            return None;
        }

        let parent = self.cell(lc).parent;
        let mut resize_first = false;
        let new = if let Some(parent) = parent.filter(|&p| self.cell(p).ty == ty) {
            let new = self.create_cell(Some(parent));
            let children = &mut self.cell_mut(parent).children;
            let pos = children.iter().position(|&c| c == lc).expect("child of parent");
            children.insert(if before { pos } else { pos + 1 }, new);
            new
        } else if full && parent.is_none() && self.cell(lc).ty == ty {
            if ty == LayoutType::LeftRight {
                self.cell_mut(lc).sx = new_size;
                self.resize_child_cells(lc);
                self.cell_mut(lc).sx = saved_size;
            } else {
                self.cell_mut(lc).sy = new_size;
                self.resize_child_cells(lc);
                self.cell_mut(lc).sy = saved_size;
            }
            resize_first = true;

            let new = self.create_cell(Some(lc));
            let rest = saved_size - 1 - new_size;
            if ty == LayoutType::LeftRight {
                self.set_size(new, rest, sy, 0, 0);
            } else {
                self.set_size(new, sx, rest, 0, 0);
            }
            let children = &mut self.cell_mut(lc).children;
            if before {
                children.insert(0, new);
            } else {
                children.push(new);
            }
            new
        } else {
            let node = self.create_cell(parent);
            self.make_node(node, ty);
            self.set_size(node, sx, sy, xoff, yoff);
            match parent {
                None => self.root = Some(node),
                Some(p) => {
                    for c in &mut self.cell_mut(p).children {
                        if *c == lc {
                            *c = node;
                        }
                    }
                }
            }
            self.cell_mut(lc).parent = Some(node);
            self.cell_mut(node).children.insert(0, lc);
            let new = self.create_cell(Some(node));
            let children = &mut self.cell_mut(node).children;
            if before {
                children.insert(0, new);
            } else {
                children.push(new);
            }
            new
        };

        let (lc1, lc2) = if before { (new, lc) } else { (lc, new) };
        if !resize_first {
            if ty == LayoutType::LeftRight {
                self.set_size(lc1, size1, sy, xoff, yoff);
                let x2 = xoff.wrapping_add(self.cell(lc1).sx).wrapping_add(1);
                self.set_size(lc2, size2, sy, x2, yoff);
            } else {
                self.set_size(lc1, sx, size1, xoff, yoff);
                let y2 = yoff.wrapping_add(self.cell(lc1).sy).wrapping_add(1);
                self.set_size(lc2, sx, size2, xoff, y2);
            }
        }
        if full {
            if !resize_first {
                self.resize_child_cells(lc);
            }
            self.fix_offsets();
        } else {
            self.make_leaf(lc, wp);
        }
        Some(new)
    }

    /// layout_close_pane
    pub(crate) fn close_pane(&mut self, wp: PaneId) {
        if let Some(lc) = self.pane(wp).cell {
            self.destroy_cell(lc);
        }
        if self.root.is_some() {
            self.fix_offsets();
            self.fix_panes(None);
        }
    }

    /// layout_spread_cell
    fn spread_cell(&mut self, parent: CellId) -> bool {
        let (ty, size, children) = {
            let c = self.cell(parent);
            (c.ty, if c.ty == LayoutType::LeftRight { c.sx } else { c.sy }, c.children.clone())
        };
        let number = children.len() as u32;
        if number <= 1 || ty == LayoutType::Pane {
            return false;
        }
        if size < number - 1 {
            return false;
        }
        let each = (size - (number - 1)) / number;
        if each == 0 {
            return false;
        }
        let mut remainder = size.wrapping_sub(number * (each + 1)).wrapping_add(1);
        let mut changed = false;
        for child in children {
            let mut this = each;
            if remainder > 0 {
                this += 1;
                remainder -= 1;
            }
            let change = this as i32 - self.size_of(child, ty) as i32;
            self.resize_adjust(child, ty, change);
            if change != 0 {
                changed = true;
            }
        }
        changed
    }

    /// layout_spread_out (select-layout -E)
    pub(crate) fn spread_out(&mut self, wp: PaneId) {
        let Some(lc) = self.pane(wp).cell else { return };
        let mut parent = self.cell(lc).parent;
        while let Some(p) = parent {
            if self.spread_cell(p) {
                self.fix_offsets();
                self.fix_panes(None);
                break;
            }
            parent = self.cell(p).parent;
        }
    }

    /// Whether the tree is consistent: every cell is between 1 and
    /// `WINDOW_MAXIMUM` cells and children exactly tile their parent. tmux
    /// 3.6 can break this (see `split_pane` with `full`), which is refused.
    pub(crate) fn layout_valid(&self) -> bool {
        let Some(root) = self.root else { return true };
        self.cell_valid(root)
    }

    fn cell_valid(&self, lc: CellId) -> bool {
        let c = self.cell(lc);
        let in_range = |v: u32| (PANE_MINIMUM..=WINDOW_MAXIMUM).contains(&v);
        if !in_range(c.sx) || !in_range(c.sy) {
            return false;
        }
        if c.ty == LayoutType::Pane {
            return true;
        }
        let mut along = 0u64;
        for &child in &c.children {
            let ch = self.cell(child);
            let (main, cross) = if c.ty == LayoutType::LeftRight {
                (ch.sx, ch.sy == c.sy)
            } else {
                (ch.sy, ch.sx == c.sx)
            };
            if !cross || !self.cell_valid(child) {
                return false;
            }
            along += u64::from(main);
        }
        along += c.children.len() as u64 - 1;
        along == u64::from(if c.ty == LayoutType::LeftRight { c.sx } else { c.sy })
    }
}
