//! A tmux-compatible model of one session: windows, panes and the key
//! tables, reproducing tmux 3.6 defaults with the tmux-pain-control bindings
//! on top. Panes are split as ghostty splits them: a tree of ratios laid out
//! in pixels with a thin divider, instead of tmux's cell grid with borders.
//!
//! The model does no I/O. It tells the GUI what to do through [`Effect`]s
//! (spawn a shell, resize or destroy a pane, write bytes, exit), and the GUI
//! reads the visible panes and dividers from [`Session::view`].

use std::collections::BTreeMap;
use std::time::Instant;

mod format;
mod keys;
mod prompt;
mod session;
mod split;
mod window;

#[cfg(test)]
mod tests;

pub use split::{Grab, Hit, DRAG_HANDLE_HEIGHT};

use keys::KeyState;
use prompt::Prompt;
use window::Window;

/// Pane identifier (tmux `%N`), never reused within a session.
pub type PaneId = u32;

/// A rectangle in logical pixels (GTK's widget coordinates), relative to the
/// top-left of the area the panes share.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

/// A rectangle in device pixels, relative to the window.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct DeviceRect {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
}

/// The client's geometry. Splits are laid out in logical pixels, as GTK
/// allocates ghostty's widgets; terminal grids are measured in the device
/// pixels each pane covers.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Metrics {
    /// The area the panes of a window share, in logical pixels.
    pub width: u32,
    pub height: u32,
    /// Where that area starts in the window (below the tab bar), in logical pixels.
    pub top: u32,
    /// Width of the divider between split panes, in logical pixels.
    pub divider: u32,
    /// The smallest a pane may get, in logical pixels.
    pub min_width: u32,
    pub min_height: u32,
    /// Device pixels per logical pixel.
    pub scale: f64,
    /// The cell size and the space on each side of a pane's grid (ghostty's
    /// `window-padding-x`/`-y`), in device pixels.
    pub cell_width: u32,
    pub cell_height: u32,
    pub padding_x: u32,
    pub padding_y: u32,
}

impl Metrics {
    pub fn area(&self) -> Rect {
        Rect { x: 0, y: 0, w: self.width, h: self.height }
    }

    /// The device pixels a pane at `rect` covers: ghostty rounds both edges
    /// (`snappedAxis` in its GTK scale.zig), half away from zero.
    pub fn device(&self, rect: Rect) -> DeviceRect {
        let s = self.scale;
        let snap = |origin: u32, size: u32| {
            let start = (f64::from(origin) * s).round();
            let end = (f64::from(origin + size) * s).round();
            (start as i32, (end - start).max(0.0) as u32)
        };
        let (x, w) = snap(rect.x, rect.w);
        let (y, h) = snap(self.top + rect.y, rect.h);
        DeviceRect { x, y, w, h }
    }

    /// The terminal grid that fits in a pane at `rect`, at least one cell.
    pub fn grid(&self, rect: Rect) -> (u16, u16) {
        let d = self.device(rect);
        let fit = |size: u32, padding: u32, cell: u32| {
            let n = size.saturating_sub(2 * padding) / cell.max(1);
            u16::try_from(n.max(1)).unwrap_or(u16::MAX)
        };
        (fit(d.w, self.padding_x, self.cell_width), fit(d.h, self.padding_y, self.cell_height))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Direction {
    Left,
    Right,
    Up,
    Down,
}

/// A key after keyboard layout processing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Key {
    pub code: KeyCode,
    pub mods: Mods,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum KeyCode {
    /// A character key with Shift already applied (`'H'`, `'|'`, `'_'`), so
    /// `Mods::shift` is not set for characters. Control combinations are the
    /// base character with `ctrl` (`C-b` is `Char('b')` + ctrl).
    Char(char),
    Enter,
    Escape,
    Tab,
    Backspace,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
    Insert,
    Delete,
    F(u8),
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Mods {
    pub ctrl: bool,
    /// Alt (tmux `M-`).
    pub meta: bool,
    /// Only meaningful for non-character keys (`S-Up`, `BTab`).
    pub shift: bool,
}

/// What the GUI must do after the model changed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Effect {
    /// Start a shell for a new pane. `cwd_of` names the pane whose current
    /// directory (`#{pane_current_path}`) to start in; `None` means the
    /// session's default directory.
    Spawn { pane: PaneId, cols: u16, rows: u16, cwd_of: Option<PaneId> },
    /// The pane's size changed; resize its PTY and terminal.
    Resize { pane: PaneId, cols: u16, rows: u16 },
    /// The pane was removed; kill its process and drop it.
    Destroy { pane: PaneId },
    /// Write bytes to the pane's PTY (e.g. `send-prefix`).
    Write { pane: PaneId, bytes: Vec<u8> },
    /// No windows are left; the program should exit.
    Exit,
}

/// Result of offering a key to the multiplexer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KeyResult {
    /// Not for the multiplexer: encode the key and send it to this pane.
    Forward(PaneId),
    /// Consumed by the multiplexer (possibly with no effects, like the prefix key).
    Handled(Vec<Effect>),
}

/// A pane visible in the current window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PaneView {
    pub pane: PaneId,
    pub rect: Rect,
    pub active: bool,
}

