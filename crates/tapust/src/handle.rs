//! ghostty's drag handle and drop overlay on split panes (surface.blp's
//! `drag_handle` and `drop_overlay`), drawn as GTK draws them.

use mux::{Direction, Rect};

use crate::frame::Shape;
use crate::theme;

/// CSS `ease`, the timing of ghostty's `transition: 0.1s opacity`:
/// cubic-bezier(0.25, 0.1, 0.25, 1).
pub fn ease(x: f64) -> f64 {
    let bezier = |t: f64, p1: f64, p2: f64| 3.0 * (1.0 - t) * (1.0 - t) * t * p1 + 3.0 * (1.0 - t) * t * t * p2 + t * t * t;
    let (mut lo, mut hi) = (0.0, 1.0);
    for _ in 0..40 {
        let mid = (lo + hi) / 2.0;
        if bezier(mid, 0.25, 0.25) < x {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    bezier((lo + hi) / 2.0, 0.1, 1.0)
}

mod cairo {
    use std::ffi::c_void;

    pub const FORMAT_A8: i32 = 2;

    #[link(name = "cairo")]
    unsafe extern "C" {
        pub fn cairo_image_surface_create(format: i32, width: i32, height: i32) -> *mut c_void;
        pub fn cairo_image_surface_get_data(surface: *mut c_void) -> *mut u8;
        pub fn cairo_image_surface_get_stride(surface: *mut c_void) -> i32;
        pub fn cairo_surface_flush(surface: *mut c_void);
        pub fn cairo_surface_destroy(surface: *mut c_void);
        pub fn cairo_create(surface: *mut c_void) -> *mut c_void;
        pub fn cairo_destroy(cr: *mut c_void);
        pub fn cairo_translate(cr: *mut c_void, tx: f64, ty: f64);
        pub fn cairo_scale(cr: *mut c_void, sx: f64, sy: f64);
        pub fn cairo_move_to(cr: *mut c_void, x: f64, y: f64);
        pub fn cairo_line_to(cr: *mut c_void, x: f64, y: f64);
        pub fn cairo_curve_to(cr: *mut c_void, x1: f64, y1: f64, x2: f64, y2: f64, x3: f64, y3: f64);
        pub fn cairo_close_path(cr: *mut c_void);
        pub fn cairo_fill(cr: *mut c_void);
    }
}

/// The coverage of Yaru's view-more-horizontal-symbolic (three 2x2 rounded
/// squares) drawn at `scale` with its top-left at device (`x`, `y`): GSK
/// rasterizes icon paths with cairo, so this does too. Returns the first
/// pixel's position, the width and the coverage of each pixel, row by row.
fn icon_mask(x: f64, y: f64, scale: f64) -> (i32, i32, usize, Vec<f64>) {
    let (px, py) = (x.floor() as i32, y.floor() as i32);
    let w = ((x + 16.0 * scale).ceil() as i32 - px) as usize;
    let h = ((y + 16.0 * scale).ceil() as i32 - py) as usize;
    let mut out = vec![0.0; w * h];
    // SAFETY: plain cairo calls on a surface and context created and
    // destroyed here; the data is read within the surface's bounds.
    unsafe {
        let surface = cairo::cairo_image_surface_create(cairo::FORMAT_A8, w as i32, h as i32);
        let cr = cairo::cairo_create(surface);
        cairo::cairo_translate(cr, x - f64::from(px), y - f64::from(py));
        cairo::cairo_scale(cr, scale, scale);
        // The SVG's path, with its group's translate(-32 -280) applied.
        for dx in [0.0, 4.0, 8.0] {
            cairo::cairo_move_to(cr, 3.805 + dx, 7.0);
            cairo::cairo_curve_to(cr, 3.26857 + dx, 7.0, 3.00031 + dx, 7.00015, 3.00031 + dx, 7.625);
            cairo::cairo_line_to(cr, 3.00031 + dx, 8.375);
            cairo::cairo_curve_to(cr, 3.00031 + dx, 8.99985, 3.26857 + dx, 9.0, 3.805 + dx, 9.0);
            cairo::cairo_line_to(cr, 4.19562 + dx, 9.0);
            cairo::cairo_curve_to(cr, 4.73205 + dx, 9.0, 5.00031 + dx, 8.99985, 5.00031 + dx, 8.375);
            cairo::cairo_line_to(cr, 5.00031 + dx, 7.625);
            cairo::cairo_curve_to(cr, 5.00031 + dx, 7.02247, 4.73205 + dx, 7.0, 4.19562 + dx, 7.0);
            cairo::cairo_close_path(cr);
        }
        cairo::cairo_fill(cr);
        cairo::cairo_surface_flush(surface);
        let data = cairo::cairo_image_surface_get_data(surface);
        let stride = cairo::cairo_image_surface_get_stride(surface) as usize;
        for row in 0..h {
            for col in 0..w {
                out[row * w + col] = f64::from(*data.add(row * stride + col)) / 255.0;
            }
        }
        cairo::cairo_destroy(cr);
        cairo::cairo_surface_destroy(surface);
    }
    (px, py, w, out)
}

/// Pushes the drag handle of a pane at `r` (logical, `top` below the window
/// top) at `opacity`: a 16px band of black at 15% with the icon centered,
/// faded as one group, as GTK applies CSS opacity.
pub fn drag_handle(r: Rect, top: u32, scale: f64, opacity: f64, out: &mut Vec<Shape>) {
    let s = scale;
    let (bx0, bx1) = (f64::from(r.x) * s, f64::from(r.x + r.w) * s);
    let by0 = f64::from(top + r.y) * s;
    let by1 = by0 + f64::from(mux::DRAG_HANDLE_HEIGHT.min(r.h)) * s;
    // GtkCenterBox centers its child at size / 2 - child / 2, in integers.
    let icon_x = f64::from(r.x + (r.w / 2).saturating_sub(8));
    let icon_y = f64::from(top + r.y);
    let (ix0, ix1) = (((icon_x * s).floor()).max(bx0.floor()), ((icon_x + 16.0) * s).ceil().min(bx1.ceil()));
    let band = |x0: f64, x1: f64, out: &mut Vec<Shape>| {
        out.push(Shape::Area { x0, y0: by0, x1, y1: by1, color: [0, 0, 0], alpha: theme::DRAG_HANDLE_ALPHA * opacity });
    };
    band(bx0, ix0.max(bx0), out);
    band(ix1.min(bx1), bx1, out);
    let (mx, my, mw, mask) = icon_mask(icon_x * s, icon_y * s, s);
    let icon = |px: i32, py: i32| {
        let (col, row) = (px - mx, py - my);
        if col < 0 || row < 0 || col as usize >= mw || row as usize * mw >= mask.len() {
            0.0
        } else {
            mask[row as usize * mw + col as usize]
        }
    };
    let [r_, g_, b_] = theme::ICON_COLOR;
    for py in (by0.floor() as i32)..(by1.ceil() as i32) {
        let row = (by1.min(f64::from(py + 1)) - by0.max(f64::from(py))).max(0.0);
        for px in (ix0 as i32)..(ix1 as i32) {
            let col = (bx1.min(f64::from(px + 1)) - bx0.max(f64::from(px))).max(0.0);
            let band_alpha = theme::DRAG_HANDLE_ALPHA * row * col;
            let c = icon(px, py);
            // The icon over the band, then the group at `opacity`.
            let alpha = c + band_alpha * (1.0 - c);
            if alpha <= 0.0 {
                continue;
            }
            let k = c / alpha;
            let color = [(f64::from(r_) * k).round() as u8, (f64::from(g_) * k).round() as u8, (f64::from(b_) * k).round() as u8];
            let p = (f64::from(px), f64::from(py));
            out.push(Shape::Area { x0: p.0, y0: p.1, x1: p.0 + 1.0, y1: p.1 + 1.0, color, alpha: alpha * opacity });
        }
    }
}

/// ghostty's calcDropDirection: the side of the pane nearest to the pointer
/// at (`x`, `y`) relative to the pane, compared left, right, top, bottom.
pub fn drop_direction(x: f64, y: f64, w: f64, h: f64) -> Direction {
    let (l, t) = (x / w, y / h);
    let (rr, b) = (1.0 - l, 1.0 - t);
    let min = l.min(t).min(rr).min(b);
    if min == l {
        Direction::Left
    } else if min == rr {
        Direction::Right
    } else if min == t {
        Direction::Up
    } else {
        Direction::Down
    }
}

/// The drop overlay of a pane at `r`: a linear gradient with a hard stop at
/// 50% that colors the half on `dir` with the accent at 20%. Gradients are
/// sampled at pixel centers, so the middle edge is sharp.
pub fn drop_overlay(r: Rect, top: u32, scale: f64, dir: Direction, out: &mut Vec<Shape>) {
    let s = scale;
    let (x0, x1) = (f64::from(r.x) * s, f64::from(r.x + r.w) * s);
    let (y0, y1) = (f64::from(top + r.y) * s, f64::from(top + r.y + r.h) * s);
    let (mx, my) = ((x0 + x1) / 2.0, (y0 + y1) / 2.0);
    // Pixels whose centers lie on the colored side of the middle.
    let before = |m: f64| (m - 0.5).ceil();
    let (x0, y0, x1, y1) = match dir {
        Direction::Left => (x0, y0, before(mx), y1),
        Direction::Right => (before(mx), y0, x1, y1),
        Direction::Up => (x0, y0, x1, before(my)),
        Direction::Down => (x0, before(my), x1, y1),
    };
    let [r_, g_, b_, _] = theme::adw::ACCENT_BG;
    out.push(Shape::Area { x0, y0, x1, y1, color: [r_, g_, b_], alpha: theme::DROP_ALPHA });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ease_runs_from_zero_to_one() {
        assert!(ease(0.0).abs() < 1e-6);
        assert!((ease(1.0) - 1.0).abs() < 1e-6);
        // cubic-bezier(0.25, 0.1, 0.25, 1) at x = 0.5 is about 0.8024.
        assert!((ease(0.5) - 0.8024).abs() < 1e-3);
    }

    #[test]
    fn icon_pixels_match_gtk_at_scale_one() {
        // GTK drew each pixel of a dot at about 92.9% (measured on this host).
        let (x, y, w, mask) = icon_mask(0.0, 0.0, 1.0);
        assert_eq!((x, y, w), (0, 0, 16));
        for (px, py) in [(3, 7), (4, 7), (3, 8), (4, 8), (7, 7), (12, 8)] {
            let c = mask[py * w + px];
            assert!((c - 0.929).abs() < 0.01, "{px},{py}: {c}");
        }
        assert_eq!(mask[7 * w + 5], 0.0);
    }

    #[test]
    fn drop_direction_prefers_left_then_right_then_up() {
        assert_eq!(drop_direction(10.0, 50.0, 100.0, 100.0), Direction::Left);
        assert_eq!(drop_direction(90.0, 50.0, 100.0, 100.0), Direction::Right);
        assert_eq!(drop_direction(50.0, 10.0, 100.0, 100.0), Direction::Up);
        assert_eq!(drop_direction(50.0, 90.0, 100.0, 100.0), Direction::Down);
        // Ties go left first.
        assert_eq!(drop_direction(10.0, 10.0, 100.0, 100.0), Direction::Left);
    }
}
