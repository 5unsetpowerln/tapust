//! Randomized differential test against the real tmux 3.6 binary: seeded
//! command sequences run on the model and on a tmux server driven by one
//! control-mode client, comparing `list-panes` and `list-windows` after
//! every step.
//!
//! `MUX_DIFF_SEQUENCES` (default 1000) and `MUX_DIFF_SEED` (default 0) pick
//! the sequences; `MUX_DIFF_THREADS` (default 8) the parallelism;
//! `MUX_DIFF_VERBOSE` prints every compared state.

mod common;

use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use common::{random_size, random_step, tmux_line, Reply, Rng, Step, Tmux};
use mux::{Effect, PaneId, Session};

const PANES_FORMAT: &str = "list-panes -F '#{pane_index} #{pane_id} #{pane_left} #{pane_top} \
                            #{pane_width} #{pane_height} #{pane_active} #{window_zoomed_flag}'";
const WINDOWS_FORMAT: &str =
    "list-windows -F '#{window_index} #{window_active} #{window_flags} #{window_width}x#{window_height}'";

fn env_u64(name: &str, default: u64) -> u64 {
    std::env::var(name).ok().and_then(|v| v.parse().ok()).unwrap_or(default)
}

fn model_panes(model: &Session) -> Vec<String> {
    let windows = model.windows();
    let Some(w) = windows.iter().find(|w| w.active) else { return Vec::new() };
    w.panes
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let r = p.rect;
            format!(
                "{i} %{} {} {} {} {} {} {}",
                p.pane, r.x, r.y, r.w, r.h, u8::from(p.active), u8::from(w.zoomed)
            )
        })
        .collect()
}

fn model_windows(model: &Session) -> Vec<String> {
    model
        .windows()
        .iter()
        .map(|w| {
            let mut flags = String::new();
            if w.active {
                flags.push('*');
            }
            if w.last {
                flags.push('-');
            }
            if w.zoomed {
                flags.push('Z');
            }
            format!("{} {} {flags} {}x{}", w.index, u8::from(w.active), w.width, w.height)
        })
        .collect()
}

#[derive(Default)]
struct Stats {
    sequences: u64,
    steps: u64,
    compared: u64,
    refused: u64,
    exits: u64,
    pane_exits: u64,
}

fn compare(model: &Session, tmux: &mut Tmux, log: &[String]) -> Result<(), String> {
    let panes = match tmux.cmd(PANES_FORMAT) {
        Reply::Ok(lines) => lines,
        other => return Err(format!("list-panes: {other:?}\n{}", log.join("\n"))),
    };
    let windows = match tmux.cmd(WINDOWS_FORMAT) {
        Reply::Ok(lines) => lines,
        other => return Err(format!("list-windows: {other:?}\n{}", log.join("\n"))),
    };
    let (mp, mw) = (model_panes(model), model_windows(model));
    if std::env::var_os("MUX_DIFF_VERBOSE").is_some() {
        eprintln!("{}\n  tmux {panes:?} {windows:?}\n  model {mp:?} {mw:?}", log.last().unwrap());
    }
    if panes != mp || windows != mw {
        return Err(format!(
            "state mismatch\nsteps:\n{}\ntmux panes:\n{}\nmodel panes:\n{}\ntmux windows:\n{}\nmodel windows:\n{}",
            log.join("\n"),
            panes.join("\n"),
            mp.join("\n"),
            windows.join("\n"),
            mw.join("\n")
        ));
    }
    Ok(())
}

/// Waits until tmux has removed `pane` after its process died (or the
/// session ended with it).
fn wait_pane_gone(tmux: &mut Tmux, pane: PaneId) -> bool {
    let id = format!("%{pane}");
    for _ in 0..300 {
        match tmux.cmd("list-panes -a -F '#{pane_id}'") {
            Reply::Ok(lines) if lines.contains(&id) => {}
            Reply::Ok(_) | Reply::Exit => return true,
            Reply::Error(_) => return false,
        }
        thread::sleep(Duration::from_millis(10));
    }
    false
}

