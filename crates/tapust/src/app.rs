//! The application: one window showing one tmux-like session.
//!
//! The `mux` crate decides the split layout and the key bindings. This
//! module carries out its effects (start, resize and stop pane processes),
//! routes keys, and composes the panes, dividers and chrome into the frame.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use mux::{ConfirmTarget, Direction, Effect, Hit, KeyResult, Metrics, Overlay, PaneId, Rect, Session, View};
use winit::application::ApplicationHandler;
use winit::event::{ElementState, KeyEvent, MouseButton, StartCause, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoopProxy};
use winit::keyboard::ModifiersState;
use winit::window::{CursorIcon, WindowId};

use crate::compose;
use crate::font::{UI, UI_BOLD};
use crate::frame::{Frame, Grid, Shape};
use crate::handle;
use crate::input;
use crate::pty::{PaneIo, Pty};
use crate::theme;
use crate::window::GlWindow;

const SCROLLBACK: usize = 10_000;
/// How often foreground process names are checked (tmux `NAME_INTERVAL`).
const NAME_INTERVAL: Duration = Duration::from_millis(500);

/// Sent by a pane's I/O thread when its terminal changed or its process exited.
#[derive(Debug, Clone, Copy)]
pub enum UserEvent {
    Pane(PaneId),
}

struct Pane {
    io: PaneIo<term::Terminal>,
    /// The terminal title (OSC 0/2) last seen; empty until the program sets one.
    title: String,
}

pub struct App {
    proxy: EventLoopProxy<UserEvent>,
    window: Option<GlWindow>,
    frame: Frame,
    session: Option<Session>,
    panes: HashMap<PaneId, Pane>,
    view: Option<View>,
    /// Session generation the view was taken from.
    drawn: u64,
    /// Copy every visible pane in full on the next frame.
    full: bool,
    palette: term::Palette,
    modifiers: ModifiersState,
    focused: bool,
    cwd: PathBuf,
    input: Vec<u8>,
    next_names: Instant,
    /// Program for the first pane (`-e`); later panes run the login shell.
    command: Option<Vec<String>>,
    debug: bool,
    /// The window size in pixels, the geometry of the pane area and the
    /// height of the tab bar above it in logical pixels (0 while hidden).
    size: winit::dpi::PhysicalSize<u32>,
    metrics: Option<Metrics>,
    top: u32,
    /// The terminal background, shown around each pane's cells.
    term_bg: [u8; 4],
    title: String,
    /// Panes showing ghostty's `cols x rows` overlay after a resize, until when.
    resize_overlays: HashMap<PaneId, Instant>,
    started: Instant,
    /// Whether the focused cursor is in the visible half of its blink.
    blink_on: bool,
    next_blink: Instant,
    /// The pointer in logical window pixels, while it is in the window.
    pointer: Option<(f64, f64)>,
    press: Press,
    cursor: CursorIcon,
    /// The pane whose drag handle the pointer is on.
    handle_hover: Option<PaneId>,
    /// Drag handles fading in or out: (from, to, since).
    fades: HashMap<PaneId, (f64, f64, Instant)>,
}

/// What the left mouse button is doing.
enum Press {
    None,
    /// Dragging a divider (GtkPaned's drag gesture).
    Divider(mux::Grab),
    /// Pressed on a pane's drag handle; dragging starts past GTK's threshold.
    Handle { pane: PaneId, start: (f64, f64) },
    /// Dragging a pane (ghostty's drag source), with where it would drop.
    Pane { source: PaneId, size: (f64, f64), target: Option<(PaneId, Direction)> },
}

impl App {
    pub fn new(proxy: EventLoopProxy<UserEvent>, command: Option<Vec<String>>) -> App {
        let palette = theme::palette();
        let term_bg = compose::resolve(term::Color::Default, &palette, palette.background);
        App {
            proxy,
            window: None,
            frame: Frame::new(theme::adw::WINDOW_BG),
            session: None,
            panes: HashMap::new(),
            view: None,
            drawn: u64::MAX,
            full: true,
            palette,
            modifiers: ModifiersState::empty(),
            focused: true,
            cwd: std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/")),
            input: Vec::new(),
            next_names: Instant::now(),
            command,
            debug: std::env::var_os("TAPUST_DEBUG").is_some(),
            size: winit::dpi::PhysicalSize::new(1, 1),
            metrics: None,
            top: 0,
            term_bg,
            title: String::new(),
            resize_overlays: HashMap::new(),
            started: Instant::now(),
            blink_on: true,
            next_blink: Instant::now() + theme::BLINK,
            pointer: None,
            press: Press::None,
            cursor: CursorIcon::Default,
            handle_hover: None,
            fades: HashMap::new(),
        }
    }

