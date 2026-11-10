//! The application: one window showing one tmux-like session.
//!
//! The `mux` crate decides layouts, key bindings and the status line. This
//! module carries out its effects (start, resize and stop pane processes),
//! routes keys, and composes the view into the frame.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use mux::{ConfirmTarget, Effect, KeyResult, Overlay, PaneId, Session, View};
use winit::application::ApplicationHandler;
use winit::event::{ElementState, KeyEvent, StartCause, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoopProxy};
use winit::keyboard::ModifiersState;
use winit::window::WindowId;

use crate::compose;
use crate::font::{UI, UI_BOLD};
use crate::frame::{Dim, Frame, FrameCell, Shape};
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
    /// Session generation the frame's dividers were built from.
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
    /// The window size in pixels and whether the tab bar is shown.
    size: winit::dpi::PhysicalSize<u32>,
    tab_bar: bool,
    title: String,
    /// Panes showing ghostty's `cols x rows` overlay after a resize, until when.
    resize_overlays: HashMap<PaneId, Instant>,
    started: Instant,
    /// Whether the focused cursor is in the visible half of its blink.
    blink_on: bool,
    next_blink: Instant,
}

impl App {
    pub fn new(proxy: EventLoopProxy<UserEvent>, command: Option<Vec<String>>) -> App {
        let palette = theme::palette();
        let default_bg = compose::resolve(term::Color::Default, &palette, palette.background);
        App {
            proxy,
            window: None,
            frame: Frame::new(1, 1, default_bg),
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
            tab_bar: false,
            title: String::new(),
            resize_overlays: HashMap::new(),
            started: Instant::now(),
            blink_on: true,
            next_blink: Instant::now() + theme::BLINK,
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

    /// Sizes the grid for the window, leaving room for the tab bar while the
    /// session has two or more windows (ghostty's `window-show-tab-bar = auto`).
    fn relayout(&mut self) -> bool {
        let Some(window) = self.window.as_mut() else { return true };
        let tabs = self.session.as_ref().is_some_and(|s| s.windows().len() > 1);
        let top = if tabs { window.tab_bar_height() } else { 0 };
        let (cols, rows) = window.resize(self.size, top);
        window.window.request_redraw();
        let changed = tabs != self.tab_bar || (cols, rows) != (self.frame.cols(), self.frame.rows());
        self.tab_bar = tabs;
        !changed || self.resize(cols, rows)
    }

    fn resize(&mut self, cols: u16, rows: u16) -> bool {
        self.frame = Frame::new(cols, rows, self.frame.default_bg);
        self.full = true;
        self.drawn = u64::MAX;
        let Some(session) = self.session.as_mut() else { return true };
        let effects = session.resize_client(cols, rows);
        self.apply(effects)
    }

    /// Rebuilds the split dividers when the layout changed. ghostty draws a
    /// thin divider whose color never depends on focus.
    fn compose_chrome(&mut self) {
        let Some(session) = &self.session else { return };
        if self.drawn == session.generation() {
            return;
        }
        let view = session.view("");
        self.drawn = session.generation();
        let default_bg = self.frame.default_bg;
        let layout_changed = self.view.as_ref().is_none_or(|v| v.panes != view.panes || v.borders != view.borders);
        if layout_changed || self.full {
            for y in 0..self.frame.rows() {
                self.frame.row_mut(y).fill(FrameCell::blank(default_bg));
            }
            let (cols, rows) = (self.frame.cols(), self.frame.rows());
            for border in view.borders.iter().filter(|b| b.y < rows && b.x < cols) {
                self.frame.row_mut(border.y)[border.x as usize] =
                    FrameCell { ch: border.glyph as u32, fg: theme::DIVIDER, bg: default_bg, flags: 0 };
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
    fn resize_overlay_shapes(&mut self, rect: mux::Rect, cols: u16, rows: u16, out: &mut Vec<Shape>) {
        let window = self.window.as_mut().expect("window");
        let scale = window.window.scale_factor();
        let px = |v: f64| (v * scale).round() as i32;
        let (cw, ch) = window.renderer.cell_size();
        let (ox, oy) = window.renderer.grid_origin();
        let text = format!("{cols} x {rows}");
        let line_h = window.renderer.fonts.ui_line_height;
        let (w, h) = (window.renderer.text_width(&text, UI) + px(16.0), line_h + px(8.0));
        let cx = ox + rect.x as i32 * cw as i32 + (rect.w as i32 * cw as i32 - w) / 2;
        let cy = oy + rect.y as i32 * ch as i32 + (rect.h as i32 * ch as i32 - h) / 2;
        out.push(Shape::Rect { x: cx, y: cy, w, h, color: theme::adw::WINDOW_BG, radius: px(6.0) as u16, ring: false });
        out.push(Shape::Rect { x: cx, y: cy, w, h, color: theme::RESIZE_OVERLAY_OUTLINE, radius: px(6.0) as u16, ring: true });
        out.push(Shape::Text { x: cx + px(8.0), y: cy + px(4.0), text, color: theme::adw::FG, face: UI });
    }

    fn redraw(&mut self) {
        if self.window.is_none() {
            return;
        }
        self.compose_chrome();
        let Some(view) = self.view.take() else { return };
        let split = view.panes.len() > 1;
        self.frame.cursors.clear();
        self.frame.dims.clear();
        for pv in &view.panes {
            let Some(pane) = self.panes.get_mut(&pv.pane) else { continue };
            let shared = &pane.io.shared;
            let dirty = shared.dirty.swap(false, Ordering::AcqRel);
            shared.want_lock.store(true, Ordering::Release);
            let mut term = shared.term.lock();
            shared.want_lock.store(false, Ordering::Release);
            let r = pv.rect;
            if dirty || self.full {
                compose::copy_pane(&mut self.frame, &mut term, r.x, r.y, r.w, r.h, self.full);
            }
            // The focused pane's cursor blinks; every other pane shows a hollow one.
            let focused = pv.active && self.focused;
            if (!focused || self.blink_on)
                && let Some(cursor) = compose::pane_cursor(&term, r.x, r.y, focused)
            {
                self.frame.cursors.push(cursor);
            }
            if split && !pv.active {
                self.frame.dims.push(Dim { x: r.x, y: r.y, w: r.w, h: r.h, color: theme::UNFOCUSED_DIM });
            }
            if term.title() != pane.title {
                pane.title = term.title().to_owned();
            }
        }
        let mut shapes = std::mem::take(&mut self.frame.shapes);
        shapes.clear();
        if self.tab_bar {
            self.tab_bar_shapes(&mut shapes);
        }
        let now = Instant::now();
        self.resize_overlays.retain(|_, until| *until > now);
        for pv in &view.panes {
            if self.resize_overlays.contains_key(&pv.pane) {
                self.resize_overlay_shapes(pv.rect, pv.rect.w, pv.rect.h, &mut shapes);
            }
        }
        if let Some(overlay) = &view.overlay {
            self.overlay_shapes(overlay, &mut shapes);
        }
        self.frame.shapes = shapes;
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
        // A title change bumps the generation; the chrome is rebuilt next frame.
        let stale = self.session.as_ref().is_some_and(|s| s.generation() != self.drawn);
        let window = self.window.as_mut().expect("window");
        window.draw(&mut self.frame);
        if stale {
            window.window.request_redraw();
        }
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
        let (cols, rows) = window.resize(self.size, 0);
        self.window = Some(window);
        self.frame = Frame::new(cols, rows, self.frame.default_bg);
        let (session, effects) = Session::without_status_line(cols, rows);
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
            WindowEvent::RedrawRequested => self.redraw(),
            _ => {}
        }
    }
}
