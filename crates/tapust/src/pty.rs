//! Pseudo-terminals, the shell process and each pane's I/O thread.
//!
//! The I/O thread reads PTY output in large chunks and parses it under the
//! terminal lock. While the renderer holds the lock, the I/O thread keeps
//! reading into its buffer instead of waiting, so one lock acquisition parses
//! as much as possible. The renderer can ask for the lock with
//! [`Shared::want_lock`]; the I/O thread then releases it after the chunk it
//! is parsing. It also watches the child process, as ghostty does, and
//! reports its exit once the output read so far is parsed.

use std::collections::VecDeque;
use std::ffi::{CStr, OsStr, OsString};
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use parking_lot::Mutex;

use crate::shell;

const READ_BUFFER: usize = 1 << 20;

/// What the I/O thread drives: a terminal, or a recorder in tests.
pub trait Feed: Send + 'static {
    fn feed(&mut self, bytes: &[u8]);
    /// Bytes to send back to the application (query replies).
    fn take_replies(&mut self) -> Option<Vec<u8>>;
    /// Whether the application must hear of a change besides the cells.
    fn take_news(&mut self) -> bool;
}

impl Feed for term::Terminal {
    fn feed(&mut self, bytes: &[u8]) {
        term::Terminal::feed(self, bytes);
    }

    fn take_replies(&mut self) -> Option<Vec<u8>> {
        term::Terminal::take_replies(self)
    }

    fn take_news(&mut self) -> bool {
        term::Terminal::take_news(self)
    }
}

fn cvt(ret: libc::c_int) -> io::Result<libc::c_int> {
    if ret == -1 { Err(io::Error::last_os_error()) } else { Ok(ret) }
}

fn winsize(cols: u16, rows: u16, cell: (u32, u32)) -> libc::winsize {
    libc::winsize {
        ws_row: rows,
        ws_col: cols,
        ws_xpixel: (cols as u32 * cell.0).min(u16::MAX as u32) as u16,
        ws_ypixel: (rows as u32 * cell.1).min(u16::MAX as u32) as u16,
    }
}

/// The user's login shell from the password database.
fn login_shell() -> io::Result<PathBuf> {
    let pw = unsafe { libc::getpwuid(libc::getuid()) };
    if pw.is_null() || unsafe { (*pw).pw_shell }.is_null() {
        return Err(io::Error::new(io::ErrorKind::NotFound, "no password entry for the current user"));
    }
    let shell = unsafe { CStr::from_ptr((*pw).pw_shell) };
    Ok(PathBuf::from(OsStr::from_bytes(shell.to_bytes())))
}

/// Variables that identify another terminal or multiplexer to its programs.
const OTHER_TERMINALS: [&str; 19] = [
    "TMUX",
    "TMUX_PANE",
    "STY",
    "VTE_VERSION",
    "KITTY_WINDOW_ID",
    "KITTY_PID",
    "KITTY_PUBLIC_KEY",
    "WEZTERM_PANE",
    "WEZTERM_UNIX_SOCKET",
    "WEZTERM_EXECUTABLE",
    "GHOSTTY_RESOURCES_DIR",
    "GHOSTTY_BIN_DIR",
    "GHOSTTY_SHELL_FEATURES",
    "ALACRITTY_SOCKET",
    "ALACRITTY_WINDOW_ID",
    "KONSOLE_VERSION",
    "LC_TERMINAL",
    "LC_TERMINAL_VERSION",
    "WINDOWID",
];

/// The controlling side of a PTY with a process on the other side.
pub struct Pty {
    master: OwnedFd,
    child: Child,
    started: Instant,
}

/// How the child process ended (ghostty's Message.ChildExited).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChildExit {
    /// The exit status, or the number of the signal that killed it.
    pub code: u32,
    /// From the spawn to the exit.
    pub runtime: Duration,
}

/// What a pane runs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Program {
    /// The login shell as a login shell (argv[0] prefixed with '-', like tmux).
    LoginShell,
    /// A program and its arguments.
    Argv(Vec<OsString>),
    /// ghostty's shell command: a command line for `/bin/sh -c`.
    ShellCommand(String),
}