    fn request_redraw(&self) {
        if let Some(window) = &self.window {
            window.window.request_redraw();
        }
    }

    /// Carries out the session's effects; false means the program should exit.
    fn apply(&mut self, effects: Vec<Effect>) -> bool {
        for effect in effects {
            match effect {
                Effect::Spawn { pane, cols, rows, cwd_of } => {
                    let cwd = cwd_of
                        .and_then(|p| self.panes.get(&p))
                        .and_then(|p| p.io.pty.foreground_cwd())
                        .unwrap_or_else(|| self.cwd.clone());
                    if let Err(err) = self.spawn(pane, cols, rows, &cwd) {
                        eprintln!("tapust: cannot start a shell: {err}");
                        let effects = self.session.as_mut().expect("session").pane_exited(pane);
                        if !self.apply(effects) {
                            return false;
                        }
                    }
                }
                Effect::Resize { pane, cols, rows } => {
                    if let (Some(p), Some(window)) = (self.panes.get(&pane), &self.window) {
                        p.io.shared.term.lock().resize(cols, rows);
                        let _ = p.io.pty.resize(cols, rows, window.renderer.cell_size());
                        if self.started.elapsed() >= theme::RESIZE_OVERLAY_DELAY {
                            self.resize_overlays.insert(pane, Instant::now() + theme::RESIZE_OVERLAY);
                        }
                    }
                }
                Effect::Destroy { pane } => {
                    self.panes.remove(&pane);
                }
                Effect::Write { pane, bytes } => {
                    if let Some(p) = self.panes.get(&pane) {
                        p.io.shared.write(&bytes);
                    }
                }
                Effect::Exit => return false,
            }
        }
        true
    }

    fn spawn(&mut self, id: PaneId, cols: u16, rows: u16, cwd: &std::path::Path) -> std::io::Result<()> {
        let cell = self.window.as_ref().expect("window").renderer.cell_size();
        let pty = match self.command.take() {
            Some(argv) => {
                let mut command = std::process::Command::new(&argv[0]);
                command.args(&argv[1..]);
                Pty::spawn_command(command, cols, rows, cell, cwd)?
            }
            None => Pty::spawn(cols, rows, cell, cwd)?,
        };
        let proxy = self.proxy.clone();
        let mut terminal = term::Terminal::new(cols, rows, SCROLLBACK);
        terminal.set_default_palette(self.palette.clone());
        let io = PaneIo::start(pty, terminal, move || {
            let _ = proxy.send_event(UserEvent::Pane(id));
        })?;
        let session = self.session.as_mut().expect("session");
        if let Some(name) = io.pty.foreground_name() {
            session.set_auto_name(id, &name);
        }
        self.panes.insert(id, Pane { io, title: String::new() });
        Ok(())
    }

    /// Sizes the pane area for the window, leaving room for the tab bar while
    /// the session has two or more windows (ghostty's `window-show-tab-bar = auto`).
    fn relayout(&mut self) -> bool {
        let Some(window) = self.window.as_mut() else { return true };
        let tabs = self.session.as_ref().is_some_and(|s| s.windows().len() > 1);
        self.top = if tabs { theme::TAB_BAR_HEIGHT as u32 } else { 0 };
        let metrics = window.resize(self.size, self.top);
        window.window.request_redraw();
        if let Some(session) = &self.session {
            window.set_min_size(session.min_area(), self.top);
        }
        if self.metrics == Some(metrics) {
            return true;
        }
        self.metrics = Some(metrics);
        // Same rectangles can hold other grids (a new cell size or padding).
        self.full = true;
        self.drawn = u64::MAX;
        let Some(session) = self.session.as_mut() else { return true };
        let effects = session.resize(metrics);
        self.apply(effects)
    }

    /// The device pixels of a pane at `r`, snapped as ghostty snaps them.
    fn window_rect(&self, r: Rect) -> (i32, i32, i32, i32) {
        let d = self.metrics.expect("metrics").device(r);
        (d.x, d.y, d.w as i32, d.h as i32)
    }

    /// The fractional device pixels a widget at `r` covers.
    fn device_area(&self, r: Rect) -> (f64, f64, f64, f64) {
        let m = self.metrics.expect("metrics");
        let (x, y) = (f64::from(r.x) * m.scale, f64::from(m.top + r.y) * m.scale);
        (x, y, x + f64::from(r.w) * m.scale, y + f64::from(r.h) * m.scale)
    }

