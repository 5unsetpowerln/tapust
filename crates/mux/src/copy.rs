//! Copy mode as far as the model goes: the `copy-mode` and `copy-mode-vi`
//! key tables of tmux 3.6 `key_bindings_init` (key-bindings.c), the
//! commands they send with `send -X`, and the mode-keys option. The GUI
//! carries the commands out on a frozen copy of the pane (window-copy.c).

use std::ffi::OsStr;

use crate::prompt::{Jump, PromptKind};
use crate::{Effect, Key, KeyCode, Mods, PaneId, Session};

/// The `mode-keys` option: which copy-mode key table a pane uses.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ModeKeys {
    #[default]
    Emacs,
    Vi,
}

impl ModeKeys {
    /// tmux's main: vi when $VISUAL, or else $EDITOR, is set and the last
    /// component of its value contains "vi", emacs when it is set without;
    /// the option's default (emacs) when neither is set.
    pub fn from_env(visual: Option<&OsStr>, editor: Option<&OsStr>) -> ModeKeys {
        let Some(value) = visual.or(editor) else { return ModeKeys::Emacs };
        let bytes = value.as_encoded_bytes();
        let name = bytes.rsplit(|&b| b == b'/').next().unwrap_or(bytes);
        if name.windows(2).any(|w| w == b"vi") { ModeKeys::Vi } else { ModeKeys::Emacs }
    }
}

/// The search string of `search-forward` and `search-backward`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SearchArg {
    Text(String),
    /// `send -FX ... '#{copy_cursor_word}'`: the word under the cursor.
    CursorWord,
}

/// A `send -X` command of window-copy.c that a default binding sends,
/// named after it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CopyCommand {
    AppendSelectionAndCancel,
    BackToIndentation,
    BeginSelection,
    BottomLine,
    Cancel,
    ClearSelection,
    /// Without a command and with an empty `copy-command`, it copies.
    CopyPipeAndCancel,
    CopyPipeEndOfLineAndCancel,
    CursorCentreHorizontal,
    CursorCentreVertical,
    CursorDown,
    CursorLeft,
    CursorRight,
    CursorUp,
    EndOfLine,
    GotoLine(String),
    HalfpageDown,
    HalfpageUp,
    HistoryBottom,
    HistoryTop,
    JumpAgain,
    JumpBackward(String),
    JumpForward(String),
    JumpReverse,
    JumpToBackward(String),
    JumpToForward(String),
    JumpToMark,
    MiddleLine,
    NextMatchingBracket,
    NextParagraph,
    NextSpace,
    NextSpaceEnd,
    NextWord,
    NextWordEnd,
    OtherEnd,
    PageDown,
    PageUp,
    PreviousMatchingBracket,
    PreviousParagraph,
    PreviousSpace,
    PreviousWord,
    RectangleToggle,
    RefreshFromPane,
    ScrollDown,
    ScrollMiddle,
    ScrollUp,
    SearchAgain,
    SearchBackward(SearchArg),
    /// The prompt's text after its prefix: `=` (changed), `-` or `+`.
    SearchBackwardIncremental(String),
    SearchForward(SearchArg),
    SearchForwardIncremental(String),
    SearchReverse,
    SelectLine,
    SetMark,
    StartOfLine,
    TogglePosition,
    TopLine,
}

/// What a copy-mode binding does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ModeAction {
    /// `send -X command`.
    Send(CopyCommand),
    /// `command-prompt` for a copy-mode command or a repeat count.
    Prompt(PromptKind),
}

/// The key as tmux reads it from what xterm sends for it (extended keys
/// off): C-m is Enter, C-i Tab, C-[ Escape, C-@ and C-2 C-Space, and Shift
/// does not change a control letter.
pub(crate) fn tmux_key(key: Key) -> Key {
    let KeyCode::Char(c) = key.code else { return key };
    if !key.mods.ctrl {
        return key;
    }
    let plain = Mods { ctrl: false, ..key.mods };
    match c {
        'm' | 'M' => Key { code: KeyCode::Enter, mods: plain },
        'i' | 'I' => Key { code: KeyCode::Tab, mods: plain },
        '[' | '3' => Key { code: KeyCode::Escape, mods: plain },
        '@' | '2' => Key { code: KeyCode::Char(' '), mods: key.mods },
        c if c.is_ascii_uppercase() => Key { code: KeyCode::Char(c.to_ascii_lowercase()), mods: key.mods },
        _ => key,
    }
}

fn send(command: CopyCommand) -> Option<ModeAction> {
    Some(ModeAction::Send(command))
}

