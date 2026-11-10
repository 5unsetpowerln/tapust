//! A real tmux 3.6 server for tests, driven by one control-mode client.
//!
//! Safety: every server runs under `prlimit --as=2GiB --cpu=60`, on its own
//! socket in the cargo target tmp directory; a watchdog kills it at its time
//! limit; [`Tmux`]'s `Drop` kills it on success, failure and panic and waits
//! until it is gone; and `exit-unattached` makes it exit when the control
//! client goes away (for example when the test process dies).

#![allow(dead_code)]

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command as Process, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread;
use std::time::Duration;

use mux::{Command, Direction, PaneId, Session};

/// Sizes sent to tmux are clamped to these ranges.
pub const MIN_COLS: u16 = 10;
pub const MAX_COLS: u16 = 300;
pub const MIN_ROWS: u16 = 5;
pub const MAX_ROWS: u16 = 100;

pub fn clamp_cols(cols: u16) -> u16 {
    cols.clamp(MIN_COLS, MAX_COLS)
}

pub fn clamp_rows(rows: u16) -> u16 {
    rows.clamp(MIN_ROWS, MAX_ROWS)
}

/// A directory for sockets under the cargo target tmp directory.
pub fn work_dir(name: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(name);
    std::fs::create_dir_all(&dir).expect("create work dir");
    dir
}

pub fn check_version() {
    let out = Process::new("tmux").arg("-V").output().expect("run tmux -V");
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "tmux 3.6", "tests need tmux 3.6");
}

/// The pattern matching every process of the tmux server on `socket` (the
/// client, the server it forks and pane children before they exec).
fn socket_pattern(socket: &Path) -> String {
    format!("^tmux -S {} ", socket.display())
}