    /// Takes a new view when the session changed. A changed layout gets new
    /// grids at the panes' origins (unchanged grids are kept) and a full copy.
    fn update_view(&mut self) {
        let (Some(session), Some(m)) = (&self.session, self.metrics) else { return };
        if self.drawn == session.generation() {
            return;
        }
        let view = session.view();
        self.drawn = session.generation();
        let layout_changed = self.view.as_ref().is_none_or(|v| v.panes != view.panes);
        if layout_changed || self.full {
            let mut old: HashMap<u32, Grid> = self.frame.grids.drain(..).map(|g| (g.id, g)).collect();
            for pv in &view.panes {
                let d = m.device(pv.rect);
                let pane = (d.x, d.y, d.w, d.h);
                let origin = (d.x + m.padding_x as i32, d.y + m.padding_y as i32);
                let size = m.grid(pv.rect);
                let grid = match old.remove(&pv.pane) {
                    Some(mut g) if (g.cols(), g.rows()) == size => {
                        ((g.x, g.y), g.pane) = (origin, pane);
                        g
                    }
                    _ => Grid::new(pv.pane, pane, origin, size, self.term_bg),
                };
                self.frame.grids.push(grid);
            }
            self.full = true;
        }
        self.view = Some(view);
    }

    /// A tab's title as ghostty shows it: the active pane's terminal title,
    /// else the application name, with a magnifier while zoomed.
    fn tab_title(&self, window: &mux::WindowInfo) -> String {
        // A name given with rename-window overrides the title, like ghostty's
        // "Change Tab Title"; a blank one restores the default.
        let renamed = Some(window.name.as_str()).filter(|n| window.renamed && !n.is_empty());
        let title = renamed.or_else(|| {
            window
                .panes
            .iter()
            .find(|p| p.active)
            .and_then(|p| self.panes.get(&p.pane))
                .map(|p| p.title.as_str())
                .filter(|t| !t.is_empty())
        });
        let title = title.unwrap_or("tapust");
        if window.zoomed { format!("\u{1f50d} {title}") } else { title.to_owned() }
    }

    /// libadwaita's tab bar: equal-width tabs, the selected one raised.
    fn tab_bar_shapes(&mut self, out: &mut Vec<Shape>) {
        let Some(session) = &self.session else { return };
        let windows = session.windows();
        let titles: Vec<String> = windows.iter().map(|w| self.tab_title(w)).collect();
        let window = self.window.as_mut().expect("window");
        let scale = window.window.scale_factor();
        let px = |v: f64| (v * scale).round() as i32;
        let (w, h) = (self.size.width as i32, window.tab_bar_height() as i32);
        out.push(Shape::Rect { x: 0, y: 0, w, h, color: theme::adw::HEADERBAR_BG, radius: 0, ring: false });
        out.push(Shape::Rect { x: 0, y: h - 1, w, h: 1, color: theme::adw::HEADERBAR_SHADE, radius: 0, ring: false });
        let (margin, gap) = (px(theme::TAB_MARGIN), px(theme::TAB_MARGIN));
        let n = windows.len() as i32;
        let tab_w = (w - 2 * margin - (n - 1) * gap) / n;
        let (tab_y, tab_h) = (margin, h - 2 * margin);
        let radius = px(theme::TAB_RADIUS) as u16;
        let line_h = window.renderer.fonts.ui_line_height;
        for (i, (win, title)) in windows.iter().zip(&titles).enumerate() {
            let x = margin + i as i32 * (tab_w + gap);
            if win.active {
                out.push(Shape::Rect { x, y: tab_y, w: tab_w, h: tab_h, color: theme::adw::SELECTED, radius, ring: false });
                out.push(Shape::Rect { x, y: tab_y, w: tab_w, h: tab_h, color: theme::adw::BORDER, radius, ring: true });
            } else if i > 0 && !windows[i - 1].active {
                let sx = x - gap / 2 - 1;
                out.push(Shape::Rect { x: sx, y: tab_y + px(3.0), w: 1, h: tab_h - px(6.0), color: theme::adw::BORDER, radius: 0, ring: false });
            }
            // Ellipsize titles that do not fit, leaving room for the close button.
            let room = tab_w - px(48.0);
            let mut text = title.clone();
            while window.renderer.text_width(&text, UI) > room && text.chars().count() > 1 {
                text = text.chars().take(text.chars().count() - 2).collect::<String>() + "\u{2026}";
            }
            let text_w = window.renderer.text_width(&text, UI);
            let ty = tab_y + (tab_h - line_h) / 2;
            out.push(Shape::Text { x: x + (tab_w - text_w) / 2, y: ty, text, color: theme::adw::FG, face: UI });
            if win.active {
                let close_w = window.renderer.text_width("\u{00d7}", UI_BOLD);
                let cx = x + tab_w - px(4.0) - px(24.0) + (px(24.0) - close_w) / 2;
                out.push(Shape::Text { x: cx, y: ty, text: "\u{00d7}".to_owned(), color: theme::adw::FG, face: UI_BOLD });
            }
        }
    }

