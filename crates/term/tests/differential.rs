//! Differential test against alacritty_terminal: identical bytes, fed in
//! random chunks, must give identical visible text and cursor positions.
//!
//! The payloads avoid the cases where alacritty (vte 0.15 + alacritty_terminal
//! 0.26) differs from xterm, which this crate follows:
//!
//! - LF/IND while a wrap is pending: alacritty keeps the pending wrap, so the
//!   next character lands one row further down; xterm clears it.
//! - EL 0 while a wrap is pending: alacritty does nothing; xterm erases the
//!   last column and clears the pending wrap.
//! - HT while a wrap is pending: alacritty wraps to the next row.
//! - IL/DL: alacritty leaves the cursor column alone; xterm moves to column 1.
//! - CUU/CUD: alacritty ignores the scroll margins.
//! - DECOM reset: alacritty does not home the cursor.
//! - DCH larger than the rest of the row: alacritty also blanks cells left of
//!   the cursor.
//! - ECH/ICH/EL splitting a wide character: alacritty keeps the other half,
//!   which later corrupts writes next to it; we blank the whole character
//!   (as ghostty does). The text comparison treats orphaned halves as blank.
//! - VS16 after a narrow character: alacritty does not widen the cell.
//! - A zero-width character with no character before it on the row: alacritty
//!   attaches it to the empty cell; we drop it.
//! - Raw C1 bytes and lone UTF-8 continuation bytes: alacritty executes or
//!   drops them; we print U+FFFD.
//! - ED 2 on the primary screen: alacritty moves the screen into the
//!   scrollback (the visible text is the same).
//! - Synchronized updates (DECSET 2026): alacritty buffers the output until the
//!   update ends, so comparisons are skipped while it is buffering.
//! - Grapheme clusters (UAX #29) beyond zero-width marks: we join flags, emoji
//!   modifiers, ZWJ sequences and the marks of the Indic and Southeast Asian
//!   scripts into one cell, as ghostty does; alacritty does not. [`compared`]
//!   takes the codepoints that can join that way out of the payloads.
//!
//! alacritty also has a chunk-boundary bug, worked around in [`run`].

#[path = "support/payloads.rs"]
mod payloads;

use alacritty_terminal::event::VoidListener;
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::test::TermSize;
use alacritty_terminal::term::{Config, Term};
use alacritty_terminal::vte::ansi::{Processor, StdSyncHandler};
use payloads::{Payload, Rng};
use term::{Terminal, Width};

const COLS: usize = 160;
const ROWS: usize = 48;

struct Pair {
    ours: Terminal,
    alac: Term<VoidListener>,
    proc: Processor<StdSyncHandler>,
}

impl Pair {
    fn new() -> Pair {
        let config = Config { scrolling_history: 1000, ..Config::default() };
        Pair {
            ours: Terminal::new(COLS as u16, ROWS as u16, 1000),
            alac: Term::new(config, &TermSize::new(COLS, ROWS), VoidListener),
            proc: Processor::new(),
        }
    }

    fn feed(&mut self, bytes: &[u8]) {
        self.ours.feed(bytes);
        self.proc.advance(&mut self.alac, bytes);
    }

    fn ours_text(&self, y: usize) -> String {
        let row = self.ours.row(y as u16);
        let mut s = String::new();
        for (x, c) in row.iter().enumerate() {
            match c.width() {
                Width::Wide => assert!(
                    row.get(x + 1).is_some_and(|n| n.width() == Width::SpacerTail),
                    "wide char without tail at {x},{y}"
                ),
                Width::SpacerTail => {
                    assert!(x > 0 && row[x - 1].width() == Width::Wide, "tail without wide char at {x},{y}")
                }
                _ => {}
            }
            match c.width() {
                Width::SpacerTail => {}
                Width::SpacerHead => s.push(' '),
                _ if c.has_grapheme() => s.push_str(self.ours.grapheme(y as u16, x as u16).expect("grapheme")),
                _ => s.push(c.char().unwrap_or(' ')),
            }
        }
        s.trim_end().to_owned()
    }

    fn alac_text(&self, y: usize) -> String {
        let row = &self.alac.grid()[Line(y as i32)];
        let mut s = String::new();
        let flags = |x: usize| {
            if x < COLS { row[Column(x)].flags } else { Flags::empty() }
        };
        for x in 0..COLS {
            let cell = &row[Column(x)];
            // alacritty leaves one half behind when an edit splits a wide
            // character; xterm (and we) blank the whole character.
            let orphan = (cell.flags.contains(Flags::WIDE_CHAR_SPACER)
                && (x == 0 || !flags(x - 1).contains(Flags::WIDE_CHAR)))
                || (cell.flags.contains(Flags::WIDE_CHAR) && !flags(x + 1).contains(Flags::WIDE_CHAR_SPACER));
            if orphan {
                s.push(' ');
                continue;
            }
            if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
                continue;
            }
            // alacritty marks tabbed-over blanks with '\t'.
            s.push(if cell.c == '\t' { ' ' } else { cell.c });
            if let Some(zw) = cell.zerowidth() {
                s.extend(zw);
            }
        }
        s.trim_end().to_owned()
    }

    fn check(&self, ctx: &str) {
        self.check_cursor(ctx);
        if self.proc.sync_bytes_count() != 0 {
            return;
        }
        for y in 0..ROWS {
            let (a, b) = (self.ours_text(y), self.alac_text(y));
            assert!(a == b, "{ctx}: row {y} differs\n ours: {a:?}\n alac: {b:?}");
        }
    }

    fn check_cursor(&self, ctx: &str) {
        // alacritty buffers synchronized updates until they end.
        if self.proc.sync_bytes_count() != 0 {
            return;
        }
        let p = self.alac.grid().cursor.point;
        let c = self.ours.cursor();
        assert_eq!((c.x as usize, c.y as usize), (p.column.0, p.line.0 as usize), "{ctx}: cursor");
    }
}

