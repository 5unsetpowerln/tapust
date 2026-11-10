//! Byte streams shared by the differential test and the benchmarks: Rust
//! equivalents of vtebench's benchmarks (as a terminal receives them through
//! a PTY, so `\n` arrives as `\r\n`) and realistic application output.

#![allow(dead_code)]

use std::fmt::Write as _;
use std::path::Path;

pub struct Payload {
    pub name: &'static str,
    /// Bytes fed once before the measured body.
    pub setup: Vec<u8>,
    pub body: Vec<u8>,
}

/// Deterministic xorshift64* generator.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Rng {
        Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }

    pub fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    pub fn below(&mut self, n: usize) -> usize {
        (self.next() >> 33) as usize % n
    }

    pub fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.below(items.len())]
    }
}

const LETTERS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ";

/// Translates `\n` to `\r\n` like a PTY with ONLCR.
pub fn onlcr(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len() + bytes.len() / 16);
    for &b in bytes {
        if b == b'\n' {
            out.push(b'\r');
        }
        out.push(b);
    }
    out
}

/// Repeats `body` until it is at least `min` bytes long (vtebench's sampling).
pub fn repeat_to(body: &[u8], min: usize) -> Vec<u8> {
    let n = min.div_ceil(body.len()).max(1);
    body.repeat(n)
}

fn letters(n: usize) -> &'static [u8] {
    &LETTERS[..n.min(LETTERS.len())]
}

/// vtebench `cursor_motion`, for the first `n` letters.
pub fn cursor_motion(cols: usize, lines: usize, n: usize) -> Vec<u8> {
    let mut s = String::new();
    for &ch in letters(n) {
        let ch = ch as char;
        let (mut cs, mut ce, mut ls, mut le) = (1, cols, 1, lines);
        loop {
            let (mut c, mut l) = (cs, ls);
            while c < ce {
                let _ = write!(s, "\x1b[{l};{c}H{ch}");
                c += 1;
            }
            while l < le {
                let _ = write!(s, "\x1b[{l};{c}H{ch}");
                l += 1;
            }
            while c > cs {
                let _ = write!(s, "\x1b[{l};{c}H{ch}");
                c -= 1;
            }
            while l > ls {
                let _ = write!(s, "\x1b[{l};{c}H{ch}");
                l -= 1;
            }
            cs += 1;
            ls += 1;
            ce -= 1;
            le -= 1;
            if cs > ce || ls > le {
                break;
            }
        }
    }
    s.into_bytes()
}

/// vtebench `dense_cells`, for the first `n` letters.
pub fn dense_cells(cols: usize, lines: usize, n: usize) -> Vec<u8> {
    let mut s = String::new();
    for (offset, &ch) in letters(n).iter().enumerate() {
        s.push_str("\x1b[H");
        for line in 1..=lines {
            for column in 1..=cols {
                let index = line + column + offset;
                let fg = index % 156 + 100;
                let bg = 255 - index % 156 + 100;
                let _ = write!(s, "\x1b[38;5;{fg};48;5;{bg};1;3;4m{}", ch as char);
            }
        }
    }
    s.into_bytes()
}

/// vtebench `light_cells`, for the first `n` letters.
pub fn light_cells(cols: usize, lines: usize, n: usize) -> Vec<u8> {
    let mut v = Vec::new();
    for &ch in letters(n) {
        v.extend_from_slice(b"\x1b[H");
        v.resize(v.len() + cols * lines, ch);
    }
    v
}

/// vtebench `scrolling_fullscreen`, for the first `n` letters.
pub fn scrolling_fullscreen(cols: usize, n: usize) -> Vec<u8> {
    let mut v = Vec::new();
    for &ch in letters(n) {
        v.resize(v.len() + cols, ch);
        v.extend_from_slice(b"\r\n");
    }
    v
}

/// The codepoint ranges of vtebench's `unicode/symbols`, in order; `None` is a newline.
const SYMBOL_RANGES: &[Option<(u32, u32)>] = &[
    Some((0xa1, 0x304)),
    Some((0x370, 0x487)),
    Some((0x48a, 0x595)),
    Some((0x5be, 0x604)),
    Some((0x606, 0x614)),
    Some((0x61b, 0x64f)),
    Some((0x660, 0x6da)),
    Some((0x6de, 0x6e3)),
    Some((0x6e5, 0x734)),
    Some((0x74b, 0x7aa)),
    Some((0x910, 0x945)),
    Some((0x949, 0x955)),
    Some((0x958, 0xa36)),
    Some((0x10a1, 0x1164)),
    Some((0x1200, 0x167d)),
    Some((0x1d00, 0x1dc4)),
    Some((0x1dfa, 0x206a)),
    Some((0x2070, 0x20d4)),
    Some((0x20f1, 0x28ff)),
    Some((0x2e80, 0x9fef)),
    Some((0xac00, 0xd7fb)),
    None,
    Some((0x1f600, 0x1f64f)),
    None,
];

