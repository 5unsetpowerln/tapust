//! Unit tests: tmux's pane and window bookkeeping and key tables over
//! ghostty's split layout, in a 1276x796 area of 10x20 cells with 2px padding
//! and a 1px divider (one pane holds 127x39 cells).

use std::time::{Duration, Instant};

use crate::*;

const M: Metrics =
    Metrics { width: 1276, height: 796, cell_width: 10, cell_height: 20, padding_x: 2, padding_y: 2, divider: 1 };

fn new_session() -> Session {
    Session::new(M).0
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
    Command::SplitWindow { horizontal, before: false, full: false, cwd_of_active: false }
}

fn full_split(horizontal: bool) -> Command {
    Command::SplitWindow { horizontal, before: false, full: true, cwd_of_active: false }
}

/// (x, y, w, h) of the current window's panes in index order.
fn rects(s: &Session) -> Vec<(u32, u32, u32, u32)> {
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

/// The text of the current prompt or message, empty without one.
fn overlay_text(s: &Session) -> String {
    match s.view().overlay {
        Some(Overlay::Confirm { prompt, .. }) => prompt,
        Some(Overlay::Input { prompt, text, .. }) => prompt + &text,
        Some(Overlay::Message(text)) => text,
        None => String::new(),
    }
}

// --- Layout ------------------------------------------------------------------

#[test]
fn split_horizontal_halves() {
    let mut s = new_session();
    ok(&mut s, split(true));
    // GtkPaned: round((1276 - 1) * 0.5) = 638 for the first pane.
    assert_eq!(rects(&s), [(0, 0, 638, 796), (639, 0, 637, 796)]);
    assert_eq!(s.view().dividers, [Rect { x: 638, y: 0, w: 1, h: 796 }]);
}

#[test]
fn split_vertical_halves() {
    let mut s = new_session();
    ok(&mut s, split(false));
    assert_eq!(rects(&s), [(0, 0, 1276, 398), (0, 399, 1276, 397)]);
}

#[test]
fn split_before_puts_the_new_pane_first() {
    let mut s = new_session();
    ok(&mut s, Command::SplitWindow { horizontal: true, before: true, full: false, cwd_of_active: false });
    let ids: Vec<PaneId> = s.windows()[0].panes.iter().map(|p| p.pane).collect();
    assert_eq!(ids, [1, 0]);
    assert_eq!(rects(&s), [(0, 0, 638, 796), (639, 0, 637, 796)]);
    assert_eq!(s.active_pane(), 1);
}

#[test]
fn resize_right_by_five_cells() {
    let mut s = new_session();
    ok(&mut s, split(true));
    select_index(&mut s, 0);
    ok(&mut s, Command::ResizePane(Direction::Right, 5));
    // The divider moves 50 of the 1276 pixels: round(1275 * (0.5 + 50 / 1276)).
    assert_eq!(rects(&s), [(0, 0, 687, 796), (688, 0, 588, 796)]);
}

#[test]
fn resize_moves_the_divider_of_the_panes_own_split() {
    // Splitting the first of two panes gives [[0 | 2] | 1]. resize-pane -R on
    // pane 2 moves the divider of its own split, on its left (ghostty);
    // tmux's flat [0 | 2 | 1] would move the one on its right.
    let mut s = new_session();
    ok(&mut s, split(true));
    select_index(&mut s, 0);
    ok(&mut s, split(true));
    assert_eq!(rects(&s), [(0, 0, 319, 796), (320, 0, 318, 796), (639, 0, 637, 796)]);
    ok(&mut s, Command::ResizePane(Direction::Right, 5));
    assert_eq!(rects(&s), [(0, 0, 368, 796), (369, 0, 269, 796), (639, 0, 637, 796)]);
}

#[test]
fn window_resize_keeps_the_ratios() {
    let mut s = new_session();
    ok(&mut s, split(true));
    select_index(&mut s, 0);
    ok(&mut s, Command::ResizePane(Direction::Right, 5));
    let before = rects(&s);
    s.resize(Metrics { width: 638, height: 398, ..M });
    assert_eq!(rects(&s), [(0, 0, 343, 398), (344, 0, 294, 398)]);
    s.resize(M);
    assert_eq!(rects(&s), before);
}

#[test]
fn kill_gives_the_space_to_the_sibling() {
    let mut s = new_session();
    ok(&mut s, split(true));
    ok(&mut s, split(true));
    assert_eq!(rects(&s), [(0, 0, 638, 796), (639, 0, 318, 796), (958, 0, 318, 796)]);
    select_index(&mut s, 1);
    ok(&mut s, Command::KillPane);
    assert_eq!(rects(&s), [(0, 0, 638, 796), (639, 0, 637, 796)]);
}

#[test]
fn kill_first_of_two() {
    let mut s = new_session();
    ok(&mut s, split(true));
    select_index(&mut s, 0);
    ok(&mut s, Command::KillPane);
    assert_eq!(rects(&s), [(0, 0, 1276, 796)]);
}

#[test]
fn select_left_wraps() {
    let mut s = new_session();
    ok(&mut s, split(true));
    select_index(&mut s, 0);
    ok(&mut s, Command::SelectPane(Direction::Left));
    assert_eq!(active_index(&s), 1);
}

#[test]
fn select_uses_recency() {
    let mut s = new_session();
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
    let mut s = new_session();
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
    let mut s = new_session();
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
    let mut s = new_session();
    assert_eq!(ok(&mut s, Command::ZoomPane), []);
    assert!(!s.windows()[0].zoomed);
}

#[test]
fn zoom_shows_the_active_pane_alone() {
    let mut s = new_session();
    ok(&mut s, split(true));
    let effects = ok(&mut s, Command::ZoomPane);
    assert_eq!(effects, [Effect::Resize { pane: 1, cols: 127, rows: 39 }]);
    let v = s.view();
    assert!(v.dividers.is_empty());
    assert_eq!(v.panes, [PaneView { pane: 1, rect: M.area(), active: true }]);
    // Splitting unzooms.
    ok(&mut s, split(false));
    assert!(!s.windows()[0].zoomed);
    assert_eq!(rects(&s), [(0, 0, 638, 796), (639, 0, 637, 398), (639, 399, 637, 397)]);
}

#[test]
fn full_vertical_split_after_horizontal() {
    let mut s = new_session();
    ok(&mut s, split(true));
    ok(&mut s, full_split(false));
    assert_eq!(rects(&s), [(0, 0, 638, 398), (639, 0, 637, 398), (0, 399, 1276, 397)]);
    assert_eq!(
        s.view().dividers,
        [Rect { x: 638, y: 0, w: 1, h: 398 }, Rect { x: 0, y: 398, w: 1276, h: 1 }]
    );
}

#[test]
fn equalize_evens_out() {
    let mut s = new_session();
    ok(&mut s, split(true));
    ok(&mut s, split(true));
    ok(&mut s, Command::Equalize);
    assert_eq!(rects(&s), [(0, 0, 425, 796), (426, 0, 425, 796), (852, 0, 424, 796)]);
}

#[test]
fn split_without_room_is_refused() {
    // A pane needs one cell inside its padding: 14 pixels across.
    let mut s = Session::new(Metrics { width: 30, ..M }).0;
    ok(&mut s, split(true));
    assert_eq!(rects(&s), [(0, 0, 15, 796), (16, 0, 14, 796)]);
    let out = run(&mut s, split(true));
    assert_eq!(out.error.as_deref(), Some("no space for new pane"));
    assert_eq!(out.effects, []);
    assert_eq!(run(&mut s, full_split(true)).error.as_deref(), Some("no space for new pane"));
}

#[test]
fn small_window_keeps_the_minimum_sizes_in_order() {
    let mut s = Session::new(Metrics { height: 100, ..M }).0;
    ok(&mut s, split(false));
    ok(&mut s, split(false));
    assert_eq!(rects(&s), [(0, 0, 1276, 50), (0, 51, 1276, 24), (0, 76, 1276, 24)]);
    s.resize(Metrics { height: 60, ..M });
    // The first panes keep their minimum; the last one gets what is left.
    assert_eq!(rects(&s), [(0, 0, 1276, 24), (0, 25, 1276, 24), (0, 50, 1276, 10)]);
    assert!(s.windows()[0].panes.iter().all(|p| M.grid(p.rect).1 == 1));
}

// --- Effects ---------------------------------------------------------------

#[test]
fn new_session_spawns_first_pane() {
    let (s, effects) = Session::new(M);
    assert_eq!(effects, [Effect::Spawn { pane: 0, cols: 127, rows: 39, cwd_of: None }]);
    assert_eq!(s.active_pane(), 0);
}

#[test]
fn split_effects() {
    let mut s = new_session();
    let effects =
        ok(&mut s, Command::SplitWindow { horizontal: true, before: false, full: false, cwd_of_active: true });
    assert_eq!(
        effects,
        [
            Effect::Resize { pane: 0, cols: 63, rows: 39 },
            Effect::Spawn { pane: 1, cols: 63, rows: 39, cwd_of: Some(0) },
        ]
    );
}

#[test]
fn resize_resizes_every_window() {
    let mut s = new_session();
    ok(&mut s, split(true));
    ok(&mut s, Command::NewWindow { cwd_of_active: false });
    let effects = s.resize(Metrics { width: 1576, height: 896, ..M });
    assert_eq!(
        effects,
        [
            Effect::Resize { pane: 0, cols: 78, rows: 44 },
            Effect::Resize { pane: 1, cols: 78, rows: 44 },
            Effect::Resize { pane: 2, cols: 157, rows: 44 },
        ]
    );
}

#[test]
fn last_pane_exit_exits() {
    let mut s = new_session();
    ok(&mut s, Command::NewWindow { cwd_of_active: false });
    assert_eq!(s.pane_exited(1), [Effect::Destroy { pane: 1 }]);
    assert_eq!(s.active_pane(), 0);
    assert_eq!(s.pane_exited(0), [Effect::Destroy { pane: 0 }, Effect::Exit]);
    assert_eq!(s.pane_exited(0), []);
}

#[test]
fn break_pane_does_not_spawn() {
    let mut s = new_session();
    ok(&mut s, split(true));
    let effects = ok(&mut s, Command::BreakPane);
    assert_eq!(
        effects,
        [Effect::Resize { pane: 0, cols: 127, rows: 39 }, Effect::Resize { pane: 1, cols: 127, rows: 39 }]
    );
    assert_eq!(s.windows().len(), 2);
    assert!(s.windows()[1].active);
}

// --- Keys ------------------------------------------------------------------

#[test]
fn prefix_then_split() {
    let t = Instant::now();
    let mut s = new_session();
    assert_eq!(s.key(ctrl('b'), t), KeyResult::Handled(vec![]));
    let KeyResult::Handled(effects) = s.key(ch('%'), t) else { panic!() };
    assert_eq!(effects.len(), 2);
    assert_eq!(rects(&s).len(), 2);
    assert_eq!(s.key(ch('a'), t), KeyResult::Forward(1));
}

#[test]
fn unbound_key_after_prefix_is_swallowed() {
    let t = Instant::now();
    let mut s = new_session();
    s.key(ctrl('b'), t);
    assert_eq!(s.key(ch('Y'), t), KeyResult::Handled(vec![]));
    assert_eq!(s.key(ch('Y'), t), KeyResult::Forward(0));
}

#[test]
fn unimplemented_default_binding_is_consumed() {
    let t = Instant::now();
    let mut s = new_session();
    for k in [ch('d'), ch('['), ch('?'), ch(' '), meta(KeyCode::Char('1')), key(KeyCode::PageUp)] {
        s.key(ctrl('b'), t);
        assert_eq!(s.key(k, t), KeyResult::Handled(vec![]), "{k:?}");
    }
}

#[test]
fn repeat_within_repeat_time() {
    let t = Instant::now();
    let mut s = new_session();
    ok(&mut s, split(true));
    select_index(&mut s, 0);
    s.key(ctrl('b'), t);
    s.key(ch('L'), t);
    assert_eq!(s.deadline(), Some(t + Duration::from_millis(500)));
    let t2 = t + Duration::from_millis(400);
    s.key(ch('L'), t2);
    assert_eq!(s.deadline(), Some(t2 + Duration::from_millis(500)));
    assert_eq!(rects(&s)[0].2, 737);
    // Still repeating at t2 + 499 ms.
    s.key(ch('L'), t2 + Duration::from_millis(499));
    assert_eq!(rects(&s)[0].2, 787);
}

#[test]
fn key_after_repeat_time_goes_to_pane() {
    let t = Instant::now();
    let mut s = new_session();
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
    let mut s = new_session();
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
    let mut s = new_session();
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
    let mut s = new_session();
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
    let mut s = new_session();
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
    let mut s = new_session();
    ok(&mut s, split(true));
    s.key(ctrl('b'), t);
    s.key(ch('x'), t);
    assert_eq!(
        s.view().overlay,
        // confirm-before appends a space to the prompt (cmd-confirm-before.c).
        Some(Overlay::Confirm { prompt: "kill-pane 1? (y/n) ".to_owned(), target: ConfirmTarget::Pane })
    );
    // A special key is ignored by a single-key prompt.
    s.key(key(KeyCode::Left), t);
    assert!(overlay_text(&s).starts_with("kill-pane 1?"));
    assert_eq!(s.key(ch('n'), t), KeyResult::Handled(vec![]));
    assert_eq!(rects(&s).len(), 2);
    s.key(ctrl('b'), t);
    s.key(ch('x'), t);
    assert_eq!(s.key(ch('y'), t), KeyResult::Handled(vec![
        Effect::Destroy { pane: 1 },
        Effect::Resize { pane: 0, cols: 127, rows: 39 },
    ]));
    assert_eq!(s.view().overlay, None);
}

#[test]
fn confirm_kill_window() {
    let t = Instant::now();
    let mut s = new_session();
    ok(&mut s, Command::NewWindow { cwd_of_active: false });
    s.set_auto_name(1, "vim");
    s.key(ctrl('b'), t);
    s.key(ch('&'), t);
    assert_eq!(
        s.view().overlay,
        Some(Overlay::Confirm { prompt: "kill-window vim? (y/n) ".to_owned(), target: ConfirmTarget::Window })
    );
    s.key(ch('y'), t);
    assert_eq!(s.windows().len(), 1);
}

#[test]
fn rename_window_editing() {
    let t = Instant::now();
    let mut s = new_session();
    s.set_auto_name(0, "bash");
    s.key(ctrl('b'), t);
    s.key(ch(','), t);
    let input = |text: &str, cursor| Some(Overlay::Input { prompt: "(rename-window) ".to_owned(), text: text.to_owned(), cursor });
    assert_eq!(s.view().overlay, input("bash", 4));
    s.key(key(KeyCode::Backspace), t);
    s.key(key(KeyCode::Home), t);
    s.key(ch('x'), t);
    s.key(ctrl('e'), t);
    s.key(ch('y'), t);
    s.key(key(KeyCode::Left), t);
    s.key(key(KeyCode::Left), t);
    s.key(ch('-'), t);
    assert_eq!(s.view().overlay, input("xba-sy", 4));
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
    let mut s = new_session();
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
    assert_eq!(s.view().overlay, None);
}

#[test]
fn rename_session_prompt() {
    let t = Instant::now();
    let mut s = new_session();
    s.key(ctrl('b'), t);
    s.key(ch('$'), t);
    assert_eq!(overlay_text(&s), "(rename-session) 0");
    s.key(key(KeyCode::Backspace), t);
    s.key(key(KeyCode::Enter), t);
    assert!(overlay_text(&s).starts_with("Invalid session: "));
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
    ok(&mut s, Command::RenameWindow("#S".to_owned()));
    assert_eq!(s.windows()[0].name, "a_b");
}

#[test]
fn error_message_and_expiry() {
    let t = Instant::now();
    let mut s = new_session();
    s.key(ctrl('b'), t);
    s.key(ch('5'), t);
    assert_eq!(s.view().overlay, Some(Overlay::Message("Can't find window: 5".to_owned())));
    assert_eq!(s.deadline(), Some(t + Duration::from_millis(750)));
    let g = s.generation();
    s.tick(t + Duration::from_millis(750));
    assert_ne!(s.generation(), g);
    assert_eq!(s.view().overlay, None);
    // A key press clears a message and is then processed normally.
    s.key(ctrl('b'), t);
    s.key(ch('n'), t);
    assert_eq!(overlay_text(&s), "No next window");
    assert_eq!(s.key(ch('q'), t), KeyResult::Forward(0));
    assert_eq!(s.view().overlay, None);
}

#[test]
fn arrow_resize_bindings() {
    let t = Instant::now();
    let mut s = new_session();
    ok(&mut s, split(true));
    select_index(&mut s, 0);
    s.key(ctrl('b'), t);
    s.key(meta(KeyCode::Right), t);
    assert_eq!(rects(&s)[0].2, 687);
    s.key(meta(KeyCode::Right), t + Duration::from_millis(100));
    assert_eq!(rects(&s)[0].2, 737);
    let later = t + Duration::from_secs(1);
    s.key(ctrl('b'), later);
    s.key(Key { code: KeyCode::Left, mods: Mods { ctrl: true, ..Mods::default() } }, later);
    assert_eq!(rects(&s)[0].2, 727);
}

#[test]
fn swap_window_bindings_repeat() {
    let t = Instant::now();
    let mut s = new_session();
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
    let mut s = new_session();
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
    assert_eq!(rects(&s), [(0, 0, 638, 796), (639, 0, 637, 398), (639, 399, 637, 397)]);
}

#[test]
fn exit_of_pane_in_other_window() {
    let mut s = new_session();
    ok(&mut s, split(true));
    ok(&mut s, Command::NewWindow { cwd_of_active: false });
    assert_eq!(s.pane_exited(1), [Effect::Destroy { pane: 1 }, Effect::Resize { pane: 0, cols: 127, rows: 39 }]);
    assert_eq!(s.active_pane(), 2);
    assert_eq!(s.pane_exited(2), [Effect::Destroy { pane: 2 }]);
    assert_eq!(s.active_pane(), 0);
}

#[test]
fn dead_session_view() {
    let mut s = new_session();
    assert_eq!(ok(&mut s, Command::KillWindow), [Effect::Destroy { pane: 0 }, Effect::Exit]);
    let v = s.view();
    assert!(v.panes.is_empty() && v.dividers.is_empty());
    assert_eq!(s.active_pane(), 0);
    assert_eq!(s.key(ctrl('b'), Instant::now()), KeyResult::Handled(vec![]));
}

// --- Formats -------------------------------------------------------------------

#[test]
fn vis_encoding() {
    assert_eq!(format::vis("a b\\c\td\u{1}\u{e9}"), "a b\\\\c\\td\\001\u{e9}");
}

#[test]
fn rename_expands_formats() {
    let mut s = new_session();
    ok(&mut s, Command::RenameWindow("#S-#I-##-#{pane_index}-#Q".to_owned()));
    assert_eq!(s.windows()[0].name, "0-0-#-0-#Q");
}