/// SIGKILLs every process of the server on `socket`.
pub fn kill_socket(socket: &Path) {
    let _ = Process::new("pkill")
        .args(["-9", "-f", &socket_pattern(socket)])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

/// Waits (up to 3 s) until no process of the server on `socket` is left.
pub fn wait_gone(socket: &Path) -> bool {
    for _ in 0..300 {
        let status = Process::new("pgrep")
            .args(["-f", &socket_pattern(socket)])
            .stdout(Stdio::null())
            .status();
        if status.is_ok_and(|s| !s.success()) {
            return true;
        }
        kill_socket(socket);
        thread::sleep(Duration::from_millis(10));
    }
    false
}

/// Kills the server on `socket` unless told to stand down before `limit`.
pub fn watchdog(limit: Duration, socket: PathBuf) -> Sender<()> {
    let (tx, rx) = mpsc::channel::<()>();
    thread::spawn(move || {
        if let Err(RecvTimeoutError::Timeout) = rx.recv_timeout(limit) {
            kill_socket(&socket);
        }
    });
    tx
}

/// Output of one control-mode command.
#[derive(Debug, PartialEq, Eq)]
pub enum Reply {
    Ok(Vec<String>),
    Error(Vec<String>),
    /// The client exited (`%exit`, or the server went away).
    Exit,
}

pub struct Tmux {
    socket: PathBuf,
    client: Child,
    stdin: Option<ChildStdin>,
    lines: Receiver<String>,
    watchdog: Option<Sender<()>>,
    exited: bool,
}

impl Tmux {
    /// Starts a server and a control client with a session `t` of one pane
    /// running `sleep`, its windows sized `cols x rows`, killed after `limit`.
    pub fn start(socket: &Path, cols: u16, rows: u16, limit: Duration) -> Result<Tmux, String> {
        kill_socket(socket);
        let _ = std::fs::remove_file(socket);
        let (cols, rows) = (clamp_cols(cols), clamp_rows(rows));
        let watchdog = watchdog(limit, socket.to_owned());
        let mut client = Process::new("prlimit")
            .args(["--as=2147483648", "--cpu=60", "--", "tmux", "-S"])
            .arg(socket)
            .args(["-f", "/dev/null", "-C", "new-session", "-s", "t"])
            .args(["-x", &cols.to_string(), "-y", &rows.to_string()])
            .arg("exec sleep 100000")
            .env_remove("TMUX")
            .env("SHELL", "/bin/sh")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("start tmux under prlimit: {e}"))?;
        let (tx, rx) = mpsc::channel();
        let stdout = client.stdout.take().expect("stdout");
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        let stdin = client.stdin.take();
        let mut tmux = Tmux {
            socket: socket.to_owned(),
            client,
            stdin,
            lines: rx,
            watchdog: Some(watchdog),
            exited: false,
        };
        // The reply to the new-session command itself.
        match tmux.read_reply() {
            Reply::Ok(_) => {}
            other => return Err(format!("tmux did not start: {other:?}")),
        }
        for setup in [
            "set -s exit-unattached on",
            "set -g destroy-unattached on",
            "set -g default-shell /bin/sh",
            "set -g default-command 'exec sleep 100000'",
            &format!("refresh-client -C {cols}x{rows}"),
        ] {
            match tmux.cmd(setup) {
                Reply::Ok(_) => {}
                other => return Err(format!("{setup}: {other:?}")),
            }
        }
        Ok(tmux)
    }

    /// Sets the control client's size, which resizes every window now
    /// (refresh-client -C calls recalculate_sizes_now(1)).
    pub fn resize(&mut self, cols: u16, rows: u16) -> Reply {
        self.cmd(&format!("refresh-client -C {}x{}", clamp_cols(cols), clamp_rows(rows)))
    }

    pub fn cmd(&mut self, line: &str) -> Reply {
        if self.exited {
            return Reply::Exit;
        }
        let Some(stdin) = self.stdin.as_mut() else { return Reply::Exit };
        if writeln!(stdin, "{line}").and_then(|()| stdin.flush()).is_err() {
            self.exited = true;
            return Reply::Exit;
        }
        self.read_reply()
    }

    fn read_reply(&mut self) -> Reply {
        let mut body: Option<Vec<String>> = None;
        loop {
            let Ok(line) = self.lines.recv_timeout(Duration::from_secs(20)) else {
                self.exited = true;
                return Reply::Exit;
            };
            if line.starts_with("%exit") {
                self.exited = true;
                return Reply::Exit;
            }
            match body.as_mut() {
                None if line.starts_with("%begin") => body = Some(Vec::new()),
                None => {}
                Some(_) if line.starts_with("%end") => return Reply::Ok(body.unwrap()),
                Some(_) if line.starts_with("%error") => return Reply::Error(body.unwrap()),
                Some(lines) => lines.push(line),
            }
        }
    }

    pub fn ok(&mut self, line: &str) -> Vec<String> {
        match self.cmd(line) {
            Reply::Ok(lines) => lines,
            other => panic!("{line}: {other:?}"),
        }
    }

    pub fn exited(&self) -> bool {
        self.exited
    }

    pub fn socket(&self) -> &Path {
        &self.socket
    }
}

impl Drop for Tmux {
    fn drop(&mut self) {
        self.stdin = None;
        kill_socket(&self.socket);
        let _ = self.client.kill();
        let _ = self.client.wait();
        let gone = wait_gone(&self.socket);
        self.watchdog = None;
        let _ = std::fs::remove_file(&self.socket);
        if !gone && !thread::panicking() {
            panic!("tmux on {} did not die", self.socket.display());
        }
    }
}

/// A small deterministic generator (SplitMix64).
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Rng {
        Rng(seed)
    }

    pub fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    /// Uniform in `lo..=hi`.
    pub fn range(&mut self, lo: u32, hi: u32) -> u32 {
        lo + (self.next() % u64::from(hi - lo + 1)) as u32
    }

    pub fn chance(&mut self, percent: u32) -> bool {
        self.range(0, 99) < percent
    }
}

#[derive(Clone, Debug)]
pub enum Step {
    Cmd(Command),
    /// Client size as tmux sees it (the model's client has one more row).
    Resize(u16, u16),
    /// The process of this pane (in any window) exits.
    PaneExit(PaneId),
}

fn dir_flag(dir: Direction) -> &'static str {
    match dir {
        Direction::Left => "-L",
        Direction::Right => "-R",
        Direction::Up => "-U",
        Direction::Down => "-D",
    }
}