/// vtebench `unicode`: its symbol list, generated.
pub fn unicode_symbols() -> Vec<u8> {
    let mut s = String::new();
    for r in SYMBOL_RANGES {
        match r {
            Some((a, b)) => s.extend((*a..=*b).filter_map(char::from_u32)),
            None => s.push_str("\r\n"),
        }
    }
    s.into_bytes()
}

/// A recorded vtebench file, if the reference checkout is present.
pub fn vtebench_file(rel: &str) -> Option<Vec<u8>> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../refs/vtebench/benchmarks").join(rel);
    std::fs::read(path).ok().map(|b| onlcr(&b))
}

/// The vtebench payloads; `scale` (1..=26) limits the letter loops so tests stay small.
pub fn vtebench(cols: usize, lines: usize, scale: usize) -> Vec<Payload> {
    let alt = b"\x1b[?1049h".to_vec();
    let region = |top: usize, bottom: usize| format!("\x1b[?1049h\x1b[{top};{bottom}r").into_bytes();
    let mut v = vec![
        Payload { name: "cursor_motion", setup: Vec::new(), body: cursor_motion(cols, lines, scale) },
        Payload { name: "dense_cells", setup: alt.clone(), body: dense_cells(cols, lines, scale) },
        Payload { name: "light_cells", setup: alt.clone(), body: light_cells(cols, lines, scale) },
        Payload { name: "scrolling", setup: b"y\r\n".to_vec(), body: b"y\r\n".to_vec() },
        Payload { name: "scrolling_bottom_region", setup: region(1, lines - 1), body: b"y\r\n".to_vec() },
        Payload { name: "scrolling_bottom_small_region", setup: region(1, lines / 2), body: b"y\r\n".to_vec() },
        Payload { name: "scrolling_fullscreen", setup: b"y\r\n".to_vec(), body: scrolling_fullscreen(cols, scale) },
        Payload { name: "scrolling_top_region", setup: region(2, lines), body: b"y\r\n".to_vec() },
        Payload { name: "scrolling_top_small_region", setup: region(lines / 2, lines), body: b"y\r\n".to_vec() },
        Payload { name: "unicode", setup: alt, body: unicode_symbols() },
    ];
    for (name, file) in
        [("medium_cells", "medium_cells/vim_session"), ("sync_medium_cells", "sync_medium_cells/vim_session")]
    {
        if let Some(body) = vtebench_file(file) {
            v.push(Payload { name, setup: Vec::new(), body });
        }
    }
    v
}

const WORDS: &[&str] = &[
    "src",
    "target",
    "Cargo.toml",
    "README.md",
    "main.rs",
    "lib.rs",
    "parser",
    "grid",
    "build.sh",
    "notes.txt",
    "a.out",
    "config",
    "assets",
    "font.ttf",
    "image.png",
    "archive.tar.gz",
    "LICENSE",
    "docs",
    "tests",
    "bench",
];

/// `ls --color` style output: colored names in columns.
pub fn colored_ls(cols: usize, lines: usize, seed: u64) -> Vec<u8> {
    let mut r = Rng::new(seed);
    let mut s = String::new();
    for _ in 0..lines {
        let mut used = 0;
        loop {
            let name = format!("{}{}", r.pick(WORDS), r.below(100));
            if used + name.len() + 2 > cols {
                break;
            }
            let color =
                *r.pick(&["01;34", "01;32", "0", "01;36", "01;31", "38;5;208", "38;2;10;200;30", "4:3;58:2::255:0:0"]);
            let _ = write!(s, "\x1b[{color}m{name}\x1b[0m  ");
            used += name.len() + 2;
        }
        s.push_str("\r\n");
    }
    s.into_bytes()
}

