//! The application: one window showing one tmux-like session.
//!
//! The `mux` crate decides layouts, key bindings and the status line. This
//! module carries out its effects (start, resize and stop pane processes),
//! routes keys, and composes the view into the frame.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use mux::{ChromeColor, Effect, KeyResult, PaneId, Session, View};
use winit::application::ApplicationHandler;
use winit::event::{ElementState, KeyEvent, StartCause, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoopProxy};
use winit::keyboard::ModifiersState;
use winit::window::WindowId;

use crate::compose;
use crate::frame::{BOLD, Frame, FrameCell, FrameCursor, UNDERLINE};
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
    /// Session generation and clock text the frame's chrome was built from.
    drawn: (u64, String),
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
}

/// Local time as tmux's default status-right shows it (`%H:%M %d-%b-%y`).
fn clock() -> String {
    unsafe {
        let now = libc::time(std::ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&now, &mut tm);
        let mut buf = [0u8; 64];
        let n = libc::strftime(buf.as_mut_ptr().cast(), buf.len(), c"%H:%M %d-%b-%y".as_ptr(), &tm);
        String::from_utf8_lossy(&buf[..n]).into_owned()
    }
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
            drawn: (u64::MAX, String::new()),
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
        }
    }

    fn color(&self, color: ChromeColor) -> [u8; 4] {
        let color = match color {
            ChromeColor::Default => term::Color::Default,
            ChromeColor::Indexed(i) => term::Color::Indexed(i),
        };
        compose::resolve(color, &self.palette, self.palette.foreground)
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
        self.drawn.0 = u64::MAX;
        let Some(session) = self.session.as_mut() else { return true };
        let effects = session.resize_client(cols, rows);
        self.apply(effects)
    }

    /// Rebuilds the borders and the status line when the session or the clock changed.
    fn compose_chrome(&mut self) {
        let Some(session) = &self.session else { return };
        let clock = clock();
        if self.drawn.0 == session.generation() && self.drawn.1 == clock {
            return;
        }
        let view = session.view(&clock);
        self.drawn = (session.generation(), clock);
        let default_bg = self.frame.default_bg;
        let layout_changed = self.view.as_ref().is_none_or(|v| v.panes != view.panes || v.borders != view.borders);
        if layout_changed || self.full {
            let (border_fg, active_fg) = (self.color(ChromeColor::Default), self.color(ChromeColor::Indexed(2)));
            for y in 0..self.frame.rows() {
                self.frame.row_mut(y).fill(FrameCell::blank(default_bg));
            }
            let (cols, rows) = (self.frame.cols(), self.frame.rows());
            for border in view.borders.iter().filter(|b| b.y < rows && b.x < cols) {
                let fg = if border.active { active_fg } else { border_fg };
                self.frame.row_mut(border.y)[border.x as usize] =
                    FrameCell { ch: border.glyph as u32, fg, bg: default_bg, flags: 0 };
            }
            self.full = true;
        }
        let status: Vec<FrameCell> = view
            .status
            .iter()
            .map(|c| {
                let mut fg = self.color(c.fg);
                let mut bg = if c.bg == ChromeColor::Default { default_bg } else { self.color(c.bg) };
                if c.reverse {
                    std::mem::swap(&mut fg, &mut bg);
                }
                let flags = if c.bold { BOLD } else { 0 } | if c.underline { UNDERLINE } else { 0 };
                FrameCell { ch: c.ch as u32, fg, bg, flags }
            })
            .collect();
        let row = self.frame.row_mut(self.frame.rows() - 1);
        let n = row.len().min(status.len());
        row[..n].copy_from_slice(&status[..n]);
        self.view = Some(view);
    }

    fn redraw(&mut self) {
        if self.window.is_none() {
            return;
        }
        self.compose_chrome();
        let Some(view) = self.view.take() else { return };
        let mut cursor = None;
        for pv in &view.panes {
            let Some(pane) = self.panes.get_mut(&pv.pane) else { continue };
            let shared = &pane.io.shared;
            let dirty = shared.dirty.swap(false, Ordering::AcqRel);
            if !dirty && !self.full && !pv.active {
                continue;
            }
            shared.want_lock.store(true, Ordering::Release);
            let mut term = shared.term.lock();
            shared.want_lock.store(false, Ordering::Release);
            let r = pv.rect;
            if dirty || self.full {
                compose::copy_pane(&mut self.frame, &mut term, r.x, r.y, r.w, r.h, self.full);
            }
            if pv.active && view.prompt_cursor.is_none() {
                cursor = compose::pane_cursor(&term, r.x, r.y, self.focused);
            }
            if !term.title().is_empty() && term.title() != pane.title {
                pane.title = term.title().to_owned();
                self.session.as_mut().expect("session").set_pane_title(pv.pane, &pane.title);
            }
        }
        if let Some(x) = view.prompt_cursor {
            cursor = Some(FrameCursor {
                x,
                y: self.frame.rows() - 1,
                shape: term::CursorShape::Block,
                color: self.color(ChromeColor::Indexed(0)),
                text_color: self.color(ChromeColor::Indexed(3)),
                wide: false,
            });
        }
        self.full = false;
        self.frame.cursor = cursor;
        self.view = Some(view);
        // A title change bumps the generation; the status line is rebuilt next frame.
        let stale = self.session.as_ref().is_some_and(|s| s.generation() != self.drawn.0);
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
        if clock() != self.drawn.1 || self.session.as_ref().map(|s| s.generation()) != before {
            self.request_redraw();
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        let deadline = self.session.as_ref().and_then(|s| s.deadline());
        let wake = deadline.map_or(self.next_names, |d| d.min(self.next_names));
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
        let (session, effects) = Session::new(cols, rows);
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