impl Pty {
    /// Starts `program` with tapust's shell integration, whose prompt
    /// cursor blinks with `cursor_blink`.
    pub fn spawn(
        program: Program,
        cursor_blink: bool,
        cols: u16,
        rows: u16,
        cell: (u32, u32),
        cwd: &Path,
    ) -> io::Result<Pty> {
        let mut env: shell::Env = std::env::vars_os().collect();
        let login = program == Program::LoginShell;
        let argv = match program {
            Program::LoginShell => shell::prepare(vec![login_shell()?.into_os_string()], true, cursor_blink, &mut env),
            Program::Argv(argv) => shell::prepare(argv, false, cursor_blink, &mut env),
            Program::ShellCommand(line) => shell::prepare_command(&line, cursor_blink, &mut env),
        };
        let mut command = Command::new(&argv[0]);
        command.args(&argv[1..]).env_clear().envs(&env);
        if login {
            let name = Path::new(&argv[0]).file_name().map(|n| n.as_bytes()).unwrap_or(b"sh");
            command.arg0(OsStr::from_bytes(&[b"-", name].concat()));
        }
        Self::spawn_command(command, cols, rows, cell, cwd)
    }

    /// Runs `command` on a new PTY of `cols x rows` cells in `cwd`.
    pub fn spawn_command(mut command: Command, cols: u16, rows: u16, cell: (u32, u32), cwd: &Path) -> io::Result<Pty> {
        let (mut master, mut slave): (RawFd, RawFd) = (-1, -1);
        let size = winsize(cols, rows, cell);
        cvt(unsafe { libc::openpty(&mut master, &mut slave, std::ptr::null_mut(), std::ptr::null(), &size) })?;
        let master = unsafe { OwnedFd::from_raw_fd(master) };
        let slave = unsafe { OwnedFd::from_raw_fd(slave) };
        // Before any child exists: a child holding the master would keep the
        // PTY open after tapust dies, so its programs would never hang up.
        for fd in [&master, &slave] {
            cvt(unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) })?;
        }
        let mut tio: libc::termios = unsafe { std::mem::zeroed() };
        cvt(unsafe { libc::tcgetattr(slave.as_raw_fd(), &mut tio) })?;
        tio.c_iflag |= libc::IUTF8;
        cvt(unsafe { libc::tcsetattr(slave.as_raw_fd(), libc::TCSANOW, &tio) })?;

        command
            .stdin(Stdio::from(slave.try_clone()?))
            .stdout(Stdio::from(slave.try_clone()?))
            .stderr(Stdio::from(slave))
            .current_dir(cwd)
            .env("TERM", "xterm-256color")
            .env("COLORTERM", "truecolor")
            .env("TERM_PROGRAM", "tapust")
            .env("TERM_PROGRAM_VERSION", env!("CARGO_PKG_VERSION"))
            .env("PWD", cwd);
        // Programs must not mistake tapust for the terminal tapust was started from.
        for var in OTHER_TERMINALS {
            command.env_remove(var);
        }
        unsafe {
            command.pre_exec(|| {
                // A new session with the PTY as its controlling terminal.
                if libc::setsid() == -1 || libc::ioctl(0, libc::TIOCSCTTY, 0) == -1 {
                    return Err(io::Error::last_os_error());
                }
                for signal in [libc::SIGCHLD, libc::SIGHUP, libc::SIGINT, libc::SIGQUIT, libc::SIGTERM, libc::SIGALRM] {
                    libc::signal(signal, libc::SIG_DFL);
                }
                Ok(())
            });
        }
        let child = command.spawn()?;
        let started = Instant::now();
        let fd = master.as_raw_fd();
        let flags = cvt(unsafe { libc::fcntl(fd, libc::F_GETFL) })?;
        cvt(unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) })?;
        Ok(Pty { master, child, started })
    }

    pub fn resize(&self, cols: u16, rows: u16, cell: (u32, u32)) -> io::Result<()> {
        let size = winsize(cols, rows, cell);
        cvt(unsafe { libc::ioctl(self.master.as_raw_fd(), libc::TIOCSWINSZ, &size) })?;
        Ok(())
    }

    /// Working directory of the foreground process (tmux 3.6 `osdep_get_cwd`).
    pub fn foreground_cwd(&self) -> Option<PathBuf> {
        let fd = self.master.as_raw_fd();
        let pgrp = unsafe { libc::tcgetpgrp(fd) };
        if pgrp == -1 {
            return None;
        }
        std::fs::read_link(format!("/proc/{pgrp}/cwd")).ok().or_else(|| {
            let mut sid: libc::pid_t = 0;
            (unsafe { libc::ioctl(fd, libc::TIOCGSID, &mut sid) } != -1)
                .then(|| std::fs::read_link(format!("/proc/{sid}/cwd")).ok())
                .flatten()
        })
    }

    /// Foreground process name for automatic window names (tmux 3.6
    /// `osdep_get_name` followed by `parse_window_name`).
    pub fn foreground_name(&self) -> Option<String> {
        let pgrp = unsafe { libc::tcgetpgrp(self.master.as_raw_fd()) };
        if pgrp == -1 {
            return None;
        }
        let cmdline = std::fs::read(format!("/proc/{pgrp}/cmdline")).ok()?;
        let argv0 = cmdline.split(|b| *b == 0).next()?;
        Some(parse_window_name(&String::from_utf8_lossy(argv0)))
    }
}

