//! The parts of tmux 3.6 formats the model needs: a subset of
//! `format_expand1` (format.c), `vis` encoding of names (utf8_stravis) and
//! display widths.

use crate::{PaneId, Session};

/// The target a format is expanded for (tmux's format tree defaults).
#[derive(Clone, Copy, Debug)]
pub(crate) struct Context {
    /// Winlink index.
    pub window: u32,
    pub pane: PaneId,
}

impl Session {
    /// format_expand for the variables the model knows. Unknown variables,
    /// modifiers (`#{=21:...}`, `#{?...}`) and `#()` jobs expand to "".
    pub(crate) fn expand(&self, fmt: &str, ctx: Context) -> String {
        let chars: Vec<char> = fmt.chars().collect();
        let mut out = String::new();
        let mut i = 0;
        while i < chars.len() {
            if chars[i] != '#' {
                out.push(chars[i]);
                i += 1;
                continue;
            }
            let Some(&ch) = chars.get(i + 1) else {
                out.push('#');
                break;
            };
            i += 2;
            match ch {
                '(' => {
                    let mut depth = 1;
                    let mut j = i;
                    while j < chars.len() {
                        match chars[j] {
                            '(' => depth += 1,
                            ')' => {
                                depth -= 1;
                                if depth == 0 {
                                    break;
                                }
                            }
                            _ => {}
                        }
                        j += 1;
                    }
                    if j >= chars.len() {
                        break;
                    }
                    i = j + 1;
                }
                '{' => {
                    let Some(end) = skip_braces(&chars, i) else { break };
                    let name: String = chars[i..end].iter().collect();
                    out.push_str(&self.format_var(&name, ctx));
                    i = end + 1;
                }
                '[' | '#' => {
                    // "#[" and "##...#[" stay for format_draw; "##" is "#".
                    let mut ptr = if ch == '[' { i - 1 } else { i };
                    let mut n = if ch == '[' { 1 } else { 2 };
                    while chars.get(ptr) == Some(&'#') {
                        ptr += 1;
                        n += 1;
                    }
                    if chars.get(ptr) == Some(&'[') {
                        out.extend(&chars[i - 2..i - 2 + n + 1]);
                        i = ptr + 1;
                    } else {
                        out.push(ch);
                    }
                }
                '}' | ',' => out.push(ch),
                _ => match alias(ch) {
                    Some(name) => out.push_str(&self.format_var(name, ctx)),
                    None => {
                        out.push('#');
                        out.push(ch);
                    }
                },
            }
        }
        out
    }

    fn format_var(&self, name: &str, ctx: Context) -> String {
        let Some(w) = self.winlinks.get(&ctx.window) else { return String::new() };
        let pane = w.panes.iter().position(|p| p.id == ctx.pane);
        let flag = |b: bool| if b { "1" } else { "0" }.to_owned();
        match name {
            "session_name" => self.name.clone(),
            "session_windows" => self.winlinks.len().to_string(),
            "window_index" => ctx.window.to_string(),
            "window_name" => w.name().to_owned(),
            "window_id" => format!("@{}", w.id),
            "window_flags" => self.window_flags(ctx.window),
            "window_active" => flag(ctx.window == self.curw),
            "window_panes" => w.panes.len().to_string(),
            "window_zoomed_flag" => flag(w.zoomed),
            "pane_id" => format!("%{}", ctx.pane),
            "pane_index" => pane.map(|i| i.to_string()).unwrap_or_default(),
            "pane_title" => pane.map(|i| w.panes[i].title.clone()).unwrap_or_default(),
            "pane_active" => flag(w.active == ctx.pane),
            // Pixel splits have no cell positions; the size is the pane's grid.
            "pane_width" => pane.map(|i| w.panes[i].cols.to_string()).unwrap_or_default(),
            "pane_height" => pane.map(|i| w.panes[i].rows.to_string()).unwrap_or_default(),
            _ => String::new(),
        }
    }

    /// window_printable_flags (no activity, bell, silence or marked pane).
    pub(crate) fn window_flags(&self, idx: u32) -> String {
        let mut flags = String::new();
        if idx == self.curw {
            flags.push('*');
        }
        if self.lastw.first() == Some(&idx) {
            flags.push('-');
        }
        if self.winlinks.get(&idx).is_some_and(|w| w.zoomed) {
            flags.push('Z');
        }
        flags
    }
}

/// The single-character aliases of format.c (`format_upper`/`format_lower`).
fn alias(ch: char) -> Option<&'static str> {
    Some(match ch {
        'D' => "pane_id",
        'F' => "window_flags",
        'H' => "host",
        'I' => "window_index",
        'P' => "pane_index",
        'S' => "session_name",
        'T' => "pane_title",
        'W' => "window_name",
        'h' => "host_short",
        _ => return None,
    })
}

/// format_skip from just inside "#{": the index of the matching "}".
fn skip_braces(chars: &[char], start: usize) -> Option<usize> {
    let mut depth = 0;
    let mut i = start;
    while i < chars.len() {
        if chars[i] == '#' && chars.get(i + 1) == Some(&'{') {
            depth += 1;
        }
        if chars[i] == '#' && chars.get(i + 1).is_some_and(|c| ",#{}:".contains(*c)) {
            i += 2;
            continue;
        }
        if chars[i] == '}' {
            if depth == 0 {
                return Some(i);
            }
            depth -= 1;
        }
        i += 1;
    }
    None
}

/// utf8_stravis with VIS_OCTAL|VIS_CSTYLE|VIS_TAB|VIS_NL, as window and
/// session names are stored.
pub(crate) fn vis(s: &str) -> String {
    let mut out = String::new();
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if !c.is_ascii() {
            out.push(c);
            continue;
        }
        match c {
            '\\' => out.push_str("\\\\"),
            ' '..='~' => out.push(c),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\x08' => out.push_str("\\b"),
            '\x07' => out.push_str("\\a"),
            '\x0b' => out.push_str("\\v"),
            '\t' => out.push_str("\\t"),
            '\x0c' => out.push_str("\\f"),
            '\0' => {
                out.push_str("\\0");
                if chars.peek().is_some_and(|n| ('0'..='7').contains(n)) {
                    out.push_str("00");
                }
            }
            _ => out.push_str(&format!("\\{:03o}", c as u32)),
        }
    }
    out
}

/// The text format_draw shows for an expanded string: runs of '#' collapse
/// as in format_draw and `#[...]` style sequences (not modelled) vanish.
pub(crate) fn drawn_text(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '#' && i + 1 < chars.len() && chars[i + 1] != '[' {
            let mut n = 1;
            while chars.get(i + n) == Some(&'#') {
                n += 1;
            }
            let even = n % 2 == 0;
            if chars.get(i + n) != Some(&'[') {
                i += n;
                let count = if even { n / 2 } else { n / 2 + 1 };
                out.extend(std::iter::repeat_n('#', count));
                continue;
            }
            i += if even { n + 1 } else { n - 1 };
            out.extend(std::iter::repeat_n('#', n / 2));
            if even {
                out.push('[');
            }
            continue;
        }
        if chars[i] == '#' && chars.get(i + 1) == Some(&'[') {
            match chars[i + 2..].iter().position(|&c| c == ']') {
                Some(end) => i += end + 3,
                None => break,
            }
            continue;
        }
        if !chars[i].is_control() {
            out.push(chars[i]);
        }
        i += 1;
    }
    out
}
