use crate::{
    Attrs, Cell, Color, CursorShape, Message, MouseEncoding, MouseMode, Palette, Point, ProgressReport, Rgb, Selection,
    Style, Terminal, Underline, Width,
};

fn term(cols: u16, rows: u16) -> Terminal {
    Terminal::new(cols, rows, 100)
}

fn feed(t: &mut Terminal, s: &str) {
    t.feed(s.as_bytes());
}

fn line(t: &Terminal, y: u16) -> String {
    let mut s = String::new();
    for (x, c) in t.row(y).iter().enumerate() {
        match c.width() {
            Width::SpacerTail => {}
            _ if c.has_grapheme() => s.push_str(t.grapheme(y, x as u16).unwrap()),
            _ => s.push(c.char().unwrap_or(' ')),
        }
    }
    s.trim_end().to_owned()
}

fn screen(t: &Terminal) -> Vec<String> {
    let mut v: Vec<String> = (0..t.rows()).map(|y| line(t, y)).collect();
    while v.last().is_some_and(|l| l.is_empty()) {
        v.pop();
    }
    v
}

fn pos(t: &Terminal) -> (u16, u16) {
    let c = t.cursor();
    (c.x, c.y)
}

fn cell(t: &Terminal, x: u16, y: u16) -> Cell {
    t.row(y)[x as usize]
}

fn style_at(t: &Terminal, x: u16, y: u16) -> Style {
    *t.style(cell(t, x, y).style())
}

fn replies(t: &mut Terminal) -> String {
    String::from_utf8(t.take_replies().unwrap_or_default()).unwrap()
}

/// Checks that every wide cell is followed by its tail and vice versa.
fn assert_wide_pairs(t: &Terminal) {
    for y in 0..t.rows() {
        let row = t.row(y);
        for (x, c) in row.iter().enumerate() {
            match c.width() {
                Width::Wide => assert_eq!(row[x + 1].width(), Width::SpacerTail, "row {y} col {x}"),
                Width::SpacerTail => assert_eq!(row[x - 1].width(), Width::Wide, "row {y} col {x}"),
                _ => {}
            }
        }
    }
}

// ---- Cursor movement ----------------------------------------------------

#[test]
fn cursor_position_and_clamping() {
    let mut t = term(20, 10);
    feed(&mut t, "\x1b[5;10H");
    assert_eq!(pos(&t), (9, 4));
    feed(&mut t, "\x1b[H");
    assert_eq!(pos(&t), (0, 0));
    feed(&mut t, "\x1b[100;200H");
    assert_eq!(pos(&t), (19, 9));
    feed(&mut t, "\x1b[0;0f");
    assert_eq!(pos(&t), (0, 0));
}

#[test]
fn relative_cursor_movement() {
    let mut t = term(20, 10);
    feed(&mut t, "\x1b[5;5H\x1b[2A");
    assert_eq!(pos(&t), (4, 2));
    feed(&mut t, "\x1b[3B");
    assert_eq!(pos(&t), (4, 5));
    feed(&mut t, "\x1b[0C");
    assert_eq!(pos(&t), (5, 5));
    feed(&mut t, "\x1b[2D");
    assert_eq!(pos(&t), (3, 5));
    feed(&mut t, "\x1b[2E");
    assert_eq!(pos(&t), (0, 7));
    feed(&mut t, "\x1b[3F");
    assert_eq!(pos(&t), (0, 4));
    feed(&mut t, "\x1b[7G");
    assert_eq!(pos(&t), (6, 4));
    feed(&mut t, "\x1b[9`");
    assert_eq!(pos(&t), (8, 4));
    feed(&mut t, "\x1b[2a");
    assert_eq!(pos(&t), (10, 4));
    feed(&mut t, "\x1b[2d");
    assert_eq!(pos(&t), (10, 1));
    feed(&mut t, "\x1b[3e");
    assert_eq!(pos(&t), (10, 4));
    feed(&mut t, "\x1b[99A\x1b[99D");
    assert_eq!(pos(&t), (0, 0));
    feed(&mut t, "\x1b[99B\x1b[99C");
    assert_eq!(pos(&t), (19, 9));
}

#[test]
fn vertical_movement_stops_at_margins() {
    let mut t = term(10, 10);
    feed(&mut t, "\x1b[3;6r\x1b[5;1H\x1b[9A");
    assert_eq!(pos(&t), (0, 2));
    feed(&mut t, "\x1b[9B");
    assert_eq!(pos(&t), (0, 5));
    // Outside the region the screen edges apply.
    feed(&mut t, "\x1b[8;1H\x1b[9B");
    assert_eq!(pos(&t), (0, 9));
    feed(&mut t, "\x1b[2;1H\x1b[9A");
    assert_eq!(pos(&t), (0, 0));
}

#[test]
fn origin_mode() {
    let mut t = term(10, 10);
    feed(&mut t, "\x1b[3;6r\x1b[?6h");
    assert_eq!(pos(&t), (0, 2));
    feed(&mut t, "\x1b[2;3H");
    assert_eq!(pos(&t), (2, 3));
    feed(&mut t, "\x1b[99;1H");
    assert_eq!(pos(&t), (0, 5));
    feed(&mut t, "\x1b[6n");
    assert_eq!(replies(&mut t), "\x1b[4;1R");
    feed(&mut t, "\x1b[?6l");
    assert_eq!(pos(&t), (0, 0));
}

#[test]
fn backspace_and_tabs() {
    let mut t = term(30, 3);
    feed(&mut t, "\x08ab\x08\x08c");
    assert_eq!(line(&t, 0), "cb");
    feed(&mut t, "\r\tx\ty");
    assert_eq!(line(&t, 0), "cb      x       y");
    feed(&mut t, "\r\n\x1b[3G\x1bH\r\tz\x1b[3G\x1b[g\r\tw");
    assert_eq!(line(&t, 1), "  z     w");
    feed(&mut t, "\r\x1b[3g\tq");
    assert_eq!(pos(&t), (29, 1));
    feed(&mut t, "\x1b[H\x1b[2I");
    assert_eq!(pos(&t), (29, 0));
    let mut t = term(30, 3);
    feed(&mut t, "\x1b[20G\x1b[2Z");
    assert_eq!(pos(&t), (8, 0));
    feed(&mut t, "\x1b[5Z");
    assert_eq!(pos(&t), (0, 0));
}

// ---- Erasing ------------------------------------------------------------

#[test]
fn erase_in_line_and_display() {
    let fill = |t: &mut Terminal| feed(t, "\x1b[H0123456789\r\nabcdefghij\r\nABCDEFGHIJ\x1b[2;5H");
    let mut t = term(10, 3);
    fill(&mut t);
    feed(&mut t, "\x1b[K");
    assert_eq!(screen(&t), ["0123456789", "abcd", "ABCDEFGHIJ"]);
    fill(&mut t);
    feed(&mut t, "\x1b[1K");
    assert_eq!(screen(&t), ["0123456789", "     fghij", "ABCDEFGHIJ"]);
    fill(&mut t);
    feed(&mut t, "\x1b[2K");
    assert_eq!(screen(&t), ["0123456789", "", "ABCDEFGHIJ"]);
    fill(&mut t);
    feed(&mut t, "\x1b[J");
    assert_eq!(screen(&t), ["0123456789", "abcd"]);
    fill(&mut t);
    feed(&mut t, "\x1b[1J");
    assert_eq!(screen(&t), ["", "     fghij", "ABCDEFGHIJ"]);
    fill(&mut t);
    feed(&mut t, "\x1b[2J");
    assert!(screen(&t).is_empty());
    assert_eq!(pos(&t), (4, 1));
    fill(&mut t);
    feed(&mut t, "\x1b[3X");
    assert_eq!(line(&t, 1), "abcd   hij");
    feed(&mut t, "\x1b[99X");
    assert_eq!(line(&t, 1), "abcd");
}

#[test]
fn erase_uses_background_color() {
    let mut t = term(10, 3);
    feed(&mut t, "abc\x1b[41m\x1b[1;2H\x1b[K");
    assert_eq!(line(&t, 0), "a");
    for x in 1..10 {
        let c = cell(&t, x, 0);
        assert_eq!(c.char(), None);
        assert_eq!(t.style(c.style()).bg, Color::Indexed(1));
    }
    assert_eq!(style_at(&t, 0, 0), Style::DEFAULT);
    // Scrolled-in lines take the background too.
    feed(&mut t, "\x1b[3;1H\n");
    assert_eq!(t.style(cell(&t, 5, 2).style()).bg, Color::Indexed(1));
}

#[test]
fn erase_resets_pending_wrap() {
    let mut t = term(5, 3);
    feed(&mut t, "abcde\x1b[Kx");
    assert_eq!(screen(&t), ["abcdx"]);
}

#[test]
fn erase_scrollback() {
    let mut t = term(5, 2);
    feed(&mut t, "1\r\n2\r\n3\r\n4");
    assert_eq!(t.history_len(), 2);
    feed(&mut t, "\x1b[3J");
    assert_eq!(t.history_len(), 0);
    assert_eq!(screen(&t), ["3", "4"]);
}

// ---- Inserting and deleting ---------------------------------------------

