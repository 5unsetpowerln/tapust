//! Unit tests: the tmux 3.6 observations of R-0001 (80x24 window area), the
//! key state machine, prompts, the status line and borders.

use std::time::{Duration, Instant};

use crate::*;

fn new80() -> Session {
    Session::new(80, 25).0
}

fn run(s: &mut Session, cmd: Command) -> CommandOutput {
    s.command(&cmd)
}

fn ok(s: &mut Session, cmd: Command) -> Vec<Effect> {
    let out = run(s, cmd);
    assert_eq!(out.error, None);
    out.effects
}

fn split(horizontal: bool) -> Command {
    Command::SplitWindow { horizontal, before: false, full: false, size: None, cwd_of_active: false }
}

fn full_split(horizontal: bool) -> Command {
    Command::SplitWindow { horizontal, before: false, full: true, size: None, cwd_of_active: false }
}

/// (x, y, w, h) of the current window's panes in index order.
fn rects(s: &Session) -> Vec<(u16, u16, u16, u16)> {
    let w = s.windows().into_iter().find(|w| w.active).unwrap();
    w.panes.iter().map(|p| (p.rect.x, p.rect.y, p.rect.w, p.rect.h)).collect()
}

fn active_index(s: &Session) -> usize {
    let w = s.windows().into_iter().find(|w| w.active).unwrap();
    w.panes.iter().position(|p| p.active).unwrap()
}

fn select_index(s: &mut Session, index: usize) {
    for _ in 0..8 {
        if active_index(s) == index {
            return;
        }
        ok(s, Command::NextPane);
    }
    panic!("no pane {index}");
}

fn window_list(s: &Session) -> Vec<(u32, String, bool)> {
    s.windows().into_iter().map(|w| (w.index, w.name, w.active)).collect()
}

fn key(code: KeyCode) -> Key {
    Key { code, mods: Mods::default() }
}

fn ch(c: char) -> Key {
    key(KeyCode::Char(c))
}

fn ctrl(c: char) -> Key {
    Key { code: KeyCode::Char(c), mods: Mods { ctrl: true, ..Mods::default() } }
}

fn meta(code: KeyCode) -> Key {
    Key { code, mods: Mods { meta: true, ..Mods::default() } }
}

fn status_text(s: &Session) -> String {
    s.view("12:34 11-Nov-26").status.iter().map(|c| c.ch).filter(|&c| c != '\0').collect()
}

// --- R-0001 observations -------------------------------------------------

#[test]
fn split_horizontal_halves() {
    let mut s = new80();
    ok(&mut s, split(true));
    assert_eq!(rects(&s), [(0, 0, 40, 24), (41, 0, 39, 24)]);
}

#[test]
fn split_vertical_halves() {
    let mut s = new80();
    ok(&mut s, split(false));
    assert_eq!(rects(&s), [(0, 0, 80, 12), (0, 13, 80, 11)]);
}

#[test]
fn resize_right_by_five() {
    let mut s = new80();
    ok(&mut s, split(true));
    select_index(&mut s, 0);
    ok(&mut s, Command::ResizePane(Direction::Right, 5));
    assert_eq!(rects(&s), [(0, 0, 45, 24), (46, 0, 34, 24)]);
}

#[test]
fn kill_middle_of_three() {
    let mut s = new80();
    ok(&mut s, split(true));
    ok(&mut s, split(true));
    assert_eq!(rects(&s), [(0, 0, 40, 24), (41, 0, 19, 24), (61, 0, 19, 24)]);
    select_index(&mut s, 1);
    ok(&mut s, Command::KillPane);
    assert_eq!(rects(&s), [(0, 0, 60, 24), (61, 0, 19, 24)]);
}

#[test]
fn kill_first_of_two() {
    let mut s = new80();
    ok(&mut s, split(true));
    select_index(&mut s, 0);
    ok(&mut s, Command::KillPane);
    assert_eq!(rects(&s), [(0, 0, 80, 24)]);
}

