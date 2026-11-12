//! ghostty's underline sprites (font/sprite/draw/special.zig): the shape of
//! each underline kind in one cell, from the cell size and the font's
//! underline position and thickness. Like on ghostty's sprite canvas, a shape
//! may reach a quarter cell beyond each side of the cell, and the cells of a
//! run each draw their own shape.

use std::f64::consts::{FRAC_1_SQRT_2, PI};

use term::Underline;

/// What the underline shapes depend on, in pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Metrics {
    pub cell_width: u32,
    pub cell_height: u32,
    /// The top of the underline, from the cell top.
    pub position: i32,
    pub thickness: u32,
}

/// The coverage of an underline whose top-left pixel is at (`left`, `top`)
/// from the cell's top-left corner.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Sprite {
    pub left: i32,
    pub top: i32,
    pub width: u32,
    pub height: u32,
    /// Coverage, one byte per pixel, row by row.
    pub alpha: Vec<u8>,
}

pub fn sprite(kind: Underline, m: Metrics) -> Sprite {
    let (w, h, t) = (m.cell_width as i32, m.cell_height as i32, m.thickness as i32);
    // The lowest top for `strokes` thicknesses that keeps them on the canvas.
    let y = |strokes: i32| m.position.min(h + h / 4 - strokes * t);
    // ghostty's canvas.rect calls, as (left, top, right, bottom).
    let rect = |x: i32, y: i32, width: i32| [x, y, x + width, y + t];
    let rects = match kind {
        Underline::None => return Sprite::default(),
        Underline::Single => vec![rect(0, y(1), w)],
        // A stroke one thickness above the single underline's place and one
        // a thickness below it.
        Underline::Double => vec![rect(0, (y(2) - t).max(0), w), rect(0, y(2) + t, w)],
        Underline::Dashed => {
            let dash = w / 3 + 1;
            // The last dash may reach into the next cell, as far as the canvas goes.
            (0..w / dash + 1).step_by(2).map(|i| rect(i * dash, y(1), dash)).collect()
        }
        Underline::Dotted => return dotted(m),
        Underline::Curly => return curly(m),
    };
    let bounds = rects.iter().fold([i32::MAX, i32::MAX, i32::MIN, i32::MIN], |b, r| {
        [b[0].min(r[0]), b[1].min(r[1]), b[2].max(r[2]), b[3].max(r[3])]
    });
    raster(m, bounds.map(f64::from), |x, y| {
        rects.iter().any(|r| (r[0] as f64..r[2] as f64).contains(&x) && (r[1] as f64..r[3] as f64).contains(&y))
    })
}

fn dotted(m: Metrics) -> Sprite {
    let (w, h, t) = (m.cell_width as f64, m.cell_height as f64, m.thickness as f64);
    let pad_y = (m.cell_height / 4) as f64;
    // A diameter of sqrt(2) thicknesses: dots as thick as the line look anemic.
    let r = FRAC_1_SQRT_2 * t;
    let y = (m.position as f64 + 0.5 * t).min(h + pad_y - r.ceil());
    // Gaps about as wide as the dots, but no narrower than their radius or a
    // pixel, and at least one dot.
    let count = (w / (4.0 * r)).ceil().min((w / (3.0 * r)).floor()).min((w / (2.0 * r + 1.0)).floor()).max(1.0);
    let step = w / count;
    let xs: Vec<f64> = (0..count as usize).map(|i| step / 2.0 + i as f64 * step).collect();
    let bounds = [xs[0] - r, y - r, xs[xs.len() - 1] + r, y + r];
    raster(m, bounds, |px, py| xs.iter().any(|&x| (px - x).powi(2) + (py - y).powi(2) <= r * r))
}