pub fn tmux_line(cmd: &Command) -> String {
    match cmd {
        Command::SplitWindow { horizontal, before, full, size, .. } => {
            let mut line = String::from(if *horizontal { "split-window -h" } else { "split-window -v" });
            if *before {
                line.push_str(" -b");
            }
            if *full {
                line.push_str(" -f");
            }
            if let Some(size) = size {
                line.push_str(&format!(" -l {size}"));
            }
            line
        }
        Command::NewWindow { .. } => "new-window".into(),
        Command::SelectPane(dir) => format!("select-pane {}", dir_flag(*dir)),
        Command::LastPane => "last-pane".into(),
        Command::NextPane => "select-pane -t :.+".into(),
        Command::ResizePane(dir, n) => format!("resize-pane {} {n}", dir_flag(*dir)),
        Command::ZoomPane => "resize-pane -Z".into(),
        Command::KillPane => "kill-pane".into(),
        Command::KillWindow => "kill-window".into(),
        Command::BreakPane => "break-pane".into(),
        Command::SwapPane { up } => format!("swap-pane {}", if *up { "-U" } else { "-D" }),
        Command::RotateWindow { down } => {
            if *down { "rotate-window -D".into() } else { "rotate-window".into() }
        }
        Command::SelectWindow(n) => format!("select-window -t :={n}"),
        Command::NextWindow => "next-window".into(),
        Command::PreviousWindow => "previous-window".into(),
        Command::LastWindow => "last-window".into(),
        Command::SwapWindow(n) => format!("swap-window -d -t {n:+}"),
        Command::SpreadLayout => "select-layout -E".into(),
        Command::RenameWindow(_) | Command::RenameSession(_) | Command::SendPrefix => {
            unreachable!("not generated")
        }
    }
}

fn random_dir(rng: &mut Rng) -> Direction {
    [Direction::Left, Direction::Right, Direction::Up, Direction::Down][rng.range(0, 3) as usize]
}

pub fn random_size(rng: &mut Rng) -> (u16, u16) {
    if rng.chance(60) {
        (rng.range(10, 45) as u16, rng.range(5, 20) as u16)
    } else {
        (rng.range(10, 300) as u16, rng.range(5, 100) as u16)
    }
}

pub fn random_step(rng: &mut Rng, model: &Session) -> Step {
    let windows = model.windows();
    let panes = windows.iter().find(|w| w.active).map_or(1, |w| w.panes.len());
    loop {
        let roll = rng.range(0, 99);
        let step = match roll {
            0..=27 => {
                if panes > 14 && rng.chance(80) {
                    continue;
                }
                let horizontal = rng.chance(50);
                Command::SplitWindow {
                    horizontal,
                    before: rng.chance(25),
                    full: rng.chance(25),
                    size: rng.chance(30).then(|| rng.range(0, 40)),
                    cwd_of_active: false,
                }
            }
            28..=36 => Command::SelectPane(random_dir(rng)),
            37..=38 => Command::LastPane,
            39..=40 => Command::NextPane,
            41..=52 => Command::ResizePane(random_dir(rng), rng.range(1, 10)),
            53..=56 => Command::ZoomPane,
            57..=61 => Command::KillPane,
            62..=63 => {
                if windows.len() == 1 && rng.chance(80) {
                    continue;
                }
                Command::KillWindow
            }
            64..=68 => {
                if windows.len() > 6 {
                    continue;
                }
                Command::NewWindow { cwd_of_active: false }
            }
            69..=71 => Command::SelectWindow(rng.range(0, 6)),
            72..=73 => Command::NextWindow,
            74..=75 => Command::PreviousWindow,
            76..=77 => Command::LastWindow,
            78..=80 => Command::SwapWindow(if rng.chance(50) { -1 } else { 1 }),
            81..=83 => Command::BreakPane,
            84..=86 => Command::SwapPane { up: rng.chance(50) },
            87..=88 => Command::RotateWindow { down: rng.chance(50) },
            89..=90 => Command::SpreadLayout,
            91..=93 => {
                let all: Vec<PaneId> = windows.iter().flat_map(|w| w.panes.iter().map(|p| p.pane)).collect();
                if all.len() == 1 && rng.chance(80) {
                    continue;
                }
                return Step::PaneExit(all[rng.range(0, all.len() as u32 - 1) as usize]);
            }
            _ => {
                let (cols, rows) = random_size(rng);
                return Step::Resize(cols, rows);
            }
        };
        return Step::Cmd(step);
    }
}
