//! ghostty's resize tests (Terminal.zig, Screen.zig and PageList.zig at
//! 1b22f33) run through the public API. A Screen or PageList resize with
//! `reflow = false` is a primary screen with DECAWM off, a screen without
//! scrollback is a terminal without one, and a tracked pin is a selection
//! whose ends are on the pin's cell.

use crate::{Attrs, Cell, Point, PromptMark, Selection, Terminal, Width};

/// ghostty keeps at least a page of scrollback for any non-zero limit.
fn with_history(cols: u16, rows: u16) -> Terminal {
    Terminal::new(cols, rows, 1000)
}

fn no_history(cols: u16, rows: u16) -> Terminal {
    Terminal::new(cols, rows, 0)
}

fn feed(t: &mut Terminal, s: &str) {
    t.feed(s.as_bytes());
}

/// ghostty's testWriteString and printString: a newline starts a new row.
fn write(t: &mut Terminal, s: &str) {
    t.feed(s.replace('\n', "\r\n").as_bytes());
}

/// ghostty's cursorAbsolute with the pending wrap reset.
fn goto(t: &mut Terminal, x: usize, y: usize) {
    feed(t, &format!("\x1b[{};{}H", y + 1, x + 1));
}

fn no_reflow(t: &mut Terminal) {
    feed(t, "\x1b[?7l");
}

/// ghostty's dumpString without unwrapping: rows end with a newline,
/// trailing cells and rows without text are left out.
fn dump(t: &Terminal, lines: std::ops::Range<u64>) -> String {
    let mut out = String::new();
    let mut newlines = 0;
    for l in lines {
        let mut row = String::new();
        let mut blanks = 0;
        for (x, c) in t.line(l).unwrap().iter().enumerate() {
            match c.char() {
                _ if c.width() == Width::SpacerTail => {}
                None => blanks += 1,
                Some(ch) => {
                    row.push_str(&" ".repeat(blanks));
                    blanks = 0;
                    match t.line_grapheme(l, x as u16) {
                        Some(g) => row.push_str(g),
                        None => row.push(ch),
                    }
                }
            }
        }
        if row.is_empty() {
            newlines += 1;
            continue;
        }
        out.push_str(&"\n".repeat(newlines));
        out.push_str(&row);
        newlines = 1;
    }
    out
}

fn viewport(t: &Terminal) -> String {
    dump(t, t.view_top()..t.view_top() + t.rows() as u64)
}

fn active_top(t: &Terminal) -> u64 {
    t.end_line() - t.rows() as u64
}

fn active(t: &Terminal) -> String {
    dump(t, active_top(t)..t.end_line())
}

fn screen(t: &Terminal) -> String {
    dump(t, t.first_line()..t.end_line())
}

fn cursor(t: &Terminal) -> (usize, usize) {
    (t.term.x, t.term.y)
}

/// The cell at (x, y) of the active area.
fn cell(t: &Terminal, x: usize, y: usize) -> Cell {
    t.line(active_top(t) + y as u64).unwrap()[x]
}

/// The cell at (x, y) counting rows from the oldest stored one.
fn screen_cell(t: &Terminal, x: usize, y: usize) -> Cell {
    t.line(t.first_line() + y as u64).unwrap()[x]
}

fn cursor_cell(t: &Terminal) -> Cell {
    let (x, y) = cursor(t);
    cell(t, x, y)
}

/// ghostty's Cell.isEmpty.
fn is_empty(c: Cell) -> bool {
    c.char().is_none() && c.width() == Width::Narrow
}

fn wrapped(t: &Terminal, y: usize) -> bool {
    t.line_wrapped(active_top(t) + y as u64)
}

fn screen_wrapped(t: &Terminal, y: usize) -> bool {
    t.line_wrapped(t.first_line() + y as u64)
}

/// Tracks the active cell (x, y) as a pin.
fn track(t: &mut Terminal, x: usize, y: usize) {
    let p = Point { line: active_top(t) + y as u64, col: x as u16 };
    t.set_selection(Some(Selection { start: p, end: p, rect: false }));
}

/// The pin's active (x, y), or None if it is in the history.
fn pin_active(t: &Terminal) -> Option<(usize, usize)> {
    let p = t.selection().unwrap().end;
    let y = p.line.checked_sub(active_top(t))?;
    Some((p.col as usize, y as usize))
}

/// The pin's (x, y) counting rows from the oldest stored one.
fn pin_screen(t: &Terminal) -> (usize, usize) {
    let p = t.selection().unwrap().end;
    (p.col as usize, (p.line - t.first_line()) as usize)
}

fn pin_cell(t: &Terminal) -> Cell {
    let p = t.selection().unwrap().end;
    t.line(p.line).unwrap()[p.col as usize]
}

/// Writes a cell with only a background color at the start of row `y`.
fn bg_cell(t: &mut Terminal, y: usize) {
    feed(t, &format!("\x1b[{};1H\x1b[41m\x1b[X\x1b[m", y + 1));
}

fn assert_wide_pairs(t: &Terminal) {
    for l in t.first_line()..t.end_line() {
        let row = t.line(l).unwrap();
        for (x, c) in row.iter().enumerate() {
            match c.width() {
                Width::Wide => assert_eq!(row[x + 1].width(), Width::SpacerTail, "line {l} col {x}"),
                Width::SpacerTail => assert_eq!(row[x - 1].width(), Width::Wide, "line {l} col {x}"),
                _ => {}
            }
        }
    }
}

fn assert_cell(c: Cell, ch: Option<char>, width: Width) {
    assert_eq!((c.char(), c.width()), (ch, width));
}

// ---- Terminal.zig ---------------------------------------------------------

// ghostty: "Terminal: saveCursor resize"
#[test]
fn terminal_save_cursor_resize() {
    let mut t = with_history(10, 5);
    feed(&mut t, "\x1b[1;10H\x1b7");
    t.resize(5, 5);
    feed(&mut t, "\x1b8X");
    assert_eq!(viewport(&t), "    X");
}

// ghostty: "Terminal: resize less cols with wide char then print"
#[test]
fn terminal_resize_less_cols_with_wide_char_then_print() {
    let mut t = with_history(3, 3);
    write(&mut t, "x\u{1f600}");
    t.resize(2, 3);
    feed(&mut t, "\x1b[1;2H\u{1f600}");
    assert_wide_pairs(&t);
}

// ghostty: "Terminal: resize less cols without reflow cutting wide char tail"
#[test]
fn terminal_resize_less_cols_without_reflow_cutting_wide_char_tail() {
    let mut t = with_history(3, 1);
    write(&mut t, "a\u{4e00}");
    no_reflow(&mut t);
    t.resize(2, 1);
    assert!(is_empty(cell(&t, 1, 0)));
}

// ghostty: "Terminal: resize with wraparound off"
#[test]
fn terminal_resize_with_wraparound_off() {
    let mut t = with_history(4, 2);
    no_reflow(&mut t);
    write(&mut t, "0123");
    t.resize(2, 2);
    assert_eq!(viewport(&t), "01");
}

// ghostty: "Terminal: resize with wraparound on"
#[test]
fn terminal_resize_with_wraparound_on() {
    let mut t = with_history(4, 2);
    write(&mut t, "0123");
    t.resize(2, 2);
    assert_eq!(viewport(&t), "01\n23");
}

// ghostty: "Terminal: resize with reflow and saved cursor"
#[test]
fn terminal_resize_with_reflow_and_saved_cursor() {
    let mut t = with_history(2, 3);
    write(&mut t, "1A2B");
    goto(&mut t, 1, 1);
    assert_eq!(cursor_cell(&t).char(), Some('B'));
    assert_eq!(viewport(&t), "1A\n2B");
    feed(&mut t, "\x1b7");
    t.resize(5, 3);
    feed(&mut t, "\x1b8");
    assert_eq!(viewport(&t), "1A2B");
    assert_eq!(cursor_cell(&t).char(), Some('B'));
}

// ghostty: "Terminal: resize with reflow and saved cursor pending wrap"
#[test]
fn terminal_resize_with_reflow_and_saved_cursor_pending_wrap() {
    let mut t = with_history(2, 3);
    write(&mut t, "1A2B");
    assert_eq!(cursor_cell(&t).char(), Some('B'));
    assert_eq!(viewport(&t), "1A\n2B");
    feed(&mut t, "\x1b7");
    t.resize(5, 3);
    feed(&mut t, "\x1b8");
    assert_eq!(viewport(&t), "1A2B");
    write(&mut t, "X");
    assert_eq!(viewport(&t), "1A2BX");
}

// ghostty: "Terminal: saved cursor survives repeated widening"
#[test]
fn terminal_saved_cursor_survives_repeated_widening() {
    let mut t = with_history(4, 5);
    write(&mut t, "abc\nAAA|");
    feed(&mut t, "\x1b7");
    t.resize(5, 5);
    t.resize(6, 5);
    feed(&mut t, "\x1b8");
    write(&mut t, "X");
    assert_eq!(viewport(&t), "abc\nAAA|X");
}

// ghostty: "Terminal: resize pending wrap live and saved cursors"
#[test]
fn terminal_resize_pending_wrap_live_and_saved_cursors() {
    let cases = [
        ("ABCD", 6, false, "ABCDX"),
        ("ABCD", 3, false, "ABC\nDX"),
        ("ABCD", 2, true, "AB\nCD\nX"),
        ("ABCD", 4, true, "ABCD\nX"),
        ("ABCDEFGH", 6, false, "ABCDEF\nGHX"),
        ("AB\u{754c}", 6, false, "AB\u{754c}X"),
        ("ABC", 6, false, "ABCX"),
    ];
    for (text, cols, pending_wrap, expected) in cases {
        for restore in [false, true] {
            let mut t = with_history(4, 5);
            write(&mut t, text);
            if restore {
                feed(&mut t, "\x1b7");
            }
            t.resize(cols, 6);
            if restore {
                feed(&mut t, "\x1b8");
            }
            assert_eq!(t.term.pending_wrap, pending_wrap, "{text} at {cols} cols, restore {restore}");
            write(&mut t, "X");
            assert_eq!(viewport(&t), expected, "{text} at {cols} cols, restore {restore}");
        }
    }
}

