//! Unit tests: tmux's pane and window bookkeeping and key tables over
//! ghostty's split tree, whose layout GTK does (geometry for directional
//! selection is laid out here as GtkPaned would, in a 1276x796 area).

use std::time::{Duration, Instant};

use crate::*;

fn new_session() -> Session {
    Session::new().0
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

/// The current window's tree, as `[a | b]` (side by side) and `[a / b]`.
fn shape(s: &Session) -> String {
    fn walk(n: &Node) -> String {
        match n {
            Node::Leaf(p) => p.to_string(),
            Node::Split(sp) => {
                format!("[{} {} {}]", walk(&sp.first), if sp.horizontal { "|" } else { "/" }, walk(&sp.second))
            }
        }
    }
    walk(s.tree(s.current()).unwrap().0)
}

fn root_ratio(s: &Session) -> f64 {
    match s.tree(s.current()).unwrap().0 {
        Node::Split(sp) => sp.ratio,
        Node::Leaf(_) => panic!("not split"),
    }
}

fn ids(s: &Session) -> Vec<PaneId> {
    let w = s.windows().into_iter().find(|w| w.active).unwrap();
    w.panes.iter().map(|p| p.pane).collect()
}

/// Lays the current window out as GtkPaned would (first child
/// round((size - 1) * ratio)) and reports the geometry, as the GUI does.
fn lay(s: &mut Session) {
    fn place(n: &Node, r: Rect, out: &mut Vec<(PaneId, Rect, u16, u16)>) {
        match n {
            Node::Leaf(p) => out.push((*p, r, 0, 0)),
            Node::Split(sp) => {
                let total = if sp.horizontal { r.w } else { r.h };
                let pos = (f64::from(total - 1) * sp.ratio).round() as u32;
                let rest = total - pos - 1;
                let (a, b) = if sp.horizontal {
                    (Rect { w: pos, ..r }, Rect { x: r.x + pos + 1, w: rest, ..r })
                } else {
                    (Rect { h: pos, ..r }, Rect { y: r.y + pos + 1, h: rest, ..r })
                };
                place(&sp.first, a, out);
                place(&sp.second, b, out);
            }
        }
    }
    let area = Rect { x: 0, y: 0, w: 1276, h: 796 };
    let idx = s.current();
    let (tree, zoomed, _) = s.tree(idx).unwrap();
    let mut out = Vec::new();
    match zoomed {
        Some(p) => out.push((p, area, 0, 0)),
        None => place(tree, area, &mut out),
    }
    s.set_geometry(idx, area, &out);
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
    match s.overlay() {
        Some(Overlay::Confirm { prompt, .. }) => prompt,
        Some(Overlay::Input { prompt, text, .. } | Overlay::Keys { prompt, text }) => prompt + &text,
        Some(Overlay::Message(text)) => text,
        None => String::new(),
    }
}

fn resize_effect(result: KeyResult) -> Vec<Effect> {
    match result {
        KeyResult::Handled(effects) => effects,
        other => panic!("{other:?}"),
    }
}

// --- Tree ----------------------------------------------------------------------

#[test]
fn splits_wrap_the_active_pane() {
    let mut s = new_session();
    ok(&mut s, split(true));
    assert_eq!(shape(&s), "[0 | 1]");
    assert_eq!(root_ratio(&s), 0.5);
    ok(&mut s, split(false));
    assert_eq!(shape(&s), "[0 | [1 / 2]]");
    assert_eq!(ids(&s), [0, 1, 2]);
}

#[test]
fn split_before_puts_the_new_pane_first() {
    let mut s = new_session();
    ok(&mut s, Command::SplitWindow { horizontal: true, before: true, full: false, cwd_of_active: false });
    assert_eq!(shape(&s), "[1 | 0]");
    assert_eq!(ids(&s), [1, 0]);
    assert_eq!(s.active_pane(), 1);
}

#[test]
fn full_split_wraps_the_whole_tree() {
    let mut s = new_session();
    ok(&mut s, split(true));
    ok(&mut s, full_split(false));
    assert_eq!(shape(&s), "[[0 | 1] / 2]");
}

#[test]
fn kill_gives_the_space_to_the_sibling() {
    let mut s = new_session();
    ok(&mut s, split(true));
    ok(&mut s, split(true));
    assert_eq!(shape(&s), "[0 | [1 | 2]]");
    select_index(&mut s, 1);
    ok(&mut s, Command::KillPane);
    assert_eq!(shape(&s), "[0 | 2]");
    select_index(&mut s, 0);
    ok(&mut s, Command::KillPane);
    assert_eq!(shape(&s), "2");
}

#[test]
fn resize_pane_asks_the_gui_and_moves_the_nearest_split() {
    let mut s = new_session();
    ok(&mut s, split(true));
    select_index(&mut s, 0);
    let effects = ok(&mut s, Command::ResizePane(Direction::Right, 5));
    assert_eq!(effects, [Effect::ResizeSplit { pane: 0, horizontal: true, cells: 5 }]);
    // The GUI turns 5 cells of 10 pixels into a share of the 1276 pixel tree.
    let mut expected = 0.5;
    for _ in 0..6 {
        s.resize_split(0, true, 50.0 / 1276.0);
        expected = f16(expected + f16(f16(50.0 / 1276.0) / 1.0));
    }
    assert_eq!(root_ratio(&s), expected);
    assert_eq!(ok(&mut s, Command::ResizePane(Direction::Up, 2)), [Effect::ResizeSplit { pane: 0, horizontal: false, cells: -2 }]);
}

#[test]
fn set_ratio_writes_back_as_f16_without_a_rebuild() {
    let mut s = new_session();
    ok(&mut s, split(true));
    let generation = s.generation();
    s.set_ratio(0, 0, 698.0 / 1275.0);
    assert_eq!(root_ratio(&s), f16(698.0 / 1275.0));
    assert_eq!(s.generation(), generation);
}

#[test]
fn equalize_evens_out() {
    let mut s = new_session();
    ok(&mut s, split(true));
    ok(&mut s, split(true));
    ok(&mut s, Command::Equalize);
    assert_eq!(root_ratio(&s), f16(1.0 / 3.0));
}

#[test]
fn select_left_wraps() {
    let mut s = new_session();
    ok(&mut s, split(true));
    lay(&mut s);
    select_index(&mut s, 0);
    ok(&mut s, Command::SelectPane(Direction::Left));
    assert_eq!(active_index(&s), 1);
}

#[test]
fn select_uses_recency() {
    let mut s = new_session();
    ok(&mut s, split(true));
    ok(&mut s, split(false));
    lay(&mut s);
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
fn zoom_shows_the_active_pane_alone_until_a_split() {
    let mut s = new_session();
    assert_eq!(ok(&mut s, Command::ZoomPane), []);
    assert!(!s.windows()[0].zoomed);
    ok(&mut s, split(true));
    ok(&mut s, Command::ZoomPane);
    assert_eq!(s.tree(0).map(|t| t.1), Some(Some(1)));
    ok(&mut s, split(false));
    assert!(!s.windows()[0].zoomed);
    assert_eq!(shape(&s), "[0 | [1 / 2]]");
}

#[test]
fn click_focuses_and_drop_moves_a_pane() {
    let mut s = new_session();
    ok(&mut s, split(true));
    assert_eq!(s.active_pane(), 1);
    s.focus_pane(0);
    assert_eq!(s.active_pane(), 0);
    // Dropping pane 1 on the left side of pane 0 swaps their places.
    s.move_pane(1, 0, Direction::Left);
    assert_eq!(shape(&s), "[1 | 0]");
    assert_eq!(ids(&s), [1, 0]);
}

#[test]
fn swap_window_wraps_and_follows() {
    let mut s = new_session();
    for name in ["b", "c"] {
        ok(&mut s, Command::NewWindow { cwd_of_active: false, after: false });
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
    ok(&mut s, Command::NewWindow { cwd_of_active: false, after: false });
    ok(&mut s, Command::NewWindow { cwd_of_active: false, after: false });
    ok(&mut s, Command::SelectWindow(1));
    ok(&mut s, Command::KillWindow);
    ok(&mut s, Command::NewWindow { cwd_of_active: false, after: false });
    let indices: Vec<u32> = s.windows().iter().map(|w| w.index).collect();
    assert_eq!(indices, [0, 1, 2]);
    assert!(s.windows()[1].active);
}

// --- Effects ---------------------------------------------------------------

#[test]
fn new_session_spawns_first_pane() {
    let (s, effects) = Session::new();
    assert_eq!(effects, [Effect::Spawn { pane: 0, parent: None, inherit_cwd: false }]);
    assert_eq!(s.active_pane(), 0);
}

#[test]
fn split_effects() {
    let mut s = new_session();
    let effects =
        ok(&mut s, Command::SplitWindow { horizontal: true, before: false, full: false, cwd_of_active: true });
    assert_eq!(effects, [Effect::Spawn { pane: 1, parent: Some(0), inherit_cwd: true }]);
    // Without -c the new window still comes from the active pane.
    let effects = ok(&mut s, Command::NewWindow { cwd_of_active: false, after: false });
    assert_eq!(effects, [Effect::Spawn { pane: 2, parent: Some(1), inherit_cwd: false }]);
}

#[test]
fn last_pane_exit_exits() {
    let mut s = new_session();
    ok(&mut s, Command::NewWindow { cwd_of_active: false, after: false });
    assert_eq!(s.pane_exited(1), [Effect::Destroy { pane: 1 }]);
    assert_eq!(s.active_pane(), 0);
    assert_eq!(s.pane_exited(0), [Effect::Destroy { pane: 0 }, Effect::Exit]);
    assert_eq!(s.pane_exited(0), []);
}

#[test]
fn break_pane_does_not_spawn() {
    let mut s = new_session();
    ok(&mut s, split(true));
    assert_eq!(ok(&mut s, Command::BreakPane), []);
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
    assert_eq!(effects, [Effect::Spawn { pane: 1, parent: Some(0), inherit_cwd: true }]);
    assert_eq!(ids(&s).len(), 2);
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
    for k in [ch('d'), ch('#'), ch('='), ch('?'), ch(' '), meta(KeyCode::Char('1'))] {
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
    let resize = vec![Effect::ResizeSplit { pane: 0, horizontal: true, cells: 5 }];
    s.key(ctrl('b'), t);
    assert_eq!(resize_effect(s.key(ch('L'), t)), resize);
    assert_eq!(s.deadline(), Some(t + Duration::from_millis(500)));
    let t2 = t + Duration::from_millis(400);
    assert_eq!(resize_effect(s.key(ch('L'), t2)), resize);
    assert_eq!(s.deadline(), Some(t2 + Duration::from_millis(500)));
    // Still repeating at t2 + 499 ms.
    assert_eq!(resize_effect(s.key(ch('L'), t2 + Duration::from_millis(499))), resize);
}

#[test]
fn key_after_repeat_time_goes_to_pane() {
    let t = Instant::now();
    let mut s = new_session();
    ok(&mut s, split(true));
    lay(&mut s);
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
    assert_eq!(ids(&s).len(), 2);
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
        s.overlay(),
        // confirm-before appends a space to the prompt (cmd-confirm-before.c).
        Some(Overlay::Confirm { prompt: "kill-pane 1? (y/n) ".to_owned(), target: ConfirmTarget::Pane })
    );
    // A special key is ignored by a single-key prompt.
    s.key(key(KeyCode::Left), t);
    assert!(overlay_text(&s).starts_with("kill-pane 1?"));
    assert_eq!(s.key(ch('n'), t), KeyResult::Handled(vec![]));
    assert_eq!(ids(&s).len(), 2);
    s.key(ctrl('b'), t);
    s.key(ch('x'), t);
    assert_eq!(s.key(ch('y'), t), KeyResult::Handled(vec![Effect::Destroy { pane: 1 }]));
    assert_eq!(s.overlay(), None);
}

#[test]
fn confirm_kill_window() {
    let t = Instant::now();
    let mut s = new_session();
    ok(&mut s, Command::NewWindow { cwd_of_active: false, after: false });
    s.set_auto_name(1, "vim");
    s.key(ctrl('b'), t);
    s.key(ch('&'), t);
    assert_eq!(
        s.overlay(),
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
    let input = |text: &str, cursor| {
        Some(Overlay::Input {
            prompt: "(rename-window) ".to_owned(),
            text: text.to_owned(),
            cursor,
            kind: InputKind::RenameWindow,
        })
    };
    assert_eq!(s.overlay(), input("bash", 4));
    s.key(key(KeyCode::Backspace), t);
    s.key(key(KeyCode::Home), t);
    s.key(ch('x'), t);
    s.key(ctrl('e'), t);
    s.key(ch('y'), t);
    s.key(key(KeyCode::Left), t);
    s.key(key(KeyCode::Left), t);
    s.key(ch('-'), t);
    assert_eq!(s.overlay(), input("xba-sy", 4));
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
    assert_eq!(s.overlay(), None);
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
    assert_eq!(s.overlay(), Some(Overlay::Message("Can't find window: 5".to_owned())));
    assert_eq!(s.deadline(), Some(t + Duration::from_millis(750)));
    let g = s.generation();
    s.tick(t + Duration::from_millis(750));
    assert_ne!(s.generation(), g);
    assert_eq!(s.overlay(), None);
    // A key press clears a message and is then processed normally.
    s.key(ctrl('b'), t);
    s.key(ch('n'), t);
    assert_eq!(overlay_text(&s), "No next window");
    assert_eq!(s.key(ch('q'), t), KeyResult::Forward(0));
    assert_eq!(s.overlay(), None);
}

#[test]
fn arrow_resize_bindings() {
    let t = Instant::now();
    let mut s = new_session();
    ok(&mut s, split(true));
    select_index(&mut s, 0);
    let resize = |cells| vec![Effect::ResizeSplit { pane: 0, horizontal: true, cells }];
    s.key(ctrl('b'), t);
    assert_eq!(resize_effect(s.key(meta(KeyCode::Right), t)), resize(5));
    assert_eq!(resize_effect(s.key(meta(KeyCode::Right), t + Duration::from_millis(100))), resize(5));
    let later = t + Duration::from_secs(1);
    s.key(ctrl('b'), later);
    let ctrl_left = Key { code: KeyCode::Left, mods: Mods { ctrl: true, ..Mods::default() } };
    assert_eq!(resize_effect(s.key(ctrl_left, later)), resize(-1));
}

#[test]
fn swap_window_bindings_repeat() {
    let t = Instant::now();
    let mut s = new_session();
    ok(&mut s, Command::NewWindow { cwd_of_active: false, after: false });
    ok(&mut s, Command::NewWindow { cwd_of_active: false, after: false });
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
        s.key(ctrl('b'), t);
    s.key(ch('{'), t);
    assert_eq!(ids(&s), [0, 2, 1]);
    assert_eq!(s.active_pane(), 2);
    s.key(ctrl('b'), t);
    s.key(ctrl('o'), t);
    assert_eq!(ids(&s), [2, 1, 0]);
    assert_eq!(shape(&s), "[2 | [1 / 0]]");
}

#[test]
fn exit_of_pane_in_other_window() {
    let mut s = new_session();
    ok(&mut s, split(true));
    ok(&mut s, Command::NewWindow { cwd_of_active: false, after: false });
    assert_eq!(s.pane_exited(1), [Effect::Destroy { pane: 1 }]);
    assert_eq!(s.active_pane(), 2);
    assert_eq!(s.pane_exited(2), [Effect::Destroy { pane: 2 }]);
    assert_eq!(s.active_pane(), 0);
}

#[test]
fn dead_session_view() {
    let mut s = new_session();
    assert_eq!(ok(&mut s, Command::KillWindow), [Effect::Destroy { pane: 0 }, Effect::Exit]);
    assert_eq!(s.overlay(), None);
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

#[test]
fn new_window_after_shuffles_up() {
    let mut s = new_session();
    ok(&mut s, Command::NewWindow { cwd_of_active: false, after: false });
    ok(&mut s, Command::NewWindow { cwd_of_active: false, after: false });
    ok(&mut s, Command::SelectWindow(0));
    let ids = |s: &Session| s.windows().iter().map(|w| (w.index, w.id)).collect::<Vec<_>>();
    assert_eq!(ids(&s), vec![(0, 0), (1, 1), (2, 2)]);
    ok(&mut s, Command::NewWindow { cwd_of_active: false, after: true });
    assert_eq!(ids(&s), vec![(0, 0), (1, 3), (2, 1), (3, 2)]);
    assert_eq!(s.current(), 1);
    // The last window (-) followed its window to the new index.
    ok(&mut s, Command::SelectWindow(3));
    ok(&mut s, Command::LastWindow);
    assert_eq!(s.current(), 1);
}

#[test]
fn kill_window_by_id_leaves_the_current_window() {
    let mut s = new_session();
    ok(&mut s, Command::NewWindow { cwd_of_active: false, after: false });
    ok(&mut s, Command::NewWindow { cwd_of_active: false, after: false });
    let effects = s.kill_window_id(1);
    assert!(effects.iter().any(|e| matches!(e, Effect::Destroy { .. })));
    let ids: Vec<u32> = s.windows().iter().map(|w| w.id).collect();
    assert_eq!(ids, vec![0, 2]);
    assert_eq!(s.windows().iter().find(|w| w.active).map(|w| w.id), Some(2));
}

// --- Copy mode and paste buffers -------------------------------------------------

fn handled(result: KeyResult) -> Vec<Effect> {
    match result {
        KeyResult::Handled(effects) => effects,
        other => panic!("{other:?}"),
    }
}

fn copy(command: CopyCommand, count: u32) -> Vec<Effect> {
    vec![Effect::CopyCommand { pane: 0, command, count }]
}

/// A session whose pane 0 is in copy mode with `keys`.
fn in_copy_mode(keys: ModeKeys) -> Session {
    let t = Instant::now();
    let mut s = new_session();
    s.set_mode_keys(keys);
    s.key(ctrl('b'), t);
    assert_eq!(handled(s.key(ch('['), t)), vec![Effect::CopyMode { pane: 0, search: None }]);
    assert!(s.in_mode(0));
    s
}

#[test]
fn mode_keys_follow_visual_then_editor() {
    use std::ffi::OsStr;
    let keys = |visual: Option<&str>, editor: Option<&str>| {
        ModeKeys::from_env(visual.map(OsStr::new), editor.map(OsStr::new))
    };
    assert_eq!(keys(None, None), ModeKeys::Emacs);
    assert_eq!(keys(Some("vim"), None), ModeKeys::Vi);
    assert_eq!(keys(None, Some("/usr/bin/nvim")), ModeKeys::Vi);
    assert_eq!(keys(None, Some("emacs -nw")), ModeKeys::Emacs);
    // $VISUAL wins whenever it is set, even empty.
    assert_eq!(keys(Some("nano"), Some("vi")), ModeKeys::Emacs);
    assert_eq!(keys(Some(""), Some("vi")), ModeKeys::Emacs);
    // Only the part after the last slash counts.
    assert_eq!(keys(None, Some("/opt/vi/nano")), ModeKeys::Emacs);
}

#[test]
fn copy_mode_takes_keys_with_its_table() {
    let t = Instant::now();
    let mut s = in_copy_mode(ModeKeys::Vi);
    assert_eq!(handled(s.key(ch('k'), t)), copy(CopyCommand::CursorUp, 1));
    assert_eq!(handled(s.key(ch(' '), t)), copy(CopyCommand::BeginSelection, 1));
    assert_eq!(handled(s.key(key(KeyCode::Enter), t)), copy(CopyCommand::CopyPipeAndCancel, 1));
    // C-[ is Escape and C-m Enter, as tmux reads them.
    assert_eq!(handled(s.key(ctrl('['), t)), copy(CopyCommand::ClearSelection, 1));
    assert_eq!(handled(s.key(ctrl('m'), t)), copy(CopyCommand::CopyPipeAndCancel, 1));
    assert_eq!(handled(s.key(ch('*'), t)), copy(CopyCommand::SearchForward(SearchArg::CursorWord), 1));
    // Keys the table does not bind are swallowed.
    assert_eq!(handled(s.key(ch('Y'), t)), vec![]);
    // The prefix table still comes first.
    s.key(ctrl('b'), t);
    let effects = handled(s.key(ch('c'), t));
    assert!(matches!(effects[0], Effect::Spawn { pane: 1, .. }));
    // The new pane is not in copy mode, so its keys go to it.
    assert_eq!(s.key(ch('k'), t), KeyResult::Forward(1));
    s.key(ctrl('b'), t);
    s.key(ch('p'), t);
    assert_eq!(handled(s.key(ch('j'), t)), copy(CopyCommand::CursorDown, 1));
}

#[test]
fn emacs_table_and_entering_twice() {
    let t = Instant::now();
    let mut s = in_copy_mode(ModeKeys::Emacs);
    assert_eq!(handled(s.key(ctrl('p'), t)), copy(CopyCommand::CursorUp, 1));
    assert_eq!(handled(s.key(ctrl(' '), t)), copy(CopyCommand::BeginSelection, 1));
    assert_eq!(handled(s.key(meta(KeyCode::Char('w')), t)), copy(CopyCommand::CopyPipeAndCancel, 1));
    let ctrl_meta = Key { code: KeyCode::Char('f'), mods: Mods { ctrl: true, meta: true, shift: false } };
    assert_eq!(handled(s.key(ctrl_meta, t)), copy(CopyCommand::NextMatchingBracket, 1));
    assert_eq!(handled(s.key(key(KeyCode::Escape), t)), copy(CopyCommand::Cancel, 1));
    // copy-mode -u on a pane already in copy mode only goes up a page.
    s.key(ctrl('b'), t);
    assert_eq!(handled(s.key(key(KeyCode::PageUp), t)), copy(CopyCommand::PageUp, 1));
    s.copy_result(0, CopyResult { cancel: true, ..CopyResult::default() });
    assert!(!s.in_mode(0));
    assert_eq!(s.key(ctrl('p'), t), KeyResult::Forward(0));
    s.key(ctrl('b'), t);
    assert_eq!(
        handled(s.key(key(KeyCode::PageUp), t)),
        vec![
            Effect::CopyMode { pane: 0, search: None },
            Effect::CopyCommand { pane: 0, command: CopyCommand::PageUp, count: 1 }
        ]
    );
}

#[test]
fn repeat_counts_come_from_a_numeric_prompt() {
    let t = Instant::now();
    let mut s = in_copy_mode(ModeKeys::Vi);
    assert_eq!(handled(s.key(ch('1'), t)), vec![]);
    s.key(ch('2'), t);
    assert_eq!(s.overlay(), Some(Overlay::Keys { prompt: "(repeat) ".to_owned(), text: "12".to_owned() }));
    // The key that ends the prompt goes on to the table with the count.
    assert_eq!(handled(s.key(ch('j'), t)), copy(CopyCommand::CursorDown, 12));
    assert_eq!(s.overlay(), None);
    assert_eq!(handled(s.key(ch('j'), t)), copy(CopyCommand::CursorDown, 1));
    let mut s = in_copy_mode(ModeKeys::Emacs);
    s.key(meta(KeyCode::Char('3')), t);
    assert_eq!(handled(s.key(ctrl('n'), t)), copy(CopyCommand::CursorDown, 3));
    s.key(meta(KeyCode::Char('9')), t);
    for _ in 0..10 {
        s.key(ch('9'), t);
    }
    s.key(ctrl('n'), t);
    assert_eq!(overlay_text(&s), "Repeat count too large");
}

#[test]
fn jumps_take_the_next_key() {
    let t = Instant::now();
    let mut s = in_copy_mode(ModeKeys::Vi);
    s.key(ch('3'), t);
    assert_eq!(handled(s.key(ch('t'), t)), vec![]);
    assert_eq!(s.overlay(), Some(Overlay::Keys { prompt: "(jump to forward) ".to_owned(), text: String::new() }));
    assert_eq!(handled(s.key(ch('x'), t)), copy(CopyCommand::JumpToForward("x".to_owned()), 3));
    assert_eq!(handled(s.key(ch(';'), t)), copy(CopyCommand::JumpAgain, 1));
}

#[test]
fn search_and_goto_prompts_send_their_text() {
    let t = Instant::now();
    let mut s = in_copy_mode(ModeKeys::Vi);
    s.key(ch('/'), t);
    let kind = InputKind::Search { forward: true, incremental: false, last: String::new() };
    assert_eq!(
        s.overlay(),
        Some(Overlay::Input { prompt: "(search down) ".to_owned(), text: String::new(), cursor: 0, kind })
    );
    assert_eq!(s.answer(Some("foo"), t), copy(CopyCommand::SearchForward(SearchArg::Text("foo".to_owned())), 1));
    s.key(ch('?'), t);
    s.key(ch('a'), t);
    let up = CopyCommand::SearchBackward(SearchArg::Text("a".to_owned()));
    assert_eq!(handled(s.key(key(KeyCode::Enter), t)), copy(up, 1));
    s.key(ch(':'), t);
    assert!(matches!(s.overlay(), Some(Overlay::Input { kind: InputKind::GotoLine, .. })));
    assert_eq!(s.answer(Some("40"), t), copy(CopyCommand::GotoLine("40".to_owned()), 1));
    // The pane left copy mode before the answer came.
    s.key(ch(':'), t);
    s.copy_result(0, CopyResult { cancel: true, ..CopyResult::default() });
    assert_eq!(s.answer(Some("1"), t), vec![]);
    assert_eq!(overlay_text(&s), "Not in a mode");
}

#[test]
fn incremental_search_runs_on_every_change() {
    let t = Instant::now();
    let mut s = in_copy_mode(ModeKeys::Emacs);
    let inc = |text: &str| copy(CopyCommand::SearchForwardIncremental(text.to_owned()), 1);
    assert_eq!(handled(s.key(ctrl('s'), t)), inc("="));
    assert_eq!(handled(s.key(ch('a'), t)), inc("=a"));
    assert_eq!(handled(s.key(ctrl('s'), t)), inc("+a"));
    assert_eq!(
        handled(s.key(ctrl('r'), t)),
        copy(CopyCommand::SearchForwardIncremental("-a".to_owned()), 1)
    );
    assert_eq!(handled(s.key(key(KeyCode::Left), t)), vec![]);
    assert_eq!(s.prompt_changed("ab", '=', t), inc("=ab"));
    // Accepting or cancelling runs nothing more.
    assert_eq!(handled(s.key(key(KeyCode::Enter), t)), vec![]);
    let search = PaneSearch { text: "foo".to_owned(), regex: false };
    s.copy_result(0, CopyResult { search: Some(search), ..CopyResult::default() });
    // C-s in an empty prompt brings the pane's last search back.
    s.key(ctrl('r'), t);
    let kind = InputKind::Search { forward: false, incremental: true, last: "foo".to_owned() };
    assert!(matches!(s.overlay(), Some(Overlay::Input { kind: k, .. }) if k == kind));
    assert_eq!(
        handled(s.key(ctrl('s'), t)),
        copy(CopyCommand::SearchBackwardIncremental("=foo".to_owned()), 1)
    );
    assert_eq!(handled(s.key(key(KeyCode::Escape), t)), vec![]);
    assert_eq!(s.overlay(), None);
}

#[test]
fn the_next_copy_mode_starts_with_the_panes_search() {
    let t = Instant::now();
    let mut s = in_copy_mode(ModeKeys::Vi);
    let search = PaneSearch { text: "x.*y".to_owned(), regex: true };
    s.copy_result(0, CopyResult { cancel: true, copied: None, search: Some(search.clone()) });
    s.key(ctrl('b'), t);
    assert_eq!(handled(s.key(ch('['), t)), vec![Effect::CopyMode { pane: 0, search: Some(search) }]);
}

#[test]
fn copies_fill_buffers_up_to_the_limit() {
    let t = Instant::now();
    let mut s = new_session();
    for i in 0..paste::BUFFER_LIMIT + 2 {
        let copied = Some(Copied::New(format!("{i}").into_bytes()));
        s.copy_result(0, CopyResult { copied, ..CopyResult::default() });
    }
    let names = s.buffers.names();
    assert_eq!(names.len(), paste::BUFFER_LIMIT);
    assert_eq!((names[0], names[names.len() - 1]), ("buffer2", "buffer51"));
    // Empty copies make no buffer.
    s.copy_result(0, CopyResult { copied: Some(Copied::New(Vec::new())), ..CopyResult::default() });
    assert_eq!(s.buffers.names().len(), paste::BUFFER_LIMIT);
    s.key(ctrl('b'), t);
    assert_eq!(handled(s.key(ch(']'), t)), vec![Effect::Paste { pane: 0, data: b"51".to_vec(), bracket: true }]);
    ok(&mut s, Command::DeleteBuffer);
    s.key(ctrl('b'), t);
    assert_eq!(handled(s.key(ch(']'), t)), vec![Effect::Paste { pane: 0, data: b"50".to_vec(), bracket: true }]);
}

#[test]
fn paste_buffer_sends_line_feeds_as_carriage_returns() {
    let mut s = new_session();
    assert_eq!(ok(&mut s, Command::PasteBuffer { bracket: true }), vec![]);
    assert_eq!(run(&mut s, Command::DeleteBuffer).error.as_deref(), Some("no buffer"));
    s.copy_result(0, CopyResult { copied: Some(Copied::New(b"a\nb\n\nc".to_vec())), ..CopyResult::default() });
    assert_eq!(
        ok(&mut s, Command::PasteBuffer { bracket: false }),
        vec![Effect::Paste { pane: 0, data: b"a\rb\r\rc".to_vec(), bracket: false }]
    );
}

#[test]
fn append_selection_names_the_newest_buffer() {
    let mut s = new_session();
    s.copy_result(0, CopyResult { copied: Some(Copied::Append(b"one".to_vec())), ..CopyResult::default() });
    s.copy_result(0, CopyResult { copied: Some(Copied::New(b"two".to_vec())), ..CopyResult::default() });
    s.copy_result(0, CopyResult { copied: Some(Copied::Append(b"+".to_vec())), ..CopyResult::default() });
    // paste_set keeps buffer1's name but makes it a named buffer, which
    // paste-buffer and delete-buffer pass over.
    assert_eq!(s.buffers.names(), ["buffer0", "buffer1"]);
    assert_eq!(
        ok(&mut s, Command::PasteBuffer { bracket: false }),
        vec![Effect::Paste { pane: 0, data: b"one".to_vec(), bracket: false }]
    );
    ok(&mut s, Command::DeleteBuffer);
    assert_eq!(s.buffers.names(), ["buffer1"]);
    assert_eq!(run(&mut s, Command::DeleteBuffer).error.as_deref(), Some("no buffer"));
}

#[test]
fn a_pane_that_exits_leaves_copy_mode() {
    let t = Instant::now();
    let mut s = in_copy_mode(ModeKeys::Vi);
    s.key(ctrl('b'), t);
    s.key(ch('%'), t);
    s.pane_exited(0);
    assert!(!s.in_mode(0));
}
