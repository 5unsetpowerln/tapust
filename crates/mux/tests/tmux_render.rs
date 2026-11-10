//! Compares borders, the status line, prompts and messages with what a real
//! tmux 3.6 client draws: an inner tmux client runs in a pane of an outer
//! tmux server, which captures it with `capture-pane -e`. Keys reach the
//! inner client through the outer server's `send-keys`; the inner server
//! loads tmux-pain-control from `refs/`.

mod common;

use std::path::{Path, PathBuf};
use std::process::{Command as Process, Stdio};
use std::sync::mpsc::Sender;
use std::thread;
use std::time::{Duration, Instant};

use common::{random_step, tmux_line, Rng, Step};
use mux::{ChromeColor, Command, Key, KeyCode, Mods, Session};

/// One drawn cell: character, fg, bg (palette index or default), bold,
/// underline, reverse.
type Cell = (char, Option<u8>, Option<u8>, bool, bool, bool);

const BLANK: Cell = (' ', None, None, false, false, false);

struct Nested {
    dir: PathBuf,
    outer: PathBuf,
    inner: PathBuf,
    cols: u16,
    rows: u16,
    _watchdogs: [Sender<()>; 2],
}

fn tmux(socket: &Path, args: &[&str]) -> Result<String, String> {
    let out = Process::new("prlimit")
        .args(["--as=2147483648", "--cpu=60", "--", "tmux", "-S"])
        .arg(socket)
        .args(args)
        .env_remove("TMUX")
        .env("LC_ALL", "C.UTF-8")
        .stdin(Stdio::null())
        .output()
        .map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).into_owned())
    }
}

impl Nested {
    /// An inner client of `cols x rows` (status line included) on session `0`.
    fn start(name: &str, cols: u16, rows: u16) -> Nested {
        let dir = common::work_dir(&format!("mux-render-{name}"));
        let (outer, inner) = (dir.join(format!("{name}-o")), dir.join(format!("{name}-i")));
        for s in [&outer, &inner] {
            common::kill_socket(s);
            let _ = std::fs::remove_file(s);
        }
        let limit = Duration::from_secs(300);
        let watchdogs = [
            common::watchdog(limit, outer.clone()),
            common::watchdog(limit, inner.clone()),
        ];
        let n = Nested { dir, outer, inner, cols, rows, _watchdogs: watchdogs };
        let client = format!(
            "env LC_ALL=C.UTF-8 TERM=screen-256color SHELL=/bin/sh tmux -S {} -u -f /dev/null \
             new-session -s 0 'exec sleep 100000'",
            n.inner.display()
        );
        let (x, y) = (cols.to_string(), rows.to_string());
        tmux(&n.outer, &["-f", "/dev/null", "new-session", "-d", "-s", "o", "-x", &x, "-y", &y, &client])
            .expect("start outer tmux");
        let start = Instant::now();
        while tmux(&n.inner, &["has-session"]).is_err() {
            assert!(start.elapsed() < Duration::from_secs(10), "inner tmux did not start");
            thread::sleep(Duration::from_millis(20));
        }
        let plugin = concat!(env!("CARGO_MANIFEST_DIR"), "/../../../refs/tmux-pain-control/pain_control.tmux");
        for setup in [
            &["set", "-s", "exit-unattached", "on"][..],
            &["set", "-g", "default-shell", "/bin/sh"],
            &["set", "-g", "default-command", "exec sleep 100000"],
            &["run-shell", plugin],
        ] {
            n.inner(setup);
        }
        n
    }

    /// Redraws the inner client. tmux does not redraw after some failing
    /// commands (split-window's "no space" path after window_push_zoom
    /// unzoomed), so its screen can lag its state until the next redraw.
    fn redraw(&self) {
        let client = self.inner(&["list-clients", "-F", "#{client_name}"]);
        self.inner(&["refresh-client", "-t", client.trim()]);
    }

    fn inner(&self, args: &[&str]) -> String {
        tmux(&self.inner, args).unwrap_or_else(|e| panic!("{args:?}: {e}"))
    }

    fn keys(&self, keys: &[&str]) {
        let mut args = vec!["send-keys", "-t", "o"];
        args.extend_from_slice(keys);
        tmux(&self.outer, &args).expect("send-keys");
    }

    /// The inner client's screen: rows of cells.
    fn capture(&self) -> Vec<Vec<Cell>> {
        let text = tmux(&self.outer, &["capture-pane", "-e", "-N", "-p", "-t", "o"]).unwrap_or_default();
        // capture-pane -e emits attributes only where they change, also
        // across lines, so one parser state runs over the whole capture.
        let mut state = BLANK;
        let mut rows: Vec<Vec<Cell>> = text.lines().map(|l| parse_sgr_line(l, &mut state)).collect();
        rows.resize(usize::from(self.rows), Vec::new());
        for row in &mut rows {
            row.resize(usize::from(self.cols), BLANK);
        }
        rows
    }