// ghostty: "Terminal: eraseLine complete resets wrap"
#[test]
fn terminal_erase_line_complete_resets_wrap() {
    let mut t = with_history(5, 5);
    write(&mut t, "ABCDE123");
    assert!(wrapped(&t, 0));
    feed(&mut t, "\x1b[1;1H\x1b[2K");
    assert!(!wrapped(&t, 0));
    write(&mut t, "X");
    t.resize(10, 5);
    assert_eq!(viewport(&t), "X\n123");
}

// ghostty: "Terminal: resize resets synchronized output"
#[test]
fn terminal_resize_resets_synchronized_output() {
    let mut t = with_history(10, 5);
    feed(&mut t, "\x1b[?2026h");
    assert!(t.modes().sync_update);
    t.resize(10, 5);
    assert!(!t.modes().sync_update);
}

// ---- Screen.zig -----------------------------------------------------------

// ghostty: "Screen: resize (no reflow) more rows"
#[test]
fn screen_no_reflow_more_rows() {
    let mut t = no_history(10, 3);
    let str = "1ABCD\n2EFGH\n3IJKL";
    write(&mut t, str);
    no_reflow(&mut t);
    t.resize(10, 10);
    assert_eq!(viewport(&t), str);
}

// ghostty: "Screen: resize (no reflow) less rows"
#[test]
fn screen_no_reflow_less_rows() {
    let mut t = no_history(10, 3);
    write(&mut t, "1ABCD\n2EFGH\n3IJKL");
    assert_eq!(cursor(&t), (5, 2));
    no_reflow(&mut t);
    t.resize(10, 2);
    assert_eq!(cursor(&t), (5, 1));
    assert_eq!(viewport(&t), "2EFGH\n3IJKL");
}

// ghostty: "Screen: resize (no reflow) less rows trims blank lines"
#[test]
fn screen_no_reflow_less_rows_trims_blank_lines() {
    let mut t = no_history(10, 3);
    write(&mut t, "1ABCD");
    bg_cell(&mut t, 1);
    bg_cell(&mut t, 2);
    goto(&mut t, 5, 0);
    no_reflow(&mut t);
    t.resize(6, 2);
    assert_eq!(cursor(&t), (5, 0));
    assert_eq!(viewport(&t), "1ABCD");
}

// ghostty: "Screen: resize (no reflow) more rows trims blank lines"
#[test]
fn screen_no_reflow_more_rows_trims_blank_lines() {
    let mut t = no_history(10, 3);
    write(&mut t, "1ABCD");
    bg_cell(&mut t, 1);
    bg_cell(&mut t, 2);
    goto(&mut t, 5, 0);
    no_reflow(&mut t);
    t.resize(10, 7);
    assert_eq!(cursor(&t), (5, 0));
    assert_eq!(viewport(&t), "1ABCD");
}

// ghostty: "Screen: resize (no reflow) more cols"
#[test]
fn screen_no_reflow_more_cols() {
    let mut t = no_history(10, 3);
    let str = "1ABCD\n2EFGH\n3IJKL";
    write(&mut t, str);
    no_reflow(&mut t);
    t.resize(20, 3);
    assert_eq!(viewport(&t), str);
}

// ghostty: "Screen: resize (no reflow) less cols"
#[test]
fn screen_no_reflow_less_cols() {
    let mut t = no_history(10, 3);
    write(&mut t, "1ABCD\n2EFGH\n3IJKL");
    no_reflow(&mut t);
    t.resize(4, 3);
    assert_eq!(viewport(&t), "1ABC\n2EFG\n3IJK");
}

// ghostty: "Screen: resize (no reflow) more rows with scrollback cursor end"
#[test]
fn screen_no_reflow_more_rows_with_scrollback_cursor_end() {
    let mut t = with_history(7, 3);
    let str = "1ABCD\n2EFGH\n3IJKL\n4ABCD\n5EFGH";
    write(&mut t, str);
    no_reflow(&mut t);
    t.resize(7, 10);
    assert_eq!(viewport(&t), str);
}

// ghostty: "Screen: resize (no reflow) less rows with scrollback"
#[test]
fn screen_no_reflow_less_rows_with_scrollback() {
    let mut t = with_history(7, 3);
    write(&mut t, "1ABCD\n2EFGH\n3IJKL\n4ABCD\n5EFGH");
    no_reflow(&mut t);
    t.resize(7, 2);
    assert_eq!(viewport(&t), "4ABCD\n5EFGH");
}

// ghostty: "Screen: resize (no reflow) less rows with empty trailing"
#[test]
fn screen_no_reflow_less_rows_with_empty_trailing() {
    let mut t = with_history(5, 3);
    write(&mut t, "1\n2\n3\n4\n5\n6\n7\n8");
    // ghostty's scrollClear: the three rows with text scroll into the history.
    feed(&mut t, "\n\n\n");
    goto(&mut t, 0, 0);
    write(&mut t, "A\nB");
    let before = cursor(&t);
    no_reflow(&mut t);
    t.resize(5, 2);
    assert_eq!(cursor(&t), before);
    assert_eq!(viewport(&t), "A\nB");
}

// ghostty: "Screen: resize (no reflow) more rows with soft wrapping"
#[test]
fn screen_no_reflow_more_rows_with_soft_wrapping() {
    let mut t = with_history(2, 3);
    write(&mut t, "1A2B\n3C4E\n5F6G");
    for y in 0..6 {
        assert_eq!(screen_wrapped(&t, y), y % 2 == 0, "row {y}");
    }
    no_reflow(&mut t);
    t.resize(2, 10);
    assert_eq!(viewport(&t), "1A\n2B\n3C\n4E\n5F\n6G");
    for y in 0..6 {
        assert_eq!(screen_wrapped(&t, y), y % 2 == 0, "row {y}");
    }
}

// ghostty: "Screen: resize more rows no scrollback"
#[test]
fn screen_resize_more_rows_no_scrollback() {
    let mut t = no_history(5, 3);
    let str = "1ABCD\n2EFGH\n3IJKL";
    write(&mut t, str);
    let before = cursor(&t);
    t.resize(5, 10);
    assert_eq!(cursor(&t), before);
    assert_eq!(viewport(&t), str);
    assert_eq!(screen(&t), str);
}

// ghostty: "Screen: resize more rows with empty scrollback"
#[test]
fn screen_resize_more_rows_with_empty_scrollback() {
    let mut t = with_history(5, 3);
    let str = "1ABCD\n2EFGH\n3IJKL";
    write(&mut t, str);
    let before = cursor(&t);
    t.resize(5, 10);
    assert_eq!(cursor(&t), before);
    assert_eq!(viewport(&t), str);
    assert_eq!(screen(&t), str);
}

// ghostty: "Screen: resize more rows with populated scrollback"
#[test]
fn screen_resize_more_rows_with_populated_scrollback() {
    let mut t = with_history(5, 3);
    write(&mut t, "1ABCD\n2EFGH\n3IJKL\n4ABCD\n5EFGH");
    assert_eq!(viewport(&t), "3IJKL\n4ABCD\n5EFGH");
    goto(&mut t, 0, 1);
    assert_eq!(cursor_cell(&t).char(), Some('4'));
    t.resize(5, 10);
    assert_eq!(cursor_cell(&t).char(), Some('4'));
    assert_eq!(viewport(&t), "3IJKL\n4ABCD\n5EFGH");
}

// ghostty: "Screen: resize more cols no reflow"
#[test]
fn screen_resize_more_cols_no_reflow() {
    let mut t = no_history(5, 3);
    let str = "1ABCD\n2EFGH\n3IJKL";
    write(&mut t, str);
    let (x, y) = cursor(&t);
    t.resize(10, 3);
    // Pending wrap becomes the next insertion position.
    assert!(!t.term.pending_wrap);
    assert_eq!(cursor(&t), (x + 1, y));
    assert_eq!(viewport(&t), str);
    assert_eq!(screen(&t), str);
}

// ghostty: "Screen: resize more cols perfect split"
#[test]
fn screen_resize_more_cols_perfect_split() {
    let mut t = no_history(5, 3);
    write(&mut t, "1ABCD2EFGH3IJKL");
    t.resize(10, 3);
    assert_eq!(screen(&t), "1ABCD2EFGH\n3IJKL");
}

// ghostty: "Screen: resize (no reflow) more cols with scrollback scrolled up"
#[test]
fn screen_no_reflow_more_cols_with_scrollback_scrolled_up() {
    let mut t = with_history(5, 3);
    let str = "1\n2\n3\n4\n5\n6\n7\n8";
    write(&mut t, str);
    assert_eq!(cursor(&t), (1, 2));
    t.scroll_view(4);
    assert_eq!(viewport(&t), "2\n3\n4");
    t.resize(8, 3);
    assert_eq!(screen(&t), str);
    assert_eq!(cursor(&t), (1, 2));
}

// ghostty: "Screen: resize (no reflow) less cols with scrollback scrolled up"
#[test]
fn screen_no_reflow_less_cols_with_scrollback_scrolled_up() {
    let mut t = with_history(5, 3);
    let str = "1\n2\n3\n4\n5\n6\n7\n8";
    write(&mut t, str);
    assert_eq!(cursor(&t), (1, 2));
    t.scroll_view(4);
    assert_eq!(viewport(&t), "2\n3\n4");
    t.resize(4, 3);
    assert_eq!(screen(&t), str);
    assert_eq!(active(&t), "6\n7\n8");
    assert_eq!(cursor(&t), (1, 2));
}

// ghostty: "Screen: resize more cols no reflow preserves semantic prompt"
#[test]
fn screen_resize_more_cols_no_reflow_preserves_semantic_prompt() {
    let mut t = no_history(5, 3);
    write(&mut t, "1ABCD\n\x1b]133;P\x072EFGH\x1b]133;C\x07\n3IJKL");
    no_reflow(&mut t);
    t.resize(10, 3);
    let expected = "1ABCD\n2EFGH\n3IJKL";
    assert_eq!(viewport(&t), expected);
    assert_eq!(screen(&t), expected);
    let prompt = |y: u64| t.line_prompt(active_top(&t) + y);
    assert_eq!([prompt(0), prompt(1), prompt(2)], [PromptMark::None, PromptMark::Prompt, PromptMark::None]);
}

