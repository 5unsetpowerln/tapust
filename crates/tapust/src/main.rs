mod font;
mod input;
mod pty;
mod frame;
mod renderer;
mod window;

use std::error::Error;

use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, EventLoop};
use winit::window::WindowId;

use crate::frame::{BOLD, Frame, ITALIC, UNDERLINE, WIDE};
use crate::window::GlWindow;

const BG: [u8; 4] = [0x1d, 0x1f, 0x21, 0xff];
const FG: [u8; 4] = [0xc5, 0xc8, 0xc6, 0xff];

struct App {
    window: Option<GlWindow>,
    frame: Option<Frame>,
}

fn demo_frame(cols: u16, rows: u16) -> Frame {
    let mut frame = Frame::new(cols, rows, BG);
    let green = [0x00, 0xcd, 0x00, 0xff];
    frame.put_str(0, 0, "tapust: ASCII text 0123456789", FG, BG, 0);
    frame.put_str(0, 1, "bold", FG, BG, BOLD);
    frame.put_str(5, 1, "italic", FG, BG, ITALIC);
    frame.put_str(12, 1, "underline", FG, BG, UNDERLINE);
    frame.put_str(0, 2, "red on blue", [0xff, 0x40, 0x40, 0xff], [0x20, 0x40, 0xa0, 0xff], 0);
    for (i, ch) in ['\u{65e5}', '\u{672c}', '\u{8a9e}'].into_iter().enumerate() {
        let cell = &mut frame.row_mut(3)[i * 2];
        (cell.ch, cell.fg, cell.flags) = (ch as u32, FG, WIDE);
    }
    let line: String = std::iter::repeat_n('\u{2500}', 10).collect();
    frame.put_str(0, 5, &line, green, BG, 0);
    frame.put_str(10, 4, "\u{2502}", green, BG, 0);
    frame.put_str(10, 5, "\u{253c}", green, BG, 0);
    frame.put_str(10, 6, "\u{2502}", green, BG, 0);
    let status = format!("{:<width$}", "[0] 0:bash*", width = cols as usize);
    frame.put_str(0, rows - 1, &status, [0, 0, 0, 0xff], green, 0);
    frame.cursor = Some(frame::FrameCursor {
        x: 30,
        y: 0,
        shape: term::CursorShape::Block,
        color: FG,
        text_color: BG,
        wide: false,
    });
    frame
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let window = GlWindow::new(event_loop, "monospace", 12.0).expect("create window");
        if std::env::var_os("TAPUST_DEBUG").is_some() {
            eprintln!("renderer: {}", window.renderer.info());
        }
        window.window.request_redraw();
        self.window = Some(window);
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        let Some(window) = self.window.as_mut() else { return };
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => {
                let (cols, rows) = window.resize(size);
                self.frame = Some(demo_frame(cols, rows));
                window.window.request_redraw();
            }
            WindowEvent::RedrawRequested => {
                if let Some(frame) = self.frame.as_mut() {
                    window.draw(frame);
                }
            }
            _ => {}
        }
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    let event_loop = EventLoop::new()?;
    event_loop.run_app(&mut App { window: None, frame: None })?;
    Ok(())
}