    fn cursor(&self) -> (u16, u16) {
        let out = tmux(&self.outer, &["display", "-p", "-t", "o", "#{cursor_x} #{cursor_y}"]).unwrap_or_default();
        let mut it = out.split_whitespace().map(|v| v.parse().unwrap_or(u16::MAX));
        (it.next().unwrap_or(u16::MAX), it.next().unwrap_or(u16::MAX))
    }
}

impl Drop for Nested {
    fn drop(&mut self) {
        for s in [&self.inner, &self.outer] {
            common::kill_socket(s);
        }
        for s in [&self.inner, &self.outer] {
            let gone = common::wait_gone(s);
            let _ = std::fs::remove_file(s);
            if !gone && !thread::panicking() {
                panic!("tmux on {} did not die", s.display());
            }
        }
        let _ = std::fs::remove_dir(&self.dir);
    }
}

/// Parses one `capture-pane -e` line into cells; `state` holds the
/// attributes in effect.
fn parse_sgr_line(line: &str, state: &mut Cell) -> Vec<Cell> {
    let mut cells = Vec::new();
    let (_, mut fg, mut bg, mut bold, mut underline, mut reverse) = *state;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\x1b' {
            cells.push((c, fg, bg, bold, underline, reverse));
            continue;
        }
        if chars.next() != Some('[') {
            continue;
        }
        let mut params = String::new();
        for c in chars.by_ref() {
            if c == 'm' {
                break;
            }
            params.push(c);
        }
        let values: Vec<u32> = params.split([';', ':']).map(|v| v.parse().unwrap_or(0)).collect();
        let mut i = 0;
        while i < values.len() {
            match values[i] {
                0 => (fg, bg, bold, underline, reverse) = (None, None, false, false, false),
                1 => bold = true,
                4 => underline = true,
                7 => reverse = true,
                22 => bold = false,
                24 => underline = false,
                27 => reverse = false,
                v @ 30..=37 => fg = Some((v - 30) as u8),
                39 => fg = None,
                v @ 40..=47 => bg = Some((v - 40) as u8),
                49 => bg = None,
                v @ 90..=97 => fg = Some((v - 90 + 8) as u8),
                v @ 100..=107 => bg = Some((v - 100 + 8) as u8),
                38 | 48 if values.get(i + 1) == Some(&5) => {
                    let colour = values.get(i + 2).map(|&v| v as u8);
                    if values[i] == 38 { fg = colour } else { bg = colour }
                    i += 2;
                }
                _ => {}
            }
            i += 1;
        }
    }
    *state = (' ', fg, bg, bold, underline, reverse);
    cells
}

fn chrome(c: ChromeColor) -> Option<u8> {
    match c {
        ChromeColor::Default => None,
        ChromeColor::Indexed(n) => Some(n),
    }
}

