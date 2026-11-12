//! The terminal-level key bindings: ghostty's `keybind` option for the
//! actions tapust has, with ghostty's defaults on Linux (Config.zig); how a
//! key press finds its binding (Binding.Set.getEvent); and how the scrolling
//! and font size bindings change the view.

use gtk::prelude::*;
use gtk::{gdk, glib};
use term::{PromptMark, Terminal};

use crate::selection;
use Physical::{Evdev, Keyvals};
use gdk::Key as K;

/// The font size never goes below this, nor above the size whose glyphs
/// still fit the renderer's fixed glyph atlas in useful numbers.
pub const MIN_POINTS: f32 = 4.0;
pub const MAX_POINTS: f32 = 72.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Binding {
    StartSearch,
    /// copy_to_clipboard (mixed).
    Copy,
    /// paste_from_clipboard, or paste_from_selection when `primary`.
    Paste { primary: bool },
    Scroll(Scroll),
    SelectAll,
    FontSize(FontSize),
    ToggleCommandPalette,
    /// open_config: os_open, or new_window.
    OpenConfig { new_window: bool },
    ReloadConfig,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scroll {
    PageUp,
    PageDown,
    Top,
    Bottom,
    /// jump_to_prompt: that many prompts down (negative: up).
    Prompt(i16),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum FontSize {
    Increase(f32),
    Decrease(f32),
    Reset,
}

/// A binding with ghostty's flags for it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Bound {
    pub binding: Binding,
    /// `performable:`: a binding that cannot be performed is no binding,
    /// and the key goes on to the program.
    pub performable: bool,
    /// Without `unconsumed:`, the key goes no further.
    pub consumed: bool,
}

type M = gdk::ModifierType;
const BINDING_MODS: M = M::CONTROL_MASK.union(M::SHIFT_MASK).union(M::ALT_MASK).union(M::SUPER_MASK);
/// The modifiers of a press: Caps Lock and Num Lock are none, Hyper and
/// Meta are ones no trigger has.
const HELD_MODS: M = BINDING_MODS.union(M::HYPER_MASK).union(M::META_MASK);

/// A key press as the bindings see it.
#[derive(Clone, Copy, Debug)]
pub struct Press {
    pub keyval: gdk::Key,
    /// The hardware keycode, which the writing system keys bind by.
    pub keycode: u32,
    pub state: M,
    /// The modifiers the layout used to make `keyval` (Shift for `+`).
    pub consumed: M,
    /// The character of the key in the current layout without modifiers
    /// (ghostty's unshifted_codepoint).
    pub unshifted: Option<char>,
}

impl Press {
    /// The press `keys` is handling.
    pub fn new(keys: &gtk::EventControllerKey, keyval: gdk::Key, keycode: u32, state: M) -> Press {
        let event = keys.current_event().and_then(|e| e.downcast::<gdk::KeyEvent>().ok());
        let consumed = event.as_ref().map_or(M::empty(), |e| e.consumed_modifiers());
        let unshifted = event.and_then(|event| {
            let layout = event.layout();
            let keys = event.display()?.map_keycode(keycode)?;
            let (_, keyval) = keys.into_iter().find(|(k, _)| k.group() as u32 == layout && k.level() == 0)?;
            keyval.to_unicode()
        });
        Press { keyval, keycode, state, consumed, unshifted }
    }
}

/// How a physical key is recognized: by its keyvals where they do not
/// depend on the layout, else by its evdev code (with its US character).
#[derive(Clone, Copy, Debug)]
enum Physical {
    Keyvals(&'static [gdk::Key]),
    Evdev(u32, char),
}

/// ghostty's physical keys (input/key.zig, W3C code names in snake case)
/// that a GDK key press can be told to be.
#[rustfmt::skip]
const PHYSICAL: &[(&str, Physical)] = &[
    ("backquote", Evdev(41, '`')), ("backslash", Evdev(43, '\\')), ("bracket_left", Evdev(26, '[')),
    ("bracket_right", Evdev(27, ']')), ("comma", Evdev(51, ',')), ("digit_0", Evdev(11, '0')),
    ("digit_1", Evdev(2, '1')), ("digit_2", Evdev(3, '2')), ("digit_3", Evdev(4, '3')), ("digit_4", Evdev(5, '4')),
    ("digit_5", Evdev(6, '5')), ("digit_6", Evdev(7, '6')), ("digit_7", Evdev(8, '7')), ("digit_8", Evdev(9, '8')),
    ("digit_9", Evdev(10, '9')), ("equal", Evdev(13, '=')), ("intl_backslash", Evdev(86, '<')),
    ("intl_ro", Evdev(89, '\\')), ("intl_yen", Evdev(124, '\u{a5}')),
    ("key_a", Evdev(30, 'a')), ("key_b", Evdev(48, 'b')), ("key_c", Evdev(46, 'c')), ("key_d", Evdev(32, 'd')),
    ("key_e", Evdev(18, 'e')), ("key_f", Evdev(33, 'f')), ("key_g", Evdev(34, 'g')), ("key_h", Evdev(35, 'h')),
    ("key_i", Evdev(23, 'i')), ("key_j", Evdev(36, 'j')), ("key_k", Evdev(37, 'k')), ("key_l", Evdev(38, 'l')),
    ("key_m", Evdev(50, 'm')), ("key_n", Evdev(49, 'n')), ("key_o", Evdev(24, 'o')), ("key_p", Evdev(25, 'p')),
    ("key_q", Evdev(16, 'q')), ("key_r", Evdev(19, 'r')), ("key_s", Evdev(31, 's')), ("key_t", Evdev(20, 't')),
    ("key_u", Evdev(22, 'u')), ("key_v", Evdev(47, 'v')), ("key_w", Evdev(17, 'w')), ("key_x", Evdev(45, 'x')),
    ("key_y", Evdev(21, 'y')), ("key_z", Evdev(44, 'z')), ("minus", Evdev(12, '-')), ("period", Evdev(52, '.')),
    ("quote", Evdev(40, '\'')), ("semicolon", Evdev(39, ';')), ("slash", Evdev(53, '/')),
    ("backspace", Keyvals(&[K::BackSpace])), ("caps_lock", Keyvals(&[K::Caps_Lock])),
    ("context_menu", Keyvals(&[K::Menu])), ("enter", Keyvals(&[K::Return])), ("space", Keyvals(&[K::space])),
    ("tab", Keyvals(&[K::Tab, K::ISO_Left_Tab])), ("delete", Keyvals(&[K::Delete])), ("end", Keyvals(&[K::End])),
    ("help", Keyvals(&[K::Help])), ("home", Keyvals(&[K::Home])), ("insert", Keyvals(&[K::Insert])),
    ("page_down", Keyvals(&[K::Page_Down])), ("page_up", Keyvals(&[K::Page_Up])),
    ("arrow_down", Keyvals(&[K::Down])), ("arrow_left", Keyvals(&[K::Left])), ("arrow_right", Keyvals(&[K::Right])),
    ("arrow_up", Keyvals(&[K::Up])), ("num_lock", Keyvals(&[K::Num_Lock])),
    ("numpad_0", Keyvals(&[K::KP_0])), ("numpad_1", Keyvals(&[K::KP_1])), ("numpad_2", Keyvals(&[K::KP_2])),
    ("numpad_3", Keyvals(&[K::KP_3])), ("numpad_4", Keyvals(&[K::KP_4])), ("numpad_5", Keyvals(&[K::KP_5])),
    ("numpad_6", Keyvals(&[K::KP_6])), ("numpad_7", Keyvals(&[K::KP_7])), ("numpad_8", Keyvals(&[K::KP_8])),
    ("numpad_9", Keyvals(&[K::KP_9])), ("numpad_add", Keyvals(&[K::KP_Add])),
    ("numpad_decimal", Keyvals(&[K::KP_Decimal])), ("numpad_divide", Keyvals(&[K::KP_Divide])),
    ("numpad_enter", Keyvals(&[K::KP_Enter])), ("numpad_equal", Keyvals(&[K::KP_Equal])),
    ("numpad_multiply", Keyvals(&[K::KP_Multiply])), ("numpad_subtract", Keyvals(&[K::KP_Subtract])),
    ("numpad_separator", Keyvals(&[K::KP_Separator])), ("numpad_up", Keyvals(&[K::KP_Up])),
    ("numpad_down", Keyvals(&[K::KP_Down])), ("numpad_right", Keyvals(&[K::KP_Right])),
    ("numpad_left", Keyvals(&[K::KP_Left])), ("numpad_begin", Keyvals(&[K::KP_Begin])),
    ("numpad_home", Keyvals(&[K::KP_Home])), ("numpad_end", Keyvals(&[K::KP_End])),
    ("numpad_insert", Keyvals(&[K::KP_Insert])), ("numpad_delete", Keyvals(&[K::KP_Delete])),
    ("numpad_page_up", Keyvals(&[K::KP_Page_Up])), ("numpad_page_down", Keyvals(&[K::KP_Page_Down])),
    ("escape", Keyvals(&[K::Escape])), ("f1", Keyvals(&[K::F1])), ("f2", Keyvals(&[K::F2])),
    ("f3", Keyvals(&[K::F3])), ("f4", Keyvals(&[K::F4])), ("f5", Keyvals(&[K::F5])), ("f6", Keyvals(&[K::F6])),
    ("f7", Keyvals(&[K::F7])), ("f8", Keyvals(&[K::F8])), ("f9", Keyvals(&[K::F9])), ("f10", Keyvals(&[K::F10])),
    ("f11", Keyvals(&[K::F11])), ("f12", Keyvals(&[K::F12])), ("f13", Keyvals(&[K::F13])),
    ("f14", Keyvals(&[K::F14])), ("f15", Keyvals(&[K::F15])), ("f16", Keyvals(&[K::F16])),
    ("f17", Keyvals(&[K::F17])), ("f18", Keyvals(&[K::F18])), ("f19", Keyvals(&[K::F19])),
    ("f20", Keyvals(&[K::F20])), ("f21", Keyvals(&[K::F21])), ("f22", Keyvals(&[K::F22])),
    ("f23", Keyvals(&[K::F23])), ("f24", Keyvals(&[K::F24])), ("f25", Keyvals(&[K::F25])),
    ("print_screen", Keyvals(&[K::Print])), ("scroll_lock", Keyvals(&[K::Scroll_Lock])),
    ("pause", Keyvals(&[K::Pause])), ("copy", Keyvals(&[K::Copy])), ("cut", Keyvals(&[K::Cut])),
    ("paste", Keyvals(&[K::Paste])),
];

/// ghostty 1.1's key names that still work (backwards_compatible_keys).
#[rustfmt::skip]
const OLD_NAMES: &[(&str, &str)] = &[
    ("grave_accent", "backquote"), ("left_bracket", "bracket_left"), ("right_bracket", "bracket_right"),
    ("up", "arrow_up"), ("down", "arrow_down"), ("left", "arrow_left"), ("right", "arrow_right"),
    ("kp_0", "numpad_0"), ("kp_1", "numpad_1"), ("kp_2", "numpad_2"), ("kp_3", "numpad_3"), ("kp_4", "numpad_4"),
    ("kp_5", "numpad_5"), ("kp_6", "numpad_6"), ("kp_7", "numpad_7"), ("kp_8", "numpad_8"), ("kp_9", "numpad_9"),
    ("kp_add", "numpad_add"), ("kp_subtract", "numpad_subtract"), ("kp_multiply", "numpad_multiply"),
    ("kp_divide", "numpad_divide"), ("kp_decimal", "numpad_decimal"), ("kp_enter", "numpad_enter"),
    ("kp_equal", "numpad_equal"), ("kp_separator", "numpad_separator"), ("kp_left", "numpad_left"),
    ("kp_right", "numpad_right"), ("kp_up", "numpad_up"), ("kp_down", "numpad_down"),
    ("kp_page_up", "numpad_page_up"), ("kp_page_down", "numpad_page_down"), ("kp_home", "numpad_home"),
    ("kp_end", "numpad_end"), ("kp_insert", "numpad_insert"), ("kp_delete", "numpad_delete"),
];
const OLD_CHARS: &[(&str, char)] = &[
    ("zero", '0'), ("one", '1'), ("two", '2'), ("three", '3'), ("four", '4'), ("five", '5'), ("six", '6'),
    ("seven", '7'), ("eight", '8'), ("nine", '9'), ("plus", '+'), ("apostrophe", '\''),
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TriggerKey {
    /// A character, case folded, matched with what the key types.
    Char(char),
    /// An index into PHYSICAL.
    Physical(usize),
}

/// ghostty's Trigger: modifiers and a key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Trigger {
    mods: M,
    key: TriggerKey,
}

/// ghostty compares characters case folded.
fn fold(c: char) -> char {
    let mut lower = c.to_lowercase();
    match (lower.next(), lower.next()) {
        (Some(l), None) => l,
        _ => c,
    }
}

fn physical(name: &str) -> Option<usize> {
    PHYSICAL.iter().position(|(n, _)| *n == name)
}

/// ghostty's Key.fromW3C: `KeyA`, `ArrowUp` or `F1` as `key_a`, `arrow_up`, `f1`.
fn from_w3c(code: &str) -> Option<usize> {
    if let Some(i) = physical(&code.to_ascii_lowercase()) {
        return Some(i);
    }
    let mut name = String::new();
    for (i, c) in code.chars().enumerate() {
        match c {
            'a'..='z' => name.push(c),
            'A'..='Z' | '0'..='9' => {
                if i > 0 {
                    name.push('_');
                }
                name.push(c.to_ascii_lowercase());
            }
            _ => return None,
        }
    }
    physical(&name)
}

impl Trigger {
    /// ghostty's Trigger.parse: `+`-separated modifiers (or their aliases)
    /// and one key, a physical key's name or a character; `+` alone (an
    /// empty part) is the plus key.
    fn parse(input: &str) -> Option<Trigger> {
        let (mut mods, mut key) = (M::empty(), None);
        let mut rest = input;
        while !rest.is_empty() {
            let part;
            (part, rest) = rest.split_once('+').unwrap_or((rest, ""));
            let modifier = match part {
                "shift" => Some(M::SHIFT_MASK),
                "ctrl" | "control" => Some(M::CONTROL_MASK),
                "alt" | "opt" | "option" => Some(M::ALT_MASK),
                "super" | "cmd" | "command" => Some(M::SUPER_MASK),
                _ => None,
            };
            if let Some(m) = modifier {
                if mods.contains(m) {
                    return None;
                }
                mods |= m;
                continue;
            }
            if key.is_some() {
                return None;
            }
            let mut chars = part.chars();
            key = Some(match (chars.next(), chars.next()) {
                (None, _) => TriggerKey::Char('+'),
                _ if physical(part).is_some() => TriggerKey::Physical(physical(part)?),
                (Some(c), None) => TriggerKey::Char(fold(c)),
                _ => match from_w3c(part) {
                    Some(i) => TriggerKey::Physical(i),
                    None => match OLD_NAMES.iter().find(|(old, _)| *old == part) {
                        Some((_, name)) => TriggerKey::Physical(physical(name)?),
                        None => TriggerKey::Char(OLD_CHARS.iter().find(|(old, _)| *old == part)?.1),
                    },
                },
            });
        }
        Some(Trigger { mods, key: key? })
    }

    /// The trigger as a GTK accelerator for AdwShortcutLabel.
    pub fn accelerator(&self) -> String {
        let mut accel = String::new();
        let names = [
            (M::SHIFT_MASK, "<Shift>"),
            (M::CONTROL_MASK, "<Control>"),
            (M::ALT_MASK, "<Alt>"),
            (M::SUPER_MASK, "<Super>"),
        ];
        for (mask, name) in names {
            if self.mods.contains(mask) {
                accel.push_str(name);
            }
        }
        let key = match self.key {
            TriggerKey::Char(c) => key_of(c),
            TriggerKey::Physical(i) => match PHYSICAL[i].1 {
                Keyvals(keys) => keys[0],
                Evdev(_, c) => key_of(c),
            },
        };
        accel + &key.name().map(String::from).unwrap_or_default()
    }
}

/// The GDK key that types `c`.
pub fn key_of(c: char) -> gdk::Key {
    // A keyval is any u32: from_glib cannot make an invalid Key.
    unsafe { glib::translate::from_glib(gdk::unicode_to_keyval(u32::from(c))) }
}

/// What ghostty's Action.parse makes of the actions tapust has; `None`
/// for `unbind`.
fn parse_action(input: &str) -> Option<Option<Binding>> {
    let (name, param) = match input.split_once(':') {
        Some((name, param)) => (name, Some(param)),
        None => (input, None),
    };
    let amount = |p: Option<&str>| p?.parse::<f32>().ok().filter(|a| a.is_finite());
    Some(Some(match (name, param) {
        ("unbind", None) => return Some(None),
        ("copy_to_clipboard", None | Some("mixed")) => Binding::Copy,
        ("paste_from_clipboard", None) => Binding::Paste { primary: false },
        ("paste_from_selection", None) => Binding::Paste { primary: true },
        ("select_all", None) => Binding::SelectAll,
        ("start_search", None) => Binding::StartSearch,
        ("scroll_page_up", None) => Binding::Scroll(Scroll::PageUp),
        ("scroll_page_down", None) => Binding::Scroll(Scroll::PageDown),
        ("scroll_to_top", None) => Binding::Scroll(Scroll::Top),
        ("scroll_to_bottom", None) => Binding::Scroll(Scroll::Bottom),
        ("jump_to_prompt", Some(p)) => Binding::Scroll(Scroll::Prompt(p.parse().ok()?)),
        ("increase_font_size", p) => Binding::FontSize(FontSize::Increase(amount(p)?)),
        ("decrease_font_size", p) => Binding::FontSize(FontSize::Decrease(amount(p)?)),
        ("reset_font_size", None) => Binding::FontSize(FontSize::Reset),
        ("toggle_command_palette", None) => Binding::ToggleCommandPalette,
        ("open_config", None | Some("os_open")) => Binding::OpenConfig { new_window: false },
        ("open_config", Some("new_window")) => Binding::OpenConfig { new_window: true },
        ("reload_config", None) => Binding::ReloadConfig,
        _ => return None,
    }))
}

/// The `keybind` option: triggers and their bindings, in the order they
/// were last set.
#[derive(Clone, Debug, PartialEq)]
pub struct Keybinds(Vec<(Trigger, Bound)>);

impl Default for Keybinds {
    /// ghostty's default keybinds on Linux for the actions tapust has, in
    /// ghostty's order: the last trigger set for an action is the one the
    /// command palette shows. tapust also binds the keypad's Insert as
    /// Insert.
    fn default() -> Keybinds {
        let mut keybinds = Keybinds(Vec::new());
        for entry in [
            "ctrl+shift+,=reload_config",
            "ctrl+,=open_config",
            "copy=copy_to_clipboard:mixed",
            "paste=paste_from_clipboard",
            "ctrl+numpad_insert=copy_to_clipboard:mixed",
            "ctrl+insert=copy_to_clipboard:mixed",
            "shift+insert=paste_from_clipboard",
            "performable:ctrl+shift+c=copy_to_clipboard:mixed",
            "performable:ctrl+shift+v=paste_from_clipboard",
            "ctrl+==increase_font_size:1",
            "ctrl++=increase_font_size:1",
            "ctrl+-=decrease_font_size:1",
            "ctrl+0=reset_font_size",
            "shift+home=scroll_to_top",
            "shift+end=scroll_to_bottom",
            "shift+page_up=scroll_page_up",
            "shift+page_down=scroll_page_down",
            "ctrl+shift+arrow_up=jump_to_prompt:-1",
            "ctrl+shift+arrow_down=jump_to_prompt:1",
            "performable:ctrl+shift+f=start_search",
            "ctrl+shift+a=select_all",
            "shift+numpad_insert=paste_from_selection",
            "shift+insert=paste_from_selection",
            "ctrl+shift+p=toggle_command_palette",
        ] {
            keybinds.parse(entry).expect("a default keybind");
        }
        keybinds
    }
}

impl Keybinds {
    /// `keybind = clear`.
    pub fn clear(&mut self) {
        self.0.clear();
    }

    /// One `keybind` value, `[performable:][unconsumed:]trigger=action`,
    /// which replaces the trigger's binding (Binding.Parser and
    /// Set.parseAndPut without sequences, chains, key tables and the all:
    /// and global: prefixes); `None` when tapust cannot take it.
    pub fn parse(&mut self, input: &str) -> Option<()> {
        let (mut performable, mut consumed) = (false, true);
        let mut rest = input;
        while let Some((prefix, after)) = rest.split_once(':') {
            match prefix {
                "performable" if !performable => performable = true,
                "unconsumed" if consumed => consumed = false,
                "performable" | "unconsumed" | "all" | "global" => return None,
                _ => break,
            }
            rest = after;
        }
        // The `=` that ends the trigger is not followed by `+` or `=`, which
        // make it the trigger's key (`ctrl+==...`, `=+ctrl=...`).
        let bytes = rest.as_bytes();
        let eq = (0..bytes.len())
            .find(|&i| bytes[i] == b'=' && !(i + 1 < bytes.len() && matches!(bytes[i + 1], b'+' | b'=')))?;
        let trigger = &rest[..eq];
        if trigger.contains('>') {
            return None;
        }
        let trigger = Trigger::parse(trigger)?;
        let binding = parse_action(&rest[eq + 1..])?;
        self.0.retain(|(t, _)| *t != trigger);
        if let Some(binding) = binding {
            self.0.push((trigger, Bound { binding, performable, consumed }));
        }
        Some(())
    }

    /// ghostty's Set.getEvent: the binding of the physical key, else of
    /// the character the key types, else of its unshifted character. A
    /// character that has no case and needs Shift to be typed also binds
    /// without the Shift (ctrl++ is ctrl+shift+= on a US layout); a letter's
    /// Shift counts (ctrl+a is not ctrl+shift+a).
    pub fn lookup(&self, press: &Press) -> Option<Bound> {
        let held = press.state & HELD_MODS;
        let find = |matches: &dyn Fn(&Trigger) -> bool| self.0.iter().rev().find(|(t, _)| matches(t)).map(|(_, b)| *b);
        let physical = |t: &Trigger| {
            t.mods == held
                && matches!(t.key, TriggerKey::Physical(i) if match PHYSICAL[i].1 {
                    Keyvals(keys) => keys.contains(&press.keyval),
                    // GDK's keycodes on Linux are evdev's plus 8.
                    Evdev(code, _) => press.keycode == code + 8,
                })
        };
        let typed = press.keyval.to_unicode().filter(|c| !c.is_control());
        let by_typed = |c: char| {
            let key = TriggerKey::Char(fold(c));
            let shifted = press.consumed.contains(M::SHIFT_MASK) && !c.is_lowercase() && !c.is_uppercase();
            let unshifted = held.difference(M::SHIFT_MASK);
            find(&|t: &Trigger| t.key == key && t.mods == held)
                .or_else(|| find(&|t: &Trigger| shifted && t.key == key && t.mods == unshifted))
        };
        let by_unshifted = |u: char| find(&|t: &Trigger| t.mods == held && t.key == TriggerKey::Char(fold(u)));
        find(&physical).or_else(|| typed.and_then(by_typed)).or_else(|| press.unshifted.and_then(by_unshifted))
    }

    /// The trigger last set for `binding`, which menus and the palette show.
    pub fn trigger(&self, binding: Binding) -> Option<Trigger> {
        self.0.iter().rev().find(|(_, b)| b.binding == binding).map(|(t, _)| *t)
    }
}

/// ghostty's increase_font_size, decrease_font_size and reset_font_size
/// from a pane's `points`; reset goes back to `base`.
pub fn font_points(points: f32, change: FontSize, base: f32) -> f32 {
    match change {
        FontSize::Increase(delta) => (points + delta.max(0.0)).min(MAX_POINTS),
        FontSize::Decrease(delta) => (points - delta.max(0.0)).max(MIN_POINTS),
        FontSize::Reset => base,
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
        Scroll::Prompt(delta) => {
            // ghostty's PageList.scrollPrompt: the delta-th prompt, or the
            // last one there is.
            let mut top = term.view_top();
            for _ in 0..delta.unsigned_abs() {
                match prompt_line(term, top, delta < 0) {
                    Some(line) => top = line,
                    None => break,
                }
            }
            term.view_top() as isize - top as isize
        }
    }
}

/// The first line of the nearest prompt that starts above line `top`, or
/// below it past the continuation lines of a prompt there. The view cannot
/// go below the screen, so a prompt on the screen brings it to the bottom.
fn prompt_line(term: &Terminal, top: u64, up: bool) -> Option<u64> {
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

    /// The default binding of a press without its hardware keycode or
    /// unshifted character.
    fn binding(keyval: gdk::Key, state: M) -> Option<Binding> {
        bound(&Keybinds::default(), keyval, state).map(|b| b.binding)
    }

    fn bound(keybinds: &Keybinds, keyval: gdk::Key, state: M) -> Option<Bound> {
        keybinds.lookup(&Press { keyval, keycode: 0, state, consumed: M::empty(), unshifted: None })
    }

    fn performable(keyval: gdk::Key, state: M) -> bool {
        bound(&Keybinds::default(), keyval, state).unwrap().performable
    }

    const PREVIOUS_PROMPT: Binding = Binding::Scroll(Scroll::Prompt(-1));
    const NEXT_PROMPT: Binding = Binding::Scroll(Scroll::Prompt(1));

    #[test]
    fn clipboard_keys() {
        let paste = Some(Binding::Paste { primary: false });
        assert_eq!(binding(K::C, ctrl_shift()), Some(Binding::Copy));
        assert!(performable(K::C, ctrl_shift()));
        assert_eq!(binding(K::c, ctrl_shift()), Some(Binding::Copy));
        assert_eq!(binding(K::Insert, CTRL), Some(Binding::Copy));
        assert!(!performable(K::Insert, CTRL));
        assert_eq!(binding(K::KP_Insert, CTRL), Some(Binding::Copy));
        assert_eq!(binding(K::Copy, M::empty()), Some(Binding::Copy));
        assert_eq!(binding(K::V, ctrl_shift()), paste);
        assert_eq!(binding(K::Paste, M::empty()), paste);
        assert_eq!(binding(K::Insert, SHIFT), Some(Binding::Paste { primary: true }));
        assert_eq!(binding(K::KP_Insert, SHIFT), Some(Binding::Paste { primary: true }));
        // Caps Lock is not a modifier of a binding.
        assert_eq!(binding(K::c, ctrl_shift() | M::LOCK_MASK), Some(Binding::Copy));
        // ctrl+c, the tmux prefix and other modifiers stay with the program.
        assert_eq!(binding(K::c, CTRL), None);
        assert_eq!(binding(K::b, CTRL), None);
        assert_eq!(binding(K::C, ctrl_shift() | M::ALT_MASK), None);
        assert_eq!(binding(K::C, ctrl_shift() | M::HYPER_MASK), None);
        assert_eq!(binding(K::Insert, M::empty()), None);
        assert_eq!(binding(K::Copy, CTRL), None);
    }

    #[test]
    fn view_and_selection_keys() {
        assert_eq!(binding(K::Page_Up, SHIFT), Some(Binding::Scroll(Scroll::PageUp)));
        assert_eq!(binding(K::Page_Down, SHIFT), Some(Binding::Scroll(Scroll::PageDown)));
        assert_eq!(binding(K::Home, SHIFT), Some(Binding::Scroll(Scroll::Top)));
        assert_eq!(binding(K::End, SHIFT), Some(Binding::Scroll(Scroll::Bottom)));
        assert_eq!(binding(K::Up, ctrl_shift()), Some(PREVIOUS_PROMPT));
        assert_eq!(binding(K::Down, ctrl_shift()), Some(NEXT_PROMPT));
        assert_eq!(binding(K::A, ctrl_shift()), Some(Binding::SelectAll));
        assert_eq!(binding(K::F, ctrl_shift()), Some(Binding::StartSearch));
        assert_eq!(binding(K::P, ctrl_shift()), Some(Binding::ToggleCommandPalette));
        assert_eq!(binding(K::p, CTRL), None);
        assert_eq!(binding(K::Page_Up, M::empty()), None);
        assert_eq!(binding(K::Page_Up, CTRL), None);
        assert_eq!(binding(K::KP_Page_Up, SHIFT), None);
        assert_eq!(binding(K::Up, SHIFT), None);
        assert_eq!(binding(K::a, CTRL), None);
    }

    /// A press whose keyval the layout made with Shift.
    fn shifted(keyval: gdk::Key, state: M) -> Option<Binding> {
        let press = Press { keyval, keycode: 0, state, consumed: SHIFT, unshifted: None };
        Keybinds::default().lookup(&press).map(|b| b.binding)
    }

    #[test]
    fn font_size_keys_go_by_the_character_typed() {
        let increase = Some(Binding::FontSize(FontSize::Increase(1.0)));
        assert_eq!(binding(K::equal, CTRL), increase);
        assert_eq!(binding(K::plus, CTRL), increase);
        assert_eq!(shifted(K::plus, ctrl_shift()), increase);
        assert_eq!(binding(K::plus, ctrl_shift()), None);
        assert_eq!(binding(K::KP_Add, CTRL), increase);
        assert_eq!(binding(K::minus, CTRL), Some(Binding::FontSize(FontSize::Decrease(1.0))));
        assert_eq!(binding(K::KP_Subtract, CTRL), Some(Binding::FontSize(FontSize::Decrease(1.0))));
        assert_eq!(binding(K::_0, CTRL), Some(Binding::FontSize(FontSize::Reset)));
        assert_eq!(binding(K::equal, M::empty()), None);
        assert_eq!(binding(K::equal, CTRL | M::ALT_MASK), None);
        // ctrl+shift+- types `_` (C-_ for the program), ctrl+shift+0 `)`.
        assert_eq!(shifted(K::underscore, ctrl_shift()), None);
        assert_eq!(shifted(K::parenright, ctrl_shift()), None);
        // Shift typing a letter is no Shift to leave out.
        assert_eq!(shifted(K::C, ctrl_shift()), Some(Binding::Copy));
        assert_eq!(shifted(K::P, ctrl_shift()), Some(Binding::ToggleCommandPalette));
    }

    /// ctrl+, opens the configuration and ctrl+shift+, reloads it: on a US
    /// layout Shift makes the comma key type `<`, so the reload binds by
    /// the key's unshifted character.
    #[test]
    fn configuration_keys() {
        let keybinds = Keybinds::default();
        let press = |keyval, state, unshifted| {
            keybinds.lookup(&Press { keyval, keycode: 59, state, consumed: M::empty(), unshifted }).map(|b| b.binding)
        };
        assert_eq!(press(K::comma, CTRL, Some(',')), Some(Binding::OpenConfig { new_window: false }));
        assert_eq!(press(K::less, ctrl_shift(), Some(',')), Some(Binding::ReloadConfig));
        // A layout whose comma key types a comma with Shift too.
        assert_eq!(press(K::comma, ctrl_shift(), Some(',')), Some(Binding::ReloadConfig));
        assert_eq!(press(K::less, CTRL, Some('<')), None);
        assert_eq!(press(K::comma, M::empty(), Some(',')), None);
    }

    fn keybinds(entries: &[&str]) -> Keybinds {
        let mut keybinds = Keybinds::default();
        for entry in entries {
            keybinds.parse(entry).unwrap_or_else(|| panic!("{entry}"));
        }
        keybinds
    }

    #[test]
    fn triggers_read_as_ghostty_reads_them() {
        let char_key = |mods, c| Some(Trigger { mods, key: TriggerKey::Char(c) });
        let physical_key = |mods, name| Some(Trigger { mods, key: TriggerKey::Physical(physical(name).unwrap()) });
        assert_eq!(Trigger::parse("ctrl+shift+a"), char_key(ctrl_shift(), 'a'));
        assert_eq!(Trigger::parse("shift+A+control"), char_key(ctrl_shift(), 'a'));
        assert_eq!(Trigger::parse("alt+opt"), None);
        assert_eq!(Trigger::parse("super+cmd+a"), None);
        assert_eq!(Trigger::parse("ctrl++"), char_key(CTRL, '+'));
        assert_eq!(Trigger::parse("+"), char_key(M::empty(), '+'));
        assert_eq!(Trigger::parse("ctrl+plus"), char_key(CTRL, '+'));
        assert_eq!(Trigger::parse("ctrl+\u{e9}"), char_key(CTRL, '\u{e9}'));
        assert_eq!(Trigger::parse("ctrl+\u{c9}"), char_key(CTRL, '\u{e9}'));
        assert_eq!(Trigger::parse("option+command+f5"), physical_key(M::ALT_MASK | M::SUPER_MASK, "f5"));
        assert_eq!(Trigger::parse("KeyA"), physical_key(M::empty(), "key_a"));
        assert_eq!(Trigger::parse("shift+ArrowUp"), physical_key(SHIFT, "arrow_up"));
        assert_eq!(Trigger::parse("up"), physical_key(M::empty(), "arrow_up"));
        assert_eq!(Trigger::parse("kp_insert"), physical_key(M::empty(), "numpad_insert"));
        assert_eq!(Trigger::parse("Digit1"), physical_key(M::empty(), "digit_1"));
        for bad in ["", "ctrl", "ctrl+", "a+b", "ctrl+ab", "catch_all", "hyper+a", "audio_volume_up"] {
            assert_eq!(Trigger::parse(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn keybinds_set_unbind_and_clear() {
        let k = keybinds(&[
            "ctrl+shift+k=scroll_page_up",
            "ctrl+shift+c=unbind",
            "unconsumed:ctrl+shift+j=jump_to_prompt:-3",
            "performable:ctrl+shift+v=copy_to_clipboard",
            "ctrl+==increase_font_size:2.5",
            "alt+f1=open_config:new_window",
        ]);
        let get = |keyval, state| bound(&k, keyval, state);
        assert_eq!(get(K::K, ctrl_shift()).map(|b| b.binding), Some(Binding::Scroll(Scroll::PageUp)));
        assert_eq!(get(K::C, ctrl_shift()), None);
        let jump = get(K::J, ctrl_shift()).unwrap();
        let flags = (jump.binding, jump.consumed, jump.performable);
        assert_eq!(flags, (Binding::Scroll(Scroll::Prompt(-3)), false, false));
        assert_eq!(get(K::V, ctrl_shift()), Some(Bound { binding: Binding::Copy, performable: true, consumed: true }));
        assert_eq!(get(K::equal, CTRL).map(|b| b.binding), Some(Binding::FontSize(FontSize::Increase(2.5))));
        assert_eq!(get(K::F1, M::ALT_MASK).map(|b| b.binding), Some(Binding::OpenConfig { new_window: true }));
        // The other defaults stay.
        assert_eq!(get(K::Insert, CTRL).map(|b| b.binding), Some(Binding::Copy));
        let mut cleared = k.clone();
        cleared.clear();
        assert_eq!(bound(&cleared, K::Insert, CTRL), None);
        cleared.parse("ctrl+insert=select_all").unwrap();
        assert_eq!(bound(&cleared, K::Insert, CTRL).map(|b| b.binding), Some(Binding::SelectAll));
    }

    #[test]
    fn keybinds_tapust_cannot_take() {
        for bad in [
            "ctrl+a",
            "ctrl+a=",
            "ctrl+a=new_tab",
            "ctrl+a=ignore",
            "ctrl+a=copy_to_clipboard:vt",
            "ctrl+a=scroll_page_up:1",
            "ctrl+a=increase_font_size",
            "ctrl+a=increase_font_size:big",
            "ctrl+a=jump_to_prompt:40000",
            "ctrl+a=open_config:elsewhere",
            "ctrl+a>n=select_all",
            "chain=select_all",
            "foo/ctrl+a=select_all",
            "global:ctrl+a=select_all",
            "all:ctrl+a=select_all",
            "performable:performable:ctrl+a=select_all",
            "=select_all",
        ] {
            assert_eq!(Keybinds::default().parse(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn writing_system_keys_bind_by_their_keycode() {
        let k = keybinds(&["ctrl+key_q=select_all"]);
        // evdev's KEY_Q (16) is the `a` of an AZERTY layout.
        let press = |keycode| {
            k.lookup(&Press { keyval: K::a, keycode, state: CTRL, consumed: M::empty(), unshifted: Some('a') })
        };
        assert_eq!(press(24).map(|b| b.binding), Some(Binding::SelectAll));
        assert_eq!(press(38), None);
    }

    #[test]
    fn the_last_trigger_set_for_an_action_is_its_accelerator() {
        let k = Keybinds::default();
        let accel = |binding| k.trigger(binding).map(|t| t.accelerator());
        assert_eq!(accel(Binding::Copy).as_deref(), Some("<Shift><Control>c"));
        assert_eq!(accel(Binding::Paste { primary: true }).as_deref(), Some("<Shift>Insert"));
        assert_eq!(accel(Binding::FontSize(FontSize::Increase(1.0))).as_deref(), Some("<Control>plus"));
        assert_eq!(accel(Binding::Scroll(Scroll::Prompt(-1))).as_deref(), Some("<Shift><Control>Up"));
        assert_eq!(accel(Binding::ReloadConfig).as_deref(), Some("<Shift><Control>comma"));
        let k = keybinds(&["ctrl+key_a=reload_config", "ctrl+shift+,=unbind"]);
        assert_eq!(k.trigger(Binding::ReloadConfig).map(|t| t.accelerator()).as_deref(), Some("<Control>a"));
        assert_eq!(k.trigger(Binding::FontSize(FontSize::Increase(2.0))), None);
    }

    #[test]
    fn font_sizes_step_within_bounds() {
        assert_eq!(font_points(12.0, FontSize::Increase(1.0), 12.0), 13.0);
        assert_eq!(font_points(12.0, FontSize::Decrease(1.5), 12.0), 10.5);
        assert_eq!(font_points(12.0, FontSize::Increase(-3.0), 12.0), 12.0);
        assert_eq!(font_points(MIN_POINTS, FontSize::Decrease(1.0), 12.0), MIN_POINTS);
        assert_eq!(font_points(MAX_POINTS, FontSize::Increase(1.0), 12.0), MAX_POINTS);
        assert_eq!(font_points(30.0, FontSize::Reset, 16.0), 16.0);
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
        assert_eq!(scrolled(&mut t, Scroll::Prompt(-1)), 12);
        assert_eq!(scrolled(&mut t, Scroll::Prompt(-1)), 6);
        assert_eq!(scrolled(&mut t, Scroll::Prompt(-1)), 0);
        assert_eq!(scroll_delta(&t, Scroll::Prompt(-1)), 0);
        assert_eq!(scrolled(&mut t, Scroll::Prompt(1)), 6);
        assert_eq!(scrolled(&mut t, Scroll::Prompt(1)), 12);
        // The last prompt is on the screen: the view goes to the bottom.
        assert_eq!(scrolled(&mut t, Scroll::Prompt(1)), 15);
        assert_eq!(scrolled(&mut t, Scroll::Prompt(1)), 15);
        // Between prompts, up goes to the one above the top line.
        t.scroll_view(7);
        assert_eq!(t.view_top(), 8);
        assert_eq!(scrolled(&mut t, Scroll::Prompt(-1)), 6);
        // More prompts at once, as far as there are.
        assert_eq!(scroll_delta(&t, Scroll::Prompt(0)), 0);
        assert_eq!(scrolled(&mut t, Scroll::Prompt(2)), 15);
        assert_eq!(scrolled(&mut t, Scroll::Prompt(-2)), 6);
        assert_eq!(scrolled(&mut t, Scroll::Prompt(-5)), 0);
        assert_eq!(scrolled(&mut t, Scroll::Prompt(2)), 12);
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
        assert_eq!(scrolled(&mut t, Scroll::Prompt(-1)), 4);
        assert_eq!(scrolled(&mut t, Scroll::Prompt(-1)), 0);
        assert_eq!(scrolled(&mut t, Scroll::Prompt(1)), 4);
        assert_eq!(scrolled(&mut t, Scroll::Prompt(1)), 6);
    }

    #[test]
    fn no_prompt_no_jump() {
        let mut t = Terminal::new(10, 4, 100);
        for i in 0..30 {
            t.feed(format!("{i}\r\n").as_bytes());
        }
        assert_eq!(scroll_delta(&t, Scroll::Prompt(-1)), 0);
        t.scroll_view(10);
        assert_eq!(scroll_delta(&t, Scroll::Prompt(-1)), 0);
        assert_eq!(scroll_delta(&t, Scroll::Prompt(1)), 0);
    }
}