#[test]
fn insert_and_delete_characters() {
    let mut t = term(10, 2);
    feed(&mut t, "0123456789\x1b[1;3H\x1b[2@");
    assert_eq!(line(&t, 0), "01  234567");
    feed(&mut t, "\x1b[3P");
    assert_eq!(line(&t, 0), "0134567");
    feed(&mut t, "\x1b[99P");
    assert_eq!(line(&t, 0), "01");
    feed(&mut t, "\x1b[4h\x1b[1;1Hxy\x1b[4l");
    assert_eq!(line(&t, 0), "xy01");
    assert_eq!(pos(&t), (2, 0));
}

#[test]
fn insert_and_delete_lines() {
    let mut t = term(5, 5);
    feed(&mut t, "a\r\nb\r\nc\r\nd\r\ne\x1b[2;3H\x1b[L");
    assert_eq!(screen(&t), ["a", "", "b", "c", "d"]);
    assert_eq!(pos(&t), (0, 1));
    feed(&mut t, "\x1b[2M");
    assert_eq!(screen(&t), ["a", "c", "d"]);
    // Only inside the scroll region.
    feed(&mut t, "\x1b[1;3r\x1b[2;1H\x1b[5L");
    assert_eq!(screen(&t), ["a"]);
    feed(&mut t, "\x1b[5;1H\x1b[L");
    assert_eq!(screen(&t), ["a"]);
    assert_eq!(t.history_len(), 0);
}

#[test]
fn edits_keep_graphemes_on_their_cells() {
    let mut t = term(10, 2);
    feed(&mut t, "ae\u{301}b\x1b[1;1H\x1b[2@");
    assert_eq!(line(&t, 0), "  ae\u{301}b");
    feed(&mut t, "\x1b[3P");
    assert_eq!(line(&t, 0), "e\u{301}b");
    feed(&mut t, "\x1b[P");
    assert_eq!(line(&t, 0), "b");
    assert!(!cell(&t, 0, 0).has_grapheme());
}

// ---- Scrolling and scrollback ---------------------------------------------

#[test]
fn scroll_region_linefeed_and_reverse_index() {
    let mut t = term(5, 5);
    feed(&mut t, "a\r\nb\r\nc\r\nd\r\ne\x1b[2;4r\x1b[4;1H\nx");
    assert_eq!(screen(&t), ["a", "c", "d", "x", "e"]);
    assert_eq!(t.history_len(), 0);
    feed(&mut t, "\x1b[2;1H\x1bMy");
    assert_eq!(screen(&t), ["a", "y", "c", "d", "e"]);
    feed(&mut t, "\x1b[2S");
    assert_eq!(screen(&t), ["a", "d", "", "", "e"]);
    feed(&mut t, "\x1b[1T");
    assert_eq!(screen(&t), ["a", "", "d", "", "e"]);
    // IND and NEL.
    feed(&mut t, "\x1b[r\x1b[5;3H\x1bDz\x1bEw");
    assert_eq!(screen(&t), ["d", "", "e", "  z", "w"]);
    assert_eq!(t.history_len(), 2);
}

#[test]
fn region_scrolls_both_ways_of_rotating() {
    // Small and large regions take different paths; compare with a model.
    for (top, bottom) in [(1, 2), (1, 9), (2, 10), (3, 7), (1, 10), (5, 6)] {
        let mut t = term(4, 10);
        let mut model: Vec<String> = (0..10).map(|i| i.to_string()).collect();
        for (y, m) in model.iter().enumerate() {
            feed(&mut t, &format!("\x1b[{};1H{m}", y + 1));
        }
        feed(&mut t, &format!("\x1b[{top};{bottom}r"));
        let (a, b) = (top - 1, bottom - 1);
        for (n, up) in [(1, true), (2, false), (3, true), (20, false), (1, false), (2, true)] {
            feed(&mut t, &format!("\x1b[{n}{}", if up { 'S' } else { 'T' }));
            let n = n.min(b - a + 1);
            let mut region: Vec<String> = model[a..=b].to_vec();
            if up {
                region.drain(..n);
                region.extend(std::iter::repeat_n(String::new(), n));
            } else {
                region.truncate(region.len() - n);
                for _ in 0..n {
                    region.insert(0, String::new());
                }
            }
            model.splice(a..=b, region);
            let got: Vec<String> = (0..10).map(|y| line(&t, y)).collect();
            assert_eq!(got, model, "region {top};{bottom} after {n} {up}");
        }
    }
}

#[test]
fn linefeed_at_the_bottom_margin() {
    // Each region shape takes a different path (ring rotation with fixed rows
    // moved back, memmove of the region); full-screen scrolls in between
    // rotate the ring so regions also wrap around its end.
    let rows = 10;
    for (top, bottom) in [(1, 10), (1, 9), (1, 8), (1, 7), (2, 10), (3, 10), (4, 10), (3, 7)] {
        let mut t = term(6, rows as u16);
        feed(&mut t, "\x1b[?1049h");
        let mut model: Vec<String> = (0..rows).map(|i| format!("r{i}")).collect();
        for (y, m) in model.iter().enumerate() {
            feed(&mut t, &format!("\x1b[{};1H{m}", y + 1));
        }
        for round in 0..4 {
            feed(&mut t, &format!("\x1b[{top};{bottom}r\x1b[{bottom};1H"));
            for k in 0..3 {
                let text = format!("{round}.{k}");
                feed(&mut t, &format!("\r\n{text}"));
                model.remove(top - 1);
                model.insert(bottom - 1, text);
            }
            feed(&mut t, &format!("\x1b[r\x1b[{rows};1H\r\nf{round}"));
            model.remove(0);
            model.push(format!("f{round}"));
            let got: Vec<String> = (0..rows as u16).map(|y| line(&t, y)).collect();
            assert_eq!(got, model, "region {top};{bottom} round {round}");
        }
    }
}

#[test]
fn scrollback_and_view() {
    let mut t = Terminal::new(10, 3, 5);
    for i in 0..10 {
        feed(&mut t, &format!("{i}\r\n"));
    }
    assert_eq!(t.history_len(), 5);
    assert_eq!(screen(&t), ["8", "9"]);
    t.scroll_view(2);
    assert_eq!(t.view_offset(), 2);
    assert_eq!(screen(&t), ["6", "7", "8"]);
    assert!(!t.cursor().visible);
    // New output does not move a scrolled-back view.
    feed(&mut t, "x\r\n");
    assert_eq!(t.view_offset(), 3);
    assert_eq!(screen(&t), ["6", "7", "8"]);
    t.scroll_view(100);
    assert_eq!(t.view_offset(), 5);
    assert_eq!(screen(&t), ["4", "5", "6"]);
    t.scroll_view(-100);
    assert_eq!(t.view_offset(), 0);
    assert_eq!(screen(&t), ["9", "x"]);
    assert!(t.cursor().visible);
}

#[test]
fn region_at_the_top_scrolls_into_history() {
    // A status line below the region stays; lines leaving the top are saved.
    for (rows, bottom) in [(5u16, 4), (12, 3)] {
        let mut t = Terminal::new(10, rows, 100);
        feed(&mut t, &format!("\x1b[{rows};1Hstatus\x1b[1;{bottom}r\x1b[1;1H"));
        for i in 0..6 {
            feed(&mut t, &format!("l{i}\r\n"));
        }
        feed(&mut t, "\x1b[2S");
        assert_eq!(t.history_len(), 6 - bottom + 1 + 2, "rows {rows}");
        assert_eq!(line(&t, rows - 1), "status");
        t.scroll_view(t.history_len() as isize);
        assert_eq!(line(&t, 0), "l0");
        // A region away from the top does not save.
        let mut t = Terminal::new(10, rows, 100);
        feed(&mut t, &format!("\x1b[2;{bottom}r\x1b[{bottom};1H\n\n\x1b[S"));
        assert_eq!(t.history_len(), 0);
    }
}

#[test]
fn no_scrollback_on_alternate_screen() {
    let mut t = Terminal::new(10, 3, 5);
    feed(&mut t, "\x1b[?1049h1\r\n2\r\n3\r\n4");
    assert_eq!(t.history_len(), 0);
    assert_eq!(screen(&t), ["2", "3", "4"]);
    t.scroll_view(3);
    assert_eq!(t.view_offset(), 0);
}

#[test]
fn zero_scrollback() {
    let mut t = Terminal::new(10, 2, 0);
    feed(&mut t, "1\r\n2\r\n3");
    assert_eq!(t.history_len(), 0);
    assert_eq!(screen(&t), ["2", "3"]);
}

// ---- Wrapping -------------------------------------------------------------

#[test]
fn autowrap_and_pending_wrap() {
    let mut t = term(5, 3);
    feed(&mut t, "abcde");
    assert_eq!(pos(&t), (4, 0));
    feed(&mut t, "f");
    assert_eq!(screen(&t), ["abcde", "f"]);
    feed(&mut t, "\r\n12345\r\n");
    assert_eq!(screen(&t), ["f", "12345"]);
    assert_eq!(pos(&t), (0, 2));
    // LF while a wrap is pending moves one line only (xterm).
    let mut t = term(5, 3);
    feed(&mut t, "abcde\nx");
    assert_eq!(screen(&t), ["abcde", "    x"]);
    // CR clears the pending wrap.
    let mut t = term(5, 3);
    feed(&mut t, "abcde\rx");
    assert_eq!(screen(&t), ["xbcde"]);
}

#[test]
fn autowrap_off() {
    let mut t = term(5, 2);
    feed(&mut t, "\x1b[?7habc\x1b[?7ldefgh");
    assert_eq!(screen(&t), ["abcdh"]);
    assert_eq!(pos(&t), (4, 0));
    feed(&mut t, "\x1b[?7h");
    feed(&mut t, "\x1b[1;5Hxy");
    assert_eq!(screen(&t), ["abcdx", "y"]);
}

