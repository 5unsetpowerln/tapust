//! The terminal-level key bindings, taken from ghostty's defaults on Linux
//! (Config.zig): which key does what, ahead of the tmux key tables, and how
//! the scrolling and font size ones change the view.

use gtk::gdk;
use term::{PromptMark, Terminal};

use crate::selection;
use crate::theme;

/// The font size never goes below this, nor above the size whose glyphs
/// still fit the renderer's fixed glyph atlas in useful numbers.
const MIN_POINTS: f32 = 4.0;
const MAX_POINTS: f32 = 72.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Binding {
    StartSearch,
    /// copy_to_clipboard; a `performable` one is no binding while nothing
    /// is selected, so the key goes on to the program (ctrl+shift+c).
    Copy { performable: bool },
    /// paste_from_clipboard, or paste_from_selection when `primary`.
    Paste { primary: bool },
    Scroll(Scroll),
    SelectAll,
    FontSize(FontSize),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scroll {
    PageUp,
    PageDown,
    Top,
    Bottom,
    /// jump_to_prompt -1 and 1.
    PreviousPrompt,
    NextPrompt,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FontSize {
    Increase,
    Decrease,
    Reset,
}

/// The binding of a key press, if it has one.
pub fn binding(keyval: gdk::Key, state: gdk::ModifierType) -> Option<Binding> {
    use gdk::Key as K;
    use gdk::ModifierType as M;
    let held = state & (M::CONTROL_MASK | M::SHIFT_MASK | M::ALT_MASK | M::SUPER_MASK | M::HYPER_MASK | M::META_MASK);
    let (ctrl, shift) = (held == M::CONTROL_MASK, held == M::SHIFT_MASK);
    let ctrl_shift = held == M::CONTROL_MASK | M::SHIFT_MASK;
    let binding = match keyval {
        K::Copy if held.is_empty() => Binding::Copy { performable: false },
        K::Paste if held.is_empty() => Binding::Paste { primary: false },
        K::Insert | K::KP_Insert if ctrl => Binding::Copy { performable: false },
        K::Insert | K::KP_Insert if shift => Binding::Paste { primary: true },
        K::Page_Up if shift => Binding::Scroll(Scroll::PageUp),
        K::Page_Down if shift => Binding::Scroll(Scroll::PageDown),
        K::Home if shift => Binding::Scroll(Scroll::Top),
        K::End if shift => Binding::Scroll(Scroll::Bottom),
        K::Up if ctrl_shift => Binding::Scroll(Scroll::PreviousPrompt),
        K::Down if ctrl_shift => Binding::Scroll(Scroll::NextPrompt),
        // GDK applies shift to the keyval (ctrl+shift+c comes as `C`).
        _ if ctrl_shift && keyval.to_lower() == K::f => Binding::StartSearch,
        _ if ctrl_shift && keyval.to_lower() == K::c => Binding::Copy { performable: true },
        _ if ctrl_shift && keyval.to_lower() == K::v => Binding::Paste { primary: false },
        _ if ctrl_shift && keyval.to_lower() == K::a => Binding::SelectAll,
        // By the character typed, whether the layout needs shift for it or
        // not: ctrl++ is ctrl+shift+= on a US layout, ctrl+= on a German one.
        _ if held.difference(M::SHIFT_MASK) == M::CONTROL_MASK => match keyval.to_unicode()? {
            '=' | '+' => Binding::FontSize(FontSize::Increase),
            '-' => Binding::FontSize(FontSize::Decrease),
            '0' => Binding::FontSize(FontSize::Reset),
            _ => return None,
        },
        _ => return None,
    };
    Some(binding)
}

/// ghostty's increase_font_size 1, decrease_font_size 1 and reset_font_size
/// from a pane's `points`.
pub fn font_points(points: f32, change: FontSize) -> f32 {
    match change {
        FontSize::Increase => (points + 1.0).min(MAX_POINTS),
        FontSize::Decrease => (points - 1.0).max(MIN_POINTS),
        FontSize::Reset => theme::FONT_POINTS,
    }
}

/// The lines `scroll` moves the view up into the scrollback (negative:
/// down), for `Terminal::scroll_view`, which keeps it within the scrollback.
pub fn scroll_delta(term: &Terminal, scroll: Scroll) -> isize {
    let (rows, offset) = (term.rows() as isize, term.view_offset() as isize);
    match scroll {
        Scroll::PageUp => rows,
        Scroll::PageDown => -rows,
        Scroll::Top => term.history_len() as isize - offset,
        Scroll::Bottom => -offset,
        Scroll::PreviousPrompt | Scroll::NextPrompt => prompt_line(term, scroll == Scroll::PreviousPrompt)
            .map_or(0, |line| term.view_top() as isize - line as isize),
    }
}

/// ghostty's PageList.scrollPrompt by one prompt: the first line of the
/// nearest prompt that starts above the view's top line, or below it past
/// the continuation lines of a prompt there. The view cannot go below the
/// screen, so a prompt on the screen brings it to the bottom.
fn prompt_line(term: &Terminal, up: bool) -> Option<u64> {
    let top = term.view_top();
    if up {
        return selection::prompt_above(term, top.checked_sub(1)?);
    }
    let mut start = top + 1;
    if term.line_prompt(top) != PromptMark::None {
        while term.line_prompt(start) == PromptMark::Continuation && start + 1 < term.end_line() {
            start += 1;
        }
    }
    selection::prompt_below(term, start).map(|(line, _)| line)
}

#[cfg(test)]
mod tests {
    use super::*;
    use gdk::Key as K;
    use gdk::ModifierType as M;

    const CTRL: M = M::CONTROL_MASK;
    const SHIFT: M = M::SHIFT_MASK;

    fn ctrl_shift() -> M {
        CTRL | SHIFT
    }

    #[test]
    fn clipboard_keys() {
        assert_eq!(binding(K::C, ctrl_shift()), Some(Binding::Copy { performable: true }));
        assert_eq!(binding(K::c, ctrl_shift()), Some(Binding::Copy { performable: true }));
        assert_eq!(binding(K::Insert, CTRL), Some(Binding::Copy { performable: false }));
        assert_eq!(binding(K::KP_Insert, CTRL), Some(Binding::Copy { performable: false }));
        assert_eq!(binding(K::Copy, M::empty()), Some(Binding::Copy { performable: false }));
        assert_eq!(binding(K::V, ctrl_shift()), Some(Binding::Paste { primary: false }));
        assert_eq!(binding(K::Paste, M::empty()), Some(Binding::Paste { primary: false }));
        assert_eq!(binding(K::Insert, SHIFT), Some(Binding::Paste { primary: true }));
        // Caps Lock is not a modifier of a binding.
        assert_eq!(binding(K::c, ctrl_shift() | M::LOCK_MASK), Some(Binding::Copy { performable: true }));
        // ctrl+c, the tmux prefix and other modifiers stay with the program.
        assert_eq!(binding(K::c, CTRL), None);
        assert_eq!(binding(K::b, CTRL), None);
        assert_eq!(binding(K::C, ctrl_shift() | M::ALT_MASK), None);
        assert_eq!(binding(K::Insert, M::empty()), None);
        assert_eq!(binding(K::Copy, CTRL), None);
    }

    #[test]
    fn view_and_selection_keys() {
        assert_eq!(binding(K::Page_Up, SHIFT), Some(Binding::Scroll(Scroll::PageUp)));
        assert_eq!(binding(K::Page_Down, SHIFT), Some(Binding::Scroll(Scroll::PageDown)));
        assert_eq!(binding(K::Home, SHIFT), Some(Binding::Scroll(Scroll::Top)));
        assert_eq!(binding(K::End, SHIFT), Some(Binding::Scroll(Scroll::Bottom)));
        assert_eq!(binding(K::Up, ctrl_shift()), Some(Binding::Scroll(Scroll::PreviousPrompt)));
        assert_eq!(binding(K::Down, ctrl_shift()), Some(Binding::Scroll(Scroll::NextPrompt)));
        assert_eq!(binding(K::A, ctrl_shift()), Some(Binding::SelectAll));
        assert_eq!(binding(K::F, ctrl_shift()), Some(Binding::StartSearch));
        assert_eq!(binding(K::Page_Up, M::empty()), None);
        assert_eq!(binding(K::Page_Up, CTRL), None);
        assert_eq!(binding(K::Up, SHIFT), None);
        assert_eq!(binding(K::a, CTRL), None);
    }

    #[test]
    fn font_size_keys_go_by_the_character_typed() {
        let increase = Some(Binding::FontSize(FontSize::Increase));
        assert_eq!(binding(K::equal, CTRL), increase);
        assert_eq!(binding(K::plus, CTRL), increase);
        assert_eq!(binding(K::plus, ctrl_shift()), increase);
        assert_eq!(binding(K::KP_Add, CTRL), increase);
        assert_eq!(binding(K::minus, CTRL), Some(Binding::FontSize(FontSize::Decrease)));
        assert_eq!(binding(K::KP_Subtract, CTRL), Some(Binding::FontSize(FontSize::Decrease)));
        assert_eq!(binding(K::_0, CTRL), Some(Binding::FontSize(FontSize::Reset)));
        assert_eq!(binding(K::equal, M::empty()), None);
        assert_eq!(binding(K::equal, CTRL | M::ALT_MASK), None);
        // ctrl+shift+- types `_` (C-_ for the program), ctrl+shift+0 `)`.
        assert_eq!(binding(K::underscore, ctrl_shift()), None);
        assert_eq!(binding(K::parenright, ctrl_shift()), None);
    }

    #[test]
    fn font_sizes_step_by_a_point_within_bounds() {
        assert_eq!(font_points(12.0, FontSize::Increase), 13.0);
        assert_eq!(font_points(12.0, FontSize::Decrease), 11.0);
        assert_eq!(font_points(MIN_POINTS, FontSize::Decrease), MIN_POINTS);
        assert_eq!(font_points(MAX_POINTS, FontSize::Increase), MAX_POINTS);
        assert_eq!(font_points(30.0, FontSize::Reset), theme::FONT_POINTS);
    }

    fn scrolled(t: &mut Terminal, scroll: Scroll) -> u64 {
        let delta = scroll_delta(t, scroll);
        t.scroll_view(delta);
        t.view_top()
    }

    #[test]
    fn pages_and_ends_of_the_scrollback() {
        let mut t = Terminal::new(10, 4, 100);
        for i in 0..30 {
            t.feed(format!("{i}\r\n").as_bytes());
        }
        // 31 lines: 27 in the scrollback, the screen shows lines 27 to 30.
        assert_eq!(t.view_top(), 27);
        assert_eq!(scrolled(&mut t, Scroll::PageUp), 23);
        assert_eq!(scrolled(&mut t, Scroll::PageUp), 19);
        assert_eq!(scrolled(&mut t, Scroll::PageDown), 23);
        assert_eq!(scrolled(&mut t, Scroll::Top), 0);
        assert_eq!(scrolled(&mut t, Scroll::PageUp), 0);
        assert_eq!(scrolled(&mut t, Scroll::Bottom), 27);
        assert_eq!(scrolled(&mut t, Scroll::PageDown), 27);
        // The alternate screen has no scrollback to move in.
        t.feed(b"\x1b[?1049h");
        for scroll in [Scroll::PageUp, Scroll::Top] {
            scrolled(&mut t, scroll);
            assert_eq!(t.view_offset(), 0);
        }
    }

    #[test]
    fn jumps_put_the_previous_or_next_prompt_at_the_top() {
        let mut t = Terminal::new(20, 4, 100);
        for i in 0..3 {
            t.feed(format!("\x1b]133;A\x07$ \x1b]133;B\x07cmd{i}\r\n\x1b]133;C\x07").as_bytes());
            for j in 0..5 {
                t.feed(format!("out{i}.{j}\r\n").as_bytes());
            }
        }
        t.feed(b"\x1b]133;A\x07$ ");
        // Prompts start lines 0, 6, 12 and 18; the screen shows 15 to 18.
        assert_eq!(t.view_top(), 15);
        assert_eq!(scrolled(&mut t, Scroll::PreviousPrompt), 12);
        assert_eq!(scrolled(&mut t, Scroll::PreviousPrompt), 6);
        assert_eq!(scrolled(&mut t, Scroll::PreviousPrompt), 0);
        assert_eq!(scroll_delta(&t, Scroll::PreviousPrompt), 0);
        assert_eq!(scrolled(&mut t, Scroll::NextPrompt), 6);
        assert_eq!(scrolled(&mut t, Scroll::NextPrompt), 12);
        // The last prompt is on the screen: the view goes to the bottom.
        assert_eq!(scrolled(&mut t, Scroll::NextPrompt), 15);
        assert_eq!(scrolled(&mut t, Scroll::NextPrompt), 15);
        // Between prompts, up goes to the one above the top line.
        t.scroll_view(7);
        assert_eq!(t.view_top(), 8);
        assert_eq!(scrolled(&mut t, Scroll::PreviousPrompt), 6);
    }

    #[test]
    fn jumps_skip_the_continuation_lines_of_a_prompt() {
        let mut t = Terminal::new(20, 3, 100);
        for i in 0..2 {
            // A two-line prompt: the line feed inside it marks a continuation.
            t.feed(format!("\x1b]133;A\x07${i}\r\n> \x1b]133;B\x07cmd\r\n\x1b]133;C\x07out\r\nout\r\n").as_bytes());
        }
        t.feed(b"\x1b]133;A\x07$ ");
        assert_eq!(t.line_prompt(1), PromptMark::Continuation);
        assert_eq!(t.line_prompt(5), PromptMark::Continuation);
        // Prompts start lines 0, 4 and 8; the screen shows 6 to 8.
        assert_eq!(t.view_top(), 6);
        assert_eq!(scrolled(&mut t, Scroll::PreviousPrompt), 4);
        assert_eq!(scrolled(&mut t, Scroll::PreviousPrompt), 0);
        assert_eq!(scrolled(&mut t, Scroll::NextPrompt), 4);
        assert_eq!(scrolled(&mut t, Scroll::NextPrompt), 6);
    }

    #[test]
    fn no_prompt_no_jump() {
        let mut t = Terminal::new(10, 4, 100);
        for i in 0..30 {
            t.feed(format!("{i}\r\n").as_bytes());
        }
        assert_eq!(scroll_delta(&t, Scroll::PreviousPrompt), 0);
        t.scroll_view(10);
        assert_eq!(scroll_delta(&t, Scroll::PreviousPrompt), 0);
        assert_eq!(scroll_delta(&t, Scroll::NextPrompt), 0);
    }
}