#[test]
fn select_left_wraps() {
    let mut s = new80();
    ok(&mut s, split(true));
    select_index(&mut s, 0);
    ok(&mut s, Command::SelectPane(Direction::Left));
    assert_eq!(active_index(&s), 1);
}

#[test]
fn select_uses_recency() {
    let mut s = new80();
    ok(&mut s, split(true));
    ok(&mut s, split(false));
    assert_eq!(active_index(&s), 2);
    ok(&mut s, Command::SelectPane(Direction::Left));
    assert_eq!(active_index(&s), 0);
    ok(&mut s, Command::SelectPane(Direction::Right));
    assert_eq!(active_index(&s), 2);
    select_index(&mut s, 0);
    select_index(&mut s, 1);
    ok(&mut s, Command::SelectPane(Direction::Left));
    assert_eq!(active_index(&s), 0);
    ok(&mut s, Command::SelectPane(Direction::Right));
    assert_eq!(active_index(&s), 1);
}

#[test]
fn swap_window_wraps_and_follows() {
    let mut s = new80();
    for name in ["b", "c"] {
        ok(&mut s, Command::NewWindow { cwd_of_active: false });
        ok(&mut s, Command::RenameWindow(name.to_owned()));
    }
    ok(&mut s, Command::SelectWindow(0));
    ok(&mut s, Command::RenameWindow("a".to_owned()));
    ok(&mut s, Command::SwapWindow(-1));
    assert_eq!(
        window_list(&s),
        [(0, "c".to_owned(), false), (1, "b".to_owned(), false), (2, "a".to_owned(), true)]
    );
}

#[test]
fn new_window_fills_gap() {
    let mut s = new80();
    ok(&mut s, Command::NewWindow { cwd_of_active: false });
    ok(&mut s, Command::NewWindow { cwd_of_active: false });
    ok(&mut s, Command::SelectWindow(1));
    ok(&mut s, Command::KillWindow);
    ok(&mut s, Command::NewWindow { cwd_of_active: false });
    let indices: Vec<u32> = s.windows().iter().map(|w| w.index).collect();
    assert_eq!(indices, [0, 1, 2]);
    assert!(s.windows()[1].active);
}

#[test]
fn zoom_single_pane_does_nothing() {
    let mut s = new80();
    assert_eq!(ok(&mut s, Command::ZoomPane), []);
    assert!(!s.windows()[0].zoomed);
}

#[test]
fn full_vertical_split_after_horizontal() {
    let mut s = new80();
    ok(&mut s, split(true));
    ok(&mut s, full_split(false));
    assert_eq!(rects(&s), [(0, 0, 40, 12), (41, 0, 39, 12), (0, 13, 80, 11)]);
}

// --- Effects ---------------------------------------------------------------

#[test]
fn new_session_spawns_first_pane() {
    let (s, effects) = Session::new(100, 30);
    assert_eq!(effects, [Effect::Spawn { pane: 0, cols: 100, rows: 29, cwd_of: None }]);
    assert_eq!(s.active_pane(), 0);
    assert_eq!(s.pane_sizes(), [(0, 100, 29)]);
}

#[test]
fn split_effects() {
    let mut s = new80();
    let effects = ok(
        &mut s,
        Command::SplitWindow { horizontal: true, before: false, full: false, size: None, cwd_of_active: true },
    );
    assert_eq!(
        effects,
        [
            Effect::Resize { pane: 0, cols: 40, rows: 24 },
            Effect::Spawn { pane: 1, cols: 39, rows: 24, cwd_of: Some(0) },
        ]
    );
}

#[test]
fn resize_client_resizes_every_window() {
    let mut s = new80();
    ok(&mut s, split(true));
    ok(&mut s, Command::NewWindow { cwd_of_active: false });
    let effects = s.resize_client(100, 31);
    assert_eq!(
        effects,
        [
            Effect::Resize { pane: 0, cols: 50, rows: 30 },
            Effect::Resize { pane: 1, cols: 49, rows: 30 },
            Effect::Resize { pane: 2, cols: 100, rows: 30 },
        ]
    );
}