/// The current window's panes and the dividers between them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct View {
    pub panes: Vec<PaneView>,
    pub dividers: Vec<Divider>,
    /// The window is split, zoomed or not (ghostty's `is-split`).
    pub split: bool,
    /// The prompt or message tmux would show on the status line.
    pub overlay: Option<Overlay>,
}

/// The divider of a split. GtkPaned draws its first child, the divider and
/// then its second child, so a divider is drawn after the first `after`
/// panes of the view and before the others.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Divider {
    pub rect: Rect,
    pub after: usize,
    /// The split puts its panes side by side (the divider is a vertical line).
    pub horizontal: bool,
}

/// A prompt or message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Overlay {
    /// `confirm-before`: answered with `y`; any other key cancels.
    Confirm { prompt: String, target: ConfirmTarget },
    /// `command-prompt`: the label, the text being edited and the cursor position in characters.
    Input { prompt: String, text: String, cursor: usize },
    /// `display-message` or an error.
    Message(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConfirmTarget {
    Pane,
    Window,
}

/// A command of the model, named after the tmux command and flags it
/// reproduces. Every command acts on the current window and its active pane,
/// as a key binding does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    /// `split-window [-h] [-b] [-f]`, with `-c "#{pane_current_path}"` when
    /// `cwd_of_active`. The new pane gets half of the space, as in ghostty.
    SplitWindow {
        horizontal: bool,
        before: bool,
        full: bool,
        cwd_of_active: bool,
    },
    /// `new-window`, with `-c "#{pane_current_path}"` when `cwd_of_active`.
    NewWindow { cwd_of_active: bool },
    /// `select-pane -L/-R/-U/-D`.
    SelectPane(Direction),
    /// `select-pane -l` (`last-pane`).
    LastPane,
    /// `select-pane -t :.+`.
    NextPane,
    /// `resize-pane -L/-R/-U/-D adjustment`: moves the nearest divider of that
    /// orientation by `adjustment` cells, as ghostty's resize_split moves it.
    ResizePane(Direction, u32),
    /// `resize-pane -Z`.
    ZoomPane,
    /// `kill-pane`.
    KillPane,
    /// `kill-window`.
    KillWindow,
    /// `break-pane`.
    BreakPane,
    /// `swap-pane -U` (`up`) or `swap-pane -D`.
    SwapPane { up: bool },
    /// `rotate-window` (`-U`) or `rotate-window -D` (`down`).
    RotateWindow { down: bool },
    /// `select-window -t :=index`.
    SelectWindow(u32),
    /// `next-window`.
    NextWindow,
    /// `previous-window`.
    PreviousWindow,
    /// `last-window`.
    LastWindow,
    /// `swap-window -d -t -N` (negative) or `-t +N` (positive).
    SwapWindow(i32),
    /// `select-layout -E`, carried out as ghostty's equalize_splits.
    Equalize,
    /// `rename-window name` (the name is format-expanded as tmux does).
    RenameWindow(String),
    /// `rename-session name`.
    RenameSession(String),
    /// `send-prefix`.
    SendPrefix,
}

