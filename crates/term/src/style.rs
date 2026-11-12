//! The per-terminal style table, the packed pen and SGR.

use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hasher};

use crate::cell::{Attrs, Color, Style, StyleId, Underline};
use crate::parser::Params;

/// A style packed into 97 bits, so that SGR edits bit fields and the packed
/// value is directly the style table key. 0 is the default style.
///
/// Colors take 26 bits: a 2-bit tag (0 default, 1 indexed, 2 RGB) above 24
/// bits of payload. Fields: fg at 0, bg at 26, underline color at 52,
/// underline shape at 78 (3 bits), attributes at 81 (16 bits).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Pen(pub u128);

const COLOR_MASK: u128 = (1 << 26) - 1;
const FG: u32 = 0;
const BG: u32 = 26;
const UL_COLOR: u32 = 52;
const UNDERLINE: u32 = 78;
const ATTRS: u32 = 81;

#[inline(always)]
fn color_bits(c: Color) -> u128 {
    match c {
        Color::Default => 0,
        Color::Indexed(i) => (1 << 24) | i as u128,
        Color::Rgb(r, g, b) => (2 << 24) | ((r as u128) << 16) | ((g as u128) << 8) | b as u128,
    }
}

fn color(bits: u128) -> Color {
    let v = bits as u32;
    match v >> 24 {
        0 => Color::Default,
        1 => Color::Indexed(v as u8),
        _ => Color::Rgb((v >> 16) as u8, (v >> 8) as u8, v as u8),
    }
}

impl Pen {
    pub const DEFAULT: Pen = Pen(0);

    #[inline(always)]
    fn set_color(&mut self, at: u32, c: u128) {
        self.0 = (self.0 & !(COLOR_MASK << at)) | (c << at);
    }

    #[inline(always)]
    fn set_underline(&mut self, u: Underline) {
        self.0 = (self.0 & !(7 << UNDERLINE)) | ((u as u128) << UNDERLINE);
    }

    #[inline(always)]
    fn set_attrs(&mut self, on: u16) {
        self.0 |= (on as u128) << ATTRS;
    }

    #[inline(always)]
    fn clear_attrs(&mut self, off: u16) {
        self.0 &= !((off as u128) << ATTRS);
    }

    /// The pen with only its background, which erased cells take.
    #[inline(always)]
    pub fn background(self) -> Pen {
        Pen(self.0 & (COLOR_MASK << BG))
    }

    pub fn from_style(s: &Style) -> Pen {
        Pen(color_bits(s.fg) << FG
            | color_bits(s.bg) << BG
            | color_bits(s.underline_color) << UL_COLOR
            | (s.underline as u128) << UNDERLINE
            | (s.attrs.0 as u128) << ATTRS)
    }

    pub fn style(self) -> Style {
        let k = self.0;
        Style {
            fg: color(k >> FG & COLOR_MASK),
            bg: color(k >> BG & COLOR_MASK),
            underline_color: color(k >> UL_COLOR & COLOR_MASK),
            underline: match k >> UNDERLINE & 7 {
                0 => Underline::None,
                1 => Underline::Single,
                2 => Underline::Double,
                3 => Underline::Curly,
                4 => Underline::Dotted,
                _ => Underline::Dashed,
            },
            attrs: Attrs((k >> ATTRS) as u16),
        }
    }
}

/// One folded multiply of a pen.
#[inline(always)]
fn mix(v: u128) -> u64 {
    let m = ((v as u64) ^ 0x243f_6a88_85a3_08d3) as u128 * (((v >> 64) as u64) ^ 0x1319_8a2e_0370_7345) as u128;
    (m as u64) ^ ((m >> 64) as u64)
}

#[derive(Default)]
pub(crate) struct KeyHasher(u64);

impl Hasher for KeyHasher {
    #[inline]
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 = (self.0.rotate_left(5) ^ b as u64).wrapping_mul(0x517c_c1b7_2722_0a95);
        }
    }

    #[inline]
    fn write_u128(&mut self, v: u128) {
        self.0 = mix(v);
    }
}

const CACHE: usize = 256;
/// Never a pen, which uses 97 bits.
const NO_PEN: u128 = u128::MAX;

/// Append-only table of styles; id 0 is the default style.
#[derive(Clone)]
pub(crate) struct StyleTable {
    styles: Vec<Style>,
    ids: HashMap<u128, StyleId, BuildHasherDefault<KeyHasher>>,
    /// Direct-mapped cache of recent lookups in front of `ids`.
    cache: Box<[(u128, StyleId); CACHE]>,
}

impl StyleTable {
    pub fn new() -> StyleTable {
        StyleTable {
            styles: vec![Style::DEFAULT],
            ids: HashMap::default(),
            cache: Box::new([(NO_PEN, StyleId::DEFAULT); CACHE]),
        }
    }

    #[inline]
    pub fn get(&self, id: StyleId) -> &Style {
        self.styles.get(id.0 as usize).unwrap_or(&Style::DEFAULT)
    }

    pub fn len(&self) -> usize {
        self.styles.len()
    }

    /// The id of `pen`, or `None` when all 65535 ids are taken.
    #[inline]
    pub fn intern(&mut self, pen: Pen) -> Option<StyleId> {
        let k = pen.0;
        if k == 0 {
            return Some(StyleId::DEFAULT);
        }
        let slot = (mix(k) >> 56) as usize;
        if self.cache[slot].0 == k {
            return Some(self.cache[slot].1);
        }
        let id = self.lookup(pen)?;
        self.cache[slot] = (k, id);
        Some(id)
    }

