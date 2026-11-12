//! Differential test against a real tmux 3.6: the layout presets,
//! next-layout and previous-layout, select-layout -E, zoom, and what
//! rotate-window, swap-pane and split-window do after a mirrored preset,
//! which leaves the pane order apart from the places in the layout.
//!
//! Every case runs on a tmux server of its own (`-f /dev/null`, a socket
//! under the cargo target directory), which prints list-panes after each
//! step. The model's tree, laid out as GtkPaned lays it out on a grid of
//! the window's size, must give the same panes in the same order with the
//! same rectangles, active pane and zoom. Each case starts with a preset:
//! before one, the panes were split in a larger window and resized, which
//! tmux and the split tree divide differently.

use std::iter::repeat_n;
use std::path::{Path, PathBuf};
use std::process::{Command as Process, Stdio};
use std::thread;

use mux::{Command, Layout, Rect, Session};

/// Window sizes in cells: one where the main pane takes all but one cell,
/// and ones wider than main-pane-width and taller than main-pane-height.
/// Every preset fits them with up to 11 panes; a preset that needs more
/// cells makes tmux's window larger than the client, which a GTK window
/// cannot follow.
const SIZES: [(u16, u16); 7] = [(40, 21), (80, 24), (91, 30), (120, 40), (159, 47), (200, 50), (300, 80)];
const MAX_PANES: usize = 9;
const PANE_FORMAT: &str =
    "#{pane_id} #{pane_left} #{pane_top} #{pane_width} #{pane_height} #{pane_active} #{window_zoomed_flag}";
const PANE_COMMAND: &str = "exec sleep 100000";
const STEP_END: &str = "@@";

#[derive(Clone, Debug)]
enum Step {
    Run(Command),
    /// `select-pane -t :.index`
    Select(usize),
}

impl Step {
    fn tmux(&self) -> Vec<String> {
        let line = match self {
            Step::Run(Command::SelectLayout(layout)) => format!("select-layout {}", layout.name()),
            Step::Run(Command::NextLayout) => "next-layout".to_owned(),
            Step::Run(Command::PreviousLayout) => "previous-layout".to_owned(),
            Step::Run(Command::SpreadOut) => "select-layout -E".to_owned(),
            Step::Run(Command::RotateWindow { down }) => format!("rotate-window{}", if *down { " -D" } else { "" }),
            Step::Run(Command::SwapPane { up }) => format!("swap-pane {}", if *up { "-U" } else { "-D" }),
            Step::Run(Command::ZoomPane) => "resize-pane -Z".to_owned(),
            Step::Run(Command::SplitWindow { horizontal, .. }) => {
                return split_args(*horizontal);
            }
            Step::Select(index) => format!("select-pane -t :.{index}"),
            Step::Run(other) => panic!("no tmux command for {other:?}"),
        };
        line.split(' ').map(str::to_owned).collect()
    }
}

fn split_args(horizontal: bool) -> Vec<String> {
    let mut args = vec!["split-window".to_owned()];
    if horizontal {
        args.push("-h".to_owned());
    }
    args.push(PANE_COMMAND.to_owned());
    args
}

fn split(horizontal: bool) -> Command {
    Command::SplitWindow { horizontal, before: false, full: false, cwd_of_active: false }
}

/// The cases for a window of `n` panes, each starting with a preset.
fn scenarios(n: usize) -> Vec<(&'static str, Vec<Step>)> {
    use Step::{Run, Select};
    let layout = |l| Run(Command::SelectLayout(l));
    let (next, previous, spread) = (Run(Command::NextLayout), Run(Command::PreviousLayout), Run(Command::SpreadOut));
    let last = n - 1;
    let mut cycle: Vec<Step> = repeat_n(next.clone(), 8).collect();
    cycle.extend(repeat_n(previous.clone(), 8));
    // The order of tmux's M-1 to M-7 bindings.
    let by_name = [
        Layout::EvenHorizontal,
        Layout::EvenVertical,
        Layout::MainHorizontal,
        Layout::MainVertical,
        Layout::Tiled,
        Layout::MainHorizontalMirrored,
        Layout::MainVerticalMirrored,
    ];
    let spread_after = [
        Layout::MainVertical,
        Layout::MainHorizontal,
        Layout::MainVerticalMirrored,
        Layout::Tiled,
        Layout::EvenVertical,
    ];
    let mut spreads = Vec::new();
    for l in spread_after {
        spreads.extend([layout(l), Select(last), spread.clone(), Select(0), spread.clone()]);
    }
    vec![
        ("next then previous", cycle),
        ("previous first", vec![previous.clone(), previous.clone(), previous.clone(), next.clone(), next.clone()]),
        ("by name", by_name.into_iter().map(layout).collect()),
        ("spread out", spreads),
        (
            "zoom",
            vec![
                layout(Layout::Tiled),
                Select(last),
                Run(Command::ZoomPane),
                next.clone(),
                Run(Command::ZoomPane),
                spread.clone(),
                Run(Command::ZoomPane),
                previous.clone(),
            ],
        ),
        (
            "order",
            vec![
                layout(Layout::MainVerticalMirrored),
                Run(Command::RotateWindow { down: false }),
                Run(Command::RotateWindow { down: true }),
                Run(Command::RotateWindow { down: true }),
                Run(Command::SwapPane { up: true }),
                Run(Command::SwapPane { up: false }),
                next.clone(),
                layout(Layout::MainHorizontalMirrored),
                Select(0),
                Run(split(true)),
                spread.clone(),
                Run(split(false)),
                next,
                previous,
                spread,
            ],
        ),
    ]
}

/// Kills the tmux server on its socket when dropped, also after a failure
/// or a hang.
struct Server(PathBuf);