#[test]
fn last_pane_exit_exits() {
    let mut s = new80();
    ok(&mut s, Command::NewWindow { cwd_of_active: false });
    assert_eq!(s.pane_exited(1), [Effect::Destroy { pane: 1 }]);
    assert_eq!(s.active_pane(), 0);
    assert_eq!(s.pane_exited(0), [Effect::Destroy { pane: 0 }, Effect::Exit]);
    assert_eq!(s.pane_exited(0), []);
}

#[test]
fn break_pane_does_not_spawn() {
    let mut s = new80();
    ok(&mut s, split(true));
    let effects = ok(&mut s, Command::BreakPane);
    assert_eq!(
        effects,
        [Effect::Resize { pane: 0, cols: 80, rows: 24 }, Effect::Resize { pane: 1, cols: 80, rows: 24 }]
    );
    assert_eq!(s.windows().len(), 2);
    assert!(s.windows()[1].active);
}

#[test]
fn full_split_overflow_is_refused() {
    // A left-right root [C, d] where C is a top-bottom cell holding a row of
    // four panes squeezed to its minimum width and d is one column wide.
    // For split-window -fv, tmux 3.6 layout_new_pane_size clamps C below its
    // minimum and layout_resize_child_cells then wraps the pane widths.
    let mut s = Session::new(11, 11).0;
    ok(&mut s, split(true));
    select_index(&mut s, 0);
    ok(&mut s, Command::ResizePane(Direction::Right, 10));
    ok(&mut s, split(false));
    select_index(&mut s, 0);
    ok(&mut s, split(true));
    ok(&mut s, split(true));
    select_index(&mut s, 0);
    ok(&mut s, split(true));
    let before = rects(&s);
    assert_eq!(
        before,
        [(0, 0, 2, 5), (3, 0, 1, 5), (5, 0, 2, 5), (8, 0, 1, 5), (0, 6, 9, 4), (10, 0, 1, 10)]
    );
    let out = run(&mut s, full_split(false));
    assert_eq!(out.error.as_deref(), Some(session::SPLIT_OVERFLOW));
    assert_eq!(out.effects, []);
    assert_eq!(rects(&s), before);
}

#[test]
fn spread_layout_hang_is_refused() {
    // Seed 496 of the differential test: tmux 3.6 stopped answering at the
    // final select-layout -E. layout_spread_cell gives every child of the
    // spread cell the same share without checking the child's minimum.
    let mut s = Session::new(21, 7).0;
    let steps = [
        Command::SplitWindow { horizontal: false, before: false, full: false, size: Some(2), cwd_of_active: false },
        Command::SplitWindow { horizontal: true, before: true, full: true, size: Some(0), cwd_of_active: false },
        Command::NextPane,
        Command::SplitWindow { horizontal: false, before: true, full: false, size: None, cwd_of_active: false },
    ];
    for step in steps {
        run(&mut s, step);
    }
    s.resize_client(22, 6);
    for step in [
        Command::ResizePane(Direction::Left, 4),
        split(false),
        Command::KillPane,
        Command::ResizePane(Direction::Down, 7),
        full_split(false),
        split(true),
        split(false),
    ] {
        run(&mut s, step);
    }
    let before = rects(&s);
    let out = run(&mut s, Command::SpreadLayout);
    assert_eq!(out.error.as_deref(), Some(session::SPREAD_OVERFLOW));
    assert_eq!(out.effects, []);
    assert_eq!(rects(&s), before);
}

#[test]
fn spread_layout_evens_out() {
    let mut s = new80();
    ok(&mut s, split(true));
    ok(&mut s, split(true));
    ok(&mut s, Command::SpreadLayout);
    assert_eq!(rects(&s), [(0, 0, 26, 24), (27, 0, 26, 24), (54, 0, 26, 24)]);
}

// --- Keys ------------------------------------------------------------------

#[test]
fn prefix_then_split() {
    let t = Instant::now();
    let mut s = new80();
    assert_eq!(s.key(ctrl('b'), t), KeyResult::Handled(vec![]));
    let KeyResult::Handled(effects) = s.key(ch('%'), t) else { panic!() };
    assert_eq!(effects.len(), 2);
    assert_eq!(rects(&s).len(), 2);
    assert_eq!(s.key(ch('a'), t), KeyResult::Forward(1));
}