fn prompt(kind: PromptKind) -> Option<ModeAction> {
    Some(ModeAction::Prompt(kind))
}

/// The binding of `key` in `copy-mode` or `copy-mode-vi`.
pub(crate) fn binding(key: Key, keys: ModeKeys) -> Option<ModeAction> {
    let key = tmux_key(key);
    match keys {
        ModeKeys::Emacs => emacs(key),
        ModeKeys::Vi => vi(key),
    }
}

/// Bindings both tables have.
fn common(key: Key) -> Option<ModeAction> {
    use CopyCommand::*;
    let m = key.mods;
    let none = m == Mods::default();
    let ctrl = m == Mods { ctrl: true, ..Mods::default() };
    match key.code {
        KeyCode::Char(',') if none => send(JumpReverse),
        KeyCode::Char(';') if none => send(JumpAgain),
        KeyCode::Char('F') if none => prompt(PromptKind::CopyJump(Jump::Backward)),
        KeyCode::Char('N') if none => send(SearchReverse),
        KeyCode::Char('P') if none => send(TogglePosition),
        KeyCode::Char('T') if none => prompt(PromptKind::CopyJump(Jump::ToBackward)),
        KeyCode::Char('X') if none => send(SetMark),
        KeyCode::Char('f') if none => prompt(PromptKind::CopyJump(Jump::Forward)),
        KeyCode::Char('n') if none => send(SearchAgain),
        KeyCode::Char('q') if none => send(Cancel),
        KeyCode::Char('r') if none => send(RefreshFromPane),
        KeyCode::Char('t') if none => prompt(PromptKind::CopyJump(Jump::ToForward)),
        KeyCode::Char('c') if ctrl => send(Cancel),
        KeyCode::Char('x') if m == Mods { meta: true, ..Mods::default() } => send(JumpToMark),
        KeyCode::Home if none => send(StartOfLine),
        KeyCode::End if none => send(EndOfLine),
        KeyCode::PageDown if none => send(PageDown),
        KeyCode::PageUp if none => send(PageUp),
        KeyCode::Up if none => send(CursorUp),
        KeyCode::Down if none => send(CursorDown),
        KeyCode::Left if none => send(CursorLeft),
        KeyCode::Right if none => send(CursorRight),
        KeyCode::Up if ctrl => send(ScrollUp),
        KeyCode::Down if ctrl => send(ScrollDown),
        _ => None,
    }
}

/// The `copy-mode` table.
fn emacs(key: Key) -> Option<ModeAction> {
    use CopyCommand::*;
    let m = key.mods;
    let none = m == Mods::default();
    let ctrl = m == Mods { ctrl: true, ..Mods::default() };
    let meta = m == Mods { meta: true, ..Mods::default() };
    let ctrl_meta = m == Mods { ctrl: true, meta: true, shift: false };
    let search = |forward| prompt(PromptKind::CopySearch { forward, incremental: true });
    let action = match key.code {
        KeyCode::Char(c) if ctrl => match c {
            ' ' => send(BeginSelection),
            'a' => send(StartOfLine),
            'e' => send(EndOfLine),
            'f' => send(CursorRight),
            'b' => send(CursorLeft),
            'g' => send(ClearSelection),
            'k' => send(CopyPipeEndOfLineAndCancel),
            'l' => send(CursorCentreVertical),
            'n' => send(CursorDown),
            'p' => send(CursorUp),
            'r' => search(false),
            's' => search(true),
            'v' => send(PageDown),
            'w' => send(CopyPipeAndCancel),
            _ => None,
        },
        KeyCode::Char(c) if meta => match c {
            'l' => send(CursorCentreHorizontal),
            '1'..='9' => prompt(PromptKind::CopyRepeat(c)),
            '<' => send(HistoryTop),
            '>' => send(HistoryBottom),
            'R' => send(TopLine),
            'b' => send(PreviousWord),
            'f' => send(NextWordEnd),
            'm' => send(BackToIndentation),
            'r' => send(MiddleLine),
            'v' => send(PageUp),
            'w' => send(CopyPipeAndCancel),
            '{' => send(PreviousParagraph),
            '}' => send(NextParagraph),
            _ => None,
        },
        KeyCode::Char('b') if ctrl_meta => send(PreviousMatchingBracket),
        KeyCode::Char('f') if ctrl_meta => send(NextMatchingBracket),
        KeyCode::Char(' ') if none => send(PageDown),
        KeyCode::Char('R') if none => send(RectangleToggle),
        KeyCode::Char('g') if none => prompt(PromptKind::CopyGotoLine),
        KeyCode::Escape if none => send(Cancel),
        KeyCode::Up if meta => send(HalfpageUp),
        KeyCode::Down if meta => send(HalfpageDown),
        _ => None,
    };
    action.or_else(|| common(key))
}