/// A vim-like editing session: alternate screen, DECSTBM scrolling with LF
/// and RI, line numbers, a status line, IL/DL at column 1.
pub fn vim_redraw(cols: usize, lines: usize, steps: usize, seed: u64) -> Vec<u8> {
    let mut r = Rng::new(seed);
    let text = |r: &mut Rng, w: usize| {
        let mut t = String::new();
        while t.len() + 12 < w {
            t.push_str(r.pick(WORDS));
            t.push(' ');
        }
        t
    };
    let mut s = String::from("\x1b[?1049h\x1b[22;0;0t\x1b[?1h\x1b=\x1b[H\x1b[2J");
    let bottom = lines - 2;
    for y in 1..=bottom {
        let _ = write!(s, "\x1b[{y};1H\x1b[33m{y:4} \x1b[0m{}\x1b[K", text(&mut r, cols - 6));
    }
    let _ = write!(s, "\x1b[{};1H\x1b[7m status \x1b[0m\x1b[K", lines - 1);
    for step in 0..steps {
        let _ = write!(s, "\x1b[1;{bottom}r");
        match r.below(5) {
            0 => {
                let _ = write!(s, "\x1b[{bottom};1H\n\x1b[33m{:4} \x1b[0m{}", step, text(&mut r, cols - 6));
            }
            1 => {
                let _ = write!(s, "\x1b[1;1H\x1bM\x1b[33m{:4} \x1b[0m{}", step, text(&mut r, cols - 6));
            }
            2 => {
                let y = 1 + r.below(bottom);
                let _ = write!(s, "\x1b[{y};1H\x1b[{}L\x1b[1m{}\x1b[22m", 1 + r.below(3), text(&mut r, cols - 6));
            }
            3 => {
                let y = 1 + r.below(bottom);
                let _ = write!(s, "\x1b[{y};1H\x1b[{}M", 1 + r.below(3));
            }
            _ => {
                let y = 1 + r.below(bottom);
                let x = 1 + r.below(cols / 2);
                let _ = write!(s, "\x1b[{y};{x}H\x1b[38;5;{}m{}\x1b[m\x1b[K", r.below(256), text(&mut r, cols / 2 - 2));
            }
        }
        let _ = write!(
            s,
            "\x1b[r\x1b[{lines};1H\x1b[K-- INSERT -- {step}\x1b[{};{}H",
            1 + r.below(bottom),
            1 + r.below(cols)
        );
    }
    s.push_str("\x1b[?1049l\x1b[23;0;0t");
    s.into_bytes()
}

/// Full-screen application output with cursor addressing and editing on
/// the alternate screen.
pub fn alt_screen(cols: usize, lines: usize, steps: usize, seed: u64) -> Vec<u8> {
    let mut r = Rng::new(seed);
    let mut s = String::from("\x1b[?1049h\x1b[?25l");
    for _ in 0..steps {
        let y = 1 + r.below(lines);
        let x = 1 + r.below(cols);
        let _ = write!(s, "\x1b[{y};{x}H");
        match r.below(8) {
            0 => {
                let _ = write!(s, "\x1b[{}X", 1 + r.below(20));
            }
            1 => {
                let _ = write!(s, "\x1b[{}K", r.below(3));
            }
            2 => {
                let _ = write!(s, "\x1b[{}@", 1 + r.below(10));
            }
            3 => {
                // alacritty clears cells left of the cursor when DCH exceeds the rest of the row.
                let _ = write!(s, "\x1b[{}P", 1 + r.below(10.min(cols + 1 - x)));
            }
            4 => {
                let _ = write!(s, "\x1b[{}b", r.below(30));
            }
            5 => {
                let _ = write!(
                    s,
                    "\x1b[{}A\x1b[{}C\x1b[{}B\x1b[{}D\x1b[{}G\x1b[{}d.",
                    1 + r.below(5),
                    r.below(9),
                    r.below(4),
                    r.below(7),
                    1 + r.below(cols),
                    1 + r.below(lines)
                );
            }
            6 => {
                let _ = write!(s, "\x1b7\x1b[1;1Hsaved\x1b8!\x1b[s\x1b[{lines};1Hscored\x1b[u?");
            }
            _ => {
                let w = r.below(cols);
                let t: String = (0..w).map(|i| (b'a' + (i % 26) as u8) as char).collect();
                let _ = write!(s, "\x1b[3{}m{t}\x1b[0m", r.below(8));
            }
        }
    }
    s.push_str("\x1b[?25h\x1b[?1049l");
    s.into_bytes()
}