// ghostty: "Screen: resize more cols with reflow that fits full width"
#[test]
fn screen_resize_more_cols_with_reflow_that_fits_full_width() {
    let mut t = no_history(5, 3);
    let str = "1ABCD2EFGH\n3IJKL";
    write(&mut t, str);
    assert_eq!(viewport(&t), "1ABCD\n2EFGH\n3IJKL");
    goto(&mut t, 0, 1);
    assert_eq!(cursor_cell(&t).char(), Some('2'));
    t.resize(10, 3);
    assert_eq!(viewport(&t), str);
    assert_eq!(cursor(&t), (5, 0));
}

// ghostty: "Screen: resize more cols with reflow that ends in newline"
#[test]
fn screen_resize_more_cols_with_reflow_that_ends_in_newline() {
    let mut t = no_history(6, 3);
    let str = "1ABCD2EFGH\n3IJKL";
    write(&mut t, str);
    assert_eq!(viewport(&t), "1ABCD2\nEFGH\n3IJKL");
    goto(&mut t, 0, 2);
    assert_eq!(cursor_cell(&t).char(), Some('3'));
    t.resize(10, 3);
    assert_eq!(viewport(&t), str);
    assert_eq!(cursor_cell(&t).char(), Some('3'));
}

// ghostty: "Screen: resize more cols with reflow that forces more wrapping"
#[test]
fn screen_resize_more_cols_with_reflow_that_forces_more_wrapping() {
    let mut t = no_history(5, 3);
    write(&mut t, "1ABCD2EFGH\n3IJKL");
    goto(&mut t, 0, 1);
    assert_eq!(cursor_cell(&t).char(), Some('2'));
    assert_eq!(viewport(&t), "1ABCD\n2EFGH\n3IJKL");
    t.resize(7, 3);
    assert_eq!(viewport(&t), "1ABCD2E\nFGH\n3IJKL");
    assert_eq!(cursor(&t), (5, 0));
}

// ghostty: "Screen: resize more cols with reflow that unwraps multiple times"
#[test]
fn screen_resize_more_cols_with_reflow_that_unwraps_multiple_times() {
    let mut t = no_history(5, 3);
    write(&mut t, "1ABCD2EFGH3IJKL");
    goto(&mut t, 0, 2);
    assert_eq!(cursor_cell(&t).char(), Some('3'));
    assert_eq!(viewport(&t), "1ABCD\n2EFGH\n3IJKL");
    t.resize(15, 3);
    assert_eq!(viewport(&t), "1ABCD2EFGH3IJKL");
    assert_eq!(cursor(&t), (10, 0));
}

// ghostty: "Screen: resize more cols with populated scrollback"
#[test]
fn screen_resize_more_cols_with_populated_scrollback() {
    let mut t = with_history(5, 3);
    write(&mut t, "1ABCD\n2EFGH\n3IJKL\n4ABCD5EFGH");
    assert_eq!(viewport(&t), "3IJKL\n4ABCD\n5EFGH");
    goto(&mut t, 0, 2);
    assert_eq!(cursor_cell(&t).char(), Some('5'));
    t.resize(10, 3);
    assert_eq!(viewport(&t), "2EFGH\n3IJKL\n4ABCD5EFGH");
    assert_eq!(cursor_cell(&t).char(), Some('5'));
}

// ghostty: "Screen: resize more cols bounded scrollback keeps viewport valid"
// and "PageList resize grow cols with unwrap fixes viewport pin"
#[test]
fn screen_resize_more_cols_bounded_scrollback_keeps_viewport_valid() {
    let mut t = with_history(2, 10);
    write(&mut t, &["AAAA"; 20].join("\n"));
    assert_eq!(t.history_len(), 30);
    // The view starts at row 28, which the unwrapping moves into the screen.
    t.scroll_view(2);
    t.resize(4, 10);
    assert!(t.history_len() + 10 < 40);
    assert_eq!(t.view_offset(), 0);
}

// ghostty: "Screen: resize more cols with reflow"
#[test]
fn screen_resize_more_cols_with_reflow() {
    let mut t = with_history(2, 3);
    write(&mut t, "1ABC\n2DEF\n3ABC\n4DEF");
    goto(&mut t, 0, 2);
    assert_eq!(cursor_cell(&t).char(), Some('E'));
    assert_eq!(viewport(&t), "BC\n4D\nEF");
    t.resize(7, 3);
    assert_eq!(screen(&t), "1ABC\n2DEF\n3ABC\n4DEF");
    assert_eq!(cursor(&t), (2, 2));
}

// ghostty: "Screen: resize more rows and cols with wrapping"
#[test]
fn screen_resize_more_rows_and_cols_with_wrapping() {
    let mut t = no_history(2, 4);
    let str = "1A2B\n3C4D";
    write(&mut t, str);
    assert_eq!(viewport(&t), "1A\n2B\n3C\n4D");
    t.resize(5, 10);
    // Reflow leaves room after the last printed cell.
    assert!(!t.term.pending_wrap);
    assert_eq!(cursor(&t), (4, 1));
    assert_eq!(viewport(&t), str);
    assert_eq!(screen(&t), str);
}

// ghostty: "Screen: resize less rows no scrollback"
#[test]
fn screen_resize_less_rows_no_scrollback() {
    let mut t = no_history(5, 3);
    write(&mut t, "1ABCD\n2EFGH\n3IJKL");
    goto(&mut t, 0, 0);
    t.resize(5, 1);
    assert_eq!(cursor(&t), (0, 0));
    // tapust differs from ghostty: the cursor's row stays on the screen and the rows below it are cut.
    assert_eq!(viewport(&t), "1ABCD");
    assert_eq!(screen(&t), "1ABCD");
}

// ghostty: "Screen: resize less rows moving cursor"
#[test]
fn screen_resize_less_rows_moving_cursor() {
    let mut t = no_history(5, 3);
    write(&mut t, "1ABCD\n2EFGH\n3IJKL");
    goto(&mut t, 1, 2);
    assert_eq!(cursor_cell(&t).char(), Some('I'));
    t.resize(5, 1);
    assert_eq!(viewport(&t), "3IJKL");
    assert_eq!(screen(&t), "3IJKL");
    assert_eq!(cursor(&t), (1, 0));
}

// ghostty: "Screen: resize less rows with empty scrollback"
#[test]
fn screen_resize_less_rows_with_empty_scrollback() {
    let mut t = with_history(5, 3);
    let str = "1ABCD\n2EFGH\n3IJKL";
    write(&mut t, str);
    t.resize(5, 1);
    assert_eq!(screen(&t), str);
    assert_eq!(viewport(&t), "3IJKL");
}

// ghostty: "Screen: resize less rows with populated scrollback"
#[test]
fn screen_resize_less_rows_with_populated_scrollback() {
    let mut t = with_history(5, 3);
    let str = "1ABCD\n2EFGH\n3IJKL\n4ABCD\n5EFGH";
    write(&mut t, str);
    assert_eq!(viewport(&t), "3IJKL\n4ABCD\n5EFGH");
    t.resize(5, 1);
    assert_eq!(screen(&t), str);
    assert_eq!(viewport(&t), "5EFGH");
}

// ghostty: "Screen: resize less rows with full scrollback"
#[test]
fn screen_resize_less_rows_with_full_scrollback() {
    let mut t = with_history(5, 3);
    let str = "00000\n1ABCD\n2EFGH\n3IJKL\n4ABCD\n5EFGH";
    write(&mut t, str);
    assert_eq!(viewport(&t), "3IJKL\n4ABCD\n5EFGH");
    assert_eq!(cursor(&t), (4, 2));
    t.resize(5, 2);
    // Same character, bottom of the screen.
    assert_eq!(cursor(&t), (4, 1));
    assert_eq!(screen(&t), str);
    assert_eq!(viewport(&t), "4ABCD\n5EFGH");
}

// ghostty: "Screen: resize less cols no reflow"
#[test]
fn screen_resize_less_cols_no_reflow() {
    let mut t = no_history(5, 3);
    let str = "1AB\n2EF\n3IJ";
    write(&mut t, str);
    goto(&mut t, 0, 0);
    t.resize(3, 3);
    assert_eq!(cursor(&t), (0, 0));
    assert_eq!(viewport(&t), str);
    assert_eq!(screen(&t), str);
}

// ghostty: "Screen: resize less cols with reflow but row space"
#[test]
fn screen_resize_less_cols_with_reflow_but_row_space() {
    let mut t = with_history(5, 3);
    write(&mut t, "1ABCD");
    goto(&mut t, 4, 0);
    assert_eq!(cursor_cell(&t).char(), Some('D'));
    t.resize(3, 3);
    assert_eq!(viewport(&t), "1AB\nCD");
    assert_eq!(screen(&t), "1AB\nCD");
    assert_eq!(cursor(&t), (1, 1));
}

// ghostty: "Screen: resize less cols with reflow with trimmed rows"
#[test]
fn screen_resize_less_cols_with_reflow_with_trimmed_rows() {
    let mut t = no_history(5, 3);
    write(&mut t, "3IJKL\n4ABCD\n5EFGH");
    t.resize(3, 3);
    assert_eq!(viewport(&t), "CD\n5EF\nGH");
    assert_eq!(screen(&t), "CD\n5EF\nGH");
}

// ghostty: "Screen: resize less cols with reflow with trimmed rows and scrollback"
#[test]
fn screen_resize_less_cols_with_reflow_with_trimmed_rows_and_scrollback() {
    let mut t = with_history(5, 3);
    write(&mut t, "3IJKL\n4ABCD\n5EFGH");
    t.resize(3, 3);
    assert_eq!(viewport(&t), "CD\n5EF\nGH");
    assert_eq!(screen(&t), "3IJ\nKL\n4AB\nCD\n5EF\nGH");
}

// ghostty: "Screen: resize less cols with reflow previously wrapped"
#[test]
fn screen_resize_less_cols_with_reflow_previously_wrapped() {
    let mut t = no_history(5, 3);
    write(&mut t, "3IJKL4ABCD5EFGH");
    assert_eq!(screen(&t), "3IJKL\n4ABCD\n5EFGH");
    t.resize(3, 3);
    assert_eq!(screen(&t), "ABC\nD5E\nFGH");
}

