//! A tmux-compatible model of one session: windows, panes, layouts, the key
//! tables and the status line, reproducing tmux 3.6 defaults with the
//! tmux-pain-control bindings on top.
//!
//! The model does no I/O. It tells the GUI what to do through [`Effect`]s
//! (spawn a shell, resize or destroy a pane, write bytes, exit), and the GUI
//! reads the visible layout, borders and status line from [`Session::view`].

use std::time::Instant;

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
}

/// One tmux session shown in one GUI window.
pub struct Session {
    // Implementation-defined.
}

impl Session {
    /// A session with one window and one pane for a client of `cols x rows`
    /// cells (the status line takes the last row). Returns the first pane's
    /// `Spawn` effect.
    pub fn new(cols: u16, rows: u16) -> (Session, Vec<Effect>) {
        let _ = (cols, rows);
        todo!()
    }

    /// The client (GUI window) was resized.
    pub fn resize_client(&mut self, cols: u16, rows: u16) -> Vec<Effect> {
        let _ = (cols, rows);
        todo!()
    }

    /// Offers a key to the key tables (prefix, repeat, prompts).
    pub fn key(&mut self, key: Key, now: Instant) -> KeyResult {
        let _ = (key, now);
        todo!()
    }

    /// When the current timed state (the repeat window of a `-r` binding, or
    /// a message) ends; the GUI calls [`Session::tick`] then.
    pub fn deadline(&self) -> Option<Instant> {
        todo!()
    }

    /// Expires timed state whose deadline passed.
    pub fn tick(&mut self, now: Instant) {
        let _ = now;
        todo!()
    }

    /// The pane's process exited: remove it as tmux does with remain-on-exit off.
    pub fn pane_exited(&mut self, pane: PaneId) -> Vec<Effect> {
        let _ = pane;
        todo!()
    }

    /// Sets `#{pane_title}` (OSC 0/2; tmux defaults it to the host name).
    pub fn set_pane_title(&mut self, pane: PaneId, title: &str) {
        let _ = (pane, title);
        todo!()
    }

    /// Reports the foreground process name of `pane` for automatic-rename;
    /// it names the window while the pane is the window's active pane.
    /// Ignored for windows renamed by the user.
    pub fn set_auto_name(&mut self, pane: PaneId, name: &str) {
        let _ = (pane, name);
        todo!()
    }

    /// The pane that receives forwarded keys.
    pub fn active_pane(&self) -> PaneId {
        todo!()
    }

    /// Every pane of every window with its current size (cols, rows).
    pub fn pane_sizes(&self) -> Vec<(PaneId, u16, u16)> {
        todo!()
    }

    /// Changes whenever anything returned by [`Session::view`] may have changed.
    pub fn generation(&self) -> u64 {
        todo!()
    }

    /// The visible state; `clock` is the formatted `%H:%M %d-%b-%y` text.
    pub fn view(&self, clock: &str) -> View {
        let _ = clock;
        todo!()
    }
}
