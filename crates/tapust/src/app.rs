//! The application: one window showing one tmux-like session.
//!
//! The `mux` crate decides layouts, key bindings and the status line. This
//! module carries out its effects (start, resize and stop pane processes),
//! routes keys, and composes the view into the frame.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use mux::{Effect, KeyResult, Overlay, PaneId, Session, View};
use winit::application::ApplicationHandler;
use winit::event::{ElementState, KeyEvent, StartCause, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoopProxy};
use winit::keyboard::ModifiersState;
use winit::window::WindowId;

use crate::compose;
use crate::frame::{Dim, Frame, FrameCell, Panel, PanelText};
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
    /// The title last reported to the session (OSC 0/2, or the host name).
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
    host: String,
    input: Vec<u8>,
    next_names: Instant,
    /// Program for the first pane (`-e`); later panes run the login shell.
    command: Option<Vec<String>>,
    debug: bool,
    /// Whether the focused cursor is in the visible half of its blink.
    blink_on: bool,
    next_blink: Instant,
}

fn host_name() -> String {
    let mut buf = [0u8; 256];
    if unsafe { libc::gethostname(buf.as_mut_ptr().cast(), buf.len()) } != 0 {
        return String::new();
    }
    let len = buf.iter().position(|b| *b == 0).unwrap_or(buf.len());
    String::from_utf8_lossy(&buf[..len]).into_owned()
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
            host: host_name(),
            input: Vec::new(),
            next_names: Instant::now(),
            command,
            debug: std::env::var_os("TAPUST_DEBUG").is_some(),
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
        session.set_pane_title(id, &self.host);
        if let Some(name) = io.pty.foreground_name() {
            session.set_auto_name(id, &name);
        }
        self.panes.insert(id, Pane { io, title: self.host.clone() });
        Ok(())
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

    /// A centered panel for a prompt or message (the status line's job in tmux).
    fn overlay_panel(&self, overlay: &Overlay) -> Panel {
        let (cw, ch) = self.window.as_ref().expect("window").renderer.cell_size();
        let (cw, ch) = (cw as i32, ch as i32);
        let (line, caret) = match overlay {
            Overlay::Confirm { prompt, .. } => (prompt.trim_end().to_owned(), None),
            Overlay::Input { prompt, text, cursor } => (format!("{prompt}{text}"), Some(prompt.chars().count() + cursor)),
            Overlay::Message(text) => (text.clone(), None),
        };
        let w = (line.chars().count() as i32 + 2) * cw + 16;
        let h = ch + 16;
        let x = (self.frame.cols() as i32 * cw - w) / 2;
        let y = (self.frame.rows() as i32 * ch - h) / 2;
        let fg = compose::resolve(term::Color::Default, &self.palette, self.palette.foreground);
        let mut texts = vec![PanelText { x: 8 + cw, y: 8, text: line, color: fg, bold: false }];
        if let Some(caret) = caret {
            texts.push(PanelText { x: 8 + cw + caret as i32 * cw, y: 8, text: "\u{258f}".to_owned(), color: fg, bold: false });
        }
        Panel { x, y, w, h, fill: [0x36, 0x36, 0x3a, 0xff], outline: Some([0x55, 0x55, 0x55, 0xff]), texts }
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
            if !term.title().is_empty() && term.title() != pane.title {
                pane.title = term.title().to_owned();
                self.session.as_mut().expect("session").set_pane_title(pv.pane, &pane.title);
            }
        }
        self.frame.panels.clear();
        if let Some(overlay) = &view.overlay {
            let panel = self.overlay_panel(overlay);
            self.frame.panels.push(panel);
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
                if !self.apply(effects) {
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
        if self.session.as_ref().map(|s| s.generation()) != before {
            self.request_redraw();
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        let deadline = self.session.as_ref().and_then(|s| s.deadline());
        let mut wake = deadline.map_or(self.next_names, |d| d.min(self.next_names));
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
        let (cols, rows) = window.resize(window.window.inner_size());
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
            if !self.apply(effects) {
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
                let Some(window) = self.window.as_mut() else { return };
                let (cols, rows) = window.resize(size);
                window.window.request_redraw();
                if (cols, rows) != (self.frame.cols(), self.frame.rows()) && !self.resize(cols, rows) {
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
