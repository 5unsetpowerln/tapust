//! The status line prompt, ported from tmux 3.6 `status_prompt_set`,
//! `status_prompt_key` (emacs `status-keys`) and the callbacks of
//! cmd-command-prompt.c and cmd-confirm-before.c, for the prompts of the
//! `,`, `$`, `x` and `&` bindings.

use std::time::Instant;

use crate::format::Context;
use crate::{Key, KeyCode, PaneId, Session};

/// The `word-separators` option.
const WORD_SEPARATORS: &str = "!\"#$%&'()*+,-./:;<=>?@[\\]^`{|}~";
/// The `prompt-history-limit` option.
const HISTORY_LIMIT: usize = 100;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum PromptKind {
    /// `command-prompt -I "#W" { rename-window "%%" }` on window `@window`.
    RenameWindow { window: u32 },
    /// `command-prompt -I "#S" { rename-session "%%" }`.
    RenameSession,
    /// `confirm-before -p "kill-pane #P? (y/n)" kill-pane`.
    ConfirmKillPane { pane: PaneId },
    /// `confirm-before -p "kill-window #W? (y/n)" kill-window` on `@window`.
    ConfirmKillWindow { window: u32 },
}

#[derive(Clone, Debug)]
pub(crate) struct Prompt {
    pub kind: PromptKind,
    /// The prompt string, format-expanded when drawn (status_prompt_redraw).
    pub template: &'static str,
    /// The window (id) and pane the prompt was opened for.
    pub window: u32,
    pub pane: PaneId,
    pub buffer: Vec<char>,
    pub index: usize,
    pub quote_next: bool,
    pub saved: Option<Vec<char>>,
    pub hindex: usize,
}

/// A key as tmux's prompt sees it after key_string normalisation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PKey {
    Char(char),
    Ctrl(char),
    Meta(char),
    Enter,
    Tab,
    Escape,
    Backspace,
    Delete,
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    CtrlLeft,
    CtrlRight,
    Other,
}

fn pkey(key: Key) -> PKey {
    let m = key.mods;
    match key.code {
        KeyCode::Char(c) if m.ctrl && !m.meta => match c {
            'm' => PKey::Enter,
            'i' => PKey::Tab,
            '[' => PKey::Escape,
            _ => PKey::Ctrl(c),
        },
        KeyCode::Char(c) if m.meta && !m.ctrl => PKey::Meta(c),
        KeyCode::Char(c) if !m.ctrl => PKey::Char(c),
        KeyCode::Enter => PKey::Enter,
        KeyCode::Tab => PKey::Tab,
        KeyCode::Escape => PKey::Escape,
        KeyCode::Backspace => PKey::Backspace,
        KeyCode::Delete if !m.ctrl && !m.meta => PKey::Delete,
        KeyCode::Left if m.ctrl && !m.meta => PKey::CtrlLeft,
        KeyCode::Right if m.ctrl && !m.meta => PKey::CtrlRight,
        _ if m.ctrl || m.meta => PKey::Other,
        KeyCode::Left => PKey::Left,
        KeyCode::Right => PKey::Right,
        KeyCode::Up => PKey::Up,
        KeyCode::Down => PKey::Down,
        KeyCode::Home => PKey::Home,
        KeyCode::End => PKey::End,
        _ => PKey::Other,
    }
}

/// The byte or character a key appends in single-key and quote-next mode.
fn single_char(key: Key) -> Option<char> {
    match key.code {
        KeyCode::Backspace => Some('\x7f'),
        KeyCode::Enter => Some('\r'),
        KeyCode::Tab => Some('\t'),
        KeyCode::Escape => Some('\x1b'),
        KeyCode::Char(c) if !c.is_ascii() => Some(c),
        KeyCode::Char(c) if key.mods.ctrl => char::from_u32(c as u32 & 0x1f),
        KeyCode::Char(c) => Some(c),
        _ => None,
    }
}

fn is_space(c: Option<&char>) -> bool {
    c == Some(&' ')
}