/// Feeds `payload` in random chunks of 1..=max bytes, comparing after each.
///
/// alacritty only gets whole codepoints: vte 0.15 drops the character that
/// follows a codepoint completed across calls when the lookahead also holds
/// the start of another one (`advance_partial_utf8` returns `valid_bytes`).
/// A held-back partial codepoint is invisible on both sides.
fn run(payload: &Payload, seed: u64, max: usize) {
    let mut pair = Pair::new();
    pair.feed(&payload.setup);
    pair.check(&format!("{} setup", payload.name));
    let mut rng = Rng::new(seed);
    let body = &payload.body;
    let (mut i, mut a, mut chunks) = (0, 0, 0u64);
    while i < body.len() {
        let n = (1 + rng.below(max)).min(body.len() - i);
        pair.ours.feed(&body[i..i + n]);
        i += n;
        let mut end = i;
        while end > a && end < body.len() && (0x80..0xC0).contains(&body[end]) {
            end -= 1;
        }
        pair.proc.advance(&mut pair.alac, &body[a..end]);
        a = end;
        chunks += 1;
        let ctx = format!("{} seed {seed} at byte {i}", payload.name);
        if chunks % 32 == 0 { pair.check(&ctx) } else { pair.check_cursor(&ctx) }
    }
    if pair.proc.sync_bytes_count() != 0 {
        pair.proc.stop_sync(&mut pair.alac);
    }
    pair.check(&format!("{} end", payload.name));
}

/// The payloads without the codepoints that can join a grapheme cluster
/// although they have a width: regional indicators, emoji modifiers, ZWJ,
/// and the Indic and Southeast Asian blocks.
fn compared(mut payloads: Vec<Payload>) -> Vec<Payload> {
    fn joins(c: char) -> bool {
        matches!(c as u32, 0x0900..=0x0DFF | 0x0E00..=0x0EFF | 0x1000..=0x109F | 0x1780..=0x17FF | 0x200D | 0x1F1E6..=0x1F1FF | 0x1F3FB..=0x1F3FF)
    }
    let filter = |bytes: &[u8]| {
        let mut out = Vec::with_capacity(bytes.len());
        for chunk in bytes.utf8_chunks() {
            let valid: String = chunk.valid().chars().filter(|&c| !joins(c)).collect();
            out.extend_from_slice(valid.as_bytes());
            out.extend_from_slice(chunk.invalid());
        }
        out
    };
    for p in &mut payloads {
        p.setup = filter(&p.setup);
        p.body = filter(&p.body);
    }
    payloads
}

fn run_all(payloads: &[Payload]) {
    for p in payloads {
        // Small chunks split most sequences; large ones exercise the fast paths.
        run(p, 1, 7);
        run(p, 2, 4096);
    }
}

#[test]
fn vtebench_payloads() {
    let payloads = compared(payloads::vtebench(COLS, ROWS, 2));
    let names: Vec<_> = payloads.iter().map(|p| p.name).collect();
    if !names.contains(&"medium_cells") {
        eprintln!("vtebench recordings not found; skipping medium_cells and sync_medium_cells");
    }
    let mut payloads = payloads;
    for p in &mut payloads {
        // Scrolling bodies are tiny; repeat them past the screen and history.
        if p.body.len() < 64 {
            p.body = payloads::repeat_to(&p.body, 4000);
        }
    }
    run_all(&payloads);
}

#[test]
fn realistic_payloads() {
    run_all(&compared(payloads::realistic(COLS, ROWS, 1)));
}

#[test]
fn random_chunk_seeds() {
    let payloads = compared(payloads::realistic(COLS, ROWS, 1));
    for seed in 10..14 {
        for p in &payloads {
            run(p, seed, 64);
        }
    }
}

#[test]
fn vtebench_payloads_in_one_feed() {
    for p in compared(payloads::vtebench(COLS, ROWS, 1)).iter().chain(&compared(payloads::realistic(COLS, ROWS, 1))) {
        let mut pair = Pair::new();
        pair.feed(&p.setup);
        pair.feed(&p.body);
        if pair.proc.sync_bytes_count() != 0 {
            pair.proc.stop_sync(&mut pair.alac);
        }
        pair.check(p.name);
    }
}

#[test]
fn mixed_rng_stream() {
    // Random interleavings of editing and scrolling. Text starts with CR so no
    // row reaches a pending wrap, and it is narrow: alacritty's ECH/ICH leave
    // orphaned wide-character halves that corrupt later writes next to them.
    let pieces: &[&[u8]] = &[
        b"\rhello",
        b"\r\n",
        b"\x1b[2;5H",
        b"\x1b[1;31m",
        b"\x1b[m",
        "\r\u{e9}\u{f1}".as_bytes(),
        "\re\u{301}".as_bytes(),
        b"\x1b[3X",
        b"\x1b[2@",
        b"\x1b[10;20r",
        b"\x1b[r",
        b"\x1bM",
        b"\x1bD",
        b"\x1b[5S",
        b"\x1b[2T",
        b"\r\x1b[48;5;21m \x1b[0m",
        b"\r\x1b(0lqqk\x1b(B",
        b"\x1b]2;title\x07",
        b"\x1b[?25l",
        b"\x1b[H\x1b[J",
        b"\x1b[47;150H",
    ];
    let mut rng = Rng::new(99);
    let mut body = Vec::new();
    for _ in 0..20000 {
        body.extend_from_slice(rng.pick(pieces));
    }
    let p = Payload { name: "mixed", setup: Vec::new(), body };
    run(&p, 5, 13);
}