    /// Prompts become libadwaita alert dialogs as ghostty shows them, and
    /// messages become a toast. The keys stay tmux's (`y` confirms).
    fn overlay_shapes(&mut self, overlay: &Overlay, out: &mut Vec<Shape>) {
        let window = self.window.as_mut().expect("window");
        let scale = window.window.scale_factor();
        let px = |v: f64| (v * scale).round() as i32;
        let (ww, wh) = (self.size.width as i32, self.size.height as i32);
        let r = &mut window.renderer;
        let line_h = r.fonts.ui_line_height;
        if let Overlay::Message(text) = overlay {
            let (w, h) = (r.text_width(text, UI) + px(36.0), line_h + px(24.0));
            let (x, y) = ((ww - w) / 2, wh - h - px(12.0));
            out.push(Shape::Rect { x, y, w, h, color: theme::adw::TOAST_BG, radius: (h / 2) as u16, ring: false });
            out.push(Shape::Rect { x, y, w, h, color: theme::adw::BORDER, radius: (h / 2) as u16, ring: true });
            out.push(Shape::Text { x: x + px(18.0), y: y + px(12.0), text: text.clone(), color: theme::adw::FG, face: UI });
            return;
        }
        let (heading, body, entry, responses) = match overlay {
            Overlay::Confirm { target: ConfirmTarget::Pane, .. } => (
                "Close Split?",
                "The currently running process in this split will be terminated.",
                None,
                [("Cancel", theme::adw::BUTTON), ("Close", theme::adw::DESTRUCTIVE)],
            ),
            Overlay::Confirm { target: ConfirmTarget::Window, .. } => (
                "Close Tab?",
                "All terminal sessions in this tab will be terminated.",
                None,
                [("Cancel", theme::adw::BUTTON), ("Close", theme::adw::DESTRUCTIVE)],
            ),
            Overlay::Input { prompt, text, cursor } => (
                if prompt.contains("session") { "Rename Session" } else { "Change Tab Title" },
                "Leave blank to restore the default title.",
                Some((text.clone(), *cursor)),
                [("Cancel", theme::adw::BUTTON), ("OK", theme::adw::SUGGESTED)],
            ),
            Overlay::Message(_) => unreachable!(),
        };
        // Word-wrap the body to the dialog's 300px content width.
        let (dialog_w, side) = (px(300.0), px(24.0));
        let mut lines: Vec<String> = Vec::new();
        for word in body.split(' ') {
            match lines.last_mut() {
                Some(line) if r.text_width(&format!("{line} {word}"), UI) <= dialog_w - 2 * side => {
                    line.push(' ');
                    line.push_str(word);
                }
                _ => lines.push(word.to_owned()),
            }
        }
        let heading_h = line_h * 136 / 100;
        let entry_h = if entry.is_some() { px(34.0) + px(12.0) } else { 0 };
        let message_h = px(32.0) + heading_h + px(12.0) + lines.len() as i32 * line_h + entry_h + px(9.0);
        let button_h = line_h.max(px(24.0)) + px(20.0);
        let dialog_h = message_h + px(12.0) + button_h + px(24.0);
        let (x, y) = ((ww - dialog_w) / 2, (wh - dialog_h) / 2);
        out.push(Shape::Rect { x: 0, y: 0, w: ww, h: wh, color: theme::adw::DIMMING, radius: 0, ring: false });
        out.push(Shape::Rect { x, y, w: dialog_w, h: dialog_h, color: theme::adw::DIALOG_BG, radius: px(18.0) as u16, ring: false });
        let center = |r: &mut crate::renderer::Renderer, text: &str, face| x + (dialog_w - r.text_width(text, face)) / 2;
        let mut ty = y + px(32.0);
        out.push(Shape::Text { x: center(r, heading, UI_BOLD), y: ty, text: heading.to_owned(), color: theme::adw::FG, face: UI_BOLD });
        ty += heading_h + px(12.0);
        for line in lines {
            out.push(Shape::Text { x: center(r, &line, UI), y: ty, color: theme::adw::FG, face: UI, text: line });
            ty += line_h;
        }
        if let Some((text, cursor)) = entry {
            ty += px(12.0);
            let (ex, ew, eh) = (x + side, dialog_w - 2 * side, px(34.0));
            out.push(Shape::Rect { x: ex, y: ty, w: ew, h: eh, color: theme::adw::BUTTON, radius: px(6.0) as u16, ring: false });
            let tx = ex + px(9.0);
            let before: String = text.chars().take(cursor).collect();
            let caret_x = tx + r.text_width(&before, UI);
            out.push(Shape::Text { x: tx, y: ty + (eh - line_h) / 2, text, color: theme::adw::FG, face: UI });
            out.push(Shape::Rect { x: caret_x, y: ty + (eh - line_h) / 2, w: px(1.0).max(1), h: line_h, color: theme::adw::FG, radius: 0, ring: false });
        }
        let by = y + message_h + px(12.0);
        let bw = (dialog_w - 2 * side - px(12.0)) / 2;
        for (i, (label, color)) in responses.into_iter().enumerate() {
            let bx = x + side + i as i32 * (bw + px(12.0));
            out.push(Shape::Rect { x: bx, y: by, w: bw, h: button_h, color, radius: px(12.0) as u16, ring: false });
            let lx = bx + (bw - r.text_width(label, UI_BOLD)) / 2;
            out.push(Shape::Text { x: lx, y: by + (button_h - line_h) / 2, text: label.to_owned(), color: theme::adw::FG, face: UI_BOLD });
        }
    }