/// The `copy-mode-vi` table.
fn vi(key: Key) -> Option<ModeAction> {
    use CopyCommand::*;
    let m = key.mods;
    let none = m == Mods::default();
    let ctrl = m == Mods { ctrl: true, ..Mods::default() };
    let search = |forward| prompt(PromptKind::CopySearch { forward, incremental: false });
    let action = match key.code {
        KeyCode::Char(c) if ctrl => match c {
            'd' => send(HalfpageDown),
            'e' => send(ScrollDown),
            'b' => send(PageUp),
            'f' => send(PageDown),
            'h' => send(CursorLeft),
            'j' => send(CopyPipeAndCancel),
            'u' => send(HalfpageUp),
            'v' => send(RectangleToggle),
            'y' => send(ScrollUp),
            _ => None,
        },
        KeyCode::Char(c) if none => match c {
            '#' => send(SearchBackward(SearchArg::CursorWord)),
            '*' => send(SearchForward(SearchArg::CursorWord)),
            ' ' => send(BeginSelection),
            '$' => send(EndOfLine),
            '/' => search(true),
            '0' => send(StartOfLine),
            '1'..='9' => prompt(PromptKind::CopyRepeat(c)),
            ':' => prompt(PromptKind::CopyGotoLine),
            '?' => search(false),
            'A' => send(AppendSelectionAndCancel),
            'B' => send(PreviousSpace),
            'D' => send(CopyPipeEndOfLineAndCancel),
            'E' => send(NextSpaceEnd),
            'G' => send(HistoryBottom),
            'H' => send(TopLine),
            'J' => send(ScrollDown),
            'K' => send(ScrollUp),
            'L' => send(BottomLine),
            'M' => send(MiddleLine),
            'V' => send(SelectLine),
            'W' => send(NextSpace),
            '^' => send(BackToIndentation),
            'b' => send(PreviousWord),
            'e' => send(NextWordEnd),
            'g' => send(HistoryTop),
            'h' => send(CursorLeft),
            'j' => send(CursorDown),
            'k' => send(CursorUp),
            'z' => send(ScrollMiddle),
            'l' => send(CursorRight),
            'o' => send(OtherEnd),
            'v' => send(RectangleToggle),
            'w' => send(NextWord),
            '{' => send(PreviousParagraph),
            '}' => send(NextParagraph),
            '%' => send(NextMatchingBracket),
            _ => None,
        },
        KeyCode::Enter if none => send(CopyPipeAndCancel),
        KeyCode::Escape if none => send(ClearSelection),
        KeyCode::Backspace if none => send(CursorLeft),
        _ => None,
    };
    action.or_else(|| common(key))
}

impl Session {
    /// `send -X command` to the active pane, with the mode's prefix as the
    /// count, which goes back to 1 (window_copy_command).
    pub(crate) fn send_copy(&mut self, command: CopyCommand) -> Result<(), String> {
        let pane = self.cur().active;
        let Some(prefix) = self.cur_mut().pane_mut(pane).mode.as_mut() else { return Err("not in a mode".to_owned()) };
        let count = std::mem::replace(prefix, 1);
        self.writes.push(Effect::CopyCommand { pane, command, count });
        Ok(())
    }

    /// `send -N count` without keys: the mode's prefix for the next command,
    /// if the active pane is in a mode.
    pub(crate) fn set_repeat(&mut self, count: &str) -> Result<(), String> {
        // args_strtonum_and_expand(args, 'N', 1, UINT_MAX).
        let count = match count.parse::<u64>() {
            Ok(0) => return Err("repeat count too small".to_owned()),
            Ok(n) => u32::try_from(n).map_err(|_| "repeat count too large".to_owned())?,
            Err(e) if *e.kind() == std::num::IntErrorKind::PosOverflow => {
                return Err("repeat count too large".to_owned());
            }
            Err(_) => return Err("repeat count invalid".to_owned()),
        };
        let pane = self.cur().active;
        if let Some(prefix) = self.cur_mut().pane_mut(pane).mode.as_mut() {
            *prefix = count;
        }
        Ok(())
    }

    /// `#{pane_search_string}`.
    pub(crate) fn search_string(&self, pane: PaneId) -> String {
        self.find_pane(pane).and_then(|p| p.search.as_ref()).map(|s| s.text.clone()).unwrap_or_default()
    }
}
