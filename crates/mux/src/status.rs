//! The status line, ported from tmux 3.6 status.c (`status_redraw`,
//! `status_message_redraw`, `status_prompt_redraw`) and format-draw.c
//! (`format_draw_left`, `format_draw_put_list`) for the default
//! `status-format` and options.

use crate::format::{self, char_width, str_width, trim_left};
use crate::prompt::prompt_char_width;
use crate::{ChromeColor, Session, StatusCell};

/// The `status-left-length` option.
const STATUS_LEFT_LENGTH: usize = 10;
/// The `status-right-length` option.
const STATUS_RIGHT_LENGTH: usize = 40;

/// `status-style` bg=green,fg=black.
const STATUS_STYLE: (ChromeColor, ChromeColor) = (ChromeColor::Indexed(0), ChromeColor::Indexed(2));
/// `message-style` bg=yellow,fg=black.
const MESSAGE_STYLE: (ChromeColor, ChromeColor) = (ChromeColor::Indexed(0), ChromeColor::Indexed(3));

/// One row of cells; a wide character is followed by a '\0' padding cell.
struct Line {
    cells: Vec<StatusCell>,
}

impl Line {
    fn new(width: usize, (fg, bg): (ChromeColor, ChromeColor)) -> Line {
        let blank =
            StatusCell { ch: ' ', fg, bg, bold: false, underline: false, reverse: false };
        Line { cells: vec![blank; width] }
    }

    /// Writes `text` from column `x`, stopping before `limit` (screen_write_cell
    /// does not draw a wide character that does not fit).
    fn put(&mut self, x: usize, text: &[(char, usize)], limit: usize) {
        let limit = limit.min(self.cells.len());
        let mut x = x;
        for &(c, w) in text {
            if w == 0 {
                continue;
            }
            if x + w > limit {
                break;
            }
            self.cells[x].ch = c;
            for pad in 1..w {
                self.cells[x + pad].ch = '\0';
            }
            x += w;
        }
    }
}

/// A one-line screen: characters with their widths.
fn screen(s: &str) -> Vec<(char, usize)> {
    s.chars().map(|c| (c, char_width(c))).collect()
}

fn width(s: &[(char, usize)]) -> usize {
    s.iter().map(|&(_, w)| w).sum()
}

/// The columns `start..start + len` of a screen, as screen_write_fast_copy
/// copies them (a wide character cut in half is dropped).
fn columns(s: &[(char, usize)], start: usize, len: usize) -> Vec<(char, usize)> {
    let mut out = Vec::new();
    let mut x = 0;
    for &(c, w) in s {
        if x >= start && x + w <= start + len {
            out.push((c, w));
        } else if x < start + len && x + w > start {
            out.extend(std::iter::repeat_n((' ', 1), (x + w).min(start + len) - x.max(start)));
        }
        x += w;
    }
    out
}

impl Session {
    pub(crate) fn status_line(&self, clock: &str) -> (Vec<StatusCell>, Option<u16>) {
        let cols = usize::from(self.cols);
        if self.dead {
            return (Line::new(cols, STATUS_STYLE).cells, None);
        }
        if let Some((text, _)) = &self.message {
            let mut line = Line::new(cols, MESSAGE_STYLE);
            line.put(0, &screen(text), cols);
            return (line.cells, None);
        }
        if let Some(p) = &self.prompt {
            return self.prompt_line(p, cols);
        }
        (self.normal_status(clock, cols), None)
    }