    /// ghostty's resize overlay: `cols x rows` centered in a pane that just changed size.
    fn resize_overlay_shapes(&mut self, rect: Rect, out: &mut Vec<Shape>) {
        let (cols, rows) = self.metrics.expect("metrics").grid(rect);
        let (rx, ry, rw, rh) = self.window_rect(rect);
        let window = self.window.as_mut().expect("window");
        let scale = window.window.scale_factor();
        let px = |v: f64| (v * scale).round() as i32;
        let text = format!("{cols} x {rows}");
        let line_h = window.renderer.fonts.ui_line_height;
        let (w, h) = (window.renderer.text_width(&text, UI) + px(16.0), line_h + px(8.0));
        let (cx, cy) = (rx + (rw - w) / 2, ry + (rh - h) / 2);
        out.push(Shape::Rect { x: cx, y: cy, w, h, color: theme::adw::WINDOW_BG, radius: px(6.0) as u16, ring: false });
        out.push(Shape::Rect { x: cx, y: cy, w, h, color: theme::RESIZE_OVERLAY_OUTLINE, radius: px(6.0) as u16, ring: true });
        out.push(Shape::Text { x: cx + px(8.0), y: cy + px(4.0), text, color: theme::adw::FG, face: UI });
    }

    fn redraw(&mut self) {
        if self.window.is_none() {
            return;
        }
        self.update_view();
        let Some(view) = self.view.take() else { return };
        let split = view.panes.len() > 1;
        let mut shapes = std::mem::take(&mut self.frame.shapes);
        shapes.clear();
        self.frame.cursors.clear();
        for (i, pv) in view.panes.iter().enumerate() {
            let (Some(pane), Some(grid)) = (self.panes.get_mut(&pv.pane), self.frame.grids.get_mut(i)) else { continue };
            let shared = &pane.io.shared;
            let dirty = shared.dirty.swap(false, Ordering::AcqRel);
            shared.want_lock.store(true, Ordering::Release);
            let mut term = shared.term.lock();
            shared.want_lock.store(false, Ordering::Release);
            if dirty || self.full {
                compose::copy_pane(grid, &mut term, self.full);
            }
            // The focused pane's cursor blinks; every other pane shows a hollow one.
            let focused = pv.active && self.focused;
            if (!focused || self.blink_on)
                && let Some(cursor) = compose::pane_cursor(&term, i, focused)
            {
                self.frame.cursors.push(cursor);
            }
            if term.title() != pane.title {
                pane.title = term.title().to_owned();
            }
        }
        let now = Instant::now();
        // In GtkPaned's order: a split's first pane, its divider, its second
        // pane. A pane's veil (ghostty's unfocused-split-opacity) covers the
        // widget's own fractional rectangle, padding included.
        let m = self.metrics.expect("metrics");
        let mut dividers = view.dividers.iter().peekable();
        for (i, pv) in view.panes.iter().enumerate() {
            while let Some(d) = dividers.next_if(|d| d.after == i) {
                let (x0, y0, mut x1, mut y1) = self.device_area(d.rect);
                // The second pane, drawn after the divider, covers it from
                // that pane's snapped edge on.
                if d.horizontal {
                    x1 = x1.min((f64::from(d.rect.x + d.rect.w) * m.scale).round());
                } else {
                    y1 = y1.min((f64::from(m.top + d.rect.y + d.rect.h) * m.scale).round());
                }
                let [r, g, b, _] = theme::adw::WINDOW_BG;
                shapes.push(Shape::Area { x0, y0, x1, y1, color: [r, g, b], alpha: 1.0 });
                shapes.push(Shape::Area { x0, y0, x1, y1, color: theme::DIVIDER_SHADOW, alpha: theme::DIVIDER_SHADOW_ALPHA });
            }
            // The surface's overlays in ghostty's order: the resize overlay,
            // the unfocused veil, the drag handle and the drop overlay.
            if self.resize_overlays.get(&pv.pane).is_some_and(|&until| until > now) {
                self.resize_overlay_shapes(pv.rect, &mut shapes);
            }
            if split && !pv.active {
                let (x0, y0, x1, y1) = self.device_area(pv.rect);
                shapes.push(Shape::Area { x0, y0, x1, y1, color: theme::UNFOCUSED_FILL, alpha: theme::UNFOCUSED_ALPHA });
            }
            let opacity = self.handle_opacity(pv.pane, now);
            if view.split && opacity > 0.0 {
                handle::drag_handle(pv.rect, m.top, m.scale, opacity, &mut shapes);
            }
            if let Press::Pane { target: Some((target, dir)), .. } = self.press
                && target == pv.pane
            {
                handle::drop_overlay(pv.rect, m.top, m.scale, dir, &mut shapes);
            }
        }
        self.resize_overlays.retain(|_, until| *until > now);
        self.fades.retain(|_, &mut (_, to, since)| to > 0.0 || now < since + theme::DRAG_HANDLE_FADE);
        if self.top > 0 {
            self.tab_bar_shapes(&mut shapes);
        }
        if let Some(overlay) = &view.overlay {
            self.overlay_shapes(overlay, &mut shapes);
        }
        self.frame.shapes = shapes;
        // ghostty's drag icon: the dragged surface at 0.2, centered on the pointer.
        self.frame.preview = match (&self.press, self.pointer) {
            (Press::Pane { size: (w, h), .. }, Some((x, y))) => {
                let (w, h) = (w * theme::DRAG_PREVIEW_SCALE, h * theme::DRAG_PREVIEW_SCALE);
                Some((x * m.scale - w / 2.0, y * m.scale - h / 2.0, w, h))
            }
            _ => None,
        };
        if self.fading(now) {
            self.request_redraw();
        }
        // The window title follows the active tab, as in ghostty.
        if let Some(active) = self.session.as_ref().and_then(|s| s.windows().into_iter().find(|w| w.active)) {
            let title = self.tab_title(&active);
            if title != self.title {
                self.window.as_ref().expect("window").window.set_title(&title);
                self.title = title;
            }
        }
        self.full = false;
        self.view = Some(view);
        // A title change bumps the generation; the view is taken again next frame.
        let stale = self.session.as_ref().is_some_and(|s| s.generation() != self.drawn);
        let window = self.window.as_mut().expect("window");
        window.draw(&mut self.frame);
        if stale {
            window.window.request_redraw();
        }
    }