fn is_separator(c: Option<&char>) -> bool {
    c.is_some_and(|c| WORD_SEPARATORS.contains(*c))
}

impl Prompt {
    fn single(&self) -> bool {
        matches!(self.kind, PromptKind::ConfirmKillPane { .. } | PromptKind::ConfirmKillWindow { .. })
    }

    fn insert(&mut self, chars: &[char]) {
        let at = self.index;
        self.buffer.splice(at..at, chars.iter().copied());
        self.index += chars.len();
    }

    /// status_prompt_forward_word (emacs)
    fn forward_word(&mut self) {
        let size = self.buffer.len();
        let mut idx = self.index;
        while idx != size && is_space(self.buffer.get(idx)) {
            idx += 1;
        }
        if idx == size {
            self.index = idx;
            return;
        }
        let word_is_sep = is_separator(self.buffer.get(idx)) && !is_space(self.buffer.get(idx));
        loop {
            idx += 1;
            if is_space(self.buffer.get(idx)) {
                break;
            }
            if idx == size || word_is_sep != is_separator(self.buffer.get(idx)) {
                break;
            }
        }
        self.index = idx;
    }

    /// The start of the word before the cursor (status_prompt_backward_word
    /// and the C-w case of status_prompt_key).
    fn word_start(&self) -> usize {
        let mut idx = self.index;
        while idx != 0 {
            idx -= 1;
            if !is_space(self.buffer.get(idx)) {
                break;
            }
        }
        let word_is_sep = is_separator(self.buffer.get(idx));
        while idx != 0 {
            idx -= 1;
            if is_space(self.buffer.get(idx)) || word_is_sep != is_separator(self.buffer.get(idx)) {
                idx += 1;
                break;
            }
        }
        idx
    }
}

impl Session {
    /// status_prompt_set for the prompts of the default bindings.
    pub(crate) fn open_prompt(&mut self, kind: PromptKind) {
        let window = self.cur().id;
        let pane = self.cur().active;
        let (template, input) = match kind {
            PromptKind::RenameWindow { .. } => ("(rename-window) ", "#W"),
            PromptKind::RenameSession => ("(rename-session) ", "#S"),
            PromptKind::ConfirmKillPane { .. } => ("kill-pane #P? (y/n) ", ""),
            PromptKind::ConfirmKillWindow { .. } => ("kill-window #W? (y/n) ", ""),
        };
        let buffer: Vec<char> =
            self.expand(input, Context { window: self.curw, pane }).chars().collect();
        self.message = None;
        self.prompt = Some(Prompt {
            kind,
            template,
            window,
            pane,
            index: buffer.len(),
            buffer,
            quote_next: false,
            saved: None,
            hindex: 0,
        });
    }

    /// The prompt string as drawn, expanded for the prompt's target.
    pub(crate) fn prompt_text(&self, p: &Prompt) -> String {
        let window = self.window_by_id(p.window).unwrap_or(self.curw);
        let pane = if self.winlinks[&window].has_pane(p.pane) {
            p.pane
        } else {
            self.winlinks[&window].active
        };
        self.expand(p.template, Context { window, pane })
    }