    /// status_prompt_redraw
    fn prompt_line(&self, p: &crate::prompt::Prompt, cols: usize) -> (Vec<StatusCell>, Option<u16>) {
        let mut line = Line::new(cols, MESSAGE_STYLE);
        let prompt = format::drawn_text(&self.prompt_text(p));
        let start = str_width(&prompt).min(cols);
        line.put(0, &screen(&prompt), start);

        let left = cols - start;
        if left == 0 {
            return (line.cells, Some(start as u16));
        }
        let widths: Vec<usize> = p.buffer.iter().map(|&c| prompt_char_width(c)).collect();
        let pcursor: usize = widths[..p.index].iter().sum();
        let mut pwidth: usize = widths.iter().sum();
        let offset = if pcursor >= left {
            pwidth = left;
            pcursor - left + 1
        } else {
            0
        };
        pwidth = pwidth.min(left);
        let cursor = start + pcursor - offset;

        // status_prompt_redraw_character
        let mut x = start;
        let mut drawn = 0;
        for (&c, &w) in p.buffer.iter().zip(&widths) {
            if drawn < offset {
                drawn += w;
                continue;
            }
            if drawn >= offset + pwidth {
                break;
            }
            drawn += w;
            if drawn > offset + pwidth {
                break;
            }
            let text: Vec<(char, usize)> = if (c as u32) <= 0x1f || c == '\x7f' {
                let shown = if c == '\x7f' { '?' } else { char::from(c as u8 | 0x40) };
                vec![('^', 1), (shown, 1)]
            } else {
                vec![(c, w)]
            };
            line.put(x, &text, cols);
            x += w;
        }
        (line.cells, Some(cursor as u16))
    }

    /// status_redraw with the default status-format (format_draw_left).
    fn normal_status(&self, clock: &str, cols: usize) -> Vec<StatusCell> {
        let mut line = Line::new(cols, STATUS_STYLE);
        let w = self.cur();

        // #{T;=/#{status-left-length}:status-left}
        let left_text = format::drawn_text(&trim_left(&format!("[{}] ", self.name), STATUS_LEFT_LENGTH));
        let left = screen(&left_text);

        // The window list, with the current window as the focus range.
        let mut list = Vec::new();
        let (mut focus_start, mut focus_end) = (0, 0);
        let count = self.winlinks.len();
        for (n, (&idx, win)) in self.winlinks.iter().enumerate() {
            let flags = self.window_flags(idx);
            let flags = if flags.is_empty() { " ".to_owned() } else { flags };
            let text = format::drawn_text(&format!("{idx}:{}{flags}", win.name()));
            if idx == self.curw {
                focus_start = width(&list);
            }
            list.extend(screen(&text));
            if idx == self.curw {
                focus_end = width(&list);
            }
            if n + 1 != count {
                list.push((' ', 1));
            }
        }

        // #{T;=/#{status-right-length}:status-right}
        let (wx, wy) = self.area();
        let bigger = if w.sx > wx || w.sy > wy { "[0,0] " } else { "" };
        let title = trim_left(&w.pane(w.active).title, 21);
        let right_text = format!("{bigger}\"{title}\" {clock}");
        let right = screen(&format::drawn_text(&trim_left(&right_text, STATUS_RIGHT_LENGTH)));

        // format_draw_left: trim the list first, then the right, then the left.
        let (mut wl, mut wr, mut wlist) = (width(&left), width(&right), width(&list));
        while wl + wr + wlist > cols {
            if wlist > 0 {
                wlist -= 1;
            } else if wr > 0 {
                wr -= 1;
            } else {
                wl -= 1;
            }
        }
        line.put(0, &columns(&left, 0, wl), wl);
        let rcx = width(&right);
        line.put(cols - wr, &columns(&right, rcx - wr, wr), cols);
        if wlist > 0 {
            self.put_list(&mut line, wl, wlist, &list, focus_start, focus_end);
        }
        line.cells
    }

    /// format_draw_put_list with the "<" and ">" markers.
    fn put_list(
        &self,
        line: &mut Line,
        offset: usize,
        width_avail: usize,
        list: &[(char, usize)],
        focus_start: usize,
        focus_end: usize,
    ) {
        let list_cx = width(list);
        let (mut offset, mut avail) = (offset, width_avail);
        if avail >= list_cx {
            line.put(offset, list, offset + avail);
            return;
        }
        let centre = focus_start + (focus_end - focus_start) / 2;
        let mut start = centre.saturating_sub(avail / 2);
        if start + avail > list_cx {
            start = list_cx - avail;
        }
        if start != 0 && avail > 1 {
            line.put(offset, &[('<', 1)], offset + 1);
            offset += 1;
            start += 1;
            avail -= 1;
        }
        if start + avail < list_cx && avail > 1 {
            line.put(offset + avail - 1, &[('>', 1)], offset + avail);
            avail -= 1;
        }
        line.put(offset, &columns(list, start, avail), offset + avail);
    }
}
