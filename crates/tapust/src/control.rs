//! Requests from programs in the panes to the tapust running them, over a
//! Unix socket whose path the panes get in TAPUST_SOCKET:
//! `tapust --save-layout FILE` asks it to write its layout there.
//!
//! A request is one line, `save-layout PATH` with an absolute path escaped
//! as in the layout file; the reply is `ok` or `error REASON`.

use std::io::{BufRead, BufReader, Write};
use std::os::fd::AsRawFd;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::DirBuilderExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

use gtk::glib;

use crate::layout;

/// The variable that gives the panes the socket.
pub const ENV: &str = "TAPUST_SOCKET";

static PATH: OnceLock<PathBuf> = OnceLock::new();

/// The socket's path once tapust listens, for the panes' environment.
pub fn path() -> Option<&'static Path> {
    PATH.get().map(PathBuf::as_path)
}

/// Listens at $XDG_RUNTIME_DIR/tapust/PID.sock on the main loop; `save`
/// writes the layout to a path.
pub fn listen(save: impl Fn(&Path) -> Result<(), String> + 'static) -> Result<(), String> {
    let runtime = std::env::var_os("XDG_RUNTIME_DIR").ok_or("XDG_RUNTIME_DIR is not set")?;
    let dir = Path::new(&runtime).join("tapust");
    let path = dir.join(format!("{}.sock", std::process::id()));
    let fail = |err: std::io::Error| format!("cannot listen at {}: {err}", path.display());
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(&dir).map_err(fail)?;
    // One left by an earlier process with this id.
    let _ = std::fs::remove_file(&path);
    let listener = UnixListener::bind(&path).map_err(fail)?;
    listener.set_nonblocking(true).map_err(fail)?;
    glib::unix_fd_add_local(listener.as_raw_fd(), glib::IOCondition::IN, move |_, _| {
        while let Ok((stream, _)) = listener.accept() {
            serve(&stream, &save);
        }
        glib::ControlFlow::Continue
    });
    let _ = PATH.set(path);
    Ok(())
}

/// Removes the socket as tapust ends.
pub fn remove() {
    if let Some(path) = path() {
        let _ = std::fs::remove_file(path);
    }
}

/// Answers one request; a client that does not send it within a second is
/// dropped, so that the main loop goes on.
fn serve(stream: &UnixStream, save: &impl Fn(&Path) -> Result<(), String>) {
    let timeout = Some(Duration::from_secs(1));
    let _ = stream.set_nonblocking(false);
    let _ = stream.set_read_timeout(timeout);
    let _ = stream.set_write_timeout(timeout);
    let mut line = Vec::new();
    let reply = match BufReader::new(stream).read_until(b'\n', &mut line) {
        Ok(_) => match parse_request(&line) {
            Some(path) => save(&path),
            None => Err("unknown request".to_owned()),
        },
        Err(err) => Err(err.to_string()),
    };
    let reply = match reply {
        Ok(()) => "ok\n".to_owned(),
        Err(err) => format!("error {}\n", err.replace('\n', " ")),
    };
    let mut stream = stream;
    let _ = stream.write_all(reply.as_bytes());
}

fn parse_request(line: &[u8]) -> Option<PathBuf> {
    let path = line.strip_suffix(b"\n")?.strip_prefix(b"save-layout ")?;
    let path = PathBuf::from(std::ffi::OsString::from_vec(layout::decode(path)?));
    path.is_absolute().then_some(path)
}

/// Asks the tapust running this pane to write its layout to `file`.
pub fn save_layout(file: &Path) -> Result<(), String> {
    let socket = std::env::var_os(ENV).ok_or("not in a tapust pane: TAPUST_SOCKET is not set")?;
    let file = std::path::absolute(file).map_err(|err| format!("{}: {err}", file.display()))?;
    request(Path::new(&socket), &file)
}

fn request(socket: &Path, file: &Path) -> Result<(), String> {
    let fail = |err: std::io::Error| format!("cannot reach tapust at {}: {err}", socket.display());
    let mut stream = UnixStream::connect(socket).map_err(fail)?;
    stream.set_read_timeout(Some(Duration::from_secs(10))).map_err(fail)?;
    let mut line = b"save-layout ".to_vec();
    layout::encode(file.as_os_str().as_bytes(), &mut line);
    line.push(b'\n');
    stream.write_all(&line).map_err(fail)?;
    let mut reply = String::new();
    BufReader::new(stream).read_line(&mut reply).map_err(fail)?;
    match reply.trim_end_matches('\n') {
        "ok" => Ok(()),
        reply => Err(reply.strip_prefix("error ").unwrap_or("tapust did not answer").to_owned()),
    }
}