fn run_sequence(seed: u64, dir: &Path, stats: &mut Stats) -> Result<(), String> {
    let mut rng = Rng::new(seed);
    let (cols, rows) = random_size(&mut rng);
    let mut tmux = Tmux::start(&dir.join(format!("s{seed}")), cols, rows, Duration::from_secs(60))?;
    let (mut model, _) = Session::new(cols, rows + 1);
    let mut log = vec![format!("seed {seed}: start {cols}x{rows}")];
    compare(&model, &mut tmux, &log)?;

    let steps = rng.range(20, 40);
    for _ in 0..steps {
        let step = random_step(&mut rng, &model);
        stats.steps += 1;
        match &step {
            Step::Resize(cols, rows) => {
                log.push(format!("refresh-client -C {cols}x{rows}"));
                model.resize_client(*cols, rows + 1);
                if !matches!(tmux.resize(*cols, *rows), Reply::Ok(_)) {
                    return Err(format!("resize failed\n{}", log.join("\n")));
                }
            }
            Step::PaneExit(pane) => {
                log.push(format!("kill -9 the process of %{pane}"));
                stats.pane_exits += 1;
                let effects = model.pane_exited(*pane);
                let pid = match tmux.cmd(&format!("display-message -p -t %{pane} '#{{pane_pid}}'")) {
                    Reply::Ok(lines) => lines.first().cloned().unwrap_or_default(),
                    other => return Err(format!("pane pid: {other:?}\n{}", log.join("\n"))),
                };
                let _ = std::process::Command::new("kill").args(["-9", &pid]).status();
                if !wait_pane_gone(&mut tmux, *pane) {
                    return Err(format!("pane did not go away\n{}", log.join("\n")));
                }
                if effects.contains(&Effect::Exit) {
                    stats.exits += 1;
                    if !tmux.exited() {
                        return Err(format!("model exited, tmux did not\n{}", log.join("\n")));
                    }
                    stats.compared += 1;
                    return Ok(());
                }
            }
            Step::Cmd(cmd) => {
                let line = tmux_line(cmd);
                let out = model.command(cmd);
                if out.error.as_deref().is_some_and(|e| e.contains("tmux 3.6 would")) {
                    // A tmux 3.6 layout bug (overflow or hang): never sent to tmux.
                    log.push(format!("{line}  (refused by the model: tmux 3.6 bug)"));
                    stats.refused += 1;
                    continue;
                }
                log.push(line.clone());
                let reply = tmux.cmd(&line);
                if out.effects.contains(&Effect::Exit) {
                    stats.exits += 1;
                    if !matches!(reply, Reply::Ok(_) | Reply::Exit)
                        || !matches!(tmux.cmd("list-sessions"), Reply::Exit)
                    {
                        return Err(format!("model exited, tmux did not\n{}", log.join("\n")));
                    }
                    stats.compared += 1;
                    return Ok(());
                }
                let tmux_error = match reply {
                    Reply::Ok(_) => None,
                    Reply::Error(lines) => Some(lines.join("\n")),
                    Reply::Exit => return Err(format!("tmux exited\n{}", log.join("\n"))),
                };
                if tmux_error != out.error {
                    return Err(format!(
                        "error mismatch: tmux {tmux_error:?}, model {:?}\n{}",
                        out.error,
                        log.join("\n")
                    ));
                }
            }
        }
        compare(&model, &mut tmux, &log)?;
        stats.compared += 1;
    }
    Ok(())
}

#[test]
fn random_sequences_match_tmux() {
    common::check_version();
    let count = env_u64("MUX_DIFF_SEQUENCES", 1000);
    let base = env_u64("MUX_DIFF_SEED", 0);
    let threads = env_u64("MUX_DIFF_THREADS", 8) as usize;
    let dir = common::work_dir("mux-diff");

    let next = Arc::new(AtomicU64::new(base));
    let totals = Arc::new(Mutex::new(Stats::default()));
    let failures = Arc::new(Mutex::new(Vec::new()));
    let workers: Vec<_> = (0..threads)
        .map(|_| {
            let (next, totals, failures, dir) =
                (Arc::clone(&next), Arc::clone(&totals), Arc::clone(&failures), dir.clone());
            thread::spawn(move || {
                loop {
                    let seed = next.fetch_add(1, Ordering::SeqCst);
                    if seed >= base + count {
                        break;
                    }
                    let mut stats = Stats::default();
                    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        run_sequence(seed, &dir, &mut stats)
                    }))
                    .unwrap_or_else(|_| Err("panicked".to_owned()));
                    let mut t = totals.lock().unwrap();
                    t.sequences += 1;
                    t.steps += stats.steps;
                    t.compared += stats.compared;
                    t.refused += stats.refused;
                    t.exits += stats.exits;
                    t.pane_exits += stats.pane_exits;
                    if let Err(e) = result {
                        failures.lock().unwrap().push((seed, e));
                    }
                }
            })
        })
        .collect();
    for w in workers {
        w.join().expect("worker");
    }
    let _ = std::fs::remove_dir(&dir);

    let t = totals.lock().unwrap();
    let mut failures = failures.lock().unwrap();
    failures.sort_by_key(|f| f.0);
    eprintln!(
        "tmux diff: {} sequences, {} steps, {} states compared, {} tmux-bug steps refused, \
         {} pane process exits, {} session exits, {} failing sequences",
        t.sequences,
        t.steps,
        t.compared,
        t.refused,
        t.pane_exits,
        t.exits,
        failures.len()
    );
    for (seed, e) in failures.iter().take(3) {
        eprintln!("--- seed {seed} ---\n{e}");
    }
    assert!(failures.is_empty(), "failing seeds: {:?}", failures.iter().map(|f| f.0).collect::<Vec<_>>());
}