// ghostty: "Screen: resize less cols with reflow and scrollback"
#[test]
fn screen_resize_less_cols_with_reflow_and_scrollback() {
    let mut t = with_history(5, 3);
    write(&mut t, "1A\n2B\n3C\n4D\n5E");
    goto(&mut t, 1, 2);
    assert_eq!(cursor_cell(&t).char(), Some('E'));
    t.resize(3, 3);
    assert_eq!(viewport(&t), "3C\n4D\n5E");
    assert_eq!(cursor(&t), (1, 2));
}

// ghostty: "Screen: resize less cols with reflow previously wrapped and scrollback"
#[test]
fn screen_resize_less_cols_with_reflow_previously_wrapped_and_scrollback() {
    let mut t = with_history(5, 3);
    write(&mut t, "1ABCD2EFGH3IJKL4ABCD5EFGH");
    assert_eq!(viewport(&t), "3IJKL\n4ABCD\n5EFGH");
    goto(&mut t, 4, 2);
    assert_eq!(cursor_cell(&t).char(), Some('H'));
    t.resize(3, 3);
    assert_eq!(viewport(&t), "CD5\nEFG\nH");
    assert_eq!(screen(&t), "1AB\nCD2\nEFG\nH3I\nJKL\n4AB\nCD5\nEFG\nH");
    assert_eq!(cursor(&t), (0, 2));
    assert_eq!(cursor_cell(&t).char(), Some('H'));
}

// ghostty: "Screen: resize less cols with scrollback keeps cursor row"
#[test]
fn screen_resize_less_cols_with_scrollback_keeps_cursor_row() {
    let mut t = with_history(5, 3);
    write(&mut t, "1A\n2B\n3C\n4D\n5E");
    // ghostty's scrollClear: the three rows with text scroll into the history.
    feed(&mut t, "\n\n\n");
    goto(&mut t, 0, 0);
    t.resize(3, 3);
    assert_eq!(viewport(&t), "");
    assert_eq!(cursor(&t), (0, 0));
}

// ghostty: "Screen: resize more rows, less cols with reflow with scrollback"
#[test]
fn screen_resize_more_rows_less_cols_with_reflow_with_scrollback() {
    let mut t = with_history(5, 3);
    write(&mut t, "1ABCD\n2EFGH3IJKL\n4MNOP");
    assert_eq!(screen(&t), "1ABCD\n2EFGH\n3IJKL\n4MNOP");
    assert_eq!(viewport(&t), "2EFGH\n3IJKL\n4MNOP");
    t.resize(2, 10);
    assert_eq!(viewport(&t), "BC\nD\n2E\nFG\nH3\nIJ\nKL\n4M\nNO\nP");
    assert_eq!(screen(&t), "1A\nBC\nD\n2E\nFG\nH3\nIJ\nKL\n4M\nNO\nP");
}

// ghostty: "Screen: resize more rows, less cols pulls scrollback like separate resizes"
#[test]
fn screen_resize_more_rows_less_cols_pulls_scrollback_like_separate_resizes() {
    let mut t = with_history(5, 2);
    write(&mut t, "1A\n2B\n3C\n");
    assert_eq!(viewport(&t), "3C");
    assert_eq!(cursor(&t).1, 1);
    t.resize(4, 4);
    assert_eq!(viewport(&t), "1A\n2B\n3C");
    assert_eq!(cursor(&t).1, 3);
}

// ghostty: "Screen: resize more rows then shrink again"
#[test]
fn screen_resize_more_rows_then_shrink_again() {
    let mut t = with_history(5, 3);
    let str = "1ABC";
    write(&mut t, str);
    for rows in [10, 3, 10] {
        t.resize(5, rows);
        assert_eq!(viewport(&t), str, "{rows} rows");
        assert_eq!(screen(&t), str, "{rows} rows");
    }
}

// ghostty: "Screen: resize less cols to eliminate wide char"
#[test]
fn screen_resize_less_cols_to_eliminate_wide_char() {
    let mut t = no_history(2, 1);
    write(&mut t, "\u{1f600}");
    assert_eq!(screen(&t), "\u{1f600}");
    assert_cell(screen_cell(&t, 0, 0), Some('\u{1f600}'), Width::Wide);
    // One column cannot hold a wide character, so it is deleted.
    t.resize(1, 1);
    assert_eq!(screen(&t), "");
    assert_cell(screen_cell(&t, 0, 0), None, Width::Narrow);
}

// ghostty: "Screen: resize less cols to wrap wide char"
#[test]
fn screen_resize_less_cols_to_wrap_wide_char() {
    let mut t = no_history(3, 3);
    write(&mut t, "x\u{1f600}");
    assert_eq!(screen(&t), "x\u{1f600}");
    assert_cell(screen_cell(&t, 1, 0), Some('\u{1f600}'), Width::Wide);
    assert_eq!(screen_cell(&t, 2, 0).width(), Width::SpacerTail);
    t.resize(2, 3);
    assert_eq!(screen(&t), "x\n\u{1f600}");
    assert_eq!(screen_cell(&t, 1, 0).width(), Width::SpacerHead);
    assert!(screen_wrapped(&t, 0));
}

// ghostty: "Screen: resize less cols to eliminate wide char with row space"
#[test]
fn screen_resize_less_cols_to_eliminate_wide_char_with_row_space() {
    let mut t = no_history(2, 2);
    write(&mut t, "\u{1f600}");
    assert_eq!(screen(&t), "\u{1f600}");
    assert_cell(screen_cell(&t, 0, 0), Some('\u{1f600}'), Width::Wide);
    assert_eq!(screen_cell(&t, 1, 0).width(), Width::SpacerTail);
    t.resize(1, 2);
    assert_eq!(screen(&t), "");
}

// ghostty: "Screen: resize less cols reflows cursor after wrapped text"
#[test]
fn screen_resize_less_cols_reflows_cursor_after_wrapped_text() {
    let mut t = no_history(50, 7);
    write(&mut t, &"a".repeat(30));
    assert_eq!(cursor(&t), (30, 0));
    t.resize(25, 7);
    assert_eq!(cursor(&t), (5, 1));
}

// ghostty: "Screen: resize less cols reflows cursor after empty cells"
#[test]
fn screen_resize_less_cols_reflows_cursor_after_empty_cells() {
    let mut t = no_history(10, 3);
    write(&mut t, "abc\x1b[6C");
    assert_eq!(cursor(&t), (9, 0));
    t.resize(5, 3);
    assert_eq!(cursor(&t), (4, 1));
}

// ghostty: "Screen: resize more cols with wide spacer head"
#[test]
fn screen_resize_more_cols_with_wide_spacer_head() {
    let mut t = no_history(3, 2);
    let str = "  \u{1f600}";
    write(&mut t, str);
    assert_eq!(screen(&t), "  \n\u{1f600}");
    assert_eq!(screen_cell(&t, 2, 0).width(), Width::SpacerHead);
    assert_eq!(screen_cell(&t, 0, 1).width(), Width::Wide);
    assert_eq!(screen_cell(&t, 1, 1).width(), Width::SpacerTail);
    // The spacer head is replaced with the wide character.
    t.resize(4, 2);
    assert_eq!(screen(&t), str);
    assert_cell(screen_cell(&t, 2, 0), Some('\u{1f600}'), Width::Wide);
    assert_eq!(screen_cell(&t, 3, 0).width(), Width::SpacerTail);
}

// ghostty: "Screen: resize more cols with wide spacer head multiple lines"
#[test]
fn screen_resize_more_cols_with_wide_spacer_head_multiple_lines() {
    let mut t = no_history(3, 3);
    let str = "xxxyy\u{1f600}";
    write(&mut t, str);
    assert_eq!(screen(&t), "xxx\nyy\n\u{1f600}");
    assert_eq!(screen_cell(&t, 2, 1).width(), Width::SpacerHead);
    assert_eq!(screen_cell(&t, 0, 2).width(), Width::Wide);
    assert_eq!(screen_cell(&t, 1, 2).width(), Width::SpacerTail);
    t.resize(8, 2);
    assert_eq!(screen(&t), str);
    assert_cell(screen_cell(&t, 5, 0), Some('\u{1f600}'), Width::Wide);
    assert_eq!(screen_cell(&t, 6, 0).width(), Width::SpacerTail);
}

// ghostty: "Screen: resize more cols requiring a wide spacer head"
#[test]
fn screen_resize_more_cols_requiring_a_wide_spacer_head() {
    let mut t = no_history(2, 2);
    write(&mut t, "xx\u{1f600}");
    assert_eq!(screen(&t), "xx\n\u{1f600}");
    assert_eq!(screen_cell(&t, 0, 1).width(), Width::Wide);
    assert_eq!(screen_cell(&t, 1, 1).width(), Width::SpacerTail);
    // Three columns cannot fit the wide character on the first row, which
    // ends with a spacer head instead.
    t.resize(3, 2);
    assert_eq!(screen(&t), "xx\n\u{1f600}");
    assert_eq!(screen_cell(&t, 2, 0).width(), Width::SpacerHead);
    assert_cell(screen_cell(&t, 0, 1), Some('\u{1f600}'), Width::Wide);
    assert_eq!(screen_cell(&t, 1, 1).width(), Width::SpacerTail);
}

// ghostty: "Screen: resize more cols with cursor at prompt"
#[test]
fn screen_resize_more_cols_with_cursor_at_prompt() {
    let mut t = with_history(10, 3);
    write(&mut t, "ABCDE\n\x1b]133;P\x07> \x1b]133;I\x07echo");
    assert_eq!(viewport(&t), "ABCDE\n> echo");
    t.resize(20, 3);
    assert_eq!(cursor(&t), (6, 1));
    assert_eq!(viewport(&t), "ABCDE");
}

// ghostty: "Screen: resize more cols with cursor not at prompt"
#[test]
fn screen_resize_more_cols_with_cursor_not_at_prompt() {
    let mut t = with_history(10, 3);
    write(&mut t, "ABCDE\n\x1b]133;P\x07> \x1b]133;I\x07echo\noutput");
    assert_eq!(viewport(&t), "ABCDE\n> echo\noutput");
    t.resize(20, 3);
    assert_eq!(cursor(&t), (6, 2));
    assert_eq!(viewport(&t), "ABCDE\n> echo\noutput");
}