impl Drop for Server {
    fn drop(&mut self) {
        let _ = Process::new("timeout")
            .args(["2", "tmux", "-S"])
            .arg(&self.0)
            .arg("kill-server")
            .stderr(Stdio::null())
            .status();
        let _ = Process::new("pkill").arg("-9").arg("-f").arg(&self.0).status();
        let _ = std::fs::remove_file(&self.0);
    }
}

/// list-panes after every step, from a real tmux.
fn tmux_run(socket: &Path, (w, h): (u16, u16), n: usize, steps: &[Step]) -> Result<Vec<Vec<String>>, String> {
    let _server = Server(socket.to_owned());
    let mut args: Vec<String> = ["new-session", "-d", "-x", "500", "-y", "200", PANE_COMMAND].map(str::to_owned).into();
    for i in 1..n {
        args.push(";".to_owned());
        args.extend(split_args(i % 2 == 1));
    }
    args.extend([";", "resize-window", "-x", &w.to_string(), "-y", &h.to_string()].map(|a| a.to_owned()));
    for step in steps {
        args.push(";".to_owned());
        args.extend(step.tmux());
        args.extend([";", "list-panes", "-F", PANE_FORMAT, ";", "display-message", "-p", STEP_END].map(str::to_owned));
    }
    let out = Process::new("timeout")
        .args(["20", "tmux", "-f", "/dev/null", "-S"])
        .arg(socket)
        .args(&args)
        .env_remove("TMUX")
        .env("SHELL", "/bin/sh")
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("run tmux: {e}"))?;
    if !out.status.success() {
        return Err(format!("tmux failed ({}): {}", out.status, String::from_utf8_lossy(&out.stderr).trim()));
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let mut blocks = vec![Vec::new()];
    for line in text.lines() {
        if line == STEP_END {
            blocks.push(Vec::new());
        } else {
            // A zoomed window's hidden panes keep stale rectangles.
            let fields: Vec<&str> = line.split(' ').collect();
            let shown = if fields[6] == "1" { format!("{} {} 1", fields[0], fields[5]) } else { line.to_owned() };
            blocks.last_mut().expect("a block").push(shown);
        }
    }
    blocks.pop();
    Ok(blocks)
}

/// The same from the model.
fn model_run((w, h): (u16, u16), n: usize, steps: &[Step]) -> Vec<Vec<String>> {
    let (mut s, _) = Session::new();
    for i in 1..n {
        s.command(&split(i % 2 == 1));
    }
    let area = Rect { x: 0, y: 0, w: u32::from(w), h: u32::from(h) };
    s.set_geometry(s.current(), area, (w, h), &[]);
    let mut blocks = Vec::new();
    for step in steps {
        match step {
            Step::Run(cmd) => {
                let out = s.command(cmd);
                assert_eq!(out.error, None, "{cmd:?}");
            }
            Step::Select(index) => {
                let pane = s.windows()[0].panes[*index].pane;
                s.focus_pane(pane);
            }
        }
        let window = s.windows().into_iter().find(|w| w.active).expect("current window");
        let (tree, zoomed, _) = s.tree(window.index).expect("tree");
        let mut rects = Vec::new();
        tree.layout(area, &mut rects);
        let lines = window
            .panes
            .iter()
            .map(|p| {
                let active = u8::from(p.active);
                if zoomed.is_some() {
                    return format!("%{} {active} 1", p.pane);
                }
                let r = rects.iter().find(|(id, _)| *id == p.pane).expect("pane in tree").1;
                format!("%{} {} {} {} {} {active} 0", p.pane, r.x, r.y, r.w, r.h)
            })
            .collect();
        blocks.push(lines);
    }
    blocks
}

#[test]
fn layouts_match_tmux() {
    let version = Process::new("tmux").arg("-V").output().expect("run tmux -V");
    assert_eq!(String::from_utf8_lossy(&version.stdout).trim(), "tmux 3.6", "the test needs tmux 3.6");
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("tmux-layout");
    std::fs::create_dir_all(&dir).expect("socket directory");
    let failures: Vec<String> = thread::scope(|scope| {
        let workers: Vec<_> = SIZES
            .iter()
            .map(|&size| {
                let dir = &dir;
                scope.spawn(move || {
                    let mut failures = Vec::new();
                    let mut cases = 0;
                    for n in 1..=MAX_PANES {
                        for (index, (name, steps)) in scenarios(n).into_iter().enumerate() {
                            let socket = dir.join(format!("{}x{}-{n}-{index}", size.0, size.1));
                            let case = format!("{}x{}, {n} panes, {name}", size.0, size.1);
                            cases += steps.len();
                            let tmux = match tmux_run(&socket, size, n, &steps) {
                                Ok(blocks) => blocks,
                                Err(e) => {
                                    failures.push(format!("{case}: {e}"));
                                    continue;
                                }
                            };
                            let model = model_run(size, n, &steps);
                            if let Some(i) = (0..steps.len()).find(|&i| tmux.get(i) != model.get(i)) {
                                failures.push(format!(
                                    "{case}, step {i} ({:?}):\n  tmux:  {:?}\n  model: {:?}",
                                    steps[i],
                                    tmux.get(i),
                                    model.get(i)
                                ));
                            }
                        }
                    }
                    (failures, cases)
                })
            })
            .collect();
        let mut failures = Vec::new();
        let mut cases = 0;
        for worker in workers {
            let (f, c) = worker.join().expect("worker");
            failures.extend(f);
            cases += c;
        }
        eprintln!("compared {cases} steps with tmux");
        failures
    });
    assert!(failures.is_empty(), "{} cases differ:\n{}", failures.len(), failures.join("\n"));
}