    /// The drag handle's opacity: CSS `transition: 0.1s opacity` with `ease`.
    fn handle_opacity(&self, pane: PaneId, now: Instant) -> f64 {
        let Some(&(from, to, since)) = self.fades.get(&pane) else { return 0.0 };
        let t = (now - since).as_secs_f64() / theme::DRAG_HANDLE_FADE.as_secs_f64();
        from + (to - from) * handle::ease(t.min(1.0))
    }

    fn fading(&self, now: Instant) -> bool {
        self.fades.values().any(|&(_, _, since)| now < since + theme::DRAG_HANDLE_FADE)
    }

    /// The pointer moves onto or off a drag handle (GTK's `:hover`).
    fn set_handle_hover(&mut self, pane: Option<PaneId>) {
        if pane == self.handle_hover {
            return;
        }
        let now = Instant::now();
        // A new transition starts from the current opacity.
        for (p, to) in [(self.handle_hover, 0.0), (pane, 1.0)] {
            if let Some(p) = p {
                let from = self.handle_opacity(p, now);
                self.fades.insert(p, (from, to, now));
            }
        }
        self.handle_hover = pane;
        self.request_redraw();
    }

    fn set_cursor(&mut self, cursor: CursorIcon) {
        if cursor != self.cursor
            && let Some(window) = &self.window
        {
            window.window.set_cursor(cursor);
            self.cursor = cursor;
        }
    }