impl Drop for Pty {
    fn drop(&mut self) {
        // Closing the master hangs up the session; SIGHUP covers shells that ignore EOF.
        let pid = self.child.id() as libc::pid_t;
        unsafe { libc::kill(pid, libc::SIGHUP) };
        // A child that has not exited yet is reaped when it does, so it never
        // stays a zombie for the rest of tapust's life.
        if let Ok(None) = self.child.try_wait() {
            let reaper = std::thread::Builder::new().name("pane-reaper".into());
            let _ = reaper.spawn(move || unsafe { libc::waitpid(pid, std::ptr::null_mut(), 0) });
        }
    }
}

/// tmux 3.6 `parse_window_name`.
pub fn parse_window_name(input: &str) -> String {
    let mut name = input.strip_prefix('"').unwrap_or(input);
    name = &name[..name.find('"').unwrap_or(name.len())];
    name = name.strip_prefix("exec ").unwrap_or(name);
    name = name.trim_start_matches([' ', '-']);
    name = &name[..name.find(' ').unwrap_or(name.len())];
    while let Some(last) = name.chars().last().filter(|_| name.len() > 1) {
        if last.is_ascii_alphanumeric() || last.is_ascii_punctuation() {
            break;
        }
        name = &name[..name.len() - last.len_utf8()];
    }
    if name.starts_with('/') {
        name = Path::new(name).file_name().and_then(|n| n.to_str()).unwrap_or(name);
    }
    name.to_owned()
}

/// State shared between a pane's I/O thread and the main thread.
pub struct Shared<T> {
    pub term: Mutex<T>,
    /// Set by the I/O thread after it changed the terminal; cleared by the renderer.
    pub dirty: AtomicBool,
    /// Set by the I/O thread when the terminal has news besides the cells
    /// (Feed::take_news); cleared by the application when woken for it.
    pub news: AtomicBool,
    /// Set by the renderer while it waits for the terminal lock.
    pub want_lock: AtomicBool,
    pub exited: OnceLock<ChildExit>,
    pending: Mutex<VecDeque<u8>>,
    master: RawFd,
    wake: OwnedFd,
}

impl<T> Shared<T> {
    /// ghostty's password input heuristic: the line discipline is canonical
    /// but does not echo.
    pub fn password_input(&self) -> bool {
        let mut tio: libc::termios = unsafe { std::mem::zeroed() };
        if unsafe { libc::tcgetattr(self.master, &mut tio) } == -1 {
            return false;
        }
        tio.c_lflag & libc::ICANON != 0 && tio.c_lflag & libc::ECHO == 0
    }

