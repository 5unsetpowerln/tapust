mod attention;
mod bindings;
mod compose;
mod config;
mod control;
mod copy;
mod font;
mod frame;
mod gl;
mod gui;
#[cfg(test)]
mod headless;
mod input;
mod layout;
mod links;
mod mouse;
mod notifications;
mod palette;
mod pty;
mod renderer;
mod scroll;
mod search;
mod selection;
mod shell;
mod stylesheet;
mod surface;
mod theme;
mod themes;
mod underline;

use std::error::Error;
use std::ffi::OsString;
use std::path::PathBuf;

use adw::prelude::*;
use gtk::gio;

const USAGE: &str = "usage: tapust [--load-layout FILE | -e PROGRAM [ARGS...]]
       tapust --save-layout FILE";

/// What the command line asks for.
#[derive(Debug, PartialEq)]
enum Cli {
    /// Run with the first pane running `command` instead of the configured
    /// command, or with the windows of the layout file `layout`.
    Run { command: Option<Vec<OsString>>, layout: Option<PathBuf> },
    /// Ask the tapust running this pane to write its layout to the file.
    SaveLayout(PathBuf),
}

fn parse_args(args: &[OsString]) -> Option<Cli> {
    let (command, layout) = match args {
        [] => (None, None),
        [flag, file] if flag == "--save-layout" => return Some(Cli::SaveLayout(file.into())),
        [flag, file] if flag == "--load-layout" => (None, Some(file.into())),
        [flag, program @ ..] if flag == "-e" && !program.is_empty() => (Some(program.to_vec()), None),
        _ => return None,
    };
    Some(Cli::Run { command, layout })
}

fn main() -> Result<(), Box<dyn Error>> {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    let (command, layout) = match parse_args(&args).ok_or(USAGE)? {
        Cli::SaveLayout(file) => {
            if let Err(err) = control::save_layout(&file) {
                eprintln!("tapust: {err}");
                std::process::exit(1);
            }
            return Ok(());
        }
        // Read before anything starts, so that a bad file is all that happens.
        Cli::Run { command, layout } => (command, layout.map(|file| layout::Layout::read(&file)).transpose()?),
    };
    pty::disable_vulkan();
    stylesheet::take_over();
    // Registered as GTK starts, which needs no fonts until the
    // configuration is read.
    let fonts = std::thread::Builder::new().name("font-register".into()).spawn(font::register_embedded_fonts)?;
    let application = adw::Application::builder()
        .application_id("dev.tapust.tapust")
        .flags(gio::ApplicationFlags::NON_UNIQUE)
        .build();
    // tapust has no resources: without a base path, GtkApplication adds no
    // icon path for the icon theme to scan in each of its directories.
    application.set_resource_base_path(None);
    // Every window's icon, set before GtkApplication starts, which otherwise
    // waits for the icon theme to load to look for one named as the
    // application id. gtk-rs wants GTK started for this, which GTK does not.
    let icon = std::ffi::CString::new(theme::ICON).expect("icon name");
    // SAFETY: a valid C string; GTK copies it.
    unsafe { gtk::ffi::gtk_window_set_default_icon_name(icon.as_ptr()) };
    let command = std::cell::RefCell::new(command);
    let layout = std::cell::RefCell::new(layout);
    let fonts = std::cell::RefCell::new(Some(fonts));
    application.connect_activate(move |application| {
        if let Some(fonts) = fonts.take() {
            let registered = fonts.join().unwrap_or_else(|panic| std::panic::resume_unwind(panic));
            if let Err(err) = registered {
                eprintln!("tapust: cannot register the bundled fonts: {err}");
                application.quit();
                return;
            }
        }
        gui::App::build(application, command.borrow_mut().take(), layout.borrow_mut().take());
    });
    application.run_with_args::<&str>(&[]);
    Ok(())
}