#[test]
fn unbound_key_after_prefix_is_swallowed() {
    let t = Instant::now();
    let mut s = new80();
    s.key(ctrl('b'), t);
    assert_eq!(s.key(ch('Y'), t), KeyResult::Handled(vec![]));
    assert_eq!(s.key(ch('Y'), t), KeyResult::Forward(0));
}

#[test]
fn unimplemented_default_binding_is_consumed() {
    let t = Instant::now();
    let mut s = new80();
    for k in [ch('d'), ch('['), ch('?'), ch(' '), meta(KeyCode::Char('1')), key(KeyCode::PageUp)] {
        s.key(ctrl('b'), t);
        assert_eq!(s.key(k, t), KeyResult::Handled(vec![]), "{k:?}");
    }
}

#[test]
fn repeat_within_repeat_time() {
    let t = Instant::now();
    let mut s = new80();
    ok(&mut s, split(true));
    select_index(&mut s, 0);
    s.key(ctrl('b'), t);
    s.key(ch('L'), t);
    assert_eq!(s.deadline(), Some(t + Duration::from_millis(500)));
    let t2 = t + Duration::from_millis(400);
    s.key(ch('L'), t2);
    assert_eq!(s.deadline(), Some(t2 + Duration::from_millis(500)));
    assert_eq!(rects(&s)[0].2, 50);
    // Still repeating at t2 + 499 ms.
    s.key(ch('L'), t2 + Duration::from_millis(499));
    assert_eq!(rects(&s)[0].2, 55);
}

#[test]
fn key_after_repeat_time_goes_to_pane() {
    let t = Instant::now();
    let mut s = new80();
    ok(&mut s, split(true));
    s.key(ctrl('b'), t);
    s.key(key(KeyCode::Left), t);
    assert_eq!(active_index(&s), 0);
    let late = t + Duration::from_millis(500);
    assert_eq!(s.key(key(KeyCode::Left), late), KeyResult::Forward(0));
    assert_eq!(s.deadline(), None);
}

#[test]
fn tick_ends_repeat() {
    let t = Instant::now();
    let mut s = new80();
    ok(&mut s, split(true));
    s.key(ctrl('b'), t);
    s.key(key(KeyCode::Left), t);
    s.tick(t + Duration::from_millis(500));
    assert_eq!(s.deadline(), None);
    assert_eq!(s.key(key(KeyCode::Right), t + Duration::from_millis(501)), KeyResult::Forward(0));
}

#[test]
fn non_repeat_key_during_repeat_is_forwarded() {
    let t = Instant::now();
    let mut s = new80();
    ok(&mut s, split(true));
    s.key(ctrl('b'), t);
    s.key(key(KeyCode::Left), t);
    assert_eq!(s.key(ch('%'), t), KeyResult::Forward(0));
    assert_eq!(rects(&s).len(), 2);
    // An unbound key during repeat is forwarded too.
    s.key(ctrl('b'), t);
    s.key(key(KeyCode::Right), t);
    assert_eq!(s.key(ch('Y'), t), KeyResult::Forward(1));
}

#[test]
fn prefix_during_repeat_enters_prefix() {
    let t = Instant::now();
    let mut s = new80();
    ok(&mut s, split(true));
    s.key(ctrl('b'), t);
    s.key(key(KeyCode::Left), t);
    assert_eq!(s.key(ctrl('b'), t), KeyResult::Handled(vec![]));
    assert_eq!(s.deadline(), None);
    s.key(ch('z'), t);
    assert!(s.windows()[0].zoomed);
}

#[test]
fn prefix_prefix_writes_ctrl_b() {
    let t = Instant::now();
    let mut s = new80();
    s.key(ctrl('b'), t);
    assert_eq!(
        s.key(ctrl('b'), t),
        KeyResult::Handled(vec![Effect::Write { pane: 0, bytes: vec![0x02] }])
    );
    assert_eq!(s.key(ctrl('b'), t), KeyResult::Handled(vec![]));
}