    /// Writes input for the application: directly when the PTY takes it,
    /// otherwise the rest is queued for the I/O thread, keeping byte order.
    pub fn write(&self, bytes: &[u8]) {
        let mut pending = self.pending.lock();
        let mut rest = bytes;
        if pending.is_empty() {
            while !rest.is_empty() {
                let n = unsafe { libc::write(self.master, rest.as_ptr().cast(), rest.len()) };
                if n <= 0 {
                    break;
                }
                rest = &rest[n as usize..];
            }
        }
        if !rest.is_empty() {
            pending.extend(rest);
            drop(pending);
            self.wake();
        }
    }

    fn wake(&self) {
        let one: u64 = 1;
        unsafe { libc::write(self.wake.as_raw_fd(), (&one as *const u64).cast(), 8) };
    }
}

/// A running pane: its PTY and I/O thread.
pub struct PaneIo<T> {
    pub pty: Pty,
    pub shared: Arc<Shared<T>>,
    thread: Option<JoinHandle<()>>,
    stop: Arc<AtomicBool>,
}

impl<T: Feed> PaneIo<T> {
    /// Starts the I/O thread. `notify` runs on the I/O thread when the
    /// terminal becomes dirty or the process exits.
    pub fn start(pty: Pty, term: T, notify: impl Fn() + Send + 'static) -> io::Result<PaneIo<T>> {
        let wake = cvt(unsafe { libc::eventfd(0, libc::EFD_CLOEXEC | libc::EFD_NONBLOCK) })?;
        let pid = pty.child.id() as libc::pid_t;
        let pidfd = cvt(unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) } as libc::c_int)?;
        let child = (unsafe { OwnedFd::from_raw_fd(pidfd) }, pty.started);
        let shared = Arc::new(Shared {
            term: Mutex::new(term),
            dirty: AtomicBool::new(false),
            news: AtomicBool::new(false),
            want_lock: AtomicBool::new(false),
            exited: OnceLock::new(),
            pending: Mutex::new(VecDeque::new()),
            master: pty.master.as_raw_fd(),
            wake: unsafe { OwnedFd::from_raw_fd(wake) },
        });
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let (shared, stop) = (shared.clone(), stop.clone());
            std::thread::Builder::new().name("pane-io".into()).spawn(move || io_loop(&shared, &stop, child, notify))?
        };
        Ok(PaneIo { pty, shared, thread: Some(thread), stop })
    }
}

impl<T> Drop for PaneIo<T> {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.shared.wake();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn io_loop<T: Feed>(shared: &Shared<T>, stop: &AtomicBool, (pidfd, started): (OwnedFd, Instant), notify: impl Fn()) {
    let mut buf = vec![0u8; READ_BUFFER].into_boxed_slice();
    let mut filled = 0;
    let mut scratch: Vec<u8> = Vec::new();
    let master = shared.master;
    let wake = shared.wake.as_raw_fd();
    // Every process closed the slave side.
    let mut eof = false;
    // The child exited, not yet reported.
    let mut exit = None;

