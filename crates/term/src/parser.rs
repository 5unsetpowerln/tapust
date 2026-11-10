//! DEC/xterm-compatible escape sequence parser (the vt100.net state machine)
//! with fast paths for printable ASCII, UTF-8 runs and complete CSI/ESC
//! sequences in the ground state.

use crate::term::Term;

pub(crate) const MAX_PARAMS: usize = 32;
const MAX_OSC: usize = 4096;
const ESC: u8 = 0x1B;

/// CSI parameters; colon-separated subparameters are flagged in `subs`.
pub(crate) struct Params {
    vals: [u16; MAX_PARAMS],
    subs: u32,
    len: usize,
}

impl Params {
    const fn new() -> Params {
        Params { vals: [0; MAX_PARAMS], subs: 0, len: 0 }
    }

    #[inline(always)]
    pub fn len(&self) -> usize {
        self.len
    }

    /// Parameter `i`, 0 when absent or empty.
    #[inline(always)]
    pub fn get(&self, i: usize) -> u16 {
        if i < self.len { self.vals[i] } else { 0 }
    }

    /// Parameter `i` as a count: absent or 0 means 1.
    #[inline(always)]
    pub fn count(&self, i: usize) -> usize {
        self.get(i).max(1) as usize
    }

    #[inline(always)]
    pub fn has_subs(&self) -> bool {
        self.subs != 0
    }

    /// True if parameter `i` was joined to the previous one with ':'.
    #[inline(always)]
    pub fn is_sub(&self, i: usize) -> bool {
        self.subs >> i & 1 != 0
    }

    #[inline(always)]
    fn clear(&mut self) {
        self.len = 0;
        self.subs = 0;
    }