#[test]
fn wide_characters_at_the_margin() {
    let mut t = term(5, 4);
    feed(&mut t, "abcd\u{4e2d}");
    assert_eq!(cell(&t, 4, 0).width(), Width::SpacerHead);
    assert_eq!(cell(&t, 0, 1).width(), Width::Wide);
    assert_eq!(cell(&t, 1, 1).width(), Width::SpacerTail);
    assert_eq!(screen(&t), ["abcd", "\u{4e2d}"]);
    assert_eq!(pos(&t), (2, 1));
    feed(&mut t, "\r\nab\u{4e2d}\u{6587}");
    assert_eq!(line(&t, 2), "ab\u{4e2d}");
    assert_eq!(cell(&t, 4, 2).width(), Width::SpacerHead);
    assert_eq!(line(&t, 3), "\u{6587}");
    assert_eq!(pos(&t), (2, 3));
    assert_wide_pairs(&t);
}

#[test]
fn overwriting_half_of_a_wide_character() {
    let mut t = term(6, 2);
    feed(&mut t, "\u{4e2d}\u{6587}\x1b[1;2Hx");
    assert_eq!(line(&t, 0), " x\u{6587}");
    feed(&mut t, "\x1b[1;3Hy");
    assert_eq!(line(&t, 0), " xy");
    assert_wide_pairs(&t);
    feed(&mut t, "\r\n\u{4e2d}\u{6587}\x1b[2;2H\x1b[K");
    assert_eq!(line(&t, 1), "");
    feed(&mut t, "\r\u{4e2d}\u{6587}\x1b[2;3H\x1b[1K");
    assert_eq!(line(&t, 1), "");
    feed(&mut t, "\r\u{4e2d}\u{6587}\x1b[2;2H\x1b[@");
    assert_eq!(line(&t, 1), "   \u{6587}");
    assert_wide_pairs(&t);
}

// ---- Combining characters -------------------------------------------------

#[test]
fn combining_marks() {
    let mut t = term(10, 2);
    feed(&mut t, "e\u{301}x");
    assert!(cell(&t, 0, 0).has_grapheme());
    assert_eq!(t.grapheme(0, 0), Some("e\u{301}"));
    assert_eq!(t.grapheme(0, 1), None);
    assert_eq!(pos(&t), (2, 0));
    // On a wide character, and on a cell with a pending wrap.
    feed(&mut t, "\u{4e2d}\u{302}\x1b[1;10Hz\u{303}\u{304}");
    assert_eq!(t.grapheme(0, 2), Some("\u{4e2d}\u{302}"));
    assert_eq!(t.grapheme(0, 9), Some("z\u{303}\u{304}"));
    // Overwriting the base drops the old marks.
    feed(&mut t, "\x1b[1;1Hq\u{300}");
    assert_eq!(t.grapheme(0, 0), Some("q\u{300}"));
    feed(&mut t, "\x1b[1;1Hr");
    assert_eq!(t.grapheme(0, 0), None);
    // Nothing to attach to at the start of a row.
    feed(&mut t, "\r\n\u{301}a");
    assert_eq!(line(&t, 1), "a");
}

#[test]
fn graphemes_survive_reuse_and_compaction() {
    let mut t = term(8, 3);
    // Overwrite the same cells many times: entries are reused or compacted.
    for i in 0..500u32 {
        if i % 7 == 0 {
            feed(&mut t, "\ryyy");
        }
        let b = char::from_u32('a' as u32 + i % 26).unwrap();
        feed(&mut t, &format!("\r{b}\u{301}x{b}\u{302}\u{303}"));
        assert_eq!(line(&t, 0), format!("{b}\u{301}x{b}\u{302}\u{303}"), "round {i}");
    }
    // Recycled rows start without graphemes.
    feed(&mut t, "\r\n\n\n\n\r\nq");
    assert_eq!(t.grapheme(2, 0), None);
    assert_eq!(line(&t, 2), "q");
}

#[test]
fn zwj_and_variation_selectors() {
    let mut t = term(10, 2);
    feed(&mut t, "\u{1f468}\u{200d}\u{1f469}");
    assert_eq!(t.grapheme(0, 0), Some("\u{1f468}\u{200d}"));
    assert_eq!(cell(&t, 2, 0).char(), Some('\u{1f469}'));
    // VS16 widens a narrow emoji-capable character.
    feed(&mut t, "\r\n\u{2764}\u{fe0f}x");
    assert_eq!(cell(&t, 0, 1).width(), Width::Wide);
    assert_eq!(cell(&t, 1, 1).width(), Width::SpacerTail);
    assert_eq!(t.grapheme(1, 0), Some("\u{2764}\u{fe0f}"));
    assert_eq!(cell(&t, 2, 1).char(), Some('x'));
    // No room at the margin: stays narrow.
    feed(&mut t, "\x1b[2;10H\u{2764}\u{fe0f}");
    assert_eq!(cell(&t, 9, 1).width(), Width::Narrow);
    assert_wide_pairs(&t);
}

// ---- SGR ------------------------------------------------------------------

#[test]
fn sgr_attributes() {
    let mut t = term(40, 2);
    feed(&mut t, "\x1b[1;2;3;5;7;8;9;53ma\x1b[22;23;25;27;28;29;55mb\x1b[4mc\x1b[21md\x1b[4:3me\x1b[4:0mf\x1b[0mg");
    let a = style_at(&t, 0, 0).attrs;
    for f in [
        Attrs::BOLD,
        Attrs::DIM,
        Attrs::ITALIC,
        Attrs::BLINK,
        Attrs::INVERSE,
        Attrs::HIDDEN,
        Attrs::STRIKE,
        Attrs::OVERLINE,
    ] {
        assert!(a.contains(f), "{f:#x}");
    }
    assert_eq!(style_at(&t, 1, 0), Style::DEFAULT);
    assert_eq!(style_at(&t, 2, 0).underline, Underline::Single);
    assert_eq!(style_at(&t, 3, 0).underline, Underline::Double);
    assert_eq!(style_at(&t, 4, 0).underline, Underline::Curly);
    assert_eq!(style_at(&t, 5, 0).underline, Underline::None);
    assert_eq!(cell(&t, 6, 0).style().0, 0);
    feed(&mut t, "\x1b[4:4mh\x1b[4:5mi\x1b[24mj");
    assert_eq!(style_at(&t, 7, 0).underline, Underline::Dotted);
    assert_eq!(style_at(&t, 8, 0).underline, Underline::Dashed);
    assert_eq!(style_at(&t, 9, 0).underline, Underline::None);
}

#[test]
fn sgr_colors() {
    let mut t = term(40, 2);
    let cases: &[(&str, Color, Color, Color)] = &[
        ("\x1b[31;42m", Color::Indexed(1), Color::Indexed(2), Color::Default),
        ("\x1b[91;102m", Color::Indexed(9), Color::Indexed(10), Color::Default),
        ("\x1b[38;5;200;48;5;17m", Color::Indexed(200), Color::Indexed(17), Color::Default),
        ("\x1b[38;2;1;2;3;48;2;4;5;6m", Color::Rgb(1, 2, 3), Color::Rgb(4, 5, 6), Color::Default),
        ("\x1b[38:2::1:2:3;48:2::4:5:6m", Color::Rgb(1, 2, 3), Color::Rgb(4, 5, 6), Color::Default),
        ("\x1b[38:2:7:8:9m", Color::Rgb(7, 8, 9), Color::Default, Color::Default),
        ("\x1b[38:5:33;48:5:44m", Color::Indexed(33), Color::Indexed(44), Color::Default),
        ("\x1b[58:2::10:20:30m", Color::Default, Color::Default, Color::Rgb(10, 20, 30)),
        ("\x1b[58;5;99m", Color::Default, Color::Default, Color::Indexed(99)),
        ("\x1b[31;58;5;9;39;59m", Color::Default, Color::Default, Color::Default),
        ("\x1b[41;49m", Color::Default, Color::Default, Color::Default),
    ];
    for (i, (seq, fg, bg, ul)) in cases.iter().enumerate() {
        feed(&mut t, &format!("\x1b[0m{seq}x"));
        let s = style_at(&t, i as u16, 0);
        assert_eq!((s.fg, s.bg, s.underline_color), (*fg, *bg, *ul), "{seq:?}");
    }
    // Attributes after an extended color still apply.
    feed(&mut t, "\x1b[0m\x1b[38;5;1;1;48;2;1;2;3;3mz");
    let s = style_at(&t, cases.len() as u16, 0);
    assert_eq!(s.fg, Color::Indexed(1));
    assert_eq!(s.bg, Color::Rgb(1, 2, 3));
    assert!(s.attrs.contains(Attrs::BOLD) && s.attrs.contains(Attrs::ITALIC));
}

#[test]
fn packed_pen_round_trips() {
    use crate::style::Pen;
    let colors = [Color::Default, Color::Indexed(0), Color::Indexed(255), Color::Rgb(0, 0, 0), Color::Rgb(255, 128, 1)];
    let lines =
        [Underline::None, Underline::Single, Underline::Double, Underline::Curly, Underline::Dotted, Underline::Dashed];
    for (i, &fg) in colors.iter().enumerate() {
        for &bg in &colors {
            for (j, &underline) in lines.iter().enumerate() {
                let s = Style {
                    fg,
                    bg,
                    underline_color: colors[(i + j) % colors.len()],
                    underline,
                    attrs: Attrs(0xA5 << j),
                };
                let pen = Pen::from_style(&s);
                assert_eq!(pen.style(), s);
                assert_eq!(pen == Pen::DEFAULT, s == Style::DEFAULT);
                assert_eq!(pen.background().style(), Style { bg, ..Style::DEFAULT });
            }
        }
    }
}

