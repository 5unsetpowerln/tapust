//! A tmux-compatible model of one session: windows, panes, layouts, the key
//! tables and the status line, reproducing tmux 3.6 defaults with the
//! tmux-pain-control bindings on top.
//!
//! The model does no I/O. It tells the GUI what to do through [`Effect`]s
//! (spawn a shell, resize or destroy a pane, write bytes, exit), and the GUI
//! reads the visible layout, borders and status line from [`Session::view`].

use std::collections::BTreeMap;
use std::time::Instant;

mod format;
mod keys;
mod layout;
mod prompt;
mod redraw;
mod session;
mod status;
mod window;

#[cfg(test)]
mod tests;

use keys::KeyState;
use prompt::Prompt;
use window::Window;

/// Pane identifier (tmux `%N`), never reused within a session.
pub type PaneId = u32;

/// A rectangle in cells, relative to the top-left of the window area.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Rect {
    pub x: u16,
    pub y: u16,
    pub w: u16,
    pub h: u16,
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

/// One border cell between panes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BorderCell {
    pub x: u16,
    pub y: u16,
    /// The line-drawing character tmux draws here (U+2502 vertical, U+2500
    /// horizontal, U+253C cross, U+251C/U+2524/U+252C/U+2534 tees, ...).
    pub glyph: char,
    /// Drawn with `pane-active-border-style` (green) instead of `pane-border-style`.
    pub active: bool,
}

/// Colors of the status line and prompts.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum ChromeColor {
    #[default]
    Default,
    /// tmux color names as palette indices: black 0, red 1, green 2, yellow 3, ...
    Indexed(u8),
}

/// One cell of the status line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StatusCell {
    pub ch: char,
    pub fg: ChromeColor,
    pub bg: ChromeColor,
    pub bold: bool,
    pub underline: bool,
    pub reverse: bool,
}

/// The current window's panes and borders, and the status line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct View {
    pub panes: Vec<PaneView>,
    pub borders: Vec<BorderCell>,
    /// Exactly `cols` cells for the bottom row of the client.
    pub status: Vec<StatusCell>,
    /// Cursor column while a prompt is being edited on the status line.
    pub prompt_cursor: Option<u16>,
    /// The prompt or message on the status line, for clients that show it
    /// elsewhere (always set while one is active, with or without the status line).
    pub overlay: Option<Overlay>,
}

/// A prompt or message the status line would show.
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
    /// `split-window [-h] [-b] [-f] [-l size]`, with
    /// `-c "#{pane_current_path}"` when `cwd_of_active`.
    SplitWindow {
        horizontal: bool,
        before: bool,
        full: bool,
        size: Option<u32>,
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
    /// `resize-pane -L/-R/-U/-D adjustment`.
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
    /// `select-layout -E` (spread the active pane's cells out evenly).
    SpreadLayout,
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
    /// tmux's error message (as `%error` in control mode), if the command
    /// failed. The model also refuses the two layouts tmux 3.6 cannot handle
    /// (an overflowing `split-window -f`, a hanging `select-layout -E`) with
    /// an error saying so, leaving the session unchanged.
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
    /// The session's current window (`*`).
    pub active: bool,
    /// The previously current window (`-`).
    pub last: bool,
    pub zoomed: bool,
    pub width: u16,
    pub height: u16,
    pub panes: Vec<PaneInfo>,
}

/// One tmux session shown in one GUI window.
pub struct Session {
    name: String,
    cols: u16,
    rows: u16,
    /// tmux's `status` option.
    status: bool,
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
    /// A session with one window and one pane for a client of `cols x rows`
    /// cells (the status line takes the last row). Returns the first pane's
    /// `Spawn` effect.
    pub fn new(cols: u16, rows: u16) -> (Session, Vec<Effect>) {
        session::new_session(cols, rows, true)
    }

    /// Like [`Session::new`] with tmux's `status` option off: the windows
    /// use every row of the client.
    pub fn without_status_line(cols: u16, rows: u16) -> (Session, Vec<Effect>) {
        session::new_session(cols, rows, false)
    }

    /// The client (GUI window) was resized.
    pub fn resize_client(&mut self, cols: u16, rows: u16) -> Vec<Effect> {
        self.op(|s| {
            s.cols = cols;
            s.rows = rows;
            // Every window at once, as refresh-client -C does
            // (recalculate_sizes_now(1)); a tty client's resize in tmux
            // leaves other windows until they become current.
            s.recalculate_sizes(true);
        })
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

    /// Every pane of every window with its current size (cols, rows).
    pub fn pane_sizes(&self) -> Vec<(PaneId, u16, u16)> {
        self.winlinks
            .values()
            .flat_map(|w| w.panes.iter())
            .map(|p| (p.id, clamp16(p.sx), clamp16(p.sy)))
            .collect()
    }

    /// Changes whenever anything returned by [`Session::view`] may have changed.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// The visible state; `clock` is the formatted `%H:%M %d-%b-%y` text.
    pub fn view(&self, clock: &str) -> View {
        let (status, prompt_cursor) = self.status_line(clock);
        if self.dead {
            return View { panes: Vec::new(), borders: Vec::new(), status, prompt_cursor, overlay: self.overlay() };
        }
        let w = self.cur();
        let panes = w
            .panes
            .iter()
            .filter(|p| w.visible(p.id))
            .map(|p| PaneView { pane: p.id, rect: rect_of(p), active: p.id == w.active })
            .collect();
        View { panes, borders: self.borders(), status, prompt_cursor, overlay: self.overlay() }
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
                active: index == self.curw && !self.dead,
                last: Some(index) == last,
                zoomed: w.zoomed,
                width: clamp16(w.sx),
                height: clamp16(w.sy),
                panes: w
                    .panes
                    .iter()
                    .map(|p| PaneInfo {
                        pane: p.id,
                        rect: rect_of(p),
                        active: p.id == w.active,
                        title: p.title.clone(),
                    })
                    .collect(),
            })
            .collect()
    }
}

fn clamp16(v: u32) -> u16 {
    u16::try_from(v).unwrap_or(u16::MAX)
}

fn rect_of(p: &window::Pane) -> Rect {
    Rect { x: clamp16(p.xoff), y: clamp16(p.yoff), w: clamp16(p.sx), h: clamp16(p.sy) }
}