#[test]
fn confirm_kill_pane() {
    let t = Instant::now();
    let mut s = new80();
    ok(&mut s, split(true));
    s.key(ctrl('b'), t);
    s.key(ch('x'), t);
    assert!(status_text(&s).starts_with("kill-pane 1? (y/n) "));
    assert_eq!(s.view("").prompt_cursor, Some(19));
    // A special key is ignored by a single-key prompt.
    s.key(key(KeyCode::Left), t);
    assert!(status_text(&s).starts_with("kill-pane 1?"));
    assert_eq!(s.key(ch('n'), t), KeyResult::Handled(vec![]));
    assert_eq!(rects(&s).len(), 2);
    s.key(ctrl('b'), t);
    s.key(ch('x'), t);
    assert_eq!(s.key(ch('y'), t), KeyResult::Handled(vec![
        Effect::Destroy { pane: 1 },
        Effect::Resize { pane: 0, cols: 80, rows: 24 },
    ]));
    assert_eq!(s.view("").prompt_cursor, None);
}

#[test]
fn confirm_kill_window() {
    let t = Instant::now();
    let mut s = new80();
    ok(&mut s, Command::NewWindow { cwd_of_active: false });
    s.set_auto_name(1, "vim");
    s.key(ctrl('b'), t);
    s.key(ch('&'), t);
    assert!(status_text(&s).starts_with("kill-window vim? (y/n) "));
    s.key(ch('y'), t);
    assert_eq!(s.windows().len(), 1);
}

#[test]
fn rename_window_editing() {
    let t = Instant::now();
    let mut s = new80();
    s.set_auto_name(0, "bash");
    s.key(ctrl('b'), t);
    s.key(ch(','), t);
    assert!(status_text(&s).starts_with("(rename-window) bash "));
    assert_eq!(s.view("").prompt_cursor, Some(20));
    s.key(key(KeyCode::Backspace), t);
    s.key(key(KeyCode::Home), t);
    s.key(ch('x'), t);
    s.key(ctrl('e'), t);
    s.key(ch('y'), t);
    s.key(key(KeyCode::Left), t);
    s.key(key(KeyCode::Left), t);
    s.key(ch('-'), t);
    assert!(status_text(&s).starts_with("(rename-window) xba-sy "));
    assert_eq!(s.view("").prompt_cursor, Some(20));
    s.key(ctrl('a'), t);
    s.key(key(KeyCode::Right), t);
    s.key(key(KeyCode::End), t);
    s.key(key(KeyCode::Enter), t);
    assert_eq!(s.windows()[0].name, "xba-sy");
    // automatic-rename is now off for this window.
    s.set_auto_name(0, "zsh");
    assert_eq!(s.windows()[0].name, "xba-sy");
}

#[test]
fn rename_prompt_cancel_and_clear() {
    let t = Instant::now();
    let mut s = new80();
    s.set_auto_name(0, "bash");
    s.key(ctrl('b'), t);
    s.key(ch(','), t);
    s.key(ctrl('u'), t);
    s.key(ch('a'), t);
    s.key(key(KeyCode::Escape), t);
    assert_eq!(s.windows()[0].name, "bash");
    s.key(ctrl('b'), t);
    s.key(ch(','), t);
    s.key(ctrl('c'), t);
    assert_eq!(s.windows()[0].name, "bash");
    assert!(status_text(&s).starts_with("[0] 0:bash*"));
}

#[test]
fn rename_session_prompt() {
    let t = Instant::now();
    let mut s = new80();
    s.key(ctrl('b'), t);
    s.key(ch('$'), t);
    assert!(status_text(&s).starts_with("(rename-session) 0 "));
    s.key(key(KeyCode::Backspace), t);
    s.key(key(KeyCode::Enter), t);
    assert!(status_text(&s).starts_with("Invalid session: "));
    for c in "a.b".chars() {
        s.key(ctrl('b'), t);
        s.key(ch('$'), t);
        s.key(ctrl('u'), t);
        s.key(ch(c), t);
        s.key(key(KeyCode::Enter), t);
    }
    s.key(ctrl('b'), t);
    s.key(ch('$'), t);
    s.key(ctrl('u'), t);
    for c in "a.b".chars() {
        s.key(ch(c), t);
    }
    s.key(key(KeyCode::Enter), t);
    assert!(status_text(&s).starts_with("[a_b] "));
}