    while !stop.load(Ordering::Acquire) {
        let has_pending = !shared.pending.lock().is_empty();
        let watching = exit.is_none() && shared.exited.get().is_none();
        let mut fds = [
            libc::pollfd {
                fd: if eof { -1 } else { master },
                events: libc::POLLIN | if has_pending { libc::POLLOUT } else { 0 },
                revents: 0,
            },
            libc::pollfd { fd: wake, events: libc::POLLIN, revents: 0 },
            libc::pollfd { fd: if watching { pidfd.as_raw_fd() } else { -1 }, events: libc::POLLIN, revents: 0 },
        ];
        // While output waits behind a busy lock, wake up soon to retry it.
        let timeout = if filled > 0 { 1 } else { -1 };
        if unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, timeout) } < 0 {
            if io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
                continue;
            }
            break;
        }
        if fds[1].revents & libc::POLLIN != 0 {
            let mut counter = 0u64;
            unsafe { libc::read(wake, (&mut counter as *mut u64).cast(), 8) };
        }
        if fds[0].revents & libc::POLLOUT != 0 {
            let mut pending = shared.pending.lock();
            scratch.clear();
            scratch.extend(pending.iter());
            let n = unsafe { libc::write(master, scratch.as_ptr().cast(), scratch.len()) };
            if n > 0 {
                pending.drain(..n as usize);
            }
        }

        if fds[0].revents & (libc::POLLIN | libc::POLLHUP | libc::POLLERR) != 0 || filled > 0 {
            loop {
                let mut got_data = false;
                while filled < buf.len() {
                    let n = unsafe { libc::read(master, buf[filled..].as_mut_ptr().cast(), buf.len() - filled) };
                    if n > 0 {
                        filled += n as usize;
                        got_data = true;
                    } else {
                        // EIO: every process closed the slave side.
                        eof = n == 0 || io::Error::last_os_error().raw_os_error() == Some(libc::EIO);
                        break;
                    }
                }
                if filled == 0 {
                    break;
                }
                let mut term = match shared.term.try_lock() {
                    Some(term) => term,
                    // The renderer holds the lock: keep reading until the buffer is full.
                    None if filled < buf.len() && !eof => break,
                    None => shared.term.lock(),
                };
                term.feed(&buf[..filled]);
                let replies = term.take_replies();
                let news = term.take_news();
                drop(term);
                filled = 0;
                if let Some(replies) = replies {
                    shared.write(&replies);
                }
                // Output the renderer has not drawn yet already woke the
                // application, but news after that waking needs another.
                let news = news && !shared.news.swap(true, Ordering::AcqRel);
                if !shared.dirty.swap(true, Ordering::AcqRel) || news {
                    notify();
                }
                if eof || !got_data || shared.want_lock.load(Ordering::Acquire) {
                    break;
                }
            }
        }
        // The child wrote its last output before it exited, so the read
        // above got it when both were ready.
        if fds[2].revents & libc::POLLIN != 0 {
            match child_exit(&pidfd, started) {
                Ok(e) => exit = Some(e),
                Err(err) => {
                    eprintln!("tapust: cannot wait for the child process: {err}");
                    break;
                }
            }
        }
        if filled == 0
            && let Some(exit) = exit.take()
        {
            let _ = shared.exited.set(exit);
            notify();
        }
        if eof && filled == 0 && shared.exited.get().is_some() {
            break;
        }
    }
}

