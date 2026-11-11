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

pub use split::{f16, Node, Split};

use keys::KeyState;
use prompt::Prompt;
use window::Window;

/// Pane identifier (tmux `%N`), never reused within a session.
pub type PaneId = u32;

/// A rectangle in logical pixels (GTK's widget coordinates), relative to the
/// top-left of a window's split tree.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
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
    /// A new pane: make its surface, and start a shell once GTK gives it a
    /// size. `cwd_of` names the pane whose current directory
    /// (`#{pane_current_path}`) to start in; `None` means the session's
    /// default directory.
    Spawn { pane: PaneId, cwd_of: Option<PaneId> },
    /// `resize-pane`: move the divider nearest to `pane` with that
    /// orientation by `cells` cells (negative is left or up), as ghostty's
    /// resize_split does with pixels; the GUI answers with [`Session::resize_split`].
    ResizeSplit { pane: PaneId, horizontal: bool, cells: i32 },
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
    /// tmux `@N`, which stays with the window when its index changes.
    pub id: u32,
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
    /// A session with one window and one pane. Returns the first pane's
    /// `Spawn` effect.
    pub fn new() -> (Session, Vec<Effect>) {
        session::new_session()
    }

    /// The split tree of the window at `index`, with its zoomed pane (GTK
    /// then shows only that pane) and its active pane.
    pub fn tree(&self, index: u32) -> Option<(&Node, Option<PaneId>, PaneId)> {
        let w = self.winlinks.get(&index)?;
        Some((&w.tree, w.zoomed.then_some(w.active), w.active))
    }

    /// GTK allocated the split tree of the window at `index`: its size, and
    /// for each pane the rectangle and the grid it holds (tmux's directional
    /// selection and formats read them).
    pub fn set_geometry(&mut self, index: u32, area: Rect, panes: &[(PaneId, Rect, u16, u16)]) {
        let Some(w) = self.winlinks.get_mut(&index) else { return };
        w.area = area;
        for &(wp, rect, cols, rows) in panes {
            if let Some(p) = w.pane_mut_opt(wp) {
                (p.rect, p.cols, p.rows) = (rect, cols, rows);
            }
        }
    }

    /// ghostty's resize_split after a `ResizeSplit` effect: `delta` is the
    /// move as a share of the tree's extent (pixels over the tree's width or
    /// height, the sign of the effect's cells).
    pub fn resize_split(&mut self, pane: PaneId, horizontal: bool, delta: f64) -> Vec<Effect> {
        self.op(|s| {
            if let Some(idx) = s.window_of(pane) {
                s.winlinks.get_mut(&idx).expect("window").resize_split(pane, horizontal, delta);
            }
        })
    }

    /// A split widget writes its ratio back (ghostty's setTreeRatio after a
    /// drag or a clamp): the tree keeps it as f16. The widgets stay, so the
    /// generation does not change.
    pub fn set_ratio(&mut self, index: u32, split: usize, ratio: f64) {
        if let Some(s) = self.winlinks.get_mut(&index).and_then(|w| w.tree.split_at(split)) {
            s.ratio = f16(ratio);
        }
    }

    /// A pane got the keyboard focus (a click on it).
    pub fn focus_pane(&mut self, pane: PaneId) -> Vec<Effect> {
        self.op(|s| s.focus(pane))
    }

    /// Drops pane `source` on the `dir` side of pane `target` (ghostty's
    /// drag and drop of a split).
    pub fn move_pane(&mut self, source: PaneId, target: PaneId, dir: Direction) -> Vec<Effect> {
        self.op(|s| s.drop_pane(source, target, dir))
    }

    /// Answers the prompt [`Session::overlay`] shows, from a dialog: `Some`
    /// confirms or accepts the text, `None` cancels.
    pub fn answer(&mut self, reply: Option<&str>, now: Instant) -> Vec<Effect> {
        self.op(|s| s.prompt_answer(reply, now))
    }

    /// The current window's index (the selected tab).
    pub fn current(&self) -> u32 {
        self.curw
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

    /// rename-window -t @`id`.
    pub fn rename_window_id(&mut self, id: u32, name: &str) {
        self.rename_window(id, name);
        self.generation += 1;
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

    /// The prompt or message tmux would show on the status line, which the
    /// GUI shows as a dialog or a toast.
    pub fn overlay(&self) -> Option<Overlay> {
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
                id: w.id,
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