// ghostty: "Screen: resize with prompt_redraw last clears only one line"
#[test]
fn screen_resize_with_prompt_redraw_last_clears_only_one_line() {
    let mut t = with_history(10, 4);
    write(&mut t, "ABCDE\n\x1b]133;A;redraw=last\x07> \x1b]133;B\x07hello\nworld");
    assert_eq!(viewport(&t), "ABCDE\n> hello\nworld");
    t.resize(20, 4);
    assert_eq!(viewport(&t), "ABCDE\n> hello");
}

// ghostty: "Screen: resize with prompt_redraw last multiline prompt clears only last line"
#[test]
fn screen_resize_with_prompt_redraw_last_multiline_prompt_clears_only_last_line() {
    let mut t = with_history(20, 5);
    write(&mut t, "\x1b]133;A;redraw=last\x07line1\n\x1b]133;P;k=c\x07line2\n\x1b]133;P;k=c\x07line3");
    assert_eq!(viewport(&t), "line1\nline2\nline3");
    t.resize(30, 5);
    assert_eq!(viewport(&t), "line1\nline2");
}

// ---- PageList.zig ---------------------------------------------------------

// ghostty: "PageList resize (no reflow) more rows"
#[test]
fn pagelist_no_reflow_more_rows() {
    let mut t = with_history(10, 3);
    goto(&mut t, 0, 2);
    track(&mut t, 0, 2);
    no_reflow(&mut t);
    t.resize(10, 10);
    assert_eq!(t.history_len(), 0);
    assert_eq!(pin_active(&t), Some((0, 2)));
}

// ghostty: "PageList resize (no reflow) more rows with history"
#[test]
fn pagelist_no_reflow_more_rows_with_history() {
    let mut t = with_history(10, 3);
    feed(&mut t, &"\n".repeat(52));
    assert_eq!(t.history_len(), 50);
    track(&mut t, 0, 2);
    no_reflow(&mut t);
    t.resize(10, 5);
    assert_eq!(t.history_len(), 48);
    // The pin moves since the rows above it come from the history.
    assert_eq!(pin_active(&t), Some((0, 4)));
}

// ghostty: "PageList resize (no reflow) less rows"
#[test]
fn pagelist_no_reflow_less_rows() {
    let mut t = with_history(10, 10);
    write(&mut t, &["A"; 10].join("\n"));
    no_reflow(&mut t);
    t.resize(10, 5);
    assert_eq!(t.history_len(), 5);
}

// ghostty: "PageList resize (no reflow) one rows"
#[test]
fn pagelist_no_reflow_one_rows() {
    let mut t = with_history(10, 10);
    write(&mut t, &["A"; 10].join("\n"));
    no_reflow(&mut t);
    t.resize(10, 1);
    assert_eq!(t.history_len(), 9);
}

// ghostty: "PageList resize (no reflow) less rows cursor on bottom"
#[test]
fn pagelist_no_reflow_less_rows_cursor_on_bottom() {
    let mut t = with_history(10, 10);
    write(&mut t, "0\n1\n2\n3\n4\n5\n6\n7\n8\n9");
    goto(&mut t, 0, 9);
    track(&mut t, 0, 9);
    assert_eq!(pin_cell(&t).char(), Some('9'));
    no_reflow(&mut t);
    t.resize(10, 5);
    assert_eq!(t.history_len(), 5);
    assert_eq!(pin_active(&t), Some((0, 4)));
}

// ghostty: "PageList resize (no reflow) less rows cursor in scrollback"
#[test]
fn pagelist_no_reflow_less_rows_cursor_in_scrollback() {
    let mut t = with_history(10, 10);
    write(&mut t, "0\n1\n2\n3\n4\n5\n6\n7\n8\n9");
    track(&mut t, 0, 2);
    assert_eq!(pin_cell(&t).char(), Some('2'));
    no_reflow(&mut t);
    t.resize(10, 5);
    assert_eq!(t.history_len(), 5);
    assert_eq!(pin_active(&t), None);
    assert_eq!(pin_screen(&t), (0, 2));
}

// ghostty: "PageList resize (no reflow) less rows trims blank lines"
#[test]
fn pagelist_no_reflow_less_rows_trims_blank_lines() {
    let mut t = with_history(10, 5);
    write(&mut t, "A");
    for y in 1..5 {
        bg_cell(&mut t, y);
    }
    goto(&mut t, 0, 0);
    track(&mut t, 0, 0);
    no_reflow(&mut t);
    t.resize(10, 2);
    assert_eq!(t.history_len(), 0);
    assert_eq!(pin_active(&t), Some((0, 0)));
}

// ghostty: "PageList resize (no reflow) less rows trims blank lines cursor in blank line"
#[test]
fn pagelist_no_reflow_less_rows_trims_blank_lines_cursor_in_blank_line() {
    let mut t = with_history(10, 5);
    write(&mut t, "A");
    for y in 1..5 {
        bg_cell(&mut t, y);
    }
    goto(&mut t, 0, 3);
    track(&mut t, 0, 3);
    no_reflow(&mut t);
    t.resize(10, 2);
    assert_eq!(t.history_len(), 2);
    assert_eq!(pin_active(&t), Some((0, 1)));
}

// ghostty: "PageList resize (no reflow) more rows extends blank lines"
#[test]
fn pagelist_no_reflow_more_rows_extends_blank_lines() {
    let mut t = with_history(10, 3);
    write(&mut t, "A");
    bg_cell(&mut t, 1);
    bg_cell(&mut t, 2);
    no_reflow(&mut t);
    t.resize(10, 7);
    assert_eq!(t.history_len(), 0);
    assert_eq!(t.rows(), 7);
}

// ghostty: "PageList resize (no reflow) more rows contains viewport"
#[test]
fn pagelist_no_reflow_more_rows_contains_viewport() {
    let mut t = with_history(5, 5);
    feed(&mut t, "\n\n\n\n\n");
    assert_eq!(t.history_len(), 1);
    t.scroll_view(1);
    assert_eq!(t.view_top(), t.first_line());
    no_reflow(&mut t);
    t.resize(5, 7);
    assert_eq!(t.history_len(), 0);
    assert_eq!(t.view_top(), t.first_line());
}

// ghostty: "PageList resize (no reflow) less cols"
#[test]
fn pagelist_no_reflow_less_cols() {
    let mut t = with_history(10, 10);
    no_reflow(&mut t);
    t.resize(5, 10);
    assert_eq!(t.history_len(), 0);
    for l in t.first_line()..t.end_line() {
        assert_eq!(t.line(l).unwrap().len(), 5);
    }
}

// ghostty: "PageList resize (no reflow) less cols pin in trimmed cols"
#[test]
fn pagelist_no_reflow_less_cols_pin_in_trimmed_cols() {
    let mut t = with_history(10, 10);
    track(&mut t, 8, 2);
    no_reflow(&mut t);
    t.resize(5, 10);
    assert_eq!(t.history_len(), 0);
    assert_eq!(pin_active(&t), Some((4, 2)));
}

// ghostty: "PageList resize (no reflow) more cols"
#[test]
fn pagelist_no_reflow_more_cols() {
    let mut t = with_history(5, 3);
    no_reflow(&mut t);
    t.resize(10, 3);
    assert_eq!(t.history_len(), 0);
    for l in t.first_line()..t.end_line() {
        assert_eq!(t.line(l).unwrap().len(), 10);
    }
}

// ghostty: "PageList resize (no reflow) more cols with spacer head"
#[test]
fn pagelist_no_reflow_more_cols_with_spacer_head() {
    let mut t = with_history(2, 3);
    write(&mut t, "x\u{1f600}");
    assert!(wrapped(&t, 0));
    assert_eq!(cell(&t, 1, 0).width(), Width::SpacerHead);
    no_reflow(&mut t);
    t.resize(3, 3);
    assert_eq!(t.history_len(), 0);
    assert_cell(cell(&t, 0, 0), Some('x'), Width::Narrow);
    assert_cell(cell(&t, 1, 0), None, Width::Narrow);
    assert_cell(cell(&t, 2, 0), None, Width::Narrow);
}

// ghostty: "PageList resize (no reflow) grow cols fast path with spacer head"
#[test]
fn pagelist_no_reflow_grow_cols_fast_path_with_spacer_head() {
    let mut t = with_history(10, 3);
    no_reflow(&mut t);
    t.resize(5, 3);
    // Rows 0 and 1 end with a spacer head.
    feed(&mut t, "\x1b[?7h");
    write(&mut t, "xxxx\u{1f600}yy\u{1f600}");
    for y in 0..2 {
        assert_eq!(cell(&t, 4, y).width(), Width::SpacerHead);
        assert!(wrapped(&t, y));
    }
    no_reflow(&mut t);
    t.resize(10, 3);
    for y in 0..2 {
        assert_eq!(cell(&t, 4, y).width(), Width::Narrow, "row {y}");
        assert!(!wrapped(&t, y), "row {y}");
    }
}

// ghostty: "PageList resize (no reflow) less cols then more cols"
#[test]
fn pagelist_no_reflow_less_cols_then_more_cols() {
    let mut t = with_history(5, 3);
    no_reflow(&mut t);
    t.resize(2, 3);
    t.resize(5, 3);
    assert_eq!(t.history_len(), 0);
    for l in t.first_line()..t.end_line() {
        assert_eq!(t.line(l).unwrap().len(), 5);
    }
}

// ghostty: "PageList resize (no reflow) less rows and cols"
#[test]
fn pagelist_no_reflow_less_rows_and_cols() {
    let mut t = with_history(10, 10);
    no_reflow(&mut t);
    t.resize(5, 7);
    for l in t.first_line()..t.end_line() {
        assert_eq!(t.line(l).unwrap().len(), 5);
    }
}

// ghostty: "PageList resize less rows and cols cursor at bottom"
#[test]
fn pagelist_resize_less_rows_and_cols_cursor_at_bottom() {
    let mut t = with_history(80, 24);
    goto(&mut t, 0, 23);
    t.resize(79, 20);
    assert_eq!(cursor(&t), (0, 19));
}

// ghostty: "PageList resize less rows and cols cursor near top pushed to scrollback"
#[test]
fn pagelist_resize_less_rows_and_cols_cursor_near_top_pushed_to_scrollback() {
    let mut t = with_history(80, 24);
    let row: String = (0..80).map(|x| (b'A' + (x % 26) as u8) as char).collect();
    write(&mut t, &vec![row.as_str(); 24].join("\n"));
    goto(&mut t, 0, 0);
    track(&mut t, 0, 0);
    t.resize(79, 20);
    // tapust differs from ghostty: the cursor's row stays on the screen, at its top, with its pin.
    assert_eq!((cursor(&t), pin_active(&t)), ((0, 0), Some((0, 0))));
    assert_eq!(t.history_len(), 0);
}

