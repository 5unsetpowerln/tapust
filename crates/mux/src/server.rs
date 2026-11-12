//! What tmux keeps in globals and every session shares: the numbering of
//! sessions (session.c), windows and panes (window.c), the paste buffers
//! (paste.c), the prompt histories (status.c) and the global options.

use std::cell::{Cell, RefCell, RefMut};
use std::collections::BTreeMap;
use std::rc::Rc;

use crate::paste::Buffers;
use crate::{Key, KeyCode, ModeKeys, Mods, PaneId};

/// The state the sessions of one process share, as tmux's server holds it.
/// Clones are handles to the same state.
#[derive(Clone, Default)]
pub struct Server(Rc<Shared>);

struct Shared {
    next_session_id: Cell<u32>,
    /// The names of the live sessions by id (tmux's `sessions` tree).
    sessions: RefCell<BTreeMap<u32, String>>,
    next_window_id: Cell<u32>,
    next_pane_id: Cell<PaneId>,
    next_active_point: Cell<u32>,
    buffers: RefCell<Buffers>,
    /// The prompt histories: commands, then searches.
    history: RefCell<[Vec<String>; 2]>,
    /// The `mode-keys` option.
    mode_keys: Cell<ModeKeys>,
    /// The `prefix` option; `None` is tmux's `None`, no prefix key.
    prefix: Cell<Option<Key>>,
}

impl Default for Shared {
    fn default() -> Shared {
        Shared {
            next_session_id: Cell::default(),
            sessions: RefCell::default(),
            next_window_id: Cell::default(),
            next_pane_id: Cell::default(),
            next_active_point: Cell::default(),
            buffers: RefCell::default(),
            history: RefCell::default(),
            mode_keys: Cell::default(),
            prefix: Cell::new(Some(DEFAULT_PREFIX)),
        }
    }
}

/// tmux's default `prefix`, C-b.
pub const DEFAULT_PREFIX: Key = Key { code: KeyCode::Char('b'), mods: Mods { ctrl: true, meta: false, shift: false } };

impl Server {
    pub fn new() -> Server {
        Server::default()
    }

    /// The `mode-keys` option, which tmux sets from $VISUAL or $EDITOR when
    /// it starts ([`ModeKeys::from_env`]).
    pub fn set_mode_keys(&self, keys: ModeKeys) {
        self.0.mode_keys.set(keys);
    }

    pub fn mode_keys(&self) -> ModeKeys {
        self.0.mode_keys.get()
    }

    /// `set -g prefix`: the key that switches to the prefix table, and
    /// which `send-prefix` sends.
    pub fn set_prefix(&self, prefix: Option<Key>) {
        self.0.prefix.set(prefix);
    }

    pub fn prefix(&self) -> Option<Key> {
        self.0.prefix.get()
    }

    /// session_create without a name: the next `$N`, named N, skipping the
    /// numbers whose name a live session has.
    pub(crate) fn add_session(&self) -> (u32, String) {
        let mut sessions = self.0.sessions.borrow_mut();
        loop {
            let id = self.0.next_session_id.get();
            self.0.next_session_id.set(id + 1);
            let name = id.to_string();
            if !sessions.values().any(|n| *n == name) {
                sessions.insert(id, name.clone());
                return (id, name);
            }
        }
    }

    /// session_destroy
    pub(crate) fn remove_session(&self, id: u32) {
        self.0.sessions.borrow_mut().remove(&id);
    }

    /// cmd_rename_session_exec's check and rename for a new name.
    pub(crate) fn rename_session(&self, id: u32, name: &str) -> Result<(), String> {
        let mut sessions = self.0.sessions.borrow_mut();
        if sessions.values().any(|n| n == name) {
            return Err(format!("duplicate session: {name}"));
        }
        sessions.insert(id, name.to_owned());
        Ok(())
    }

    pub(crate) fn new_window_id(&self) -> u32 {
        let id = self.0.next_window_id.get();
        self.0.next_window_id.set(id + 1);
        id
    }

    pub(crate) fn new_pane_id(&self) -> PaneId {
        let id = self.0.next_pane_id.get();
        self.0.next_pane_id.set(id + 1);
        id
    }

    /// tmux's `next_active_point`, which orders the panes by activity.
    pub(crate) fn active_point(&self) -> &Cell<u32> {
        &self.0.next_active_point
    }

    pub(crate) fn buffers(&self) -> RefMut<'_, Buffers> {
        self.0.buffers.borrow_mut()
    }

    pub(crate) fn history(&self, kind: usize) -> Vec<String> {
        self.0.history.borrow()[kind].clone()
    }

    /// status_prompt_add_history
    pub(crate) fn add_history(&self, kind: usize, line: &str, limit: usize) {
        let history = &mut self.0.history.borrow_mut()[kind];
        if !line.is_empty() && history.last().map(String::as_str) != Some(line) {
            history.push(line.to_owned());
            if history.len() > limit {
                history.remove(0);
            }
        }
    }
}