/// Mixed CJK, emoji, combining marks and ASCII, wrapping at the margin.
pub fn cjk_emoji(lines: usize, seed: u64) -> Vec<u8> {
    let mut r = Rng::new(seed);
    let pieces = [
        "\u{65e5}\u{672c}\u{8a9e}",
        "\u{4e2d}\u{6587}\u{5b57}",
        "\u{d55c}\u{ad6d}\u{c5b4}",
        "\u{1f600}",
        "\u{1f680}\u{fe0f}",
        "\u{1f468}\u{200d}\u{1f469}\u{200d}\u{1f467}",
        "e\u{301}",
        "a\u{308}\u{323}",
        "abc ",
        "x",
        "\u{ff21}\u{ff22}",
        "\u{3042}\u{3044}\u{3046}",
        "-",
        "\u{1f1ef}\u{1f1f5}",
    ];
    let mut s = String::new();
    for _ in 0..lines {
        let n = 1 + r.below(60);
        for _ in 0..n {
            s.push_str(r.pick(&pieces));
        }
        s.push_str("\r\n");
    }
    s.into_bytes()
}

/// Long lines that wrap several times, with color changes inside.
pub fn long_lines(lines: usize, seed: u64) -> Vec<u8> {
    let mut r = Rng::new(seed);
    let mut s = String::new();
    for i in 0..lines {
        let len = 100 + r.below(900);
        let mut n = 0;
        while n < len {
            let w = r.pick(WORDS);
            if r.below(4) == 0 {
                let _ = write!(s, "\x1b[3{}m{w}\x1b[39m ", r.below(8));
            } else {
                let _ = write!(s, "{w} ");
            }
            n += w.len() + 1;
        }
        let _ = write!(s, "<{i}>\r\n");
    }
    s.into_bytes()
}

/// An interactive shell: titles, bracketed paste, prompt redraws with EL,
/// typed commands with backspaces, colored command output, tabs.
pub fn shell_session(commands: usize, seed: u64) -> Vec<u8> {
    let mut r = Rng::new(seed);
    let mut s = String::new();
    for i in 0..commands {
        let _ = write!(s, "\x1b]0;user@host: ~/project\x07\x1b]7;file://host/home/user/project\x1b\\");
        let prompt = "\x1b[1;32muser@host\x1b[0m:\x1b[1;34m~/project\x1b[0m$ ";
        let _ = write!(s, "\x1b[?2004h{prompt}");
        let cmd = *r.pick(&["ls -la", "git status", "cargo build", "cat notes.txt", "echo hello"]);
        for ch in cmd.chars() {
            s.push(ch);
            if r.below(10) == 0 {
                s.push_str("x\x08\x1b[K");
            }
        }
        let _ = write!(s, "\x1b[?2004l\r\r\n");
        for _ in 0..r.below(30) {
            match r.below(4) {
                0 => {
                    let _ = write!(
                        s,
                        "-rw-r--r--\t1 user user\t{}\tNov 11 05:{:02}\t\x1b[01;34m{}\x1b[0m\r\n",
                        r.below(100000),
                        r.below(60),
                        r.pick(WORDS)
                    );
                }
                1 => {
                    let _ = write!(
                        s,
                        "\x1b[32m   Compiling\x1b[0m {} v0.1.{} (/home/user/project)\r\n",
                        r.pick(WORDS),
                        r.below(10)
                    );
                }
                2 => {
                    let _ = write!(s, "\tmodified:   \x1b[31m{}\x1b[m\r\n", r.pick(WORDS));
                }
                _ => {
                    let _ = write!(s, "line {i} of output with some text\r\n");
                }
            }
        }
    }
    s.into_bytes()
}

/// The realistic payloads.
pub fn realistic(cols: usize, lines: usize, scale: usize) -> Vec<Payload> {
    vec![
        Payload { name: "colored_ls", setup: Vec::new(), body: colored_ls(cols, 40 * scale, 1) },
        Payload { name: "vim_redraw", setup: Vec::new(), body: vim_redraw(cols, lines, 60 * scale, 2) },
        Payload { name: "alt_screen", setup: Vec::new(), body: alt_screen(cols, lines, 300 * scale, 3) },
        Payload { name: "cjk_emoji", setup: Vec::new(), body: cjk_emoji(30 * scale, 4) },
        Payload { name: "long_lines", setup: Vec::new(), body: long_lines(20 * scale, 5) },
        Payload { name: "shell_session", setup: Vec::new(), body: shell_session(10 * scale, 6) },
    ]
}