/// The `%H:%M %d-%b-%y` text tmux shows at the end of its status line (it
/// redraws the clock only every status-interval), else the current time.
fn clock(status: &[Cell]) -> String {
    let text: String = status.iter().map(|c| c.0).collect();
    let tail: Vec<char> = text.trim_end().chars().collect();
    if tail.len() >= 15 {
        let candidate: String = tail[tail.len() - 15..].iter().collect();
        let shape = |c: char, p: char| match p {
            '9' => c.is_ascii_digit(),
            'a' => c.is_ascii_alphabetic(),
            _ => c == p,
        };
        if candidate.chars().zip("99:99 99-aaa-99".chars()).all(|(c, p)| shape(c, p)) {
            return candidate;
        }
    }
    let out = Process::new("date").arg("+%H:%M %d-%b-%y").env("LC_ALL", "C.UTF-8").output().expect("date");
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

/// What the model says the client shows, with the clock text `clock`.
fn expected(model: &Session, cols: u16, rows: u16, clock: &str) -> (Vec<Vec<Cell>>, Option<(u16, u16)>) {
    let view = model.view(clock);
    let mut screen = vec![vec![BLANK; usize::from(cols)]; usize::from(rows)];
    for b in &view.borders {
        screen[usize::from(b.y)][usize::from(b.x)] =
            (b.glyph, b.active.then_some(2), None, false, false, false);
    }
    let status = &mut screen[usize::from(rows) - 1];
    for (x, c) in view.status.iter().enumerate().filter(|(_, c)| c.ch != '\0') {
        status[x] = (c.ch, chrome(c.fg), chrome(c.bg), c.bold, c.underline, c.reverse);
    }
    (screen, view.prompt_cursor.map(|x| (x, rows - 1)))
}

fn render(screen: &[Vec<Cell>]) -> String {
    screen
        .iter()
        .map(|row| {
            row.iter()
                .map(|c| match (c.1, c.2) {
                    (Some(2), None) => '#',
                    _ => c.0,
                })
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Waits until the inner client shows what the model expects.
fn check(n: &Nested, model: &Session, what: &str) {
    let start = Instant::now();
    loop {
        let got = n.capture();
        let (want, cursor) = expected(model, n.cols, n.rows, &clock(&got[usize::from(n.rows) - 1]));
        let cursor_ok = cursor.is_none_or(|c| n.cursor() == c);
        if got == want && cursor_ok {
            return;
        }
        if start.elapsed() > Duration::from_secs(4) {
            let first = got
                .iter()
                .zip(&want)
                .enumerate()
                .flat_map(|(y, (g, w))| {
                    g.iter().zip(w).enumerate().filter(|(_, (a, b))| a != b).map(move |(x, p)| (x, y, p))
                })
                .next();
            panic!(
                "{what}: screen mismatch at {first:?}, cursor tmux {:?} model {cursor:?}\n\
                 tmux (# = green):\n{}\nmodel:\n{}",
                n.cursor(),
                render(&got),
                render(&want)
            );
        }
        thread::sleep(Duration::from_millis(30));
    }
}

fn key(code: KeyCode, mods: Mods) -> Key {
    Key { code, mods }
}

fn plain(c: char) -> Key {
    key(KeyCode::Char(c), Mods::default())
}

fn prefix() -> Key {
    key(KeyCode::Char('b'), Mods { ctrl: true, ..Mods::default() })
}

/// Sends keys to both: `names` for tmux send-keys, `keys` for the model.
fn press(n: &Nested, model: &mut Session, names: &[&str], keys: &[Key]) {
    n.keys(names);
    let now = Instant::now();
    for &k in keys {
        model.key(k, now);
    }
}

/// Gives every untitled pane the title `p<id>` and names the current
/// window `w<index>` if it still has its automatic name, in both.
fn name_things(n: &Nested, model: &mut Session) {
    for w in model.windows() {
        for p in w.panes.iter().filter(|p| p.title.is_empty()) {
            let title = format!("p{}", p.pane);
            n.inner(&["select-pane", "-t", &format!("%{}", p.pane), "-T", &title]);
            model.set_pane_title(p.pane, &title);
        }
        if w.active && w.name.is_empty() {
            let name = format!("w{}", w.index);
            n.inner(&["rename-window", &name]);
            model.command(&Command::RenameWindow(name));
        }
    }
}

fn run(n: &Nested, model: &mut Session, cmd: Command) {
    let line = tmux_line(&cmd);
    let args: Vec<&str> = line.split(' ').collect();
    let tmux_ok = tmux(&n.inner, &args).is_ok();
    let out = model.command(&cmd);
    assert_eq!(tmux_ok, out.error.is_none(), "{line}: {:?}", out.error);
    if !tmux_ok {
        n.redraw();
    }
    name_things(n, model);
    check(n, model, &line);
}

#[test]
fn scenarios_match_tmux() {
    common::check_version();
    let (cols, rows) = (80, 24);
    let mut n = Nested::start("scenarios", cols, rows);
    let (mut model, _) = Session::new(cols, rows);
    name_things(&n, &mut model);
    check(&n, &model, "single pane");

    let split = |horizontal, full| Command::SplitWindow {
        horizontal,
        before: false,
        full,
        size: None,
        cwd_of_active: false,
    };
    run(&n, &mut model, split(true, false));
    run(&n, &mut model, Command::SelectPane(mux::Direction::Left));
    run(&n, &mut model, split(false, false));
    run(&n, &mut model, Command::SelectPane(mux::Direction::Right));
    run(&n, &mut model, split(false, true));
    run(&n, &mut model, split(true, true));
    run(&n, &mut model, Command::ZoomPane);
    run(&n, &mut model, Command::ZoomPane);
    run(&n, &mut model, Command::NewWindow { cwd_of_active: false });
    run(&n, &mut model, split(false, false));
    run(&n, &mut model, Command::NewWindow { cwd_of_active: false });
    run(&n, &mut model, Command::LastWindow);
    run(&n, &mut model, Command::ZoomPane);

    // A long pane title is cut to 21 columns.
    let title = "abcdefghijklmnopqrstuvwxyz0123";
    n.inner(&["select-pane", "-T", title]);
    let active = model.active_pane();
    model.set_pane_title(active, title);
    check(&n, &model, "long title");

    // Prompts and messages through real keys.
    press(&n, &mut model, &["C-b", ","], &[prefix(), plain(',')]);
    check(&n, &model, "rename-window prompt");
    press(
        &n,
        &mut model,
        &["x", "y", "Left", "BSpace"],
        &[plain('x'), plain('y'), key(KeyCode::Left, Mods::default()), key(KeyCode::Backspace, Mods::default())],
    );
    check(&n, &model, "rename-window editing");
    press(&n, &mut model, &["Home", "z"], &[key(KeyCode::Home, Mods::default()), plain('z')]);
    check(&n, &model, "rename-window home");
    press(&n, &mut model, &["Enter"], &[key(KeyCode::Enter, Mods::default())]);
    check(&n, &model, "renamed window");

    press(&n, &mut model, &["C-b", "x"], &[prefix(), plain('x')]);
    check(&n, &model, "confirm kill-pane");
    press(&n, &mut model, &["n"], &[plain('n')]);
    check(&n, &model, "confirm cancelled");

    press(&n, &mut model, &["C-b", "&"], &[prefix(), plain('&')]);
    check(&n, &model, "confirm kill-window");
    press(&n, &mut model, &["Escape"], &[key(KeyCode::Escape, Mods::default())]);
    check(&n, &model, "confirm escaped");

    press(&n, &mut model, &["C-b", "7"], &[prefix(), plain('7')]);
    check(&n, &model, "error message");
    // Any key clears the message (and is then handled normally).
    press(&n, &mut model, &["C-b", "p"], &[prefix(), plain('p')]);
    check(&n, &model, "message cleared, previous window");

    press(&n, &mut model, &["C-b", "$"], &[prefix(), plain('$')]);
    check(&n, &model, "rename-session prompt");
    press(&n, &mut model, &["C-u", "s", "e", "s", "Enter"], &[
        key(KeyCode::Char('u'), Mods { ctrl: true, ..Mods::default() }),
        plain('s'),
        plain('e'),
        plain('s'),
        key(KeyCode::Enter, Mods::default()),
    ]);
    check(&n, &model, "renamed session");

    // Key bindings from tmux-pain-control: | splits, C-l selects right.
    press(&n, &mut model, &["C-b", "|"], &[prefix(), plain('|')]);
    name_things(&n, &mut model);
    check(&n, &model, "pain-control split");
    let ctrl_l = key(KeyCode::Char('l'), Mods { ctrl: true, ..Mods::default() });
    press(&n, &mut model, &["C-b", "C-l"], &[prefix(), ctrl_l]);
    check(&n, &model, "pain-control select");

    // Many windows on a narrow client: the window list is cut with markers.
    for _ in 0..7 {
        run(&n, &mut model, Command::NewWindow { cwd_of_active: false });
    }
    tmux(&n.outer, &["resize-window", "-t", "o", "-x", "40"]).expect("resize outer");
    n.cols = 40;
    model.resize_client(40, rows);
    check(&n, &model, "narrow status");
    run(&n, &mut model, Command::SelectWindow(1));
    run(&n, &mut model, Command::SelectWindow(4));
}

#[test]
fn random_layouts_match_tmux() {
    common::check_version();
    let env = |name: &str, default: u64| std::env::var(name).ok().and_then(|v| v.parse().ok()).unwrap_or(default);
    let (sequences, first) = (env("MUX_RENDER_SEQUENCES", 25), env("MUX_RENDER_SEED", 0));
    let mut steps = 0;
    for seed in first..first + sequences {
        let mut rng = Rng::new(0x5eed_0000 + seed);
        let cols = rng.range(20, 120) as u16;
        let rows = rng.range(8, 40) as u16;
        let n = Nested::start(&format!("r{seed}"), cols, rows);
        let (mut model, _) = Session::new(cols, rows);
        name_things(&n, &mut model);
        check(&n, &model, &format!("seed {seed} start"));
        let mut log = format!("seed {seed}: {cols}x{rows}");
        for _ in 0..12 {
            let Step::Cmd(cmd) = random_step(&mut rng, &model) else { continue };
            let windows = model.windows();
            let panes = windows.iter().find(|w| w.active).map_or(0, |w| w.panes.len());
            let last = matches!(cmd, Command::KillWindow) || (matches!(cmd, Command::KillPane) && panes == 1);
            if last && windows.len() == 1 {
                continue;
            }
            let out = model.command(&cmd);
            if out.error.as_deref().is_some_and(|e| e.contains("tmux 3.6 would")) {
                // A layout tmux 3.6 cannot survive: never sent to tmux.
                continue;
            }
            let line = tmux_line(&cmd);
            log.push_str(&format!("\n{line}"));
            let args: Vec<&str> = line.split(' ').collect();
            let tmux_ok = tmux(&n.inner, &args).is_ok();
            assert_eq!(tmux_ok, out.error.is_none(), "{log}: {:?}", out.error);
            if !tmux_ok {
                n.redraw();
            }
            name_things(&n, &mut model);
            check(&n, &model, &log);
            steps += 1;
        }
    }
    eprintln!("tmux render: {sequences} sequences, {steps} steps compared");
}