#[test]
fn private_m_is_not_sgr() {
    let mut t = term(10, 2);
    feed(&mut t, "\x1b[>4;1m\x1b[?1mx");
    assert_eq!(style_at(&t, 0, 0), Style::DEFAULT);
}

#[test]
fn style_ids_are_shared_and_compacted() {
    let mut t = term(10, 2);
    feed(&mut t, "\x1b[31ma\x1b[32mb\x1b[31mc");
    assert_eq!(cell(&t, 0, 0).style(), cell(&t, 2, 0).style());
    assert_ne!(cell(&t, 0, 0).style(), cell(&t, 1, 0).style());
    // More distinct styles than ids: the table compacts and live cells keep their styles.
    let mut s = String::from("\r");
    for i in 0..70_000u32 {
        s.push_str(&format!(
            "\x1b[38;2;{};{};{}m{}",
            i >> 16,
            (i >> 8) & 255,
            i & 255,
            (b'a' + (i % 26) as u8) as char
        ));
        if i % 10 == 9 {
            s.push('\r');
        }
    }
    feed(&mut t, &s);
    for x in 0..10u32 {
        let i = 70_000 - 10 + x;
        assert_eq!(style_at(&t, x as u16, 0).fg, Color::Rgb((i >> 16) as u8, (i >> 8) as u8, i as u8));
    }
    assert_eq!(style_at(&t, 0, 1), Style::DEFAULT);
}

// ---- Charsets, REP, DECALN, save/restore, resets --------------------------

#[test]
fn dec_special_graphics() {
    let mut t = term(20, 2);
    feed(&mut t, "\x1b(0lqk\x1b(Bq");
    assert_eq!(line(&t, 0), "\u{250c}\u{2500}\u{2510}q");
    feed(&mut t, "\r\n\x1b)0a\x0eq\x0fq");
    assert_eq!(line(&t, 1), "a\u{2500}q");
    // A single shift applies to one character.
    feed(&mut t, "\x1b*0\x1bNqq");
    assert_eq!(line(&t, 1), "a\u{2500}q\u{2500}q");
}

#[test]
fn repeat_previous_character() {
    let mut t = term(20, 2);
    feed(&mut t, "ab\x1b[3b\u{4e2d}\x1b[2b");
    assert_eq!(line(&t, 0), "abbbb\u{4e2d}\u{4e2d}\u{4e2d}");
    feed(&mut t, "\r\n\x1b(0q\x1b[2b");
    assert_eq!(line(&t, 1), "\u{2500}\u{2500}\u{2500}");
}

#[test]
fn screen_alignment() {
    let mut t = term(3, 2);
    feed(&mut t, "\x1b[2;2r\x1b#8");
    assert_eq!(screen(&t), ["EEE", "EEE"]);
    assert_eq!(pos(&t), (0, 0));
}

#[test]
fn save_and_restore_cursor() {
    let mut t = term(10, 5);
    feed(&mut t, "\x1b[2;3H\x1b[31m\x1b(0\x1b7\x1b[H\x1b[0m\x1b(Bq\x1b8q");
    assert_eq!(pos(&t), (3, 1));
    assert_eq!(cell(&t, 2, 1).char(), Some('\u{2500}'));
    assert_eq!(style_at(&t, 2, 1).fg, Color::Indexed(1));
    assert_eq!(cell(&t, 0, 0).char(), Some('q'));
    feed(&mut t, "\x1b[4;5H\x1b[s\x1b[H\x1b[u");
    assert_eq!(pos(&t), (4, 3));
    // Without a saved state DECRC homes the cursor.
    let mut t = term(10, 5);
    feed(&mut t, "\x1b[3;3H\x1b8");
    assert_eq!(pos(&t), (0, 0));
}

#[test]
fn soft_and_full_reset() {
    let mut t = term(10, 5);
    feed(&mut t, "abc\x1b[2;4r\x1b[?6h\x1b[4h\x1b[?25l\x1b[1m\x1b[!p");
    assert!(t.cursor().visible);
    feed(&mut t, "\x1b[5;1Hx");
    assert_eq!(pos(&t), (1, 4));
    assert_eq!(style_at(&t, 0, 4), Style::DEFAULT);
    assert_eq!(line(&t, 0), "abc");
    feed(&mut t, "\x1b]2;title\x07\x1b]4;1;#ffffff\x07\x1b[?1049h\x1bc");
    assert!(screen(&t).is_empty());
    assert_eq!(t.title(), "");
    assert!(!t.modes().alt_screen);
    assert_eq!(*t.palette(), Palette::default());
    assert_eq!(pos(&t), (0, 0));
}

// ---- Alternate screen -------------------------------------------------------

#[test]
fn alternate_screen_1049() {
    let mut t = term(10, 4);
    feed(&mut t, "main\x1b[2;3H\x1b[?1049h");
    assert!(t.modes().alt_screen);
    assert!(screen(&t).is_empty());
    assert_eq!(pos(&t), (2, 1));
    feed(&mut t, "alt\x1b[H\x1b[?1049l");
    assert!(!t.modes().alt_screen);
    assert_eq!(screen(&t), ["main"]);
    assert_eq!(pos(&t), (2, 1));
    // The alternate screen is cleared on entry.
    feed(&mut t, "\x1b[?1049h");
    assert!(screen(&t).is_empty());
}

#[test]
fn alternate_screen_47_and_1047() {
    let mut t = term(10, 4);
    feed(&mut t, "main\x1b[?47halt\x1b[?47l");
    assert_eq!(screen(&t), ["main"]);
    feed(&mut t, "\x1b[?47h");
    assert_eq!(screen(&t), ["    alt"]);
    feed(&mut t, "\x1b[?1047l\x1b[?1047h");
    assert!(screen(&t).is_empty());
    feed(&mut t, "\x1b[?1047l\x1b[H\x1b[?1048h\x1b[3;3H\x1b[?1048l");
    assert_eq!(pos(&t), (0, 0));
}

// ---- Resize and reflow -----------------------------------------------------

#[test]
fn reflow_joins_and_splits_lines() {
    let mut t = term(10, 5);
    feed(&mut t, "0123456789abcdefghij\r\nshort\r\n$ ");
    assert_eq!(screen(&t), ["0123456789", "abcdefghij", "short", "$"]);
    t.resize(20, 5);
    assert_eq!(screen(&t), ["0123456789abcdefghij", "short", "$"]);
    assert_eq!(pos(&t), (2, 2));
    t.resize(7, 4);
    assert_eq!(screen(&t), ["789abcd", "efghij", "short", "$"]);
    assert_eq!(t.history_len(), 1);
    assert_eq!(pos(&t), (2, 3));
    t.resize(10, 5);
    assert_eq!(screen(&t), ["0123456789", "abcdefghij", "short", "$"]);
    assert_eq!(t.history_len(), 0);
    assert_eq!(pos(&t), (2, 3));
    assert!(t.dirty().any());
}

#[test]
fn reflow_keeps_wide_characters_whole() {
    let mut t = term(6, 3);
    feed(&mut t, "ab\u{4e2d}\u{6587}\u{5b57}");
    assert_eq!(screen(&t), ["ab\u{4e2d}\u{6587}", "\u{5b57}"]);
    t.resize(5, 3);
    assert_eq!(screen(&t), ["ab\u{4e2d}", "\u{6587}\u{5b57}"]);
    assert_eq!(cell(&t, 4, 0).width(), Width::SpacerHead);
    t.resize(3, 4);
    assert_eq!(screen(&t), ["ab", "\u{4e2d}", "\u{6587}", "\u{5b57}"]);
    t.resize(20, 3);
    assert_eq!(screen(&t), ["ab\u{4e2d}\u{6587}\u{5b57}"]);
    assert_eq!(pos(&t), (8, 0));
    assert_wide_pairs(&t);
}

#[test]
fn reflow_moves_rows_between_screen_and_history() {
    let mut t = Terminal::new(10, 4, 100);
    feed(&mut t, "1\r\n2\r\n3\r\n4\r\n5\r\n6");
    assert_eq!(t.history_len(), 2);
    t.resize(10, 2);
    assert_eq!(screen(&t), ["5", "6"]);
    assert_eq!(t.history_len(), 4);
    t.resize(10, 6);
    assert_eq!(screen(&t), ["1", "2", "3", "4", "5", "6"]);
    assert_eq!(t.history_len(), 0);
    assert_eq!(pos(&t), (1, 5));
    // Blank rows below the cursor are dropped before content.
    let mut t = term(10, 5);
    feed(&mut t, "top\x1b[2;1Hcur");
    t.resize(10, 2);
    assert_eq!(screen(&t), ["top", "cur"]);
    assert_eq!(pos(&t), (3, 1));
}

#[test]
fn reflow_keeps_pending_wrap_and_graphemes() {
    let mut t = term(5, 3);
    feed(&mut t, "abcde\u{301}");
    t.resize(8, 3);
    assert_eq!(screen(&t), ["abcde\u{301}"]);
    assert_eq!(pos(&t), (5, 0));
    feed(&mut t, "f");
    assert_eq!(screen(&t), ["abcde\u{301}f"]);
}

