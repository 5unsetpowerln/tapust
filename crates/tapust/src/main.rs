mod app;
mod compose;
mod font;
mod frame;
mod input;
mod pty;
mod renderer;
mod window;

use std::error::Error;

use winit::event_loop::EventLoop;

fn main() -> Result<(), Box<dyn Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    // `tapust -e PROGRAM [ARGS...]` runs PROGRAM in the first pane instead of the login shell.
    let command = match args.split_first() {
        None => None,
        Some((flag, rest)) if flag == "-e" && !rest.is_empty() => Some(rest.to_vec()),
        Some(_) => return Err("usage: tapust [-e PROGRAM [ARGS...]]".into()),
    };
    let event_loop = EventLoop::<app::UserEvent>::with_user_event().build()?;
    let mut app = app::App::new(event_loop.create_proxy(), command);
    event_loop.run_app(&mut app)?;
    Ok(())
}