fn curly(m: Metrics) -> Sprite {
    let (w, h, t) = (m.cell_width as f64, m.cell_height as f64, m.thickness as f64);
    let pad_y = (m.cell_height / 4) as f64;
    let amplitude = w / PI;
    let top = (m.position as f64).min(h + pad_y - amplitude - t);
    let bottom = top + amplitude;
    // ghostty's curvature: one wave that peaks at the cell's center.
    let (r, c) = (0.4, 0.5 * w);
    let curves = [
        [(0.0, bottom), (c * r, bottom), (c - c * r, top), (c, top)],
        [(c, top), (c + c * r, top), (w - c * r, bottom), (w, bottom)],
    ];
    const STEPS: usize = 32;
    let mut points = vec![(0.0, bottom)];
    for [p0, p1, p2, p3] in curves {
        points.extend((1..=STEPS).map(|i| {
            let s = i as f64 / STEPS as f64;
            let (a, b, c, d) = ((1.0 - s).powi(3), 3.0 * (1.0 - s).powi(2) * s, 3.0 * (1.0 - s) * s * s, s.powi(3));
            (a * p0.0 + b * p1.0 + c * p2.0 + d * p3.0, a * p0.1 + b * p1.1 + c * p2.1 + d * p3.1)
        }));
    }
    // A stroke with round caps: everything within half the line width of the curve.
    let half = 0.5 * t;
    let bounds = [-half, top - half, w + half, bottom + half];
    raster(m, bounds, |px, py| points.windows(2).any(|s| segment_distance_squared((px, py), s[0], s[1]) <= half * half))
}

fn segment_distance_squared(p: (f64, f64), a: (f64, f64), b: (f64, f64)) -> f64 {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let len = dx * dx + dy * dy;
    let s = if len == 0.0 { 0.0 } else { (((p.0 - a.0) * dx + (p.1 - a.1) * dy) / len).clamp(0.0, 1.0) };
    (p.0 - a.0 - s * dx).powi(2) + (p.1 - a.1 - s * dy).powi(2)
}

/// The coverage of `inside` over the pixels that `bounds` (left, top, right,
/// bottom) touches, clipped to ghostty's canvas: the cell and a quarter cell
/// around it, from 4 x 4 samples per pixel.
fn raster(m: Metrics, bounds: [f64; 4], inside: impl Fn(f64, f64) -> bool) -> Sprite {
    let (w, h) = (m.cell_width as i32, m.cell_height as i32);
    let left = (bounds[0].floor() as i32).max(-(w / 4));
    let top = (bounds[1].floor() as i32).max(-(h / 4));
    let right = (bounds[2].ceil() as i32).min(w + w / 4);
    let bottom = (bounds[3].ceil() as i32).min(h + h / 4);
    let mut alpha = Vec::new();
    for y in top..bottom {
        for x in left..right {
            let hits = (0..16)
                .filter(|i| inside(x as f64 + (i % 4) as f64 / 4.0 + 0.125, y as f64 + (i / 4) as f64 / 4.0 + 0.125))
                .count();
            alpha.push((hits * 255 / 16) as u8);
        }
    }
    let (width, height) = ((right - left).max(0) as u32, (bottom - top).max(0) as u32);
    Sprite { left, top, width, height, alpha }
}

#[cfg(test)]
mod tests {
    use super::*;

    const M: Metrics = Metrics { cell_width: 10, cell_height: 22, position: 19, thickness: 1 };

    /// The rows of `s`: '#' covered, '.' not, '+' partly.
    fn rows(s: &Sprite) -> Vec<String> {
        let mark = |a: &u8| match a {
            0 => '.',
            255 => '#',
            _ => '+',
        };
        s.alpha.chunks(s.width as usize).map(|row| row.iter().map(mark).collect()).collect()
    }

    /// The columns of `s` (relative to the cell) whose pixels are mostly covered.
    fn covered_columns(s: &Sprite) -> Vec<i32> {
        (0..s.width as usize)
            .filter(|&x| (0..s.height as usize).any(|y| s.alpha[y * s.width as usize + x] >= 128))
            .map(|x| s.left + x as i32)
            .collect()
    }