#[test]
fn resize_alternate_screen_truncates() {
    let mut t = term(10, 4);
    feed(&mut t, "main line that wraps\x1b[?1049h\x1b[H0123456789\x1b[4;1Hlast");
    t.resize(5, 2);
    assert_eq!(screen(&t), ["", "last"]);
    assert_eq!(pos(&t), (4, 1));
    feed(&mut t, "\x1b[?1049l");
    assert_eq!(screen(&t), ["that", "wraps"]);
    t.resize(10, 4);
    assert_eq!(screen(&t), ["main line", "that wraps"]);
}

// ---- Chunk boundaries and UTF-8 ---------------------------------------------

#[test]
fn split_feeds_match_one_feed() {
    let input = "\x1b[1;31mred\x1b[0m \u{65e5}\u{672c}\u{8a9e} e\u{301} \u{1f600}\r\n\x1b]0;t\u{e9}tle\x1b\\\
                 \x1b[38:2::1:2:3mrgb\x1b[m\x1bP1$qm\x1b\\\x1b[?2004h\x1b[5;10Hx\x1b(0q\x1b(B\x1b[2;4r\x1bM";
    let mut whole = term(30, 6);
    whole.feed(input.as_bytes());
    for step in 1..=7 {
        let mut t = term(30, 6);
        for chunk in input.as_bytes().chunks(step) {
            t.feed(chunk);
        }
        assert_eq!(screen(&t), screen(&whole), "chunks of {step}");
        assert_eq!(pos(&t), pos(&whole));
        assert_eq!(t.title(), "t\u{e9}tle");
        assert_eq!(t.modes(), whole.modes());
        for x in 0..30 {
            assert_eq!(t.row(0)[x], whole.row(0)[x]);
        }
    }
}

#[test]
fn invalid_utf8_becomes_replacement() {
    let mut t = term(20, 2);
    t.feed(b"a\xffb\xe6\x97c\xf0\x9f\x98");
    t.feed(b"\x1b[31md\xc3");
    t.feed(b"\xa9\xed\xa0\x80");
    assert_eq!(line(&t, 0), "a\u{fffd}b\u{fffd}c\u{fffd}d\u{e9}\u{fffd}\u{fffd}\u{fffd}");
    // C1 controls encoded in UTF-8 are not printed.
    let mut t = term(20, 2);
    t.feed("x\u{85}y".as_bytes());
    assert_eq!(line(&t, 0), "xy");
}

#[test]
fn escape_sequences_split_everywhere() {
    let seqs: [&[u8]; 6] =
        [b"\x1b[38;2;10;20;30m", b"\x1b[?1049h", b"\x1b]10;?\x07", b"\x1b[>c", b"\x1b(0", b"\x1bP+q544e\x1b\\"];
    for seq in seqs {
        for split in 1..seq.len() {
            let mut a = term(10, 2);
            a.feed(seq);
            a.feed(b"q");
            let mut b = term(10, 2);
            b.feed(&seq[..split]);
            b.feed(&seq[split..]);
            b.feed(b"q");
            assert_eq!(a.row(0), b.row(0), "{seq:?} split at {split}");
            assert_eq!(a.modes(), b.modes());
            assert_eq!(a.take_replies(), b.take_replies());
        }
    }
}

#[test]
fn strings_are_consumed_safely() {
    let mut t = term(20, 2);
    feed(&mut t, "a\x1bP1;2|ignored [31m data\x1b\\b\x1b_apc\x1b\\c\x1b^pm\x1b\\d\x1bXsos\x1b\\e");
    assert_eq!(line(&t, 0), "abcde");
    // CAN aborts a sequence.
    feed(&mut t, "\x1b[3\x18f\x1b]2;x\x1ag");
    assert_eq!(line(&t, 0), "abcdefg");
    assert_eq!(t.title(), "");
    // Controls inside CSI are executed.
    feed(&mut t, "\x1b[2\r;1Hh");
    assert_eq!(pos(&t), (1, 1));
    // An overlong OSC is dropped.
    t.feed(format!("\x1b]2;{}\x07", "x".repeat(10_000)).as_bytes());
    assert_eq!(t.title(), "");
}

// ---- Replies and OSC --------------------------------------------------------

#[test]
fn device_replies() {
    let mut t = term(80, 24);
    feed(&mut t, "\x1b[c\x1b[0c\x1b[>c\x1b[5n\x1b[3;7H\x1b[6n\x1b[18t\x1b[19t\x1b[14t");
    assert_eq!(replies(&mut t), "\x1b[?62;22c\x1b[?62;22c\x1b[>1;10;0c\x1b[0n\x1b[3;7R\x1b[8;24;80t\x1b[9;24;80t");
    assert_eq!(t.take_replies(), None);
}

#[test]
fn mode_reports() {
    let mut t = term(10, 2);
    feed(&mut t, "\x1b[?2026h\x1b[?2026$p\x1b[?2004$p\x1b[?7$p\x1b[?9999$p\x1b[4$p\x1b[4h\x1b[4$p\x1b[99$p");
    assert_eq!(replies(&mut t), "\x1b[?2026;1$y\x1b[?2004;2$y\x1b[?7;1$y\x1b[?9999;0$y\x1b[4;2$y\x1b[4;1$y\x1b[99;0$y");
}

#[test]
fn osc_colors() {
    let mut t = term(10, 2);
    feed(&mut t, "\x1b]10;?\x07\x1b]11;?\x1b\\\x1b]12;?\x07");
    assert_eq!(
        replies(&mut t),
        "\x1b]10;rgb:e5e5/e5e5/e5e5\x07\x1b]11;rgb:0000/0000/0000\x1b\\\x1b]12;rgb:e5e5/e5e5/e5e5\x07"
    );
    feed(&mut t, "\x1b]10;#102030\x07\x1b]11;rgb:f/80/abc\x07\x1b]12;#fff\x07");
    let p = t.palette();
    assert_eq!(p.foreground, Rgb { r: 0x10, g: 0x20, b: 0x30 });
    assert_eq!(p.background, Rgb { r: 0xff, g: 0x80, b: 0xab });
    assert_eq!(p.cursor, Some(Rgb { r: 0xf0, g: 0xf0, b: 0xf0 }));
    feed(&mut t, "\x1b]10;?;?\x07");
    assert_eq!(replies(&mut t), "\x1b]10;rgb:1010/2020/3030\x07\x1b]11;rgb:ffff/8080/abab\x07");
    feed(&mut t, "\x1b]110\x07\x1b]111\x07\x1b]112\x07");
    assert_eq!(*t.palette(), Palette::default());
    feed(&mut t, "\x1b]4;1;?;2;#010203\x07\x1b]4;2;?\x1b\\");
    assert_eq!(replies(&mut t), "\x1b]4;1;rgb:cdcd/0000/0000\x07\x1b]4;2;rgb:0101/0202/0303\x1b\\");
    feed(&mut t, "\x1b]4;3;#000000\x07\x1b]104;2\x07");
    assert_eq!(t.palette().indexed[2], Palette::default().indexed[2]);
    assert_eq!(t.palette().indexed[3], Rgb::default());
    feed(&mut t, "\x1b]104\x07");
    assert_eq!(*t.palette(), Palette::default());
}

#[test]
fn title_cwd_bell() {
    let mut t = term(10, 2);
    feed(&mut t, "\x1b]0;one\x07");
    assert_eq!(t.title(), "one");
    feed(&mut t, "\x1b]2;two; with semicolon\x1b\\");
    assert_eq!(t.title(), "two; with semicolon");
    feed(&mut t, "\x1b]1;icon\x07\x1b]8;;http://x\x07link\x1b]8;;\x07\x1b]52;c;aGk=\x07");
    assert_eq!(t.title(), "two; with semicolon");
    assert_eq!(line(&t, 0), "link");
    feed(&mut t, "\x1b]7;file://localhost/home/me/a%20b\x07");
    assert_eq!(t.reported_cwd(), Some("/home/me/a b"));
    assert!(!t.take_bell());
    feed(&mut t, "\x07\x07");
    assert!(t.take_bell());
    assert!(!t.take_bell());
}

#[test]
fn default_palette() {
    let p = Palette::default();
    assert_eq!(p.indexed[1], Rgb { r: 0xcd, g: 0, b: 0 });
    assert_eq!(p.indexed[12], Rgb { r: 0x5c, g: 0x5c, b: 0xff });
    assert_eq!(p.indexed[16], Rgb { r: 0, g: 0, b: 0 });
    assert_eq!(p.indexed[17], Rgb { r: 0, g: 0, b: 0x5f });
    assert_eq!(p.indexed[231], Rgb { r: 0xff, g: 0xff, b: 0xff });
    assert_eq!(p.indexed[232], Rgb { r: 8, g: 8, b: 8 });
    assert_eq!(p.indexed[255], Rgb { r: 0xee, g: 0xee, b: 0xee });
    assert_eq!(p.cursor, None);
}

// ---- Modes ----------------------------------------------------------------

