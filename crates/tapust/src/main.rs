mod attention;
mod bindings;
mod compose;
mod config;
mod copy;
mod font;
mod frame;
mod gl;
mod gui;
mod input;
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

use adw::prelude::*;
use gtk::gio;

fn main() -> Result<(), Box<dyn Error>> {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    // `tapust -e PROGRAM [ARGS...]` runs PROGRAM in the first pane instead of the configured command.
    let command = match args.split_first() {
        None => None,
        Some((flag, rest)) if flag == "-e" && !rest.is_empty() => Some(rest.to_vec()),
        Some(_) => return Err("usage: tapust [-e PROGRAM [ARGS...]]".into()),
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
        gui::App::build(application, command.borrow_mut().take());
    });
    application.run_with_args::<&str>(&[]);
    Ok(())
}
