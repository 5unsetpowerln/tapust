//! The status line prompt, ported from tmux 3.6 `status_prompt_set`,
//! `status_prompt_key` (emacs `status-keys`) and the callbacks of
//! cmd-command-prompt.c and cmd-confirm-before.c, for the prompts of the
//! default bindings: `,`, `$`, `x` and `&`, and copy mode's searches,
//! jumps, goto-line and repeat counts.

use std::time::Instant;

use crate::copy::{CopyCommand, SearchArg};
use crate::format::Context;
use crate::{InputKind, Key, KeyCode, Mods, PaneId, Session, WORD_SEPARATORS};

/// The `prompt-history-limit` option.
const HISTORY_LIMIT: usize = 100;

/// The jump of a copy-mode `command-prompt -1`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Jump {
    Forward,
    Backward,
    ToForward,
    ToBackward,
}

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
    /// `command-prompt -T search -p'(search down)' { send -X search-forward
    /// -- '%%' }`, or `(search up)` and search-backward (copy-mode-vi `/`
    /// and `?`). `incremental` is copy-mode's `C-s` and `C-r`: `-i
    /// -I'#{pane_search_string}'` and search-forward-incremental (or
    /// search-backward-incremental).
    CopySearch { forward: bool, incremental: bool },
    /// `command-prompt -1p'(jump forward)' { send -X jump-forward -- '%%' }`
    /// and the other jumps.
    CopyJump(Jump),
    /// `command-prompt -p'(goto line)' { send -X goto-line -- '%%' }`.
    CopyGotoLine,
    /// `command-prompt -Np'(repeat)' -I<digit> { send -N '%%' }`.
    CopyRepeat(char),
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
    /// prompt_last: what C-r and C-s bring back into an empty incremental
    /// prompt.
    pub last: String,
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
    /// PROMPT_SINGLE: the first key answers.
    fn single(&self) -> bool {
        matches!(
            self.kind,
            PromptKind::ConfirmKillPane { .. } | PromptKind::ConfirmKillWindow { .. } | PromptKind::CopyJump(_)
        )
    }

    /// PROMPT_NUMERIC: digits are typed, any other key answers.
    fn numeric(&self) -> bool {
        matches!(self.kind, PromptKind::CopyRepeat(_))
    }

    /// PROMPT_INCREMENTAL: every change runs the command.
    fn incremental(&self) -> bool {
        matches!(self.kind, PromptKind::CopySearch { incremental: true, .. })
    }

    /// The prompt's history: `-T search` keeps its own.
    fn history_type(&self) -> usize {
        matches!(self.kind, PromptKind::CopySearch { .. }) as usize
    }

    /// What the GUI shows for an input prompt.
    pub(crate) fn input_kind(&self) -> InputKind {
        match self.kind {
            PromptKind::RenameSession => InputKind::RenameSession,
            PromptKind::CopySearch { forward, incremental } => {
                InputKind::Search { forward, incremental, last: self.last.clone() }
            }
            PromptKind::CopyGotoLine => InputKind::GotoLine,
            _ => InputKind::RenameWindow,
        }
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
    /// status_prompt_set for the prompts of the default bindings; an
    /// incremental prompt runs its command at once with an empty text.
    pub(crate) fn open_prompt(&mut self, kind: PromptKind, now: Instant) {
        let window = self.cur().id;
        let pane = self.cur().active;
        let (template, input) = match kind {
            PromptKind::RenameWindow { .. } => ("(rename-window) ", "#W"),
            PromptKind::RenameSession => ("(rename-session) ", "#S"),
            PromptKind::ConfirmKillPane { .. } => ("kill-pane #P? (y/n) ", ""),
            PromptKind::ConfirmKillWindow { .. } => ("kill-window #W? (y/n) ", ""),
            PromptKind::CopySearch { forward: true, .. } => ("(search down) ", ""),
            PromptKind::CopySearch { forward: false, .. } => ("(search up) ", ""),
            PromptKind::CopyJump(Jump::Forward) => ("(jump forward) ", ""),
            PromptKind::CopyJump(Jump::Backward) => ("(jump backward) ", ""),
            PromptKind::CopyJump(Jump::ToForward) => ("(jump to forward) ", ""),
            PromptKind::CopyJump(Jump::ToBackward) => ("(jump to backward) ", ""),
            PromptKind::CopyGotoLine => ("(goto line) ", ""),
            PromptKind::CopyRepeat(_) => ("(repeat) ", ""),
        };
        let mut buffer: Vec<char> = self.expand(input, Context { window: self.curw, pane }).chars().collect();
        if let PromptKind::CopyRepeat(digit) = kind {
            buffer = vec![digit];
        }
        let incremental = matches!(kind, PromptKind::CopySearch { incremental: true, .. });
        // `-I'#{pane_search_string}'` is not typed but kept as prompt_last.
        let last = if incremental { self.search_string(pane) } else { String::new() };
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
            last,
        });
        if incremental {
            self.prompt_incremental("", '=', now);
        }
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

    /// status_prompt_key. Returns whether the key goes on to the key tables,
    /// as the key that answers a numeric prompt does.
    pub(crate) fn prompt_key(&mut self, key: Key, now: Instant) -> bool {
        let Some(p) = self.prompt.as_mut() else { return false };

        if p.numeric() {
            if let KeyCode::Char(c @ '0'..='9') = key.code
                && key.mods == Mods::default()
            {
                p.insert(&[c]);
                return false;
            }
            let p = self.prompt.take().expect("prompt");
            let line: String = p.buffer.iter().collect();
            self.prompt_accept(p.kind, &line, now);
            return true;
        }

        if p.single() || p.quote_next {
            let Some(c) = single_char(key) else { return false };
            p.quote_next = false;
            p.insert(&[c]);
            if p.single() {
                let p = self.prompt.take().expect("prompt");
                if p.buffer.len() == 1 {
                    self.prompt_accept(p.kind, &p.buffer[0].to_string(), now);
                }
            } else if p.incremental() {
                let text: String = p.buffer.iter().collect();
                self.prompt_incremental(&text, '=', now);
            }
            return false;
        }

        let size = p.buffer.len();
        let types = p.history_type();
        let history = &self.history[types];
        // The incremental prompt's command runs again on `goto changed`,
        // with this prefix before the text.
        let mut prefix = '=';
        let changed = match pkey(key) {
            PKey::Left | PKey::Ctrl('b') => {
                p.index = p.index.saturating_sub(1);
                false
            }
            PKey::Right | PKey::Ctrl('f') => {
                p.index = (p.index + 1).min(size);
                false
            }
            PKey::Home | PKey::Ctrl('a') => {
                p.index = 0;
                false
            }
            PKey::End | PKey::Ctrl('e') => {
                p.index = size;
                false
            }
            PKey::Backspace | PKey::Ctrl('h') => {
                let at = p.index;
                if at != 0 {
                    p.buffer.remove(at - 1);
                    p.index -= 1;
                }
                at != 0
            }
            PKey::Delete | PKey::Ctrl('d') => {
                if p.index != size {
                    p.buffer.remove(p.index);
                }
                p.index != size
            }
            PKey::Ctrl('u') => {
                p.buffer.clear();
                p.index = 0;
                true
            }
            PKey::Ctrl('k') => {
                p.buffer.truncate(p.index);
                p.index < size
            }
            PKey::Ctrl('w') => {
                let start = p.word_start();
                p.saved = Some(p.buffer.drain(start..p.index).collect());
                p.index = start;
                true
            }
            PKey::CtrlRight | PKey::Meta('f') => {
                p.forward_word();
                true
            }
            PKey::CtrlLeft | PKey::Meta('b') => {
                p.index = p.word_start();
                true
            }
            PKey::Up | PKey::Ctrl('p') => {
                let hsize = history.len();
                let found = hsize != 0 && p.hindex != hsize;
                if found {
                    p.hindex += 1;
                    p.buffer = history[hsize - p.hindex].chars().collect();
                    p.index = p.buffer.len();
                }
                found
            }
            PKey::Down | PKey::Ctrl('n') => {
                let hsize = history.len();
                if hsize != 0 && p.hindex != 0 {
                    p.hindex -= 1;
                }
                p.buffer = match p.hindex {
                    0 => Vec::new(),
                    i => history[hsize - i].chars().collect(),
                };
                p.index = p.buffer.len();
                true
            }
            PKey::Ctrl('y') => match p.saved.clone() {
                Some(saved) => {
                    p.insert(&saved);
                    true
                }
                None => false,
            },
            PKey::Ctrl('t') => {
                let idx = if p.index < size { p.index + 1 } else { p.index };
                if idx >= 2 {
                    p.buffer.swap(idx - 2, idx - 1);
                    p.index = idx;
                }
                idx >= 2
            }
            PKey::Ctrl('v') => {
                p.quote_next = true;
                false
            }
            PKey::Ctrl(c @ ('r' | 's')) if p.incremental() => {
                if p.buffer.is_empty() {
                    p.buffer = p.last.chars().collect();
                    p.index = p.buffer.len();
                } else {
                    prefix = if c == 'r' { '-' } else { '+' };
                }
                true
            }
            PKey::Enter => {
                let p = self.prompt.take().expect("prompt");
                let line: String = p.buffer.iter().collect();
                let history = &mut self.history[types];
                if !line.is_empty() && history.last() != Some(&line) {
                    history.push(line.clone());
                    if history.len() > HISTORY_LIMIT {
                        history.remove(0);
                    }
                }
                self.prompt_accept(p.kind, &line, now);
                return false;
            }
            // An incremental prompt's command does nothing when cancelled.
            PKey::Escape | PKey::Ctrl('c') | PKey::Ctrl('g') => {
                self.prompt = None;
                return false;
            }
            PKey::Char(c) if c as u32 >= 0x20 && c != '\x7f' => {
                p.insert(&[c]);
                true
            }
            _ => false,
        };
        if changed && p.incremental() {
            let text: String = p.buffer.iter().collect();
            self.prompt_incremental(&text, prefix, now);
        }
        false
    }

    /// The incremental prompt's text changed (status_prompt_key's
    /// `changed`): cmd_command_prompt_callback runs its command with the
    /// text after `prefix` (`=`, or `-` and `+` for C-r and C-s).
    pub(crate) fn prompt_incremental(&mut self, text: &str, prefix: char, now: Instant) {
        let Some(PromptKind::CopySearch { forward, incremental: true }) = self.prompt.as_ref().map(|p| &p.kind) else {
            return;
        };
        let arg = format!("{prefix}{text}");
        let command = if *forward {
            CopyCommand::SearchForwardIncremental(arg)
        } else {
            CopyCommand::SearchBackwardIncremental(arg)
        };
        if let Err(error) = self.send_copy(command) {
            self.show_error(&error, now);
        }
    }

    /// Answers the open prompt from a dialog: `Some` confirms (a
    /// confirmation) or accepts the text (an input prompt), `None` cancels.
    pub(crate) fn prompt_answer(&mut self, reply: Option<&str>, now: Instant) {
        let Some(p) = self.prompt.take() else { return };
        let Some(text) = reply else { return };
        self.prompt_accept(p.kind, text, now);
    }

    /// The callbacks of cmd-confirm-before.c and cmd-command-prompt.c: run
    /// the command with "%%" replaced.
    fn prompt_accept(&mut self, kind: PromptKind, line: &str, now: Instant) {
        let result = match kind {
            PromptKind::RenameWindow { window } => {
                let id = if self.window_by_id(window).is_some() { window } else { self.cur().id };
                self.rename_window(id, line);
                Ok(())
            }
            PromptKind::RenameSession => self.exec(&crate::Command::RenameSession(line.to_owned())),
            PromptKind::ConfirmKillPane { pane } => {
                if line.starts_with('y') {
                    self.kill_pane_target(pane);
                }
                Ok(())
            }
            PromptKind::ConfirmKillWindow { window } => {
                if line.starts_with('y') {
                    self.kill_window_target(window);
                }
                Ok(())
            }
            PromptKind::CopySearch { incremental: true, .. } => Ok(()),
            PromptKind::CopySearch { forward: true, .. } => {
                self.send_copy(CopyCommand::SearchForward(SearchArg::Text(line.to_owned())))
            }
            PromptKind::CopySearch { forward: false, .. } => {
                self.send_copy(CopyCommand::SearchBackward(SearchArg::Text(line.to_owned())))
            }
            PromptKind::CopyJump(jump) => {
                let c = line.to_owned();
                self.send_copy(match jump {
                    Jump::Forward => CopyCommand::JumpForward(c),
                    Jump::Backward => CopyCommand::JumpBackward(c),
                    Jump::ToForward => CopyCommand::JumpToForward(c),
                    Jump::ToBackward => CopyCommand::JumpToBackward(c),
                })
            }
            PromptKind::CopyGotoLine => self.send_copy(CopyCommand::GotoLine(line.to_owned())),
            PromptKind::CopyRepeat(_) => self.set_repeat(line),
        };
        if let Err(error) = result {
            self.show_error(&error, now);
        }
    }
}