#[test]
fn input_and_mouse_modes() {
    let mut t = term(10, 2);
    feed(&mut t, "\x1b[?1h\x1b=\x1b[?2004h\x1b[?1004h\x1b[?1007h\x1b[?2026h\x1b[?5h\x1b[?1002h\x1b[?1006h");
    let m = t.modes();
    assert!(m.app_cursor && m.app_keypad && m.bracketed_paste && m.focus_events);
    assert!(m.alternate_scroll && m.sync_update && m.reverse_video);
    assert_eq!(m.mouse_mode, MouseMode::ButtonMotion);
    assert_eq!(m.mouse_encoding, MouseEncoding::Sgr);
    feed(&mut t, "\x1b[?1003h\x1b[?1015h");
    assert_eq!(t.modes().mouse_mode, MouseMode::AnyMotion);
    assert_eq!(t.modes().mouse_encoding, MouseEncoding::Urxvt);
    feed(&mut t, "\x1b[?1006l\x1b[?1003l\x1b[?9h\x1b[?1005h");
    assert_eq!(t.modes().mouse_mode, MouseMode::X10);
    assert_eq!(t.modes().mouse_encoding, MouseEncoding::Utf8);
    feed(&mut t, "\x1b[?1000h\x1b[?1005l");
    assert_eq!(t.modes().mouse_mode, MouseMode::Normal);
    assert_eq!(t.modes().mouse_encoding, MouseEncoding::Default);
    feed(&mut t, "\x1b[?1l\x1b>\x1b[?2004l\x1b[?1004l\x1b[?1007l\x1b[?2026l\x1b[?5l\x1b[?1000l");
    assert_eq!(t.modes(), crate::Modes::default());
}

#[test]
fn cursor_style_and_visibility() {
    let mut t = term(10, 2);
    let c = t.cursor();
    assert!(c.visible && c.blinking && c.shape == CursorShape::Block);
    for (seq, shape, blink) in [
        ("\x1b[6 q", CursorShape::Bar, false),
        ("\x1b[5 q", CursorShape::Bar, true),
        ("\x1b[4 q", CursorShape::Underline, false),
        ("\x1b[3 q", CursorShape::Underline, true),
        ("\x1b[2 q", CursorShape::Block, false),
        ("\x1b[0 q", CursorShape::Block, true),
    ] {
        feed(&mut t, seq);
        assert_eq!((t.cursor().shape, t.cursor().blinking), (shape, blink), "{seq:?}");
    }
    feed(&mut t, "\x1b[?12l\x1b[?25l");
    assert!(!t.cursor().blinking && !t.cursor().visible);
    feed(&mut t, "\x1b[?25h");
    assert!(t.cursor().visible);
}

#[test]
fn newline_mode() {
    let mut t = term(10, 3);
    feed(&mut t, "\x1b[20hab\ncd\x1b[20l\nef");
    assert_eq!(screen(&t), ["ab", "cd", "  ef"]);
}

// ---- Dirty tracking ---------------------------------------------------------

#[test]
fn dirty_rows() {
    let mut t = term(10, 70);
    assert!(t.dirty().any());
    t.clear_dirty();
    assert!(!t.dirty().any());
    feed(&mut t, "\x1b[4;1Hx\x1b[66;1Hy");
    let dirty: Vec<u16> = (0..70).filter(|&y| t.dirty().is_dirty(y)).collect();
    assert_eq!(dirty, [3, 65]);
    t.clear_dirty();
    feed(&mut t, "\x1b[70;1H\n");
    assert!((0..70).all(|y| t.dirty().is_dirty(y)));
    assert!(!t.dirty().is_dirty(70));
    t.clear_dirty();
    t.mark_all_dirty();
    assert!((0..70).all(|y| t.dirty().is_dirty(y)));
    // With the view scrolled back, changes land on shifted rows.
    t.scroll_view(1);
    t.clear_dirty();
    feed(&mut t, "\x1b[1;1Hz");
    assert!(t.dirty().is_dirty(1) && !t.dirty().is_dirty(0));
}

#[test]
fn configured_palette_survives_resets() {
    let mut t = Terminal::new(10, 3, 0);
    let mut configured = Palette { foreground: Rgb { r: 1, g: 2, b: 3 }, ..Palette::default() };
    configured.indexed[1] = Rgb { r: 4, g: 5, b: 6 };
    t.set_default_palette(configured.clone());
    assert_eq!(*t.palette(), configured);
    t.feed(b"\x1b]4;1;rgb:ff/ff/ff\x07\x1b]10;rgb:00/00/00\x07");
    assert_ne!(*t.palette(), configured);
    t.feed(b"\x1b]104\x07\x1b]110\x07");
    assert_eq!(*t.palette(), configured);
    t.feed(b"\x1b]4;1;rgb:ff/ff/ff\x07\x1bc");
    assert_eq!(*t.palette(), configured);
}

#[test]
fn alternate_scroll_starts_on() {
    let mut t = term(10, 2);
    assert!(t.modes().alternate_scroll);
    feed(&mut t, "\x1b[?1007l");
    assert!(!t.modes().alternate_scroll);
    feed(&mut t, "\x1bc");
    assert!(t.modes().alternate_scroll);
}

#[test]
fn focus_reporting_starts_with_the_current_state() {
    let mut t = term(10, 2);
    t.set_focused(false);
    feed(&mut t, "\x1b[?1004h");
    assert_eq!(t.take_replies().as_deref(), Some(&b"\x1b[O"[..]));
    feed(&mut t, "\x1b[?1016h");
    assert_eq!(t.modes().mouse_encoding, MouseEncoding::SgrPixels);
}

#[test]
fn absolute_lines_follow_their_content() {
    let mut t = term(4, 2);
    feed(&mut t, "ab\r\ncd");
    let top = t.first_line();
    assert_eq!(&t.line(top).unwrap()[..2], &t.row(0)[..2]);
    feed(&mut t, "\r\nef");
    // "ab" scrolled into the history and kept its number.
    assert_eq!(t.first_line(), top);
    assert_eq!(t.line(top).unwrap()[0].char(), Some('a'));
    assert_eq!(t.view_top(), top + 1);
    assert_eq!(t.line(t.end_line() - 1).unwrap()[0].char(), Some('e'));
    assert!(t.line(t.end_line()).is_none());
    feed(&mut t, "\x1b[2J\x1b[Hwxyzq");
    assert!(t.line_wrapped(t.view_top()));
}

#[test]
fn mouse_shape_follows_osc_22_and_reporting() {
    let mut t = term(10, 2);
    assert_eq!(t.mouse_shape(), "text");
    feed(&mut t, "\x1b[?1000h");
    assert_eq!(t.mouse_shape(), "default");
    feed(&mut t, "\x1b]22;hand\x07");
    assert_eq!(t.mouse_shape(), "pointer");
    feed(&mut t, "\x1b]22;nonsense\x07");
    assert_eq!(t.mouse_shape(), "pointer");
    feed(&mut t, "\x1b]22;\x07");
    assert_eq!(t.mouse_shape(), "default");
    feed(&mut t, "\x1b[?1000l");
    assert_eq!(t.mouse_shape(), "text");
}

#[test]
fn osc_133_marks_prompts_input_and_output() {
    use crate::{PromptClick, PromptMark, Semantic};
    let mut t = term(20, 4);
    assert!(!t.cursor_at_prompt());
    feed(&mut t, "out\x1b]133;A;click_events=1\x07$ \x1b]133;B\x07ls");
    // A fresh line was started for the prompt.
    let top = t.view_top();
    assert_eq!(t.line_prompt(top + 1), PromptMark::Prompt);
    assert!(t.cursor_at_prompt());
    assert_eq!(t.prompt_click(), Some(PromptClick::Absolute));
    let row = t.line(top + 1).unwrap();
    assert_eq!((row[0].semantic(), row[2].semantic()), (Semantic::Prompt, Semantic::Input));
    feed(&mut t, "\r\n\x1b]133;C\x07file\r\n\x1b]133;D;0\x07");
    assert!(!t.cursor_at_prompt());
    assert_eq!(t.line(top + 2).unwrap()[0].semantic(), Semantic::Output);
    // A multi-line prompt continues on the next row.
    feed(&mut t, "\x1b]133;A\x07a\r\nb");
    assert_eq!(t.line_prompt(t.end_line() - 1), PromptMark::Continuation);
    // The alternate screen is never at a prompt.
    feed(&mut t, "\x1b[?1049h");
    assert!(!t.cursor_at_prompt());
}

#[test]
fn osc_7_follows_ghostty() {
    let host = std::fs::read_to_string("/proc/sys/kernel/hostname").unwrap().trim_end().to_owned();
    let mut t = term(10, 2);
    // Until a program sets a title, the directory is the title.
    feed(&mut t, &format!("\x1b]7;kitty-shell-cwd://{host}/tmp/a b\x07"));
    assert_eq!((t.reported_cwd(), t.title()), (Some("/tmp/a b"), "/tmp/a b"));
    feed(&mut t, "\x1b]7;file://localhost/x%20y\x07");
    assert_eq!(t.reported_cwd(), Some("/x y"));
    // Other hosts, an empty host and bare paths are ignored.
    for url in ["file://elsewhere.invalid/z", "file:///z", "/z"] {
        feed(&mut t, &format!("\x1b]7;{url}\x07"));
        assert_eq!(t.reported_cwd(), Some("/x y"), "{url}");
    }
    feed(&mut t, "\x1b]2;title\x07\x1b]7;\x07");
    assert_eq!((t.reported_cwd(), t.title()), (None, "title"));
}

fn messages(t: &mut Terminal, s: &str) -> Vec<Message> {
    feed(t, s);
    t.take_messages()
}