#[test]
fn error_message_and_expiry() {
    let t = Instant::now();
    let mut s = new80();
    s.key(ctrl('b'), t);
    s.key(ch('5'), t);
    assert!(status_text(&s).starts_with("Can't find window: 5 "));
    let view = s.view("");
    assert_eq!(view.status[0].bg, ChromeColor::Indexed(3));
    assert_eq!(s.deadline(), Some(t + Duration::from_millis(750)));
    let g = s.generation();
    s.tick(t + Duration::from_millis(750));
    assert_ne!(s.generation(), g);
    assert!(status_text(&s).starts_with("[0] 0:*"));
    // A key press clears a message and is then processed normally.
    s.key(ctrl('b'), t);
    s.key(ch('n'), t);
    assert!(status_text(&s).starts_with("No next window"));
    assert_eq!(s.key(ch('q'), t), KeyResult::Forward(0));
    assert!(status_text(&s).starts_with("[0] "));
}

#[test]
fn arrow_resize_bindings() {
    let t = Instant::now();
    let mut s = new80();
    ok(&mut s, split(true));
    select_index(&mut s, 0);
    s.key(ctrl('b'), t);
    s.key(meta(KeyCode::Right), t);
    assert_eq!(rects(&s)[0].2, 45);
    s.key(meta(KeyCode::Right), t + Duration::from_millis(100));
    assert_eq!(rects(&s)[0].2, 50);
    let later = t + Duration::from_secs(1);
    s.key(ctrl('b'), later);
    s.key(Key { code: KeyCode::Left, mods: Mods { ctrl: true, ..Mods::default() } }, later);
    assert_eq!(rects(&s)[0].2, 49);
}

#[test]
fn swap_window_bindings_repeat() {
    let t = Instant::now();
    let mut s = new80();
    ok(&mut s, Command::NewWindow { cwd_of_active: false });
    ok(&mut s, Command::NewWindow { cwd_of_active: false });
    let panes = |s: &Session| -> Vec<PaneId> { s.windows().iter().map(|w| w.panes[0].pane).collect() };
    s.key(ctrl('b'), t);
    s.key(ch('<'), t);
    assert_eq!(panes(&s), [0, 2, 1]);
    s.key(ch('<'), t);
    assert_eq!(panes(&s), [2, 0, 1]);
    s.key(ch('>'), t);
    assert_eq!(panes(&s), [0, 2, 1]);
    assert!(s.windows()[1].active);
}

#[test]
fn swap_and_rotate_bindings() {
    let t = Instant::now();
    let mut s = new80();
    ok(&mut s, split(true));
    ok(&mut s, split(false));
    let ids = |s: &Session| -> Vec<PaneId> { s.windows()[0].panes.iter().map(|p| p.pane).collect() };
    s.key(ctrl('b'), t);
    s.key(ch('{'), t);
    assert_eq!(ids(&s), [0, 2, 1]);
    assert_eq!(s.active_pane(), 2);
    s.key(ctrl('b'), t);
    s.key(ctrl('o'), t);
    assert_eq!(ids(&s), [2, 1, 0]);
    assert_eq!(rects(&s), [(0, 0, 40, 24), (41, 0, 39, 12), (41, 13, 39, 11)]);
}

#[test]
fn window_bigger_than_client() {
    // Three stacked panes need 5 rows; a client with a 3-row window area
    // leaves the window taller than the client (resize_window keeps the
    // layout size), which status-right shows as "[x,y] ".
    let mut s = Session::new(60, 13).0;
    ok(&mut s, split(false));
    ok(&mut s, split(false));
    s.resize_client(60, 4);
    let w = &s.windows()[0];
    assert_eq!((w.width, w.height), (60, 5));
    assert!(status_text(&s).ends_with(" [0,0] \"\" 12:34 11-Nov-26"), "{}", status_text(&s));
    // The rows of the 3-row window area: pane, border, pane.
    assert_eq!(s.view("").borders.len(), 60);
    // On a 5-column client the list goes first, then the right part from its left.
    s.resize_client(5, 4);
    assert_eq!(status_text(&s), "[0] 6");
}