    /// The pointer in logical pixels of the pane area.
    fn area_point(&self) -> Option<(f64, f64)> {
        self.pointer.map(|(x, y)| (x, y - f64::from(self.top)))
    }

    fn pane_rect(&self, pane: PaneId) -> Option<Rect> {
        self.view.as_ref()?.panes.iter().find(|p| p.pane == pane).map(|p| p.rect)
    }

    /// Returns false when the program should exit.
    fn pointer_moved(&mut self, x: f64, y: f64) -> bool {
        self.pointer = Some((x, y));
        let Some((ax, ay)) = self.area_point() else { return true };
        let Some(session) = self.session.as_mut() else { return true };
        match self.press {
            Press::Divider(grab) => {
                let effects = session.drag_divider(grab, ax, ay);
                self.request_redraw();
                return self.apply(effects);
            }
            Press::Handle { pane, start } => {
                if (x - start.0).abs() > theme::DRAG_THRESHOLD || (y - start.1).abs() > theme::DRAG_THRESHOLD {
                    // The drag icon is a snapshot of the surface when the drag begins.
                    if let (Some(m), Some(r)) = (self.metrics, self.pane_rect(pane)) {
                        let d = m.device(r);
                        self.frame.capture = Some((d.x, d.y, d.w, d.h));
                        self.press = Press::Pane { source: pane, size: (f64::from(d.w), f64::from(d.h)), target: None };
                        self.set_handle_hover(None);
                        self.set_cursor(CursorIcon::Move);
                    }
                }
            }
            Press::Pane { source, size, .. } => {
                // Surfaces are the drop targets; a surface rejects itself.
                let target = match session.hit(ax, ay) {
                    Some(Hit::Pane(p) | Hit::DragHandle(p)) if p != source => self.pane_rect(p).map(|r| {
                        let (px, py) = (ax - f64::from(r.x), ay - f64::from(r.y));
                        (p, handle::drop_direction(px, py, f64::from(r.w), f64::from(r.h)))
                    }),
                    _ => None,
                };
                self.press = Press::Pane { source, size, target };
            }
            Press::None => {
                let hit = session.hit(ax, ay);
                self.set_cursor(match hit {
                    Some(Hit::Divider { horizontal: true }) => CursorIcon::ColResize,
                    Some(Hit::Divider { horizontal: false }) => CursorIcon::RowResize,
                    Some(Hit::DragHandle(_)) => CursorIcon::Grab,
                    Some(Hit::Pane(_)) => CursorIcon::Text,
                    None => CursorIcon::Default,
                });
                self.set_handle_hover(match hit {
                    Some(Hit::DragHandle(p)) => Some(p),
                    _ => None,
                });
            }
        }
        self.request_redraw();
        true
    }

    /// Returns false when the program should exit.
    fn left_button(&mut self, pressed: bool) -> bool {
        let Some((ax, ay)) = self.area_point() else { return true };
        let Some(session) = self.session.as_mut() else { return true };
        if pressed {
            if let Some(grab) = session.grab_divider(ax, ay) {
                self.press = Press::Divider(grab);
                return true;
            }
            match session.hit(ax, ay) {
                Some(Hit::DragHandle(pane)) => {
                    self.press = Press::Handle { pane, start: self.pointer.expect("pointer") };
                }
                Some(Hit::Pane(pane)) => {
                    let effects = session.focus_pane(pane);
                    self.request_redraw();
                    return self.apply(effects);
                }
                _ => {}
            }
            return true;
        }
        let press = std::mem::replace(&mut self.press, Press::None);
        let mut ok = true;
        if let Press::Pane { source, target: Some((target, dir)), .. } = press {
            let effects = session.move_pane(source, target, dir);
            ok = self.apply(effects);
        }
        self.request_redraw();
        // The pointer may now be over something else.
        if let Some((x, y)) = self.pointer {
            ok &= self.pointer_moved(x, y);
        }
        ok
    }

    fn key(&mut self, event_loop: &ActiveEventLoop, event: &KeyEvent) {
        if event.state != ElementState::Pressed {
            return;
        }
        let Some((key, text)) = input::from_winit(event, self.modifiers) else { return };
        // Typing shows the cursor and restarts its blink, as in ghostty.
        self.blink_on = true;
        self.next_blink = Instant::now() + theme::BLINK;
        self.request_redraw();
        let Some(session) = self.session.as_mut() else { return };
        let before = session.generation();
        let result = session.key(key, Instant::now());
        if self.debug {
            eprintln!("key {key:?} {text:?} -> {result:?}");
        }
        match result {
            KeyResult::Forward(id) => {
                let Some(pane) = self.panes.get(&id) else { return };
                let app_cursor = pane.io.shared.term.lock().modes().app_cursor;
                self.input.clear();
                input::encode(key, text.as_deref(), app_cursor, &mut self.input);
                pane.io.shared.write(&self.input);
            }
            KeyResult::Handled(effects) => {
                if !self.apply(effects) || !self.relayout() {
                    event_loop.exit();
                    return;
                }
            }
        }
        if self.session.as_ref().is_some_and(|s| s.generation() != before) {
            self.request_redraw();
        }
    }