fn notification(title: &str, body: &str) -> Vec<Message> {
    vec![Message::DesktopNotification { title: title.to_owned(), body: body.to_owned() }]
}

#[test]
fn osc_9_is_a_notification_unless_conemu_parses_it() {
    let mut t = term(10, 2);
    assert_eq!(messages(&mut t, "\x1b]9;Hello world\x07"), notification("", "Hello world"));
    assert_eq!(messages(&mut t, "\x1b]9;H\x1b\\"), notification("", "H"));
    // Incomplete ConEmu commands fall back to notifications.
    for body in
        ["", "1", "1a", "10;", "10;5", "10x", "11", "13", "2", "2a", "3", "3a", "4", "4;", "4;5", "4;5a", "6", "9"]
    {
        assert_eq!(messages(&mut t, &format!("\x1b]9;{body}\x07")), notification("", body), "{body:?}");
    }
    // ConEmu commands ghostty parses and ignores.
    for data in ["1;420", "1;", "10", "10;1", "11;c", "2;hello", "3;foo", "3;", "5", "5abc", "6;m", "7;p", "8;v"] {
        assert_eq!(messages(&mut t, &format!("\x1b]9;{data}\x07")), [], "{data:?}");
    }
    // Without the `;` nothing is captured.
    assert_eq!(messages(&mut t, "\x1b]9\x07"), []);
}

#[test]
fn osc_9_conemu_pwd_and_prompt() {
    let mut t = term(10, 3);
    feed(&mut t, "\x1b]9;9;file://localhost/x\x07");
    assert_eq!(t.reported_cwd(), Some("/x"));
    feed(&mut t, "out\x1b]9;12\x07");
    assert_eq!(t.line_prompt(t.view_top() + 1), crate::PromptMark::Prompt);
    assert!(t.cursor_at_prompt());
}

#[test]
fn osc_9_4_reports_progress_like_ghostty() {
    use crate::ProgressState::{Error, Indeterminate, Pause, Remove, Set};
    let mut t = term(10, 2);
    for (data, state, progress) in [
        ("1;100", Set, Some(100)),
        ("1;900", Set, Some(100)),
        ("1;9", Set, Some(9)),
        ("1;94", Set, Some(94)),
        ("1", Set, Some(0)),
        ("1x", Set, Some(0)),
        ("1;", Set, None),
        ("1;4_2", Set, None),
        ("1;+5", Set, None),
        ("1;100;", Set, None),
        ("1;99999999999999999999999", Set, None),
        ("0;", Remove, None),
        ("0;;", Remove, None),
        ("0;100", Remove, None),
        ("2", Error, None),
        ("2;100", Error, Some(100)),
        ("3;50", Indeterminate, None),
        ("4", Pause, None),
        ("4;100", Pause, Some(100)),
    ] {
        let report = ProgressReport { state, progress };
        assert_eq!(messages(&mut t, &format!("\x1b]9;4;{data}\x07")), [Message::ProgressReport(report)], "{data:?}");
    }
}

#[test]
fn osc_777_notify() {
    let mut t = term(10, 2);
    assert_eq!(messages(&mut t, "\x1b]777;notify;Title;Body\x07"), notification("Title", "Body"));
    assert_eq!(messages(&mut t, "\x1b]777;notify;T;a;b\x07"), notification("T", "a;b"));
    assert_eq!(messages(&mut t, "\x1b]777;notify;;\x07"), notification("", ""));
    for data in ["notify;T", "notify", "other;T;B", ""] {
        assert_eq!(messages(&mut t, &format!("\x1b]777;{data}\x07")), [], "{data:?}");
    }
}

#[test]
fn notifications_fit_ghosttys_buffers() {
    let mut t = term(10, 2);
    let got = messages(&mut t, &format!("\x1b]777;notify;{};{}\x07", "a".repeat(70), "b".repeat(300)));
    assert_eq!(got, notification(&"a".repeat(63), &"b".repeat(255)));
    // A cut never splits a character.
    let got = messages(&mut t, &format!("\x1b]777;notify;{};x\x07", "\u{e9}".repeat(40)));
    assert_eq!(got, notification(&"\u{e9}".repeat(31), "x"));
    // The text after `9;` and `777;` must leave room for a terminator in 2048 bytes.
    let body = "a".repeat(2047);
    assert_eq!(messages(&mut t, &format!("\x1b]9;{body}\x07")), notification("", &body[..255]));
    assert_eq!(messages(&mut t, &format!("\x1b]9;{body}a\x07")), []);
    let tail = "c".repeat(2047 - "notify;t;".len());
    assert_eq!(messages(&mut t, &format!("\x1b]777;notify;t;{tail}\x07")), notification("t", &tail[..255]));
    assert_eq!(messages(&mut t, &format!("\x1b]777;notify;t;{tail}c\x07")), []);
}

#[test]
fn osc_133_start_and_stop_commands() {
    let mut t = term(10, 2);
    assert_eq!(messages(&mut t, "\x1b]133;C\x07"), [Message::StartCommand]);
    for (data, code) in [
        ("D", 0),
        ("D;", 0),
        ("D;0", 0),
        ("D;5", 5),
        ("D;+7", 7),
        ("D;5;aid=1", 5),
        ("D;-1", 1),
        ("D;256", 1),
        ("D;abc", 0),
        ("D;aid=1", 0),
    ] {
        assert_eq!(messages(&mut t, &format!("\x1b]133;{data}\x07")), [Message::StopCommand(code)], "{data:?}");
    }
    assert_eq!(messages(&mut t, "\x1b]133;Dx\x07"), []);
}

#[test]
fn ris_removes_the_progress_report() {
    let mut t = term(10, 2);
    let remove = Message::ProgressReport(ProgressReport { state: crate::ProgressState::Remove, progress: None });
    assert_eq!(messages(&mut t, "\x1b]9;a\x07\x1bc"), [notification("", "a").remove(0), remove]);
    // The reset action resets without telling the surface.
    t.full_reset();
    assert_eq!(t.take_messages(), []);
}

// ---- Selection --------------------------------------------------------------

fn select(t: &mut Terminal, start: (u64, u16), end: (u64, u16)) -> Selection {
    let top = t.view_top();
    let point = |(row, col)| Point { line: top + row, col };
    let sel = Selection { start: point(start), end: point(end), rect: false };
    t.set_selection(Some(sel));
    sel
}

/// The cells under the selection's ends.
fn ends(t: &Terminal) -> (Cell, Cell) {
    let s = t.selection().unwrap();
    let at = |p: Point| t.line(p.line).unwrap()[p.col as usize];
    (at(s.start), at(s.end))
}

fn chars(cells: (Cell, Cell)) -> (Option<char>, Option<char>) {
    (cells.0.char(), cells.1.char())
}

#[test]
fn ris_clears_the_selection() {
    let mut t = term(10, 3);
    feed(&mut t, "hello");
    let sel = select(&mut t, (0, 0), (0, 4));
    assert_eq!(t.selection(), Some(sel));
    feed(&mut t, "\x1bc");
    assert_eq!(t.selection(), None);
    select(&mut t, (0, 0), (0, 4));
    t.full_reset();
    assert_eq!(t.selection(), None);
}

#[test]
fn screen_switches_clear_the_selection() {
    let mut t = term(10, 3);
    feed(&mut t, "primary");
    let sel = select(&mut t, (0, 0), (0, 6));
    // Leaving a screen that is not active switches nothing.
    feed(&mut t, "\x1b[?1049l\x1b[?1047l\x1b[?47l");
    assert_eq!(t.selection(), Some(sel));
    feed(&mut t, "\x1b[?1049h");
    assert_eq!(t.selection(), None);
    let alt = select(&mut t, (1, 0), (1, 3));
    feed(&mut t, "\x1b[?1049h\x1b[?1047h\x1b[?47h");
    assert_eq!(t.selection(), Some(alt));
    // The primary screen's selection does not come back either.
    feed(&mut t, "\x1b[?1049l");
    assert_eq!(t.selection(), None);
    for mode in [47, 1047] {
        select(&mut t, (0, 0), (0, 6));
        feed(&mut t, &format!("\x1b[?{mode}h"));
        assert_eq!(t.selection(), None, "entering with {mode}");
        select(&mut t, (0, 0), (0, 6));
        feed(&mut t, &format!("\x1b[?{mode}l"));
        assert_eq!(t.selection(), None, "leaving with {mode}");
    }
}

#[test]
fn erasing_and_scrolling_keep_the_selection() {
    let mut t = term(10, 3);
    feed(&mut t, "abc\r\ndef");
    let sel = select(&mut t, (0, 0), (0, 2));
    // "abc" scrolls into the history and keeps its line.
    feed(&mut t, "\r\n\r\n\r\n");
    assert_eq!(t.selection(), Some(sel));
    assert_eq!(chars(ends(&t)), (Some('a'), Some('c')));
    let sel = select(&mut t, (0, 1), (2, 5));
    feed(&mut t, "\x1b[2J\x1b[H\x1b[J\x1b[1J\x1b[Kxyz");
    assert_eq!(t.selection(), Some(sel));
}

