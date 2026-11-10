//! Key tables and the prefix/repeat state machine, ported from tmux 3.6
//! `server_client_handle_key` and `server_client_key_callback`
//! (server-client.c), with the default bindings of `key_bindings_init`
//! (key-bindings.c) overlaid by tmux-pain-control.

use std::time::{Duration, Instant};

use crate::prompt::PromptKind;
use crate::{Command, Direction, Key, KeyCode, KeyResult, Session};

/// The `repeat-time` option.
pub(crate) const REPEAT_TIME: Duration = Duration::from_millis(500);
/// The `display-time` option.
pub(crate) const DISPLAY_TIME: Duration = Duration::from_millis(750);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Table {
    #[default]
    Root,
    Prefix,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct KeyState {
    pub table: Table,
    /// `CLIENT_REPEAT`, with the expiry of the repeat timer.
    pub repeat: Option<Instant>,
}

impl KeyState {
    pub(crate) fn repeat_deadline(&self) -> Option<Instant> {
        self.repeat
    }

    /// server_client_repeat_timer
    pub(crate) fn expire(&mut self, now: Instant) {
        if self.repeat.is_some_and(|deadline| deadline <= now) {
            self.repeat = None;
            self.table = Table::Root;
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Action {
    Run(Command),
    /// `command-prompt -I "#W" { rename-window "%%" }` or the session variant.
    RenamePrompt { session: bool },
    /// `confirm-before -p "kill-pane #P? (y/n)" kill-pane` or the window variant.
    ConfirmKill { window: bool },
    /// `next-window -a` / `previous-window -a`; no window has an alert in
    /// this model, so they fail as tmux does then.
    AlertWindow { next: bool },
    /// A default binding the model does not implement: the key is consumed.
    Unsupported,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Binding {
    pub repeat: bool,
    pub action: Action,
}

fn bind(repeat: bool, action: Action) -> Option<Binding> {
    Some(Binding { repeat, action })
}

fn run(cmd: Command) -> Option<Binding> {
    bind(false, Action::Run(cmd))
}

fn split(horizontal: bool, full: bool) -> Option<Binding> {
    run(Command::SplitWindow { horizontal, before: false, full, cwd_of_active: true })
}

pub(crate) fn is_prefix(key: Key) -> bool {
    key.code == KeyCode::Char('b') && key.mods.ctrl && !key.mods.meta
}

/// The prefix table: tmux 3.6 defaults with tmux-pain-control on top.
pub(crate) fn prefix_binding(key: Key) -> Option<Binding> {
    use Action::Unsupported;
    use Direction::{Down, Left, Right, Up};
    let m = key.mods;
    match key.code {
        KeyCode::Char(c) if !m.ctrl && !m.meta => match c {
            '!' => run(Command::BreakPane),
            '"' | '-' => split(false, false),
            '%' | '|' => split(true, false),
            '\\' => split(true, true),
            '_' => split(false, true),
            '$' => bind(false, Action::RenamePrompt { session: true }),
            ',' => bind(false, Action::RenamePrompt { session: false }),
            '&' => bind(false, Action::ConfirmKill { window: true }),
            'x' => bind(false, Action::ConfirmKill { window: false }),
            '0'..='9' => run(Command::SelectWindow(c as u32 - '0' as u32)),
            ';' => run(Command::LastPane),
            '<' => bind(true, Action::Run(Command::SwapWindow(-1))),
            '>' => bind(true, Action::Run(Command::SwapWindow(1))),
            'E' => run(Command::Equalize),
            'H' => bind(true, Action::Run(Command::ResizePane(Left, 5))),
            'J' => bind(true, Action::Run(Command::ResizePane(Down, 5))),
            'K' => bind(true, Action::Run(Command::ResizePane(Up, 5))),
            'L' => bind(true, Action::Run(Command::ResizePane(Right, 5))),
            'c' => run(Command::NewWindow { cwd_of_active: true }),
            'h' => run(Command::SelectPane(Left)),
            'j' => run(Command::SelectPane(Down)),
            'k' => run(Command::SelectPane(Up)),
            'l' => run(Command::SelectPane(Right)),
            'n' => run(Command::NextWindow),
            'o' => run(Command::NextPane),
            'p' => run(Command::PreviousWindow),
            'z' => run(Command::ZoomPane),
            '{' => run(Command::SwapPane { up: true }),
            '}' => run(Command::SwapPane { up: false }),
            ' ' | '#' | '\'' | '(' | ')' | '.' | '/' | ':' | '=' | '?' | 'C' | 'D' | 'M'
            | '[' | ']' | 'd' | 'f' | 'i' | 'm' | 'q' | 'r' | 's' | 't' | 'w' | '~' => {
                bind(false, Unsupported)
            }
            _ => None,
        },
        KeyCode::Char(c) if m.ctrl && !m.meta => match c {
            'b' => run(Command::SendPrefix),
            'o' => run(Command::RotateWindow { down: false }),
            'h' => run(Command::SelectPane(Left)),
            'j' => run(Command::SelectPane(Down)),
            'k' => run(Command::SelectPane(Up)),
            'l' => run(Command::SelectPane(Right)),
            'z' => bind(false, Unsupported),
            _ => None,
        },
        KeyCode::Char(c) if m.meta && !m.ctrl => match c {
            '1'..='7' => bind(false, Unsupported),
            'n' => bind(false, Action::AlertWindow { next: true }),
            'p' => bind(false, Action::AlertWindow { next: false }),
            'o' => run(Command::RotateWindow { down: true }),
            _ => None,
        },
        KeyCode::Up | KeyCode::Down | KeyCode::Left | KeyCode::Right => {
            let dir = match key.code {
                KeyCode::Up => Up,
                KeyCode::Down => Down,
                KeyCode::Left => Left,
                _ => Right,
            };
            match (m.ctrl, m.meta, m.shift) {
                (false, false, false) => bind(true, Action::Run(Command::SelectPane(dir))),
                (false, true, false) => bind(true, Action::Run(Command::ResizePane(dir, 5))),
                (true, false, false) => bind(true, Action::Run(Command::ResizePane(dir, 1))),
                (false, false, true) => bind(true, Unsupported),
                _ => None,
            }
        }
        KeyCode::Delete if m == Default::default() => bind(true, Unsupported),
        KeyCode::PageUp if m == Default::default() => bind(false, Unsupported),
        _ => None,
    }
}

impl Session {
    /// server_client_handle_key followed by server_client_key_callback.
    pub(crate) fn handle_key(&mut self, key: Key, now: Instant) -> KeyResult {
        self.tick(now);
        if self.dead {
            return KeyResult::Handled(Vec::new());
        }
        if self.message.take().is_some() {
            self.generation += 1;
        }
        if self.prompt.is_some() {
            return KeyResult::Handled(self.op(|s| s.prompt_key(key, now)));
        }

        let mut table = self.keys.table;
        let mut first = table;
        loop {
            if is_prefix(key) && table != Table::Prefix {
                self.keys.table = Table::Prefix;
                return KeyResult::Handled(Vec::new());
            }
            let repeating = self.keys.repeat.is_some();

            let binding = if table == Table::Prefix { prefix_binding(key) } else { None };
            if let Some(binding) = binding {
                if repeating && !binding.repeat {
                    self.keys.table = Table::Root;
                    table = Table::Root;
                    first = Table::Root;
                    self.keys.repeat = None;
                    continue;
                }
                if binding.repeat {
                    self.keys.repeat = Some(now + REPEAT_TIME);
                } else {
                    self.keys.repeat = None;
                    self.keys.table = Table::Root;
                }
                return KeyResult::Handled(self.op(|s| s.dispatch(binding.action, now)));
            }

            if table != Table::Root || repeating {
                self.keys.table = Table::Root;
                table = Table::Root;
                if repeating {
                    first = table;
                }
                self.keys.repeat = None;
                continue;
            }
            if first != table && !repeating {
                self.keys.table = Table::Root;
                return KeyResult::Handled(Vec::new());
            }
            return KeyResult::Forward(self.cur().active);
        }
    }

    fn dispatch(&mut self, action: Action, now: Instant) {
        let result = match action {
            Action::Run(cmd) => self.exec(&cmd),
            Action::RenamePrompt { session: true } => {
                self.open_prompt(PromptKind::RenameSession);
                Ok(())
            }
            Action::RenamePrompt { session: false } => {
                let window = self.cur().id;
                self.open_prompt(PromptKind::RenameWindow { window });
                Ok(())
            }
            Action::ConfirmKill { window: true } => {
                let window = self.cur().id;
                self.open_prompt(PromptKind::ConfirmKillWindow { window });
                Ok(())
            }
            Action::ConfirmKill { window: false } => {
                let pane = self.cur().active;
                self.open_prompt(PromptKind::ConfirmKillPane { pane });
                Ok(())
            }
            Action::AlertWindow { next: true } => Err("no next window".to_owned()),
            Action::AlertWindow { next: false } => Err("no previous window".to_owned()),
            Action::Unsupported => Ok(()),
        };
        if let Err(error) = result {
            self.show_error(&error, now);
        }
    }

    /// cmdq_error for a command run by an attached client: the message is
    /// shown for display-time with its first letter capitalised.
    pub(crate) fn show_error(&mut self, error: &str, now: Instant) {
        let mut chars = error.chars();
        let text = match chars.next() {
            Some(c) => c.to_ascii_uppercase().to_string() + chars.as_str(),
            None => String::new(),
        };
        self.message = Some((text, now + DISPLAY_TIME));
    }
}