/// How the child behind `pidfd` exited, as libxev's process watcher reports
/// it (the siginfo status). WNOWAIT leaves the child for Pty's drop to reap.
fn child_exit(pidfd: &OwnedFd, started: Instant) -> io::Result<ChildExit> {
    let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
    let options = libc::WEXITED | libc::WNOWAIT;
    while unsafe { libc::waitid(libc::P_PIDFD, pidfd.as_raw_fd() as libc::id_t, &mut info, options) } == -1 {
        let err = io::Error::last_os_error();
        if err.kind() != io::ErrorKind::Interrupted {
            return Err(err);
        }
    }
    Ok(ChildExit { code: unsafe { info.si_status() } as u32, runtime: started.elapsed() })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    #[derive(Default)]
    struct Recorder(Vec<u8>);

    impl Feed for Recorder {
        fn feed(&mut self, bytes: &[u8]) {
            self.0.extend_from_slice(bytes);
        }

        fn take_replies(&mut self) -> Option<Vec<u8>> {
            None
        }

        fn take_news(&mut self) -> bool {
            false
        }
    }

    fn start(script: &str) -> (PaneIo<Recorder>, mpsc::Receiver<()>) {
        let mut command = Command::new("/bin/sh");
        command.arg("-c").arg(script);
        let pty = Pty::spawn_command(command, 80, 24, (8, 16), Path::new("/")).unwrap();
        let (tx, rx) = mpsc::channel();
        let io = PaneIo::start(pty, Recorder::default(), move || {
            let _ = tx.send(());
        })
        .unwrap();
        (io, rx)
    }

    fn wait_for(io: &PaneIo<Recorder>, rx: &mpsc::Receiver<()>, done: impl Fn(&[u8], bool) -> bool) -> String {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            {
                let term = io.shared.term.lock();
                if done(&term.0, io.shared.exited.get().is_some()) {
                    return String::from_utf8_lossy(&term.0).into_owned();
                }
            }
            io.shared.dirty.store(false, Ordering::Release);
            let left = deadline.saturating_duration_since(Instant::now());
            assert!(!left.is_zero(), "timed out: {:?}", String::from_utf8_lossy(&io.shared.term.lock().0));
            let _ = rx.recv_timeout(left.min(Duration::from_millis(100)));
        }
    }

    #[test]
    fn reads_output_and_detects_exit() {
        let (io, rx) = start("printf 'hello from pty'");
        let out = wait_for(&io, &rx, |_, exited| exited);
        assert!(out.contains("hello from pty"), "{out:?}");
    }

    #[test]
    fn exit_reports_the_status_and_runtime() {
        let (io, rx) = start("exit 3");
        wait_for(&io, &rx, |_, exited| exited);
        let exit = *io.shared.exited.get().unwrap();
        assert_eq!(exit.code, 3);
        assert!(exit.runtime < Duration::from_secs(5), "{exit:?}");
        let (io, rx) = start("kill -9 $$");
        wait_for(&io, &rx, |_, exited| exited);
        assert_eq!(io.shared.exited.get().unwrap().code, 9);
    }

    #[test]
    fn exit_does_not_wait_for_the_pty_to_close() {
        // The background sleep keeps the slave side open.
        let started = Instant::now();
        let (io, rx) = start("trap '' HUP; sleep 3 & printf done");
        let out = wait_for(&io, &rx, |_, exited| exited);
        assert!(started.elapsed() < Duration::from_secs(2), "{:?}", started.elapsed());
        assert_eq!(out, "done");
        assert_eq!(io.shared.exited.get().unwrap().code, 0);
    }

    #[test]
    fn window_size_reaches_the_child() {
        let (io, rx) = start("read _; stty size");
        io.pty.resize(100, 37, (8, 16)).unwrap();
        io.shared.write(b"\n");
        let out = wait_for(&io, &rx, |_, exited| exited);
        assert!(out.contains("37 100"), "{out:?}");
    }

    #[test]
    fn foreground_cwd_and_name_follow_the_process() {
        let (io, rx) = start("cd /usr/share && printf ready; exec sleep 5");
        wait_for(&io, &rx, |out, _| out.ends_with(b"ready"));
        let deadline = Instant::now() + Duration::from_secs(5);
        while io.pty.foreground_name().as_deref() != Some("sleep") {
            assert!(Instant::now() < deadline, "name: {:?}", io.pty.foreground_name());
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(io.pty.foreground_cwd(), Some(PathBuf::from("/usr/share")));
    }

    #[test]
    fn large_input_does_not_block_the_writer() {
        let (io, rx) = start("stty -echo -icanon; printf ready; head -c 1048576 > /dev/null; printf done");
        wait_for(&io, &rx, |out, _| out.ends_with(b"ready"));
        let chunk = vec![b'x'; 64 * 1024];
        let started = Instant::now();
        for _ in 0..16 {
            io.shared.write(&chunk);
        }
        assert!(started.elapsed() < Duration::from_secs(1), "the write blocked the caller");
        let out = wait_for(&io, &rx, |_, exited| exited);
        assert!(out.ends_with("done"), "{out:?}");
    }

    #[test]
    fn window_names_follow_tmux() {
        assert_eq!(parse_window_name("-bash"), "bash");
        assert_eq!(parse_window_name("/usr/bin/vim"), "vim");
        assert_eq!(parse_window_name("\"exec  /bin/zsh\" -l"), "zsh");
        assert_eq!(parse_window_name("python3 script.py"), "python3");
    }
}