    /// tmux automatic-rename: follow each pane's foreground process name.
    fn update_names(&mut self) {
        let Some(session) = self.session.as_mut() else { return };
        let before = session.generation();
        for (&id, pane) in &self.panes {
            if let Some(name) = pane.io.pty.foreground_name() {
                session.set_auto_name(id, &name);
            }
        }
        if session.generation() != before {
            self.request_redraw();
        }
    }
}

impl ApplicationHandler<UserEvent> for App {
    fn new_events(&mut self, _event_loop: &ActiveEventLoop, cause: StartCause) {
        if !matches!(cause, StartCause::ResumeTimeReached { .. }) {
            return;
        }
        let now = Instant::now();
        let before = self.session.as_ref().map(|s| s.generation());
        if let Some(session) = self.session.as_mut() {
            session.tick(now);
        }
        if now >= self.next_names {
            self.next_names = now + NAME_INTERVAL;
            self.update_names();
        }
        if now >= self.next_blink {
            self.blink_on = !self.blink_on;
            self.next_blink = now + theme::BLINK;
            self.request_redraw();
        }
        if self.session.as_ref().map(|s| s.generation()) != before || self.resize_overlays.values().any(|&u| u <= now) {
            self.request_redraw();
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        let deadline = self.session.as_ref().and_then(|s| s.deadline());
        let mut wake = deadline.map_or(self.next_names, |d| d.min(self.next_names));
        if let Some(&until) = self.resize_overlays.values().min() {
            wake = wake.min(until);
        }
        if self.focused {
            wake = wake.min(self.next_blink);
        }
        // ghostty's idle callbacks run once the pending events are handled.
        if let Some(session) = self.session.as_mut() {
            session.idle();
        }
        event_loop.set_control_flow(ControlFlow::WaitUntil(wake));
    }

    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let mut window = match GlWindow::new(event_loop) {
            Ok(window) => window,
            Err(err) => {
                eprintln!("tapust: cannot create the window: {err}");
                event_loop.exit();
                return;
            }
        };
        if self.debug {
            eprintln!("renderer: {}", window.renderer.info());
        }
        self.size = window.window.inner_size();
        let metrics = window.resize(self.size, 0);
        self.window = Some(window);
        self.metrics = Some(metrics);
        let (session, effects) = Session::new(metrics);
        self.window.as_ref().expect("window").set_min_size(session.min_area(), 0);
        self.session = Some(session);
        if !self.apply(effects) || self.panes.is_empty() {
            event_loop.exit();
            return;
        }
        self.request_redraw();
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: UserEvent) {
        let UserEvent::Pane(id) = event;
        if self.panes.get(&id).is_some_and(|p| p.io.shared.exited.load(Ordering::Acquire)) {
            self.panes.remove(&id);
            let effects = self.session.as_mut().expect("session").pane_exited(id);
            if !self.apply(effects) || !self.relayout() {
                event_loop.exit();
                return;
            }
        }
        self.request_redraw();
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => {
                self.size = size;
                if !self.relayout() {
                    event_loop.exit();
                }
            }
            WindowEvent::ModifiersChanged(modifiers) => self.modifiers = modifiers.state(),
            WindowEvent::Focused(focused) => {
                self.focused = focused;
                self.request_redraw();
            }
            WindowEvent::KeyboardInput { event, .. } => self.key(event_loop, &event),
            WindowEvent::CursorMoved { position, .. } => {
                let scale = self.window.as_ref().map_or(1.0, |w| w.window.scale_factor());
                if !self.pointer_moved(position.x / scale, position.y / scale) {
                    event_loop.exit();
                }
            }
            WindowEvent::CursorLeft { .. } => {
                self.pointer = None;
                self.set_handle_hover(None);
            }
            WindowEvent::MouseInput { state, button: MouseButton::Left, .. } => {
                if !self.left_button(state == ElementState::Pressed) {
                    event_loop.exit();
                }
            }
            WindowEvent::RedrawRequested => self.redraw(),
            _ => {}
        }
    }
}