    #[test]
    fn straight_kinds_follow_ghostty() {
        assert_eq!(sprite(Underline::None, M), Sprite::default());
        let s = sprite(Underline::Single, M);
        assert_eq!((s.left, s.top, rows(&s)), (0, 19, vec!["##########".to_owned()]));
        let s = sprite(Underline::Double, M);
        assert_eq!((s.left, s.top, rows(&s)), (0, 18, vec!["#".repeat(10), ".".repeat(10), "#".repeat(10)]));
        // Dashes of width / 3 + 1, every other one; the second runs 2px into
        // the next cell (the canvas reaches 10 / 4 = 2px past the cell).
        let s = sprite(Underline::Dashed, M);
        assert_eq!((s.left, s.top, rows(&s)), (0, 19, vec!["####....####".to_owned()]));
        // 9px cells: the second dash is clipped at 9 + 2.
        let s = sprite(Underline::Dashed, Metrics { cell_width: 9, ..M });
        assert_eq!((s.left, s.top, rows(&s)), (0, 19, vec!["####....###".to_owned()]));
    }

    #[test]
    fn low_underlines_stay_within_a_quarter_cell_below() {
        // 22 + 22 / 4 = 27 is the lowest row plus one.
        let m = Metrics { position: 40, thickness: 2, ..M };
        let s = sprite(Underline::Single, m);
        assert_eq!((s.top, s.height), (25, 2));
        // Two rows of stroke, two of gap, two of stroke.
        let s = sprite(Underline::Double, m);
        let (stroke, gap) = ("#".repeat(10), ".".repeat(10));
        let expected = [&stroke, &stroke, &gap, &gap, &stroke, &stroke].map(String::clone).to_vec();
        assert_eq!((s.top, rows(&s)), (21, expected));
        for kind in [Underline::Dotted, Underline::Curly, Underline::Dashed] {
            let s = sprite(kind, m);
            assert!(s.top + s.height as i32 <= 27, "{kind:?}: {s:?}");
            assert!(s.alpha.contains(&255), "{kind:?}");
        }
    }

    #[test]
    fn dots_are_spread_evenly() {
        // Thickness 1: radius 0.707, 10 / (2r + 1) = 4.14, so 4 dots 2.5px
        // apart, centered at 1.25, 3.75, 6.25 and 8.75, on y = 19.5.
        let s = sprite(Underline::Dotted, M);
        assert_eq!((s.left, s.top, s.width, s.height), (0, 18, 10, 3));
        let row = &s.alpha[s.width as usize..2 * s.width as usize];
        assert_eq!(covered_columns(&s), vec![1, 3, 6, 8]);
        assert!(row[1] > row[0] && row[3] > row[2] && row[6] > row[5]);
        // Thickness 2: radius 1.41, 10 / 5.66 rounds up to 2 dots.
        let s = sprite(Underline::Dotted, Metrics { thickness: 2, ..M });
        assert_eq!(covered_columns(&s), vec![1, 2, 3, 6, 7, 8]);
    }

    #[test]
    fn curly_is_one_wave_from_the_underline_down() {
        let s = sprite(Underline::Curly, M);
        // 10 / pi = 3.18px of amplitude from y = 19, a 0.5px half stroke
        // around it and round caps 0.5px outside the cell.
        assert_eq!((s.left, s.top, s.width, s.height), (-1, 18, 12, 5));
        let column = |x: i32| -> Vec<u8> {
            (0..s.height as usize).map(|y| s.alpha[y * s.width as usize + (x - s.left) as usize]).collect()
        };
        // The peak at the center is on the underline's row; the ends are at the bottom.
        let peak = column(5);
        assert_eq!(peak.iter().position(|&a| a == *peak.iter().max().unwrap()), Some(1));
        let end = column(0);
        assert!(end[..2] == [0, 0] && end[3] > 0 && end[4] > 0, "{end:?}");
        // Symmetric around the center.
        for x in 0..5 {
            assert_eq!(column(x), column(9 - x), "column {x}");
        }
    }
}