#[test]
fn reflow_keeps_the_selection_on_its_text() {
    let mut t = term(10, 4);
    feed(&mut t, "0123456789abcdefghij\r\nshort");
    select(&mut t, (0, 5), (1, 3));
    assert_eq!(chars(ends(&t)), (Some('5'), Some('d')));
    t.resize(7, 4);
    assert_eq!(screen(&t), ["0123456", "789abcd", "efghij", "short"]);
    assert_eq!(chars(ends(&t)), (Some('5'), Some('d')));
    assert_eq!(t.selection().unwrap().end, Point { line: t.view_top() + 1, col: 6 });
    t.resize(20, 4);
    assert_eq!(chars(ends(&t)), (Some('5'), Some('d')));
    assert_eq!(t.selection().unwrap().end, Point { line: t.view_top(), col: 13 });
    t.resize(10, 4);
    assert_eq!(chars(ends(&t)), (Some('5'), Some('d')));
    // The second half of a wide character moves with it.
    let mut t = term(6, 3);
    feed(&mut t, "ab\u{4e2d}\u{6587}\u{5b57}");
    select(&mut t, (0, 3), (1, 1));
    t.resize(5, 3);
    assert_eq!(screen(&t), ["ab\u{4e2d}", "\u{6587}\u{5b57}"]);
    let s = t.selection().unwrap();
    assert_eq!((s.start.col, s.end.col), (3, 3));
    assert_eq!((ends(&t).0.width(), ends(&t).1.width()), (Width::SpacerTail, Width::SpacerTail));
    assert_eq!(t.line(s.end.line).unwrap()[2].char(), Some('\u{5b57}'));
}

#[test]
fn reflow_clamps_a_point_in_trailing_blanks() {
    // The row starts the line: the point stays on the row's first
    // destination row instead of making it wrap.
    let mut t = term(10, 3);
    feed(&mut t, "abc\r\nxyz");
    select(&mut t, (0, 1), (0, 8));
    t.resize(5, 3);
    assert_eq!(screen(&t), ["abc", "xyz"]);
    assert_eq!(t.selection().unwrap().end, Point { line: t.view_top(), col: 4 });
    // The row continues a wrapped line at destination column 6: 9 - 1 - 6.
    let mut t = term(6, 3);
    feed(&mut t, "abcdefgh\r\nz");
    select(&mut t, (0, 0), (1, 5));
    t.resize(9, 3);
    assert_eq!(screen(&t), ["abcdefgh", "z"]);
    assert_eq!(t.selection().unwrap().end, Point { line: t.view_top(), col: 8 });
    // A blank row below the cursor that holds a point is kept.
    let mut t = term(10, 5);
    feed(&mut t, "top\x1b[2;1Hcur");
    select(&mut t, (0, 0), (3, 2));
    t.resize(8, 5);
    assert_eq!(t.selection().unwrap().end, Point { line: t.view_top() + 3, col: 2 });
}

#[test]
fn alternate_screen_resize_moves_the_selection_with_its_rows() {
    let mut t = term(10, 4);
    feed(&mut t, "\x1b[?1049h\x1b[Hrow0\r\nrow1\r\nrow2\r\nrow3");
    select(&mut t, (0, 2), (2, 8));
    // Two rows leave the top to keep the cursor on screen.
    t.resize(5, 2);
    assert_eq!(screen(&t), ["row2", "row3"]);
    let s = t.selection().unwrap();
    assert_eq!(s.start, Point { line: t.first_line(), col: 0 });
    assert_eq!(s.end, Point { line: t.view_top(), col: 4 });
    // tapust differs from ghostty: the cursor's row stays, and the end on
    // the row cut below it moves to the last kept cell.
    feed(&mut t, "\x1b[H");
    select(&mut t, (0, 1), (1, 2));
    t.resize(5, 1);
    assert_eq!(screen(&t), ["row2"]);
    assert_eq!(pos(&t), (0, 0));
    let s = t.selection().unwrap();
    assert_eq!((s.start, s.end), (Point { line: t.view_top(), col: 1 }, Point { line: t.view_top(), col: 4 }));
}

#[test]
fn narrowing_reflow_keeps_line_numbers_valid() {
    let mut t = Terminal::new(20, 5, 100);
    t.feed(b"aaaaaaaaaaaaaaaaaa\r\nbbbbbbbbbbbbbbbbbb\r\ncccccccccccccccccc\r\ndddd");
    t.resize(10, 5);
    // The rows wrapped into the scrollback come before the oldest line's
    // number instead of below zero.
    assert_eq!((t.first_line(), t.view_top(), t.end_line()), (0, 2, 7));
    assert_eq!(t.line(0).unwrap()[0].char(), Some('a'));
}

#[test]
fn narrowing_a_wide_char_after_a_full_row_does_not_overflow() {
    let mut t = Terminal::new(10, 3, 100);
    // U+4E2D and U+6587 are wide.
    t.feed("abcdefghij\u{4e2d}\u{6587}\u{4e2d}\u{6587}".as_bytes());
    t.resize(6, 3);
    // "ghij" and a wide char fill a row exactly, so the next one starts the
    // next row.
    assert_eq!(screen(&t), ["ghij\u{4e2d}", "\u{6587}\u{4e2d}\u{6587}"]);
}

#[test]
fn discarded_lines_shrink_or_clear_the_selection() {
    let mut t = Terminal::new(10, 2, 2);
    feed(&mut t, "a\r\nb\r\nc");
    let first = t.first_line();
    let sel = Selection { start: Point { line: first, col: 0 }, end: Point { line: first + 2, col: 0 }, rect: false };
    t.set_selection(Some(sel));
    // "a" and "b" leave the two-line scrollback: the selection starts at "c".
    feed(&mut t, "\r\nd\r\ne\r\nf");
    let s = t.selection().unwrap();
    assert_eq!((s.start, s.end), (Point { line: t.first_line(), col: 0 }, sel.end));
    // ED 3 discards the rest of it.
    feed(&mut t, "\x1b[3J");
    assert_eq!(t.selection(), None);
}

#[test]
fn region_scrolls_keep_the_selection_on_its_text() {
    let mut t = Terminal::new(10, 6, 100);
    t.feed(b"r0\r\nr1\r\nr2\r\nr3\r\nr4\r\nr5\x1b[3;5r\x1b[5;1H");
    // Rows 2 to 4 scroll up and "r2" is discarded: the start moves to the
    // region's new first row, the end moves up with "r3".
    select(&mut t, (2, 1), (3, 1));
    t.feed(b"\n");
    let top = t.view_top();
    let s = t.selection().unwrap();
    assert_eq!((s.start, s.end), (Point { line: top + 2, col: 0 }, Point { line: top + 2, col: 1 }));
    assert_eq!(chars(ends(&t)), (Some('r'), Some('3')));
    // An end on discarded text keeps the part above the region.
    select(&mut t, (1, 0), (2, 1));
    t.feed(b"\n");
    let s = t.selection().unwrap();
    assert_eq!((s.start, s.end), (Point { line: top + 1, col: 0 }, Point { line: top + 1, col: 9 }));
    // With nothing kept, the selection goes away.
    select(&mut t, (2, 0), (2, 1));
    t.feed(b"\n");
    assert_eq!(t.selection(), None);
    // Inserted and deleted lines move the text and the selection with it.
    let mut t = Terminal::new(10, 4, 100);
    t.feed(b"a0\r\na1\r\na2\r\na3\x1b[2;1H");
    select(&mut t, (2, 0), (2, 1));
    t.feed(b"\x1b[L");
    assert_eq!(t.selection().unwrap().start.line, t.view_top() + 3);
    assert_eq!(chars(ends(&t)), (Some('a'), Some('2')));
    t.feed(b"\x1b[2M");
    assert_eq!(t.selection().unwrap().start.line, t.view_top() + 1);
    assert_eq!(chars(ends(&t)), (Some('a'), Some('2')));
}

#[test]
fn alternate_screen_scrolls_keep_the_selection_on_its_text() {
    let mut t = Terminal::new(10, 4, 100);
    t.feed(b"\x1b[?1049ha0\r\na1\r\na2\r\na3");
    select(&mut t, (0, 1), (2, 1));
    t.feed(b"\n");
    let top = t.view_top();
    let s = t.selection().unwrap();
    // "a0" is gone: the selection starts at "a1", now the first row.
    assert_eq!((s.start, s.end), (Point { line: top, col: 0 }, Point { line: top + 1, col: 1 }));
    // SU 2 discards "a1" and "a2", leaving nothing selected.
    t.feed(b"\x1b[2S");
    assert_eq!(t.selection(), None);
}

#[test]
fn xtversion_and_xtgettcap_replies() {
    let mut t = Terminal::new(10, 2, 0);
    t.feed(b"\x1b[>q");
    assert_eq!(t.take_replies(), Some(format!("\x1bP>|tapust {}\x1b\\", env!("CARGO_PKG_VERSION")).into_bytes()));
    // "Tc" is a boolean, "Co" a number, "Ss" a string; "zz" is unknown.
    t.feed(b"\x1bP+q5463;436F;5373;7A7A\x1b\\");
    let hex = |s: &str| s.bytes().map(|b| format!("{b:02X}")).collect::<String>();
    let expected = format!(
        "\x1bP1+r5463\x1b\\\x1bP1+r436F={}\x1b\\\x1bP1+r5373={}\x1b\\\x1bP0+r7A7A\x1b\\",
        hex("256"),
        hex("\x1b[%p1%d q")
    );
    assert_eq!(String::from_utf8(t.take_replies().unwrap()).unwrap(), expected);
    // Other DCS strings are still ignored, and printing goes on after ST.
    t.feed(b"\x1bP1$qm\x1b\\ok");
    assert_eq!(t.take_replies(), None);
    assert_eq!(t.line(t.view_top()).unwrap()[0].char(), Some('o'));
}