/// Result of [`Session::command`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandOutput {
    pub effects: Vec<Effect>,
    /// tmux's error message, if the command failed.
    pub error: Option<String>,
}

/// A pane as `list-panes` reports it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PaneInfo {
    pub pane: PaneId,
    /// The pane's last laid-out position; hidden panes of a zoomed window
    /// keep the one they had before the zoom, as in tmux.
    pub rect: Rect,
    pub active: bool,
    pub title: String,
}

/// A window as `list-windows` reports it, with its panes in index order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WindowInfo {
    pub index: u32,
    pub name: String,
    /// The name was set with rename-window, which turns automatic-rename off.
    pub renamed: bool,
    /// The session's current window (`*`).
    pub active: bool,
    /// The previously current window (`-`).
    pub last: bool,
    pub zoomed: bool,
    pub panes: Vec<PaneInfo>,
}

/// One tmux session shown in one GUI window.
pub struct Session {
    name: String,
    metrics: Metrics,
    /// Winlinks by index; a winlink is identified by its index.
    winlinks: BTreeMap<u32, Window>,
    curw: u32,
    /// `s->lastw`, most recent first.
    lastw: Vec<u32>,
    next_pane_id: PaneId,
    next_window_id: u32,
    /// tmux's global `next_active_point`.
    next_active_point: u32,
    keys: KeyState,
    prompt: Option<Prompt>,
    message: Option<(String, Instant)>,
    /// tmux's command prompt history.
    history: Vec<String>,
    generation: u64,
    /// No windows are left.
    dead: bool,
    /// The active pane when the session died.
    last_active: PaneId,
    /// Panes created by the running operation, with `cwd_of`.
    spawned: Vec<(PaneId, Option<PaneId>)>,
    /// Write effects of the running operation.
    writes: Vec<Effect>,
}

impl Session {
    /// A session with one window and one pane for a client of `metrics`.
    /// Returns the first pane's `Spawn` effect.
    pub fn new(metrics: Metrics) -> (Session, Vec<Effect>) {
        session::new_session(metrics)
    }

    /// The client (GUI window) was resized, or its cell size changed. The
    /// splits keep their ratios.
    pub fn resize(&mut self, metrics: Metrics) -> Vec<Effect> {
        self.op(|s| s.set_metrics(metrics))
    }

    /// The smallest pane area the windows can be laid out in, in logical
    /// pixels; the GUI makes it the window's minimum size, as GTK does.
    pub fn min_area(&self) -> (u32, u32) {
        self.min_area_of_windows()
    }

    /// What the pointer at (`x`, `y`) is over, in logical pixels of the pane area.
    pub fn hit(&self, x: f64, y: f64) -> Option<Hit> {
        if self.dead { None } else { self.cur().hit((x, y)) }
    }

    /// The divider a pointer press at (`x`, `y`) starts dragging, if any.
    pub fn grab_divider(&self, x: f64, y: f64) -> Option<Grab> {
        if self.dead { None } else { self.cur().grab((x, y)) }
    }

    /// Moves a grabbed divider with the pointer at (`x`, `y`).
    pub fn drag_divider(&mut self, grab: Grab, x: f64, y: f64) -> Vec<Effect> {
        self.op(|s| s.cur_mut().tree.drag(grab, (x, y)))
    }

    /// ghostty's idle callbacks: dragged dividers write their ratio to the
    /// tree. The GUI calls this when it has handled the pending events.
    pub fn idle(&mut self) {
        if !self.dead {
            let w = self.cur_mut();
            let m = w.metrics;
            w.tree.sync_dragged(&m);
        }
    }

    /// A click on a pane focuses it.
    pub fn focus_pane(&mut self, pane: PaneId) -> Vec<Effect> {
        self.op(|s| s.focus(pane))
    }

    /// Drops pane `source` on the `dir` side of pane `target` (ghostty's
    /// drag and drop of a split).
    pub fn move_pane(&mut self, source: PaneId, target: PaneId, dir: Direction) -> Vec<Effect> {
        self.op(|s| s.drop_pane(source, target, dir))
    }

    /// Offers a key to the key tables (prefix, repeat, prompts).
    pub fn key(&mut self, key: Key, now: Instant) -> KeyResult {
        self.handle_key(key, now)
    }