    /// status_prompt_key
    pub(crate) fn prompt_key(&mut self, key: Key, now: Instant) {
        let Some(p) = self.prompt.as_mut() else { return };

        if p.single() || p.quote_next {
            let Some(c) = single_char(key) else { return };
            p.quote_next = false;
            p.insert(&[c]);
            if p.single() {
                let p = self.prompt.take().expect("prompt");
                if p.buffer.len() == 1 && p.buffer[0] == 'y' {
                    match p.kind {
                        PromptKind::ConfirmKillPane { pane } => self.kill_pane_target(pane),
                        PromptKind::ConfirmKillWindow { window } => self.kill_window_target(window),
                        _ => {}
                    }
                }
            }
            return;
        }

        let size = p.buffer.len();
        match pkey(key) {
            PKey::Left | PKey::Ctrl('b') => p.index = p.index.saturating_sub(1),
            PKey::Right | PKey::Ctrl('f') => p.index = (p.index + 1).min(size),
            PKey::Home | PKey::Ctrl('a') => p.index = 0,
            PKey::End | PKey::Ctrl('e') => p.index = size,
            PKey::Backspace | PKey::Ctrl('h') => {
                if p.index != 0 {
                    p.buffer.remove(p.index - 1);
                    p.index -= 1;
                }
            }
            PKey::Delete | PKey::Ctrl('d') => {
                if p.index != size {
                    p.buffer.remove(p.index);
                }
            }
            PKey::Ctrl('u') => {
                p.buffer.clear();
                p.index = 0;
            }
            PKey::Ctrl('k') => p.buffer.truncate(p.index),
            PKey::Ctrl('w') => {
                let start = p.word_start();
                p.saved = Some(p.buffer.drain(start..p.index).collect());
                p.index = start;
            }
            PKey::CtrlRight | PKey::Meta('f') => p.forward_word(),
            PKey::CtrlLeft | PKey::Meta('b') => p.index = p.word_start(),
            PKey::Up | PKey::Ctrl('p') => {
                let hsize = self.history.len();
                if hsize != 0 && p.hindex != hsize {
                    p.hindex += 1;
                    p.buffer = self.history[hsize - p.hindex].chars().collect();
                    p.index = p.buffer.len();
                }
            }
            PKey::Down | PKey::Ctrl('n') => {
                let hsize = self.history.len();
                if hsize != 0 && p.hindex != 0 {
                    p.hindex -= 1;
                }
                p.buffer = match p.hindex {
                    0 => Vec::new(),
                    i => self.history[hsize - i].chars().collect(),
                };
                p.index = p.buffer.len();
            }
            PKey::Ctrl('y') => {
                if let Some(saved) = p.saved.clone() {
                    p.insert(&saved);
                }
            }
            PKey::Ctrl('t') => {
                let idx = if p.index < size { p.index + 1 } else { p.index };
                if idx >= 2 {
                    p.buffer.swap(idx - 2, idx - 1);
                    p.index = idx;
                }
            }
            PKey::Ctrl('v') => p.quote_next = true,
            PKey::Enter => {
                let p = self.prompt.take().expect("prompt");
                let line: String = p.buffer.iter().collect();
                if !line.is_empty() && self.history.last() != Some(&line) {
                    self.history.push(line.clone());
                    if self.history.len() > HISTORY_LIMIT {
                        self.history.remove(0);
                    }
                }
                self.prompt_accept(p.kind, &line, now);
            }
            PKey::Escape | PKey::Ctrl('c') | PKey::Ctrl('g') => self.prompt = None,
            PKey::Char(c) if c as u32 >= 0x20 && c != '\x7f' => p.insert(&[c]),
            _ => {}
        }
    }

    /// Answers the open prompt from a dialog: `Some` confirms (a
    /// confirmation) or accepts the text (an input prompt), `None` cancels.
    pub(crate) fn prompt_answer(&mut self, reply: Option<&str>, now: Instant) {
        let Some(p) = self.prompt.take() else { return };
        let Some(text) = reply else { return };
        if p.single() {
            match p.kind {
                PromptKind::ConfirmKillPane { pane } => self.kill_pane_target(pane),
                PromptKind::ConfirmKillWindow { window } => self.kill_window_target(window),
                _ => {}
            }
        } else {
            self.prompt_accept(p.kind, text, now);
        }
    }

    /// cmd_command_prompt_callback: run the command with "%%" replaced.
    fn prompt_accept(&mut self, kind: PromptKind, line: &str, now: Instant) {
        let result = match kind {
            PromptKind::RenameWindow { window } => {
                let id = if self.window_by_id(window).is_some() { window } else { self.cur().id };
                self.rename_window(id, line);
                Ok(())
            }
            PromptKind::RenameSession => self.exec(&crate::Command::RenameSession(line.to_owned())),
            _ => Ok(()),
        };
        if let Err(error) = result {
            self.show_error(&error, now);
        }
    }
}