// ghostty: "PageList resize (no reflow) more rows and less cols"
#[test]
fn pagelist_no_reflow_more_rows_and_less_cols() {
    let mut t = with_history(10, 10);
    no_reflow(&mut t);
    t.resize(5, 20);
    assert_eq!(t.rows(), 20);
    assert_eq!(t.history_len(), 0);
    for l in t.first_line()..t.end_line() {
        assert_eq!(t.line(l).unwrap().len(), 5);
    }
}

// ghostty: "PageList resize more rows and cols doesn't fit in single std page"
#[test]
fn pagelist_resize_more_rows_and_cols_doesnt_fit_in_single_std_page() {
    let mut t = with_history(10, 10);
    t.resize(600, 600);
    assert_eq!((t.cols(), t.rows()), (600, 600));
    assert_eq!(t.history_len(), 0);
}

// ghostty: "PageList resize (no reflow) empty screen"
#[test]
fn pagelist_no_reflow_empty_screen() {
    let mut t = with_history(5, 5);
    no_reflow(&mut t);
    t.resize(10, 10);
    assert_eq!(t.history_len(), 0);
    for l in t.first_line()..t.end_line() {
        assert_eq!(t.line(l).unwrap().len(), 10);
    }
}

// ghostty: "PageList resize (no reflow) more rows adds blank rows if cursor at bottom"
#[test]
fn pagelist_no_reflow_more_rows_adds_blank_rows_if_cursor_at_bottom() {
    let mut t = with_history(5, 3);
    write(&mut t, "0\n1\n2\n3\n4");
    assert_eq!(t.history_len(), 2);
    goto(&mut t, 0, 1);
    assert_eq!(cursor_cell(&t).char(), Some('3'));
    no_reflow(&mut t);
    t.resize(5, 10);
    // The cursor is not on the last row, so no history comes back.
    assert_eq!(cursor(&t), (0, 1));
    assert_eq!(t.history_len(), 2);
    assert_eq!(active(&t), "2\n3\n4");
}

// ghostty: "PageList resize reflow more cols no wrapped rows"
#[test]
fn pagelist_reflow_more_cols_no_wrapped_rows() {
    let mut t = with_history(5, 3);
    write(&mut t, "AAAAA\nAAAAA\nAAAAA");
    t.resize(10, 3);
    assert_eq!(t.history_len(), 0);
    for l in t.first_line()..t.end_line() {
        assert_eq!(t.line(l).unwrap().len(), 10);
        assert_eq!(t.line(l).unwrap()[0].char(), Some('A'));
    }
}

// ghostty: "PageList resize reflow more cols wrapped rows"
#[test]
fn pagelist_reflow_more_cols_wrapped_rows() {
    let mut t = with_history(2, 4);
    write(&mut t, "AAAA\nAAAA");
    assert!(wrapped(&t, 0) && !wrapped(&t, 1) && wrapped(&t, 2));
    t.resize(4, 4);
    assert_eq!(t.history_len(), 0);
    assert!(!screen_wrapped(&t, 0));
    assert_eq!(screen_cell(&t, 0, 0).char(), Some('A'));
    assert_eq!(screen_cell(&t, 2, 0).char(), Some('A'));
}

// ghostty: "PageList resize reflow more cols cursor in wrapped row"
#[test]
fn pagelist_reflow_more_cols_cursor_in_wrapped_row() {
    let mut t = with_history(2, 4);
    write(&mut t, "abcd");
    track(&mut t, 1, 1);
    t.resize(4, 4);
    assert_eq!(t.history_len(), 0);
    assert_eq!(pin_active(&t), Some((3, 0)));
}

// ghostty: "PageList resize reflow more cols cursor in not wrapped row"
#[test]
fn pagelist_reflow_more_cols_cursor_in_not_wrapped_row() {
    let mut t = with_history(2, 4);
    write(&mut t, "abcd");
    track(&mut t, 1, 0);
    t.resize(4, 4);
    assert_eq!(t.history_len(), 0);
    assert_eq!(pin_active(&t), Some((1, 0)));
}

// ghostty: "PageList resize reflow more cols cursor in wrapped row that isn't unwrapped"
#[test]
fn pagelist_reflow_more_cols_cursor_in_wrapped_row_that_isnt_unwrapped() {
    let mut t = with_history(2, 4);
    write(&mut t, "abcdef");
    track(&mut t, 1, 2);
    t.resize(4, 4);
    assert_eq!(t.history_len(), 0);
    assert_eq!(pin_active(&t), Some((1, 1)));
}

// ghostty: "PageList resize reflow more cols no reflow preserves semantic prompt"
#[test]
fn pagelist_reflow_more_cols_no_reflow_preserves_semantic_prompt() {
    let mut t = with_history(2, 4);
    feed(&mut t, "\x1b[2;1H\x1b]133;P\x07");
    t.resize(4, 4);
    assert_eq!(t.history_len(), 0);
    assert_eq!(t.line_prompt(active_top(&t) + 1), PromptMark::Prompt);
}

// ghostty: "PageList resize reflow more cols unwrap wide spacer head"
#[test]
fn pagelist_reflow_more_cols_unwrap_wide_spacer_head() {
    let mut t = with_history(2, 2);
    write(&mut t, "x\u{1f600}");
    t.resize(4, 2);
    assert_eq!(t.history_len(), 0);
    assert_cell(cell(&t, 0, 0), Some('x'), Width::Narrow);
    assert!(!wrapped(&t, 0));
    assert_cell(cell(&t, 1, 0), Some('\u{1f600}'), Width::Wide);
    assert_cell(cell(&t, 2, 0), None, Width::SpacerTail);
}

// ghostty: "PageList resize reflow more cols unwrap wide spacer head across two rows"
#[test]
fn pagelist_reflow_more_cols_unwrap_wide_spacer_head_across_two_rows() {
    let mut t = with_history(2, 3);
    write(&mut t, "xxx\u{1f600}");
    assert_eq!(cell(&t, 1, 1).width(), Width::SpacerHead);
    t.resize(4, 3);
    assert_eq!(t.history_len(), 0);
    assert!(wrapped(&t, 0));
    for x in 0..3 {
        assert_cell(cell(&t, x, 0), Some('x'), Width::Narrow);
    }
    assert_cell(cell(&t, 3, 0), None, Width::SpacerHead);
    assert_cell(cell(&t, 0, 1), Some('\u{1f600}'), Width::Wide);
    assert_cell(cell(&t, 1, 1), None, Width::SpacerTail);
}

// ghostty: "PageList resize reflow more cols unwrap still requires wide spacer head"
#[test]
fn pagelist_reflow_more_cols_unwrap_still_requires_wide_spacer_head() {
    let mut t = with_history(2, 2);
    write(&mut t, "xx\u{1f600}");
    t.resize(3, 2);
    assert_eq!(t.history_len(), 0);
    assert!(wrapped(&t, 0));
    assert_cell(cell(&t, 0, 0), Some('x'), Width::Narrow);
    assert_cell(cell(&t, 1, 0), Some('x'), Width::Narrow);
    assert_cell(cell(&t, 2, 0), None, Width::SpacerHead);
    assert_cell(cell(&t, 0, 1), Some('\u{1f600}'), Width::Wide);
    assert_cell(cell(&t, 1, 1), None, Width::SpacerTail);
}

// ghostty: "PageList resize reflow less cols no reflow preserves semantic prompt"
#[test]
fn pagelist_reflow_less_cols_no_reflow_preserves_semantic_prompt() {
    let mut t = with_history(4, 4);
    feed(&mut t, "\x1b[2;1H\x1b]133;P\x07abcd\x1b]133;C\x07");
    t.resize(2, 4);
    assert_eq!(t.history_len(), 0);
    assert!(wrapped(&t, 1));
    assert_eq!(t.line_prompt(active_top(&t) + 1), PromptMark::Prompt);
    // tapust differs from ghostty: a row split off a prompt row continues the prompt.
    assert_eq!(t.line_prompt(active_top(&t) + 2), PromptMark::Continuation);
}

// ghostty: "PageList resize reflow less cols no reflow preserves semantic prompt on first line"
#[test]
fn pagelist_reflow_less_cols_no_reflow_preserves_semantic_prompt_on_first_line() {
    let mut t = with_history(4, 4);
    feed(&mut t, "\x1b]133;P\x07");
    t.resize(2, 4);
    assert_eq!(t.history_len(), 0);
    assert_eq!(t.line_prompt(t.first_line()), PromptMark::Prompt);
}

// ghostty: "PageList resize reflow less cols wrap preserves semantic prompt"
#[test]
fn pagelist_reflow_less_cols_wrap_preserves_semantic_prompt() {
    let mut t = with_history(4, 4);
    feed(&mut t, "\x1b]133;P\x07");
    t.resize(2, 4);
    assert_eq!(t.history_len(), 0);
    assert_eq!(t.line_prompt(t.first_line()), PromptMark::Prompt);
}

// ghostty: "PageList resize reflow less cols no wrapped rows"
#[test]
fn pagelist_reflow_less_cols_no_wrapped_rows() {
    let mut t = with_history(10, 3);
    write(&mut t, "abcd\nabcd\nabcd");
    t.resize(5, 3);
    assert_eq!(t.history_len(), 0);
    assert_eq!(screen(&t), "abcd\nabcd\nabcd");
}

// ghostty: "PageList resize reflow less cols wrapped rows"
#[test]
fn pagelist_reflow_less_cols_wrapped_rows() {
    let mut t = with_history(4, 2);
    write(&mut t, "abcd\nabcd");
    t.resize(2, 2);
    assert_eq!(t.history_len(), 2);
    for (y, (wrap, first)) in [(true, 'a'), (false, 'c'), (true, 'a'), (false, 'c')].into_iter().enumerate() {
        assert_eq!(screen_wrapped(&t, y), wrap, "row {y}");
        assert_eq!(screen_cell(&t, 0, y).char(), Some(first), "row {y}");
    }
}