    #[inline(always)]
    fn push(&mut self, v: u32, sub: bool) {
        if self.len < MAX_PARAMS {
            self.vals[self.len] = v as u16;
            self.subs |= (sub as u32) << self.len;
            self.len += 1;
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = u16> + '_ {
        self.vals[..self.len].iter().copied()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    Ground,
    Escape,
    EscInter,
    CsiEntry,
    CsiParam,
    CsiInter,
    CsiIgnore,
    DcsEntry,
    DcsParam,
    DcsInter,
    /// DCS passthrough, DCS ignore and SOS/PM/APC strings: skipped until ST.
    StringIgnore,
    Osc,
}

pub(crate) struct Parser {
    state: State,
    params: Params,
    acc: u32,
    next_sub: bool,
    private: u8,
    inter: u8,
    n_inter: u8,
    osc: Vec<u8>,
    osc_overflow: bool,
    utf8: [u8; 4],
    utf8_len: u8,
}

/// Length of the printable ASCII (0x20..=0x7E) prefix of `s`.
#[inline]
fn printable_prefix(s: &[u8]) -> usize {
    #[inline(always)]
    fn printable(b: u8) -> bool {
        b.wrapping_sub(0x20) < 0x5F
    }
    match s {
        [b, ..] if !printable(*b) => return 0,
        // One-character runs (as between escapes, or before CR LF) skip the vector setup.
        [_, b, ..] if !printable(*b) => return 1,
        [] => return 0,
        _ => {}
    }
    let mut i = 0;
    #[cfg(target_arch = "x86_64")]
    {
        use core::arch::x86_64::*;
        // SAFETY: SSE2 is part of the x86_64 baseline and every load reads
        // 16 bytes inside `s`.
        unsafe {
            let us = _mm_set1_epi8(0x1F);
            let del = _mm_set1_epi8(0x7F);
            while i + 16 <= s.len() {
                let v = _mm_loadu_si128(s.as_ptr().add(i) as *const __m128i);
                // Bytes >= 0x80 are negative as i8, so the signed compare rejects them.
                let ok = _mm_andnot_si128(_mm_cmpeq_epi8(v, del), _mm_cmpgt_epi8(v, us));
                let mask = _mm_movemask_epi8(ok) as u32;
                if mask != 0xFFFF {
                    return i + (!mask).trailing_zeros() as usize;
                }
                i += 16;
            }
        }
    }
    while i < s.len() && printable(s[i]) {
        i += 1;
    }
    i
}

/// Position of the first CAN, SUB or ESC, which end ignored strings.
#[inline]
fn string_end(s: &[u8]) -> usize {
    s.iter().position(|&b| b == 0x18 || b == 0x1A || b == ESC).unwrap_or(s.len())
}

/// Total length of a UTF-8 sequence starting with `b`, and the allowed range
/// of its second byte; `None` for bytes that cannot start a sequence.
#[inline(always)]
fn utf8_lead(b: u8) -> Option<(usize, u8, u8)> {
    Some(match b {
        0xC2..=0xDF => (2, 0x80, 0xBF),
        0xE0 => (3, 0xA0, 0xBF),
        0xE1..=0xEC | 0xEE..=0xEF => (3, 0x80, 0xBF),
        0xED => (3, 0x80, 0x9F),
        0xF0 => (4, 0x90, 0xBF),
        0xF1..=0xF3 => (4, 0x80, 0xBF),
        0xF4 => (4, 0x80, 0x8F),
        _ => return None,
    })
}

#[inline(always)]
fn utf8_decode(s: &[u8]) -> u32 {
    let mut cp = (s[0] as u32) & (0x7F >> s.len());
    for &b in &s[1..] {
        cp = (cp << 6) | (b & 0x3F) as u32;
    }
    cp
}

const RUN: usize = 256;

impl Parser {
    pub fn new() -> Parser {
        Parser {
            state: State::Ground,
            params: Params::new(),
            acc: 0,
            next_sub: false,
            private: 0,
            inter: 0,
            n_inter: 0,
            osc: Vec::new(),
            osc_overflow: false,
            utf8: [0; 4],
            utf8_len: 0,
        }
    }

    pub fn advance(&mut self, t: &mut Term, bytes: &[u8]) {
        let mut i = 0;
        if self.utf8_len != 0 {
            i = self.finish_utf8(t, bytes);
        }
        while i < bytes.len() {
            i = if self.state == State::Ground { self.ground(t, bytes, i) } else { self.escape(t, bytes, i) };
        }
    }

    fn ground(&mut self, t: &mut Term, bytes: &[u8], mut i: usize) -> usize {
        let len = bytes.len();
        while i < len {
            let n = printable_prefix(&bytes[i..]);
            if n != 0 {
                t.print_ascii(&bytes[i..i + n]);
                i += n;
                if i == len {
                    break;
                }
            }
            let b = bytes[i];
            match b {
                ESC => match self.fast_escape(t, &bytes[i..]) {
                    Some(k) => i += k,
                    None => {
                        self.state = State::Escape;
                        self.n_inter = 0;
                        return i + 1;
                    }
                },
                b'\n' => {
                    t.linefeed();
                    i += 1;
                }
                b'\r' => {
                    t.carriage_return();
                    i += 1;
                    if bytes.get(i) == Some(&b'\n') {
                        t.linefeed();
                        i += 1;
                    }
                }
                0..=0x1F => {
                    t.execute(b);
                    i += 1;
                }
                0x7F => i += 1,
                _ => {
                    i = self.utf8_run(t, bytes, i);
                    if self.utf8_len != 0 {
                        break;
                    }
                }
            }
        }
        i
    }

    /// Decodes printable text starting at a non-ASCII byte up to the next
    /// control byte and prints it as runs; keeps an incomplete trailing
    /// sequence for the next call.
    fn utf8_run(&mut self, t: &mut Term, bytes: &[u8], mut i: usize) -> usize {
        let mut buf = [0u32; RUN];
        let mut n = 0;
        let len = bytes.len();
        while i < len {
            let b = bytes[i];
            let cp = if b < 0x80 {
                if b < 0x20 || b == 0x7F {
                    break;
                }
                i += 1;
                b as u32
            } else if let Some((need, lo, hi)) = utf8_lead(b) {
                let mut k = 1;
                while k < need {
                    if i + k == len {
                        self.utf8[..k].copy_from_slice(&bytes[i..len]);
                        self.utf8_len = k as u8;
                        if n != 0 {
                            t.print_run(&buf[..n]);
                        }
                        return len;
                    }
                    let c = bytes[i + k];
                    let (lo, hi) = if k == 1 { (lo, hi) } else { (0x80, 0xBF) };
                    if c < lo || c > hi {
                        break;
                    }
                    k += 1;
                }
                let cp = if k == need { utf8_decode(&bytes[i..i + need]) } else { 0xFFFD };
                i += k;
                cp
            } else {
                i += 1;
                0xFFFD
            };
            buf[n] = cp;
            n += 1;
            if n == RUN {
                t.print_run(&buf);
                n = 0;
            }
        }
        if n != 0 {
            t.print_run(&buf[..n]);
        }
        i
    }

    /// Completes a UTF-8 sequence split across calls; returns bytes consumed.
    fn finish_utf8(&mut self, t: &mut Term, bytes: &[u8]) -> usize {
        let (need, lo, hi) = utf8_lead(self.utf8[0]).expect("partial UTF-8 starts with a lead byte");
        let mut i = 0;
        while (self.utf8_len as usize) < need {
            let Some(&c) = bytes.get(i) else { return i };
            let k = self.utf8_len as usize;
            let (lo, hi) = if k == 1 { (lo, hi) } else { (0x80, 0xBF) };
            if c < lo || c > hi {
                self.utf8_len = 0;
                t.print_run(&[0xFFFD]);
                return i;
            }
            self.utf8[k] = c;
            self.utf8_len += 1;
            i += 1;
        }
        self.utf8_len = 0;
        t.print_run(&[utf8_decode(&self.utf8[..need])]);
        i
    }

    /// Dispatches a complete ESC or CSI sequence at the start of `s` without
    /// entering the state machine; `None` if it is incomplete or unusual.
    #[inline(always)]
    fn fast_escape(&mut self, t: &mut Term, s: &[u8]) -> Option<usize> {
        let b1 = *s.get(1)?;
        match b1 {
            b'[' => self.fast_csi(t, s),
            b']' | b'P' | b'X' | b'^' | b'_' => None,
            0x30..=0x7E => {
                t.esc_dispatch(0, b1);
                Some(2)
            }
            0x20..=0x2F => {
                let b2 = *s.get(2)?;
                if (0x30..=0x7E).contains(&b2) {
                    t.esc_dispatch(b1, b2);
                    Some(3)
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    #[inline(always)]
    fn fast_csi(&mut self, t: &mut Term, s: &[u8]) -> Option<usize> {
        let mut j = 2;
        let mut private = 0;
        let first = *s.get(j)?;
        if (0x3C..=0x3F).contains(&first) {
            private = first;
            j += 1;
        }
        // The count and the subparameter mask stay in registers until dispatch.
        let p = &mut self.params;
        let (mut len, mut subs) = (0, 0u32);
        let mut acc: u32 = 0;
        let mut sub = false;
        let mut push = |acc: u32, sub: bool| {
            if len < MAX_PARAMS {
                p.vals[len] = acc as u16;
                subs |= (sub as u32) << len;
                len += 1;
            }
        };
        loop {
            let c = *s.get(j)?;
            j += 1;
            match c {
                b'0'..=b'9' => acc = (acc * 10 + (c - b'0') as u32).min(0xFFFF),
                b';' | b':' => {
                    push(acc, sub);
                    acc = 0;
                    sub = c == b':';
                }
                0x40..=0x7E => {
                    push(acc, sub);
                    (self.params.len, self.params.subs) = (len, subs);
                    t.csi_dispatch(&self.params, private, 0, c);
                    return Some(j);
                }
                0x20..=0x2F => {
                    let f = *s.get(j)?;
                    if !(0x40..=0x7E).contains(&f) {
                        return None;
                    }
                    push(acc, sub);
                    (self.params.len, self.params.subs) = (len, subs);
                    t.csi_dispatch(&self.params, private, c, f);
                    return Some(j + 1);
                }
                _ => return None,
            }
        }
    }

    fn enter_csi(&mut self) {
        self.state = State::CsiEntry;
        self.params.clear();
        self.acc = 0;
        self.next_sub = false;
        self.private = 0;
        self.n_inter = 0;
    }

    #[inline]
    fn param_byte(&mut self, b: u8) {
        match b {
            b'0'..=b'9' => self.acc = (self.acc * 10 + (b - b'0') as u32).min(0xFFFF),
            _ => {
                self.params.push(self.acc, self.next_sub);
                self.acc = 0;
                self.next_sub = b == b':';
            }
        }
    }

    fn osc_put(&mut self, s: &[u8]) {
        if self.osc.len() + s.len() > MAX_OSC {
            self.osc_overflow = true;
        } else {
            self.osc.extend_from_slice(s);
        }
    }

    fn osc_end(&mut self, t: &mut Term, bel: bool) {
        if !self.osc_overflow {
            t.osc_dispatch(&self.osc, bel);
        }
    }

    /// Runs the byte-wise state machine until it returns to ground.
    fn escape(&mut self, t: &mut Term, bytes: &[u8], mut i: usize) -> usize {
        while i < bytes.len() {
            let b = bytes[i];
            i += 1;
            match b {
                0x18 | 0x1A => {
                    self.state = State::Ground;
                    return i;
                }
                ESC => {
                    if self.state == State::Osc {
                        self.osc_end(t, false);
                    }
                    self.state = State::Escape;
                    self.n_inter = 0;
                    continue;
                }
                _ => {}
            }
            match self.state {
                State::Ground => return i - 1,
                State::Escape => match b {
                    0x00..=0x1F => t.execute(b),
                    0x20..=0x2F => {
                        self.inter = b;
                        self.n_inter = 1;
                        self.state = State::EscInter;
                    }
                    b'[' => self.enter_csi(),
                    b']' => {
                        self.osc.clear();
                        self.osc_overflow = false;
                        self.state = State::Osc;
                    }
                    b'P' => self.state = State::DcsEntry,
                    b'X' | b'^' | b'_' => self.state = State::StringIgnore,
                    0x30..=0x7E => {
                        self.state = State::Ground;
                        t.esc_dispatch(0, b);
                        return i;
                    }
                    _ => {}
                },
                State::EscInter => match b {
                    0x00..=0x1F => t.execute(b),
                    0x20..=0x2F => self.n_inter = self.n_inter.saturating_add(1),
                    0x30..=0x7E => {
                        self.state = State::Ground;
                        if self.n_inter == 1 {
                            t.esc_dispatch(self.inter, b);
                        }
                        return i;
                    }
                    _ => {}
                },
                State::CsiEntry | State::CsiParam => match b {
                    0x00..=0x1F => t.execute(b),
                    b'0'..=b';' => {
                        self.param_byte(b);
                        self.state = State::CsiParam;
                    }
                    0x3C..=0x3F => {
                        if self.state == State::CsiEntry {
                            self.private = b;
                            self.state = State::CsiParam;
                        } else {
                            self.state = State::CsiIgnore;
                        }
                    }
                    0x20..=0x2F => {
                        self.inter = b;
                        self.n_inter = 1;
                        self.state = State::CsiInter;
                    }
                    0x40..=0x7E => {
                        self.state = State::Ground;
                        self.params.push(self.acc, self.next_sub);
                        t.csi_dispatch(&self.params, self.private, 0, b);
                        return i;
                    }
                    _ => {}
                },
                State::CsiInter => match b {
                    0x00..=0x1F => t.execute(b),
                    0x20..=0x2F => self.n_inter = self.n_inter.saturating_add(1),
                    0x30..=0x3F => self.state = State::CsiIgnore,
                    0x40..=0x7E => {
                        self.state = State::Ground;
                        self.params.push(self.acc, self.next_sub);
                        if self.n_inter == 1 {
                            t.csi_dispatch(&self.params, self.private, self.inter, b);
                        }
                        return i;
                    }
                    _ => {}
                },
                State::CsiIgnore => match b {
                    0x00..=0x1F => t.execute(b),
                    0x40..=0x7E => {
                        self.state = State::Ground;
                        return i;
                    }
                    _ => {}
                },
                State::DcsEntry | State::DcsParam | State::DcsInter => match b {
                    0x20..=0x2F => self.state = State::DcsInter,
                    0x30..=0x3B if self.state != State::DcsInter => self.state = State::DcsParam,
                    0x3C..=0x3F if self.state == State::DcsEntry => self.state = State::DcsParam,
                    0x30..=0x3F => self.state = State::StringIgnore,
                    0x40..=0x7E => self.state = State::StringIgnore,
                    _ => {}
                },
                State::StringIgnore => i += string_end(&bytes[i..]),
                State::Osc => match b {
                    0x07 => {
                        self.state = State::Ground;
                        self.osc_end(t, true);
                        return i;
                    }
                    0x00..=0x1F => {}
                    _ => {
                        let rest = &bytes[i - 1..];
                        let n = rest.iter().position(|&c| c < 0x20).unwrap_or(rest.len());
                        self.osc_put(&rest[..n]);
                        i += n - 1;
                    }
                },
            }
        }
        i
    }
}
