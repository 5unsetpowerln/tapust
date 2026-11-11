mod compose;
mod font;
mod frame;
mod gl;
mod gui;
mod input;
mod pty;
mod renderer;
mod surface;
mod theme;

use std::error::Error;

use adw::prelude::*;
use gtk::{gdk, gio};

fn main() -> Result<(), Box<dyn Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    // `tapust -e PROGRAM [ARGS...]` runs PROGRAM in the first pane instead of the login shell.
    let command = match args.split_first() {
        None => None,
        Some((flag, rest)) if flag == "-e" && !rest.is_empty() => Some(rest.to_vec()),
        Some(_) => return Err("usage: tapust [-e PROGRAM [ARGS...]]".into()),
    };
    font::register_embedded_fonts()?;
    let application = adw::Application::builder()
        .application_id("dev.tapust.tapust")
        .flags(gio::ApplicationFlags::NON_UNIQUE)
        .build();
    application.connect_startup(|_| {
        let provider = gtk::CssProvider::new();
        provider.load_from_string(&gui::css());
        let display = gdk::Display::default().expect("display");
        gtk::style_context_add_provider_for_display(&display, &provider, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
    });
    let command = std::cell::RefCell::new(command);
    application.connect_activate(move |application| {
        gui::App::build(application, command.borrow_mut().take());
    });
    application.run_with_args::<&str>(&[]);
    Ok(())
}