// ghostty: "PageList resize reflow less cols wrapped rows with graphemes"
#[test]
fn pagelist_reflow_less_cols_wrapped_rows_with_graphemes() {
    let mut t = with_history(4, 2);
    write(&mut t, "abc\u{301}d\nabc\u{301}d");
    t.resize(2, 2);
    assert_eq!(t.history_len(), 2);
    let rows = [(true, "a"), (false, "c\u{301}"), (true, "a"), (false, "c\u{301}")];
    for (y, (wrap, first)) in rows.into_iter().enumerate() {
        let line = t.first_line() + y as u64;
        assert_eq!(t.line_wrapped(line), wrap, "row {y}");
        let c = t.line(line).unwrap()[0];
        let text = t.line_grapheme(line, 0).map_or_else(|| c.char().unwrap().to_string(), str::to_owned);
        assert_eq!(text, first, "row {y}");
    }
}

// ghostty: "PageList resize reflow less cols cursor in wrapped row"
#[test]
fn pagelist_reflow_less_cols_cursor_in_wrapped_row() {
    let mut t = with_history(4, 2);
    write(&mut t, "abcd\nabcd");
    track(&mut t, 2, 1);
    t.resize(2, 2);
    assert_eq!(t.history_len(), 2);
    assert_eq!(pin_active(&t), Some((0, 1)));
}

// ghostty: "PageList resize reflow less cols wraps spacer head"
#[test]
fn pagelist_reflow_less_cols_wraps_spacer_head() {
    let mut t = with_history(4, 3);
    write(&mut t, "xxx\u{1f600}");
    assert_eq!(cell(&t, 3, 0).width(), Width::SpacerHead);
    t.resize(3, 3);
    assert_eq!(t.history_len(), 0);
    assert!(wrapped(&t, 0));
    for x in 0..3 {
        assert_cell(cell(&t, x, 0), Some('x'), Width::Narrow);
    }
    assert_cell(cell(&t, 0, 1), Some('\u{1f600}'), Width::Wide);
    assert_cell(cell(&t, 1, 1), None, Width::SpacerTail);
}

// ghostty: "PageList resize reflow less cols cursor goes to scrollback"
#[test]
fn pagelist_reflow_less_cols_cursor_goes_to_scrollback() {
    let mut t = with_history(4, 2);
    write(&mut t, "abcd\nabcd");
    track(&mut t, 2, 0);
    t.resize(2, 2);
    assert_eq!(t.history_len(), 2);
    assert_eq!(pin_active(&t), None);
}

// ghostty: "PageList resize reflow less cols cursor in unchanged row"
#[test]
fn pagelist_reflow_less_cols_cursor_in_unchanged_row() {
    let mut t = with_history(4, 2);
    write(&mut t, "ab\nab");
    goto(&mut t, 0, 0);
    track(&mut t, 1, 0);
    t.resize(2, 2);
    assert_eq!(t.history_len(), 0);
    assert_eq!(pin_active(&t), Some((1, 0)));
}

// ghostty: "PageList resize reflow less cols cursor in blank cell"
#[test]
fn pagelist_reflow_less_cols_cursor_in_blank_cell() {
    let mut t = with_history(6, 2);
    write(&mut t, "ab\nab");
    goto(&mut t, 0, 0);
    track(&mut t, 2, 0);
    t.resize(4, 2);
    assert_eq!(t.history_len(), 0);
    assert_eq!(pin_active(&t), Some((2, 0)));
}

// ghostty: "PageList resize reflow less cols cursor in final blank cell"
#[test]
fn pagelist_reflow_less_cols_cursor_in_final_blank_cell() {
    let mut t = with_history(6, 2);
    write(&mut t, "ab\nab");
    goto(&mut t, 0, 0);
    track(&mut t, 3, 0);
    t.resize(4, 2);
    assert_eq!(t.history_len(), 0);
    assert_eq!(pin_active(&t), Some((3, 0)));
}

// ghostty: "PageList resize reflow pin in blank cells after line break"
#[test]
fn pagelist_reflow_pin_in_blank_cells_after_line_break() {
    let cases = [
        // A hard line break starts the pin row at column zero.
        ("abc\ng", 5, 8, (5, 1)),
        // A soft continuation uses the remaining destination columns.
        ("abcdefg", 3, 8, (7, 0)),
        // Pending wrap starts the continuation at column zero.
        ("abcdefg", 2, 3, (2, 2)),
    ];
    for (text, pin_x, cols, expected) in cases {
        let mut t = with_history(6, 4);
        write(&mut t, text);
        track(&mut t, pin_x, 1);
        t.resize(cols, 4);
        assert_eq!(pin_active(&t), Some(expected), "{text:?} at {cols} cols");
        assert!(is_empty(pin_cell(&t)), "{text:?} at {cols} cols");
    }
}

// ghostty: "PageList resize reflow less cols cursor in wrapped blank cell"
#[test]
fn pagelist_reflow_less_cols_cursor_in_wrapped_blank_cell() {
    let mut t = with_history(6, 2);
    write(&mut t, "ab\nab");
    goto(&mut t, 0, 0);
    track(&mut t, 5, 0);
    t.resize(4, 2);
    assert_eq!(t.history_len(), 0);
    assert_eq!(pin_active(&t), Some((3, 0)));
}

// ghostty: "PageList resize reflow less cols blank lines"
#[test]
fn pagelist_reflow_less_cols_blank_lines() {
    let mut t = with_history(4, 3);
    write(&mut t, "abcd");
    t.resize(2, 3);
    assert_eq!(t.history_len(), 0);
    assert!(wrapped(&t, 0));
    assert_eq!(cell(&t, 0, 0).char(), Some('a'));
    assert!(!wrapped(&t, 1));
    assert_eq!(cell(&t, 0, 1).char(), Some('c'));
}

// ghostty: "PageList resize reflow less cols blank lines between"
#[test]
fn pagelist_reflow_less_cols_blank_lines_between() {
    let mut t = with_history(4, 3);
    write(&mut t, "abcd\n\nabcd");
    t.resize(2, 3);
    assert_eq!(t.history_len(), 2);
    assert!(!wrapped(&t, 0));
    assert!(wrapped(&t, 1));
    assert_eq!(cell(&t, 0, 1).char(), Some('a'));
    assert!(!wrapped(&t, 2));
    assert_eq!(cell(&t, 0, 2).char(), Some('c'));
}

// ghostty: "PageList resize reflow less cols blank lines between no scrollback"
#[test]
fn pagelist_reflow_less_cols_blank_lines_between_no_scrollback() {
    let mut t = with_history(5, 3);
    write(&mut t, "A\n\nC");
    t.resize(2, 3);
    assert_eq!(t.history_len(), 0);
    assert!(!wrapped(&t, 0));
    assert_eq!(cell(&t, 0, 0).char(), Some('A'));
    assert_eq!(cell(&t, 0, 1).char(), None);
    assert!(!wrapped(&t, 2));
    assert_eq!(cell(&t, 0, 2).char(), Some('C'));
}

// ghostty: "PageList resize reflow less cols cursor not on last line preserves location"
#[test]
fn pagelist_reflow_less_cols_cursor_not_on_last_line_preserves_location() {
    let mut t = with_history(5, 5);
    write(&mut t, "ab\nab\nab\nab\nab");
    // Blank rows push the rows into the history.
    feed(&mut t, "\n\n\n\n\n");
    assert_eq!(t.history_len(), 5);
    track(&mut t, 0, 0);
    goto(&mut t, 1, 1);
    t.resize(4, 5);
    assert_eq!(t.history_len(), 5);
    assert_eq!(pin_active(&t), Some((0, 0)));
}

// ghostty: "PageList resize reflow less cols copy style"
#[test]
fn pagelist_reflow_less_cols_copy_style() {
    let mut t = with_history(4, 2);
    write(&mut t, "\x1b[1mabc\x1b[m");
    t.resize(2, 2);
    assert_eq!(t.history_len(), 0);
    for y in 0..2 {
        assert!(t.style(cell(&t, 0, y).style()).attrs.contains(Attrs::BOLD), "row {y}");
    }
}

// ghostty: "PageList resize reflow less cols to eliminate a wide char"
#[test]
fn pagelist_reflow_less_cols_to_eliminate_a_wide_char() {
    let mut t = with_history(2, 1);
    write(&mut t, "\u{1f600}");
    t.resize(1, 1);
    assert_eq!(t.history_len(), 0);
    assert_cell(cell(&t, 0, 0), None, Width::Narrow);
}

// ghostty: "PageList resize reflow less cols to wrap a wide char"
#[test]
fn pagelist_reflow_less_cols_to_wrap_a_wide_char() {
    let mut t = with_history(3, 1);
    write(&mut t, "x\u{1f600}");
    t.resize(2, 1);
    assert_eq!(t.history_len(), 1);
    assert_cell(screen_cell(&t, 0, 0), Some('x'), Width::Narrow);
    assert!(screen_wrapped(&t, 0));
    assert_cell(screen_cell(&t, 1, 0), None, Width::SpacerHead);
    assert_cell(screen_cell(&t, 0, 1), Some('\u{1f600}'), Width::Wide);
    assert_cell(screen_cell(&t, 1, 1), None, Width::SpacerTail);
}

const CJK: [char; 4] = ['\u{4e00}', '\u{4e01}', '\u{4e02}', '\u{4e03}'];

// ghostty: "PageList resize reflow less cols wide char bulk run"
#[test]
fn pagelist_reflow_less_cols_wide_char_bulk_run() {
    let mut t = with_history(8, 1);
    write(&mut t, &CJK.iter().collect::<String>());
    // Exactly two pairs per row: no spacer heads.
    t.resize(4, 1);
    assert_eq!(t.history_len(), 1);
    for (i, &c) in CJK.iter().enumerate() {
        let (x, y) = ((i % 2) * 2, i / 2);
        assert_cell(screen_cell(&t, x, y), Some(c), Width::Wide);
        assert_cell(screen_cell(&t, x + 1, y), None, Width::SpacerTail);
    }
    assert!(screen_wrapped(&t, 0));
    assert!(!screen_wrapped(&t, 1));
}