#[test]
fn exit_of_pane_in_other_window() {
    let mut s = new80();
    ok(&mut s, split(true));
    ok(&mut s, Command::NewWindow { cwd_of_active: false });
    assert_eq!(s.pane_exited(1), [Effect::Destroy { pane: 1 }, Effect::Resize { pane: 0, cols: 80, rows: 24 }]);
    assert_eq!(s.active_pane(), 2);
    assert_eq!(s.pane_exited(2), [Effect::Destroy { pane: 2 }]);
    assert_eq!(s.active_pane(), 0);
}

#[test]
fn dead_session_view() {
    let mut s = new80();
    assert_eq!(ok(&mut s, Command::KillWindow), [Effect::Destroy { pane: 0 }, Effect::Exit]);
    let v = s.view("x");
    assert!(v.panes.is_empty() && v.borders.is_empty());
    assert_eq!(v.status.len(), 80);
    assert_eq!(s.active_pane(), 0);
    assert_eq!(s.key(ctrl('b'), Instant::now()), KeyResult::Handled(vec![]));
}

// --- Status line ------------------------------------------------------------

#[test]
fn default_status_line() {
    let mut s = new80();
    s.set_auto_name(0, "bash");
    ok(&mut s, Command::NewWindow { cwd_of_active: false });
    s.set_auto_name(1, "vim");
    s.set_pane_title(1, "host");
    let view = s.view("12:34 11-Nov-26");
    assert_eq!(view.status.len(), 80);
    let text = status_text(&s);
    let right = "\"host\" 12:34 11-Nov-26";
    assert_eq!(text, format!("[0] 0:bash- 1:vim*{}{right}", " ".repeat(80 - 18 - right.len())));
    assert!(view.status.iter().all(|c| c.fg == ChromeColor::Indexed(0)
        && c.bg == ChromeColor::Indexed(2)
        && !c.bold
        && !c.reverse));
    assert_eq!(view.prompt_cursor, None);
}

#[test]
fn status_flags_and_space() {
    let mut s = new80();
    ok(&mut s, split(true));
    ok(&mut s, Command::ZoomPane);
    ok(&mut s, Command::NewWindow { cwd_of_active: false });
    ok(&mut s, Command::NewWindow { cwd_of_active: false });
    assert!(status_text(&s).starts_with("[0] 0:Z 1:- 2:* "));
}

#[test]
fn status_list_is_trimmed_around_focus() {
    let mut s = Session::new(40, 10).0;
    for _ in 0..9 {
        ok(&mut s, Command::NewWindow { cwd_of_active: false });
    }
    let text = status_text(&s);
    assert_eq!(text.chars().count(), 40);
    assert!(text.starts_with("[0] <"), "{text}");
    assert!(text.contains("9:*"), "{text}");
}

#[test]
fn long_title_is_trimmed() {
    let mut s = new80();
    s.set_pane_title(0, "abcdefghijklmnopqrstuvwxyz");
    assert!(status_text(&s).ends_with("\"abcdefghijklmnopqrstu\" 12:34 11-Nov-26"));
}

// --- Borders ------------------------------------------------------------------

fn border_at(v: &View, x: u16, y: u16) -> Option<(char, bool)> {
    v.borders.iter().find(|b| b.x == x && b.y == y).map(|b| (b.glyph, b.active))
}

#[test]
fn two_pane_half_border() {
    let mut s = new80();
    ok(&mut s, split(true));
    let v = s.view("");
    assert_eq!(v.borders.len(), 24);
    // The left pane owns rows 0..=12 (py <= sy / 2), the right one the rest.
    for y in 0..24 {
        assert_eq!(border_at(&v, 40, y), Some(('\u{2502}', y > 12)), "y {y}");
    }
    select_index(&mut s, 0);
    let v = s.view("");
    for y in 0..24 {
        assert_eq!(border_at(&v, 40, y), Some(('\u{2502}', y <= 12)), "y {y}");
    }
}