    fn lookup(&mut self, pen: Pen) -> Option<StyleId> {
        if let Some(&id) = self.ids.get(&pen.0) {
            return Some(id);
        }
        if self.styles.len() > u16::MAX as usize {
            return None;
        }
        let id = StyleId(self.styles.len() as u16);
        self.styles.push(pen.style());
        self.ids.insert(pen.0, id);
        Some(id)
    }

    /// Drops the ids not marked in `live` and renumbers the rest densely;
    /// returns the old-to-new id map (dropped ids map to 0).
    pub fn compact(&mut self, live: &[bool]) -> Vec<u16> {
        let old = std::mem::replace(&mut self.styles, vec![Style::DEFAULT]);
        self.ids.clear();
        self.cache.fill((NO_PEN, StyleId::DEFAULT));
        let mut remap = vec![0u16; old.len()];
        for (id, s) in old.into_iter().enumerate().skip(1) {
            if live[id] {
                let new = StyleId(self.styles.len() as u16);
                self.styles.push(s);
                self.ids.insert(Pen::from_style(&s).0, new);
                remap[id] = new.0;
            }
        }
        remap
    }
}

#[inline(always)]
fn byte(v: u16) -> u128 {
    v.min(255) as u128
}

#[inline(always)]
fn indexed(i: u16) -> u128 {
    (1 << 24) | byte(i)
}

#[inline(always)]
fn rgb(r: u16, g: u16, b: u16) -> u128 {
    (2 << 24) | byte(r) << 16 | byte(g) << 8 | byte(b)
}

/// Parses an extended color (38/48/58) starting at param `i` whose
/// colon-joined group ends at `end`; returns its bits and the next param.
#[inline]
fn extended_color(p: &Params, i: usize, end: usize) -> (Option<u128>, usize) {
    if end > i + 1 {
        let n = end - i;
        let c = match p.get(i + 1) {
            5 if n >= 3 => Some(indexed(p.get(i + 2))),
            // 38:2:<colorspace>:r:g:b
            2 if n >= 6 => Some(rgb(p.get(i + 3), p.get(i + 4), p.get(i + 5))),
            // 38:2:r:g:b
            2 if n == 5 => Some(rgb(p.get(i + 2), p.get(i + 3), p.get(i + 4))),
            _ => None,
        };
        return (c, end);
    }
    match p.get(i + 1) {
        5 if i + 2 < p.len() => (Some(indexed(p.get(i + 2))), i + 3),
        2 if i + 4 < p.len() => (Some(rgb(p.get(i + 2), p.get(i + 3), p.get(i + 4))), i + 5),
        _ => (None, p.len()),
    }
}

/// Applies SGR parameters to `pen`.
#[inline]
pub(crate) fn apply_sgr(pen: &mut Pen, p: &Params) {
    let len = p.len();
    if len == 0 {
        *pen = Pen::DEFAULT;
        return;
    }
    let mut i = 0;
    while i < len {
        let v = p.get(i);
        let mut end = i + 1;
        while p.has_subs() && end < len && p.is_sub(end) {
            end += 1;
        }
        match v {
            0 => *pen = Pen::DEFAULT,
            1 => pen.set_attrs(Attrs::BOLD),
            2 => pen.set_attrs(Attrs::DIM),
            3 => pen.set_attrs(Attrs::ITALIC),
            4 => pen.set_underline(if end > i + 1 {
                match p.get(i + 1) {
                    0 => Underline::None,
                    2 => Underline::Double,
                    3 => Underline::Curly,
                    4 => Underline::Dotted,
                    5 => Underline::Dashed,
                    _ => Underline::Single,
                }
            } else {
                Underline::Single
            }),
            5 | 6 => pen.set_attrs(Attrs::BLINK),
            7 => pen.set_attrs(Attrs::INVERSE),
            8 => pen.set_attrs(Attrs::HIDDEN),
            9 => pen.set_attrs(Attrs::STRIKE),
            21 => pen.set_underline(Underline::Double),
            22 => pen.clear_attrs(Attrs::BOLD | Attrs::DIM),
            23 => pen.clear_attrs(Attrs::ITALIC),
            24 => pen.set_underline(Underline::None),
            25 => pen.clear_attrs(Attrs::BLINK),
            27 => pen.clear_attrs(Attrs::INVERSE),
            28 => pen.clear_attrs(Attrs::HIDDEN),
            29 => pen.clear_attrs(Attrs::STRIKE),
            30..=37 => pen.set_color(FG, indexed(v - 30)),
            39 => pen.set_color(FG, 0),
            40..=47 => pen.set_color(BG, indexed(v - 40)),
            49 => pen.set_color(BG, 0),
            53 => pen.set_attrs(Attrs::OVERLINE),
            55 => pen.clear_attrs(Attrs::OVERLINE),
            59 => pen.set_color(UL_COLOR, 0),
            90..=97 => pen.set_color(FG, indexed(v - 90 + 8)),
            100..=107 => pen.set_color(BG, indexed(v - 100 + 8)),
            38 | 48 | 58 => {
                let (c, next) = extended_color(p, i, end);
                if let Some(c) = c {
                    let at = match v {
                        38 => FG,
                        48 => BG,
                        _ => UL_COLOR,
                    };
                    pen.set_color(at, c);
                }
                end = next;
            }
            _ => {}
        }
        i = end;
    }
}