// ghostty: "PageList resize reflow less cols wide char bulk run odd cols spacer head"
#[test]
fn pagelist_reflow_less_cols_wide_char_bulk_run_odd_cols_spacer_head() {
    let mut t = with_history(8, 1);
    write(&mut t, &CJK.iter().collect::<String>());
    t.resize(5, 1);
    assert_eq!(t.history_len(), 1);
    for (i, &c) in CJK.iter().enumerate() {
        let (x, y) = ((i % 2) * 2, i / 2);
        assert_cell(screen_cell(&t, x, y), Some(c), Width::Wide);
        assert_cell(screen_cell(&t, x + 1, y), None, Width::SpacerTail);
    }
    assert_cell(screen_cell(&t, 4, 0), None, Width::SpacerHead);
    assert!(screen_wrapped(&t, 0));
    assert_eq!(screen_cell(&t, 4, 1).width(), Width::Narrow);
}

// ghostty: "PageList resize reflow less cols wide char bulk run mixed narrow round trip"
#[test]
fn pagelist_reflow_less_cols_wide_char_bulk_run_mixed_narrow_round_trip() {
    let shape = [
        (Some(CJK[0]), Width::Wide),
        (None, Width::SpacerTail),
        (Some('x'), Width::Narrow),
        (Some(CJK[1]), Width::Wide),
        (None, Width::SpacerTail),
        (Some('y'), Width::Narrow),
        (Some(CJK[2]), Width::Wide),
        (None, Width::SpacerTail),
        (Some('z'), Width::Narrow),
    ];
    let mut t = with_history(9, 1);
    write(&mut t, &format!("{}x{}y{}z", CJK[0], CJK[1], CJK[2]));
    t.resize(6, 1);
    assert_eq!(t.history_len(), 1);
    for (i, &(ch, width)) in shape.iter().enumerate() {
        assert_cell(screen_cell(&t, i % 6, i / 6), ch, width);
    }
    assert!(screen_wrapped(&t, 0));
    // The cursor stays inside the rows the pair rejoins.
    goto(&mut t, 0, 0);
    t.resize(9, 1);
    assert_eq!(t.history_len(), 0);
    for (i, &(ch, width)) in shape.iter().enumerate() {
        assert_cell(screen_cell(&t, i, 0), ch, width);
    }
    assert!(!screen_wrapped(&t, 0));
}

// ghostty: "PageList resize reflow less cols wide char bulk run styled"
#[test]
fn pagelist_reflow_less_cols_wide_char_bulk_run_styled() {
    let mut t = with_history(8, 1);
    write(&mut t, &format!("\x1b[1m{}\x1b[m", CJK.iter().collect::<String>()));
    t.resize(4, 1);
    assert_eq!(t.history_len(), 1);
    for y in 0..2 {
        for x in 0..4 {
            let c = screen_cell(&t, x, y);
            assert!(t.style(c.style()).attrs.contains(Attrs::BOLD), "({x}, {y})");
            assert_eq!(c.width(), if x % 2 == 0 { Width::Wide } else { Width::SpacerTail });
        }
    }
}

// ghostty: "PageList resize reflow less cols to wrap a multi-codepoint grapheme
// with a spacer head", with a combining mark: tapust does not cluster ZWJ
// sequences into one cell.
#[test]
fn pagelist_reflow_less_cols_to_wrap_a_multi_codepoint_grapheme_with_a_spacer_head() {
    let mut t = with_history(4, 2);
    write(&mut t, "\u{4e2d}\u{302}\u{4e2d}\u{302}");
    t.resize(3, 2);
    assert_eq!(t.history_len(), 0);
    let first = t.first_line();
    assert_cell(screen_cell(&t, 0, 0), Some('\u{4e2d}'), Width::Wide);
    assert_eq!(t.line_grapheme(first, 0), Some("\u{4e2d}\u{302}"));
    assert!(screen_wrapped(&t, 0));
    assert_cell(screen_cell(&t, 1, 0), None, Width::SpacerTail);
    assert_cell(screen_cell(&t, 2, 0), None, Width::SpacerHead);
    assert_cell(screen_cell(&t, 0, 1), Some('\u{4e2d}'), Width::Wide);
    assert_eq!(t.line_grapheme(first + 1, 0), Some("\u{4e2d}\u{302}"));
    assert_cell(screen_cell(&t, 1, 1), None, Width::SpacerTail);
}

// ghostty: "PageList resize trimmed rows have default state"
#[test]
fn pagelist_resize_trimmed_rows_have_default_state() {
    let mut t = with_history(5, 5);
    feed(&mut t, "\x1b[5;1H\x1b]133;P;k=c\x07\x1b[48;5;42m\x1b[X\x1b[m\x1b[H");
    assert_eq!(t.line_prompt(active_top(&t) + 4), PromptMark::Continuation);
    no_reflow(&mut t);
    t.resize(5, 4);
    t.resize(5, 5);
    assert!(!wrapped(&t, 4));
    assert_eq!(t.line_prompt(active_top(&t) + 4), PromptMark::None);
    assert_eq!(cell(&t, 0, 4), Cell::EMPTY);
}

// ---- ghostty behaviors without a test of their own ------------------------

// tapust differs from ghostty: tab stops stay in the remaining columns; new columns get the default ones.
#[test]
fn column_changes_keep_the_tab_stops() {
    let mut t = with_history(20, 3);
    feed(&mut t, "\x1b[3g\x1b[1;5H\x1bH\r");
    t.resize(20, 4);
    feed(&mut t, "\t");
    assert_eq!(cursor(&t), (4, 0));
    t.resize(30, 4);
    feed(&mut t, "\r\t");
    assert_eq!(cursor(&t), (4, 0));
    feed(&mut t, "\t");
    assert_eq!(cursor(&t), (24, 0));
}

// OSC 133;A redraw=0: the shell does not redraw its prompt, so it stays.
#[test]
fn a_prompt_the_shell_does_not_redraw_is_kept() {
    let mut t = with_history(10, 3);
    write(&mut t, "\x1b]133;A;redraw=0\x07> \x1b]133;B\x07echo");
    t.resize(20, 3);
    assert_eq!(viewport(&t), "> echo");
}

// PageList's viewport pin: a view scrolled into the history stays on its row.
#[test]
fn a_scrolled_back_view_stays_on_its_row() {
    let mut t = with_history(4, 3);
    write(&mut t, "1\n2\n3\n4\n5\n6\n7\n8");
    t.scroll_view(3);
    assert_eq!(viewport(&t), "3\n4\n5");
    t.resize(2, 2);
    assert_eq!(viewport(&t), "3\n4");
}

// Each screen resizes with its own cursor: the primary screen's stays where
// it was left, here holding the bottom row that would otherwise be trimmed.
#[test]
fn the_inactive_primary_screen_resizes_with_its_own_cursor() {
    let mut t = with_history(10, 5);
    write(&mut t, "top\x1b[5;1H");
    feed(&mut t, "\x1b[?47h\x1b[H");
    t.resize(10, 2);
    feed(&mut t, "\x1b[?47l");
    assert_eq!(t.history_len(), 3);
    assert_eq!(viewport(&t), "");
}

// ---- tapust's own resize rules -------------------------------------------

// tapust differs from ghostty: a selection end does not keep a trailing row without text, and goes with it.
#[test]
fn selection_ends_do_not_keep_trailing_blank_rows() {
    let mut t = with_history(10, 5);
    write(&mut t, "top");
    track(&mut t, 2, 3);
    t.resize(10, 2);
    assert_eq!(t.history_len(), 0);
    assert_eq!(viewport(&t), "top");
    assert_eq!(t.selection().unwrap().end, Point { line: t.first_line(), col: 0 });
}

// tapust differs from ghostty: a saved cursor whose row leaves the screen is clamped to it, not homed.
#[test]
fn a_saved_cursor_whose_row_leaves_the_screen_is_clamped() {
    // Its row moves into the history above the cursor's.
    let mut t = with_history(10, 5);
    write(&mut t, "a\nb\nc\nd\ne\x1b[1;3H\x1b7\x1b[5;2H");
    t.resize(10, 2);
    feed(&mut t, "\x1b8");
    assert_eq!(cursor(&t), (2, 0));
    // Its row is cut below the cursor's.
    let mut t = with_history(10, 5);
    write(&mut t, "a\nb\nc\nd\ne\x1b[5;3H\x1b7\x1b[H");
    t.resize(10, 2);
    assert_eq!(viewport(&t), "a\nb");
    feed(&mut t, "\x1b8");
    assert_eq!(cursor(&t), (2, 1));
}

// tapust differs from ghostty: a selection end on a wide character pushed to the next row moves with it.
#[test]
fn a_selection_end_on_a_pushed_wide_character_moves_with_it() {
    let mut t = with_history(5, 3);
    write(&mut t, "abc\u{4e2d}");
    track(&mut t, 3, 0);
    t.resize(4, 3);
    assert_eq!(cell(&t, 3, 0).width(), Width::SpacerHead);
    assert_eq!(pin_active(&t), Some((0, 1)));
    assert_eq!(pin_cell(&t).char(), Some('\u{4e2d}'));
}

// tapust differs from ghostty: a row joining a prompt row with its continuation keeps the prompt's mark.
#[test]
fn joining_a_prompt_row_with_its_continuation_keeps_the_prompt_mark() {
    let mut t = with_history(10, 3);
    write(&mut t, "\x1b]133;A\x07$ \x1b]133;B\x07echo hello\x1b]133;C\x07\n");
    assert_eq!(t.line_prompt(active_top(&t)), PromptMark::Prompt);
    assert_eq!(t.line_prompt(active_top(&t) + 1), PromptMark::Continuation);
    t.resize(20, 3);
    assert_eq!(viewport(&t), "$ echo hello");
    assert_eq!(t.line_prompt(active_top(&t)), PromptMark::Prompt);
    assert_eq!(t.line_prompt(active_top(&t) + 1), PromptMark::None);
}

// tapust differs from ghostty: a prompt split by a reflow stays one prompt, so the redraw clears all of it.
#[test]
fn a_prompt_split_by_a_reflow_is_cleared_whole() {
    let mut t = with_history(20, 3);
    write(&mut t, "out\n\x1b]133;A\x07$ \x1b]133;B\x07echo abcdefghij");
    t.resize(10, 3);
    assert_eq!(viewport(&t), "out");
    assert_eq!(t.line_prompt(active_top(&t) + 1), PromptMark::Prompt);
    assert_eq!(t.line_prompt(active_top(&t) + 2), PromptMark::Continuation);
}
