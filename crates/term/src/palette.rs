//! The default palette and X11 color specifications (OSC 4/10/11/12).

use crate::{Palette, Rgb};

const ANSI: [(u8, u8, u8); 16] = [
    (0x00, 0x00, 0x00),
    (0xcd, 0x00, 0x00),
    (0x00, 0xcd, 0x00),
    (0xcd, 0xcd, 0x00),
    (0x00, 0x00, 0xee),
    (0xcd, 0x00, 0xcd),
    (0x00, 0xcd, 0xcd),
    (0xe5, 0xe5, 0xe5),
    (0x7f, 0x7f, 0x7f),
    (0xff, 0x00, 0x00),
    (0x00, 0xff, 0x00),
    (0xff, 0xff, 0x00),
    (0x5c, 0x5c, 0xff),
    (0xff, 0x00, 0xff),
    (0x00, 0xff, 0xff),
    (0xff, 0xff, 0xff),
];

/// xterm's 256 colors: 16 ANSI colors, the 6x6x6 cube and 24 grays.
pub(crate) fn xterm_indexed() -> [Rgb; 256] {
    let mut c = [Rgb::default(); 256];
    for (i, &(r, g, b)) in ANSI.iter().enumerate() {
        c[i] = Rgb { r, g, b };
    }
    let level = |v: usize| if v == 0 { 0 } else { (55 + v * 40) as u8 };
    for i in 0..216 {
        c[16 + i] = Rgb { r: level(i / 36), g: level(i / 6 % 6), b: level(i % 6) };
    }
    for i in 0..24 {
        let v = (8 + i * 10) as u8;
        c[232 + i] = Rgb { r: v, g: v, b: v };
    }
    c
}

pub(crate) fn default_palette() -> Palette {
    Palette {
        indexed: xterm_indexed(),
        foreground: Rgb { r: 0xe5, g: 0xe5, b: 0xe5 },
        background: Rgb { r: 0x00, g: 0x00, b: 0x00 },
        cursor: None,
    }
}

fn hex(s: &[u8]) -> Option<u32> {
    if s.is_empty() || s.len() > 4 {
        return None;
    }
    s.iter().try_fold(0u32, |v, &b| Some(v * 16 + (b as char).to_digit(16)?))
}

/// Parses `rgb:r/g/b` (1 to 4 hex digits each, scaled) and `#rgb`,
/// `#rrggbb`, `#rrrgggbbb`, `#rrrrggggbbbb` (most significant bits).
pub(crate) fn parse_color(s: &[u8]) -> Option<Rgb> {
    if let Some(rest) = s.strip_prefix(b"rgb:") {
        let mut it = rest.split(|&b| b == b'/');
        let mut comp = || {
            let h = it.next()?;
            let v = hex(h)?;
            let max = (1u32 << (4 * h.len())) - 1;
            Some(((v * 255 + max / 2) / max) as u8)
        };
        let (r, g, b) = (comp()?, comp()?, comp()?);
        return if it.next().is_none() { Some(Rgb { r, g, b }) } else { None };
    }
    let h = s.strip_prefix(b"#")?;
    if h.is_empty() || h.len() % 3 != 0 || h.len() > 12 {
        return None;
    }
    let k = h.len() / 3;
    let comp = |i: usize| -> Option<u8> {
        let v = hex(&h[i * k..(i + 1) * k])?;
        Some(if k == 1 { v << 4 } else { v >> (4 * (k - 2)) } as u8)
    };
    Some(Rgb { r: comp(0)?, g: comp(1)?, b: comp(2)? })
}