    /// When the current timed state (the repeat window of a `-r` binding, or
    /// a message) ends; the GUI calls [`Session::tick`] then.
    pub fn deadline(&self) -> Option<Instant> {
        let repeat = self.keys.repeat_deadline();
        let message = self.message.as_ref().map(|m| m.1);
        match (repeat, message) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    /// Expires timed state whose deadline passed.
    pub fn tick(&mut self, now: Instant) {
        self.keys.expire(now);
        if self.message.as_ref().is_some_and(|m| m.1 <= now) {
            self.message = None;
            self.generation += 1;
        }
    }

    /// The pane's process exited: remove it as tmux does with remain-on-exit off.
    pub fn pane_exited(&mut self, pane: PaneId) -> Vec<Effect> {
        self.op(|s| s.destroy_pane(pane))
    }

    /// Sets `#{pane_title}` (OSC 0/2; tmux defaults it to the host name).
    pub fn set_pane_title(&mut self, pane: PaneId, title: &str) {
        if let Some(p) = self.find_pane_mut(pane)
            && p.title != title
        {
            p.title = title.to_owned();
            self.generation += 1;
        }
    }

    /// Reports the foreground process name of `pane` for automatic-rename;
    /// it names the window while the pane is the window's active pane.
    /// Ignored for windows renamed by the user.
    pub fn set_auto_name(&mut self, pane: PaneId, name: &str) {
        let name = format::vis(name);
        if let Some(p) = self.find_pane_mut(pane)
            && p.auto_name != name
        {
            p.auto_name = name;
            self.generation += 1;
        }
    }

    /// The pane that receives forwarded keys.
    pub fn active_pane(&self) -> PaneId {
        if self.dead { self.last_active } else { self.cur().active }
    }

    /// Changes whenever anything returned by [`Session::view`] may have changed.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// The visible state of the current window.
    pub fn view(&self) -> View {
        if self.dead {
            return View { panes: Vec::new(), dividers: Vec::new(), split: false, overlay: self.overlay() };
        }
        let w = self.cur();
        let panes = w
            .panes
            .iter()
            .filter(|p| w.visible(p.id))
            .map(|p| PaneView { pane: p.id, rect: p.rect, active: p.id == w.active })
            .collect();
        View { panes, dividers: w.dividers.clone(), split: w.panes.len() > 1, overlay: self.overlay() }
    }

    fn overlay(&self) -> Option<Overlay> {
        if let Some((text, _)) = &self.message {
            return Some(Overlay::Message(text.clone()));
        }
        let p = self.prompt.as_ref()?;
        let prompt = format::drawn_text(&self.prompt_text(p));
        Some(match p.kind {
            prompt::PromptKind::ConfirmKillPane { .. } => Overlay::Confirm { prompt, target: ConfirmTarget::Pane },
            prompt::PromptKind::ConfirmKillWindow { .. } => Overlay::Confirm { prompt, target: ConfirmTarget::Window },
            _ => Overlay::Input { prompt, text: p.buffer.iter().collect(), cursor: p.index },
        })
    }

    /// Runs a command on the current window and pane. Errors are returned
    /// instead of being shown on the status line.
    pub fn command(&mut self, cmd: &Command) -> CommandOutput {
        let mut error = None;
        let effects = self.op(|s| error = s.exec(cmd).err());
        CommandOutput { effects, error }
    }

    /// The windows in index order, as `list-windows` and `list-panes` report them.
    pub fn windows(&self) -> Vec<WindowInfo> {
        let last = self.lastw.first().copied();
        self.winlinks
            .iter()
            .map(|(&index, w)| WindowInfo {
                index,
                name: w.name().to_owned(),
                renamed: w.fixed_name.is_some(),
                active: index == self.curw && !self.dead,
                last: Some(index) == last,
                zoomed: w.zoomed,
                panes: w
                    .panes
                    .iter()
                    .map(|p| PaneInfo {
                        pane: p.id,
                        rect: p.rect,
                        active: p.id == w.active,
                        title: p.title.clone(),
                    })
                    .collect(),
            })
            .collect()
    }
}