#[test]
fn three_pane_joins() {
    let mut s = new80();
    ok(&mut s, split(true));
    ok(&mut s, split(false));
    let v = s.view("");
    assert_eq!(border_at(&v, 40, 12), Some(('\u{251c}', true)));
    assert_eq!(border_at(&v, 41, 12), Some(('\u{2500}', true)));
    assert_eq!(border_at(&v, 79, 12), Some(('\u{2500}', true)));
    assert_eq!(border_at(&v, 40, 0), Some(('\u{2502}', false)));
    assert_eq!(border_at(&v, 40, 13), Some(('\u{2502}', true)));
    assert_eq!(border_at(&v, 40, 23), Some(('\u{2502}', true)));
    // split-window -fv: the old root keeps 12 rows (panes 0, 1 and 2) and
    // the new full-width pane 3 is active below row 12.
    ok(&mut s, full_split(false));
    assert_eq!(rects(&s), [(0, 0, 40, 12), (41, 0, 39, 6), (41, 7, 39, 5), (0, 13, 80, 11)]);
    let v = s.view("");
    assert_eq!(border_at(&v, 40, 12), Some(('\u{2534}', true)));
    assert_eq!(border_at(&v, 40, 6), Some(('\u{251c}', false)));
    assert_eq!(border_at(&v, 0, 12), Some(('\u{2500}', true)));
}

#[test]
fn zoomed_window_has_no_borders() {
    let mut s = new80();
    ok(&mut s, split(true));
    ok(&mut s, Command::ZoomPane);
    let v = s.view("");
    assert!(v.borders.is_empty());
    assert_eq!(v.panes, [PaneView { pane: 1, rect: Rect { x: 0, y: 0, w: 80, h: 24 }, active: true }]);
}

// --- Formats -------------------------------------------------------------------

#[test]
fn vis_encoding() {
    assert_eq!(format::vis("a b\\c\td\u{1}\u{e9}"), "a b\\\\c\\td\\001\u{e9}");
}

#[test]
fn rename_expands_formats() {
    let mut s = new80();
    ok(&mut s, Command::RenameWindow("#S-#I-##-#{pane_index}-#Q".to_owned()));
    assert_eq!(s.windows()[0].name, "0-0-#-0-#Q");
}

#[test]
fn status_line_off_uses_every_row_and_still_reports_prompts() {
    use crate::{ConfirmTarget, Effect, Key, KeyCode, KeyResult, Mods, Overlay, Session};
    let now = std::time::Instant::now();
    let (mut s, effects) = Session::without_status_line(80, 24);
    assert_eq!(effects, vec![Effect::Spawn { pane: 0, cols: 80, rows: 24, cwd_of: None }]);
    assert_eq!(s.view("").overlay, None);
    let prefix = Key { code: KeyCode::Char('b'), mods: Mods { ctrl: true, ..Mods::default() } };
    let plain = |c| Key { code: KeyCode::Char(c), mods: Mods::default() };
    assert_eq!(s.key(prefix, now), KeyResult::Handled(vec![]));
    s.key(plain('x'), now);
    assert_eq!(
        s.view("").overlay,
        // confirm-before appends a space to the prompt (cmd-confirm-before.c).
        Some(Overlay::Confirm { prompt: "kill-pane 0? (y/n) ".to_owned(), target: ConfirmTarget::Pane })
    );
    s.key(plain('n'), now);
    assert_eq!(s.view("").overlay, None);
    s.key(prefix, now);
    s.key(plain(','), now);
    match s.view("").overlay {
        Some(Overlay::Input { prompt, text, cursor }) => {
            assert_eq!(prompt, "(rename-window) ");
            assert_eq!(cursor, text.chars().count());
        }
        other => panic!("expected a rename prompt, got {other:?}"),
    }
}
