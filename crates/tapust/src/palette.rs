//! ghostty's command palette (command_palette.zig, command-palette.blp): a
//! dialog with a search entry over a list of commands, each row with its
//! title over what it runs and its key binding at the end. The commands are
//! the terminal bindings (bindings.rs), the window's actions, the bindings of
//! tmux's prefix table that the model implements, shown as the prefix and
//! then the key, and ghostty's jump commands to every terminal.
//!
//! ghostty filters by a case-insensitive substring of the title or the action
//! and keeps its alphabetical order, so Enter, which runs the first row, runs
//! the alphabetically first match ("split" runs Focus Split: Down). Here every
//! word of the query has to match, in any order; a word matches as ghostty's
//! query does or as the starts of title words ("fsr" for Focus Split: Right),
//! and the matches are ranked so that "split" runs Split Down: a title that
//! starts with the word first, then the word as the starts of title words or
//! at the start of one, then inside the title, and last in the action.

use std::cmp::Ordering;
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gdk, gio, glib, pango};

use crate::bindings::{self, Binding};
use crate::input;
use crate::theme;
use Source::{Action, Key, Prefix};
use gdk::Key as K;
use gdk::ModifierType as M;

/// What an entry runs, on the pane that was active when the palette opened.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Run {
    /// A terminal binding, as its key runs it.
    Binding(Binding),
    /// The binding of tmux's prefix table for the key.
    Prefix(mux::Key),
    /// A GTK action of the pane's surface, its window or the application.
    Action(&'static str),
    /// ghostty's jump command: presents the surface with this id.
    Focus(u64),
}

pub struct Entry {
    title: String,
    /// ghostty shows it as the row's tooltip.
    description: String,
    /// What the entry runs, shown under the title in monospace and matched
    /// as ghostty matches its action key: ghostty's action, tmux's command or
    /// the GTK action; the directory of a jump command's terminal.
    action: String,
    /// The key binding for AdwShortcutLabel; a tmux binding is a sequence of
    /// the prefix and its key.
    accel: String,
    run: Run,
    /// ghostty's sort_key: jump commands with the same title in the order
    /// their surfaces were made.
    order: u64,
}

enum Source {
    /// A key of the terminal bindings, and ghostty's name for its action.
    Key(gdk::ModifierType, gdk::Key, &'static str),
    /// A key of tmux's prefix table.
    Prefix(gdk::ModifierType, gdk::Key),
    /// A GTK action and ghostty's name for it, or the GTK action's own.
    Action(&'static str, &'static str),
}

const NONE: M = M::empty();
const SHIFT: M = M::SHIFT_MASK;
const CTRL: M = M::CONTROL_MASK;
const ALT: M = M::ALT_MASK;
const CTRL_SHIFT: M = M::CONTROL_MASK.union(M::SHIFT_MASK);

/// tmux's prefix, C-b.
const PREFIX: (M, gdk::Key) = (CTRL, K::b);

/// Titles and descriptions follow ghostty's commands (command.zig) where
/// ghostty has the command, and its words for tmux's: a window is a tab,
/// a pane a split. Keys are as GDK gives them, an uppercase letter with Shift.
const COMMANDS: &[(&str, &str, Source)] = &[
    (
        "Copy to Clipboard",
        "Copy the selected text to the clipboard in both plain and styled formats.",
        Key(CTRL_SHIFT, K::c, "copy_to_clipboard:mixed"),
    ),
    (
        "Paste from Clipboard",
        "Paste the contents of the main clipboard.",
        Key(CTRL_SHIFT, K::v, "paste_from_clipboard"),
    ),
    (
        "Paste from Selection",
        "Paste the contents of the selection clipboard.",
        Key(SHIFT, K::Insert, "paste_from_selection"),
    ),
    ("Select All", "Select all text on the screen.", Key(CTRL_SHIFT, K::a, "select_all")),
    ("Start Search", "Start a search if one isn't already active.", Key(CTRL_SHIFT, K::f, "start_search")),
    ("Scroll Page Up", "Scroll the screen up by a page.", Key(SHIFT, K::Page_Up, "scroll_page_up")),
    ("Scroll Page Down", "Scroll the screen down by a page.", Key(SHIFT, K::Page_Down, "scroll_page_down")),
    ("Scroll to Top", "Scroll to the top of the scrollback.", Key(SHIFT, K::Home, "scroll_to_top")),
    ("Scroll to Bottom", "Scroll to the bottom of the scrollback.", Key(SHIFT, K::End, "scroll_to_bottom")),
    ("Jump to Previous Prompt", "Scroll to the previous shell prompt.", Key(CTRL_SHIFT, K::Up, "jump_to_prompt:-1")),
    ("Jump to Next Prompt", "Scroll to the next shell prompt.", Key(CTRL_SHIFT, K::Down, "jump_to_prompt:1")),
    ("Increase Font Size", "Increase the font size by 1 point.", Key(CTRL, K::plus, "increase_font_size:1")),
    ("Decrease Font Size", "Decrease the font size by 1 point.", Key(CTRL, K::minus, "decrease_font_size:1")),
    ("Reset Font Size", "Reset the font size to the default.", Key(CTRL, K::_0, "reset_font_size")),
    ("Clear Screen", "Clear the screen and scrollback.", Action("win.clear", "clear_screen")),
    ("Reset Terminal", "Reset the terminal to a clean state.", Action("win.reset", "reset")),
    (
        "Change Terminal Title\u{2026}",
        "Prompt for a new title for the current terminal.",
        Action("win.prompt-surface-title", "prompt_surface_title"),
    ),
    (
        "Change Window Title\u{2026}",
        "Prompt for a new title for the current window.",
        Action("win.prompt-window-title", "prompt_window_title"),
    ),
    (
        "Notify on Next Command Finish",
        "Toggle ringing the bell when the next command in the current terminal finishes.",
        Action("surface.notify-on-next-command-finish", "surface.notify-on-next-command-finish"),
    ),
    ("Split Left", "Split the terminal to the left.", Action("win.split-left", "new_split:left")),
    ("Split Up", "Split the terminal up.", Action("win.split-up", "new_split:up")),
    ("New Window", "Open a new window.", Action("win.new-window", "new_window")),
    ("Close Window", "Close the current window.", Action("win.close", "close_window")),
    (
        "Toggle Fullscreen",
        "Toggle the fullscreen state of the current window.",
        Action("win.toggle-fullscreen", "toggle_fullscreen"),
    ),
    ("Toggle Tab Overview", "Toggle the tab overview.", Action("win.toggle-tab-overview", "toggle_tab_overview")),
    ("About tapust", "Show the version of tapust.", Action("win.about", "win.about")),
    ("Quit", "Quit the application.", Action("app.quit", "quit")),
    ("Split Right", "Split the terminal to the right.", Prefix(NONE, K::bar)),
    ("Split Down", "Split the terminal down.", Prefix(NONE, K::minus)),
    ("Split Tab Right", "Add a terminal as tall as the tab at its right.", Prefix(NONE, K::backslash)),
    ("Split Tab Down", "Add a terminal as wide as the tab at its bottom.", Prefix(NONE, K::underscore)),
    ("New Tab", "Open a new tab.", Prefix(NONE, K::c)),
    ("Close Terminal", "Close the current terminal, after asking.", Prefix(NONE, K::x)),
    ("Close Tab", "Close the current tab, after asking.", Prefix(NONE, K::ampersand)),
    ("Change Tab Title\u{2026}", "Prompt for a new title for the current tab.", Prefix(NONE, K::comma)),
    ("Rename Session\u{2026}", "Prompt for a new name for the session of the current window.", Prefix(NONE, K::dollar)),
    ("Focus Split: Left", "Focus the split to the left, if it exists.", Prefix(NONE, K::Left)),
    ("Focus Split: Right", "Focus the split to the right, if it exists.", Prefix(NONE, K::Right)),
    ("Focus Split: Up", "Focus the split above, if it exists.", Prefix(NONE, K::Up)),
    ("Focus Split: Down", "Focus the split below, if it exists.", Prefix(NONE, K::Down)),
    ("Focus Split: Next", "Focus the next split, if any.", Prefix(NONE, K::o)),
    ("Focus Split: Last Active", "Focus the split that was active before the current one.", Prefix(NONE, K::semicolon)),
    ("Focus Tab: Next", "Focus the next tab.", Prefix(NONE, K::n)),
    ("Focus Tab: Previous", "Focus the previous tab.", Prefix(NONE, K::p)),
    ("Move Tab Left", "Move the current tab to the left.", Prefix(NONE, K::less)),
    ("Move Tab Right", "Move the current tab to the right.", Prefix(NONE, K::greater)),
    ("Move Split to New Tab", "Move the current split into a tab of its own.", Prefix(NONE, K::exclam)),
    ("Toggle Split Zoom", "Toggle the zoom state of the current split.", Prefix(NONE, K::z)),
    ("Swap Split: Previous", "Swap the current split with the previous one.", Prefix(NONE, K::braceleft)),
    ("Swap Split: Next", "Swap the current split with the next one.", Prefix(NONE, K::braceright)),
    ("Rotate Splits", "Move every split of the tab into the place of the one before it.", Prefix(CTRL, K::o)),
    ("Rotate Splits in Reverse", "Move every split of the tab into the place of the one after it.", Prefix(ALT, K::o)),
    ("Resize Split: Left", "Move the divider next to the current split 5 cells to the left.", Prefix(SHIFT, K::H)),
    ("Resize Split: Down", "Move the divider next to the current split 5 cells down.", Prefix(SHIFT, K::J)),
    ("Resize Split: Up", "Move the divider next to the current split 5 cells up.", Prefix(SHIFT, K::K)),
    ("Resize Split: Right", "Move the divider next to the current split 5 cells to the right.", Prefix(SHIFT, K::L)),
    (
        "Resize Split: Left by One Cell",
        "Move the divider next to the current split a cell to the left.",
        Prefix(CTRL, K::Left),
    ),
    (
        "Resize Split: Down by One Cell",
        "Move the divider next to the current split a cell down.",
        Prefix(CTRL, K::Down),
    ),
    ("Resize Split: Up by One Cell", "Move the divider next to the current split a cell up.", Prefix(CTRL, K::Up)),
    (
        "Resize Split: Right by One Cell",
        "Move the divider next to the current split a cell to the right.",
        Prefix(CTRL, K::Right),
    ),
    ("Next Layout", "Arrange the splits in the next preset layout.", Prefix(NONE, K::space)),
    ("Spread Splits Out Evenly", "Spread out the current split and the splits next to it evenly.", Prefix(SHIFT, K::E)),
    ("Select Layout: Even Horizontal", "Put the splits side by side, equally wide.", Prefix(ALT, K::_1)),
    ("Select Layout: Even Vertical", "Stack the splits, equally tall.", Prefix(ALT, K::_2)),
    (
        "Select Layout: Main Horizontal",
        "Put the first split large at the top and the others side by side below it.",
        Prefix(ALT, K::_3),
    ),
    (
        "Select Layout: Main Vertical",
        "Put the first split large at the left and stack the others at its right.",
        Prefix(ALT, K::_4),
    ),
    ("Select Layout: Tiled", "Spread the splits as evenly as possible in rows and columns.", Prefix(ALT, K::_5)),
    (
        "Select Layout: Main Horizontal Mirrored",
        "Put the first split large at the bottom and the others side by side above it.",
        Prefix(ALT, K::_6),
    ),
    (
        "Select Layout: Main Vertical Mirrored",
        "Put the first split large at the right and stack the others at its left.",
        Prefix(ALT, K::_7),
    ),
    (
        "Enter Copy Mode",
        "Select and copy text of the terminal with the keyboard, as in tmux.",
        Prefix(NONE, K::bracketleft),
    ),
    ("Enter Copy Mode and Page Up", "Enter copy mode a page up the scrollback.", Prefix(NONE, K::Page_Up)),
    ("Paste from Buffer", "Paste the most recent buffer that copy mode copied.", Prefix(NONE, K::bracketright)),
    ("Send Prefix Key", "Send Ctrl+B to the program in the terminal.", Prefix(CTRL, K::b)),
];

/// The palette's commands, each running what its key binding runs.
pub fn commands() -> Vec<Entry> {
    COMMANDS
        .iter()
        .map(|(title, description, source)| {
            let (action, accel, run) = match *source {
                Key(mods, key, action) => {
                    let binding = bindings::binding(key, mods).expect("a terminal binding");
                    (action.to_owned(), accelerator(mods, key), Run::Binding(binding))
                }
                Prefix(mods, key) => {
                    let (pressed, _) = input::from_gdk(key, mods).expect("a key");
                    let command = mux::prefix_command(pressed).expect("a binding of the prefix table");
                    let accel = format!("{}+{}", accelerator(PREFIX.0, PREFIX.1), accelerator(mods, key));
                    (command, accel, Run::Prefix(pressed))
                }
                Action(name, action) => (action.to_owned(), String::new(), Run::Action(name)),
            };
            Entry { title: (*title).to_owned(), description: (*description).to_owned(), action, accel, run, order: 0 }
        })
        .collect()
}

/// ghostty's jump command for a terminal: its title, and as its directory
/// the one its program reported unless the title shows it.
pub fn focus(id: u64, title: &str, pwd: Option<&str>) -> Entry {
    let title = if title.is_empty() { "Untitled" } else { title };
    let pwd = pwd.filter(|pwd| !title.contains(pwd)).unwrap_or_default();
    Entry {
        title: format!("Focus: {title}"),
        description: pwd.to_owned(),
        action: pwd.to_owned(),
        accel: String::new(),
        run: Run::Focus(id),
        order: id,
    }
}

/// A key as a GTK accelerator, named in lowercase after its modifiers:
/// AdwShortcutLabel shows any letter in uppercase, so Shift tells H from h.
fn accelerator(mods: M, key: gdk::Key) -> String {
    let mut accel = String::new();
    for (mask, name) in [(SHIFT, "<Shift>"), (CTRL, "<Control>"), (ALT, "<Alt>")] {
        if mods.contains(mask) {
            accel.push_str(name);
        }
    }
    accel + &key.to_lower().name().expect("a named key")
}

/// ghostty's compareCommands: by title ignoring case with a colon before a
/// space ("Focus:" before "Focus Split"), then jump commands by surface.
fn compare(a: &Entry, b: &Entry) -> Ordering {
    fn key(e: &Entry) -> impl Iterator<Item = char> + '_ {
        e.title.chars().map(|c| if c == ':' { '\t' } else { c.to_ascii_lowercase() })
    }
    key(a).cmp(key(b)).then(a.order.cmp(&b.order))
}

/// The entries that match `query`, best first and in their order among
/// equals; all of them for an empty query.
fn filter(entries: &[Entry], query: &str) -> Vec<usize> {
    let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
    let mut ranked: Vec<(u32, usize)> =
        entries.iter().enumerate().filter_map(|(i, e)| Some((rank(e, &words)?, i))).collect();
    ranked.sort_unstable();
    ranked.into_iter().map(|(_, i)| i).collect()
}

/// How well an entry matches every word of the query, lower is better.
fn rank(entry: &Entry, query: &[String]) -> Option<u32> {
    let title = entry.title.to_lowercase();
    let action = entry.action.to_lowercase();
    let words: Vec<Vec<char>> =
        title.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).map(|w| w.chars().collect()).collect();
    query.iter().map(|word| word_rank(&title, &words, &action, word)).sum()
}

/// 0: the title starts with `word`; 1: `word` is the starts of title words
/// from the first; 2: a title word starts with it; 3: it is the starts of
/// later title words; 4: it is inside the title; 5: inside the action.
fn word_rank(title: &str, words: &[Vec<char>], action: &str, word: &str) -> Option<u32> {
    let at_word = |at: usize| title[..at].chars().next_back().is_none_or(|c| !c.is_alphanumeric());
    let inside = (0..title.len())
        .filter(|&at| title.is_char_boundary(at) && title[at..].starts_with(word))
        .map(|at| match at {
            0 => 0,
            _ if at_word(at) => 2,
            _ => 4,
        })
        .min();
    let chars: Vec<char> = word.chars().collect();
    let starts = words.split_first().and_then(|(first, rest)| {
        let from_first = (1..=common_prefix(first, &chars)).any(|n| word_starts(&chars[n..], rest));
        if from_first { Some(1) } else { word_starts(&chars, words).then_some(3) }
    });
    [inside, starts, action.contains(word).then_some(5)].into_iter().flatten().min()
}

fn common_prefix(a: &[char], b: &[char]) -> usize {
    a.iter().zip(b).take_while(|(x, y)| x == y).count()
}

/// Whether `chars` is made of the starts of some of `words` in their order,
/// each at least a character ("splri" of split and right).
fn word_starts(chars: &[char], words: &[Vec<char>]) -> bool {
    // matched[n]: the first n characters are starts of the words seen so far.
    let mut matched = vec![false; chars.len() + 1];
    matched[0] = true;
    for word in words {
        // Downwards, so that a word extends only matches of earlier words.
        for n in (0..chars.len()).rev() {
            if matched[n] {
                for taken in 1..=common_prefix(word, &chars[n..]) {
                    matched[n + taken] = true;
                }
            }
        }
    }
    matched[chars.len()]
}

/// ghostty's row: the title over the action, the key binding at the end
/// and the description as the tooltip.
fn row(entry: &Entry) -> gtk::Box {
    let label = |text: &str, classes: &[&str]| {
        theme::styled(
            gtk::Label::builder()
                .label(text)
                .ellipsize(pango::EllipsizeMode::End)
                .halign(gtk::Align::Start)
                .wrap(false)
                .single_line_mode(true)
                .build(),
            classes,
        )
    };
    let text = gtk::Box::builder().orientation(gtk::Orientation::Vertical).hexpand(true).build();
    text.append(&label(&entry.title, &["title"]));
    text.append(&label(&entry.action, &["subtitle", "monospace"]));
    let row = gtk::Box::builder().orientation(gtk::Orientation::Horizontal).spacing(10).build();
    row.set_tooltip_text(Some(entry.description.as_str()).filter(|d| !d.is_empty()));
    row.append(&text);
    let keys = adw::ShortcutLabel::new(&entry.accel);
    keys.set_valign(gtk::Align::Center);
    row.append(&keys);
    row
}

/// Presents the palette over `parent`, with the search entry focused. Enter
/// or a click runs the selected entry with `run` once the palette closed
/// and gave the focus back; Up and Down move the selection, which stays on
/// the best match as the query changes; Escape closes, and ctrl+shift+p
/// closes it too (ghostty's toggle).
pub fn present(parent: &impl IsA<gtk::Widget>, mut entries: Vec<Entry>, run: impl Fn(Run) + 'static) {
    entries.sort_by(compare);
    let entries = Rc::new(entries);
    let search = theme::styled(
        gtk::SearchEntry::builder().hexpand(true).placeholder_text("Execute a command\u{2026}").build(),
        &["command-palette-search"],
    );
    let header = adw::HeaderBar::builder().title_widget(&search).build();
    let store = gio::ListStore::new::<glib::BoxedAnyObject>();
    let selection = gtk::SingleSelection::new(Some(store.clone()));
    let factory = gtk::SignalListItemFactory::new();
    let shown = entries.clone();
    factory.connect_bind(move |_, item| {
        let item = item.downcast_ref::<gtk::ListItem>().expect("a list item");
        let index = *item.item().and_downcast::<glib::BoxedAnyObject>().expect("an entry").borrow::<usize>();
        item.set_child(Some(&row(&shown[index])));
    });
    let view = theme::styled(
        gtk::ListView::builder()
            .model(&selection)
            .factory(&factory)
            .show_separators(true)
            .single_click_activate(true)
            .build(),
        &["rich-list", "command-palette-view"],
    );
    let scrolled = gtk::ScrolledWindow::builder().min_content_height(300).child(&view).build();
    let toolbar = adw::ToolbarView::builder().top_bar_style(adw::ToolbarStyle::Flat).content(&scrolled).build();
    toolbar.add_top_bar(&header);
    let dialog = adw::Dialog::builder().content_width(700).child(&toolbar).focus_widget(&search).build();

    let refresh = {
        let (entries, view) = (entries.clone(), view.clone());
        move |query: &str| {
            let items: Vec<_> = filter(&entries, query).into_iter().map(glib::BoxedAnyObject::new).collect();
            store.splice(0, store.n_items(), &items);
            if !items.is_empty() {
                view.scroll_to(0, gtk::ListScrollFlags::SELECT, None);
            }
        }
    };
    refresh("");
    search.connect_changed(move |search| refresh(&search.text()));

    let activate = Rc::new({
        let (selection, dialog) = (selection.clone(), dialog.downgrade());
        move |position: u32| {
            let Some(item) = selection.item(position).and_downcast::<glib::BoxedAnyObject>() else { return };
            let entry = entries[*item.borrow::<usize>()].run;
            // ghostty closes first, so that a dialog the entry opens does
            // not stack on the palette; closing focuses the terminal again.
            if let Some(dialog) = dialog.upgrade() {
                dialog.close();
            }
            run(entry);
        }
    });
    let on_row = activate.clone();
    view.connect_activate(move |_, position| on_row(position));
    let selected = selection.clone();
    search.connect_activate(move |_| activate(selected.selected()));
    let weak = dialog.downgrade();
    search.connect_stop_search(move |_| {
        if let Some(dialog) = weak.upgrade() {
            dialog.close();
        }
    });
    let keys = gtk::EventControllerKey::builder().propagation_phase(gtk::PropagationPhase::Capture).build();
    let weak = dialog.downgrade();
    keys.connect_key_pressed(move |_, keyval, _, state| {
        if bindings::binding(keyval, state) == Some(Binding::ToggleCommandPalette) {
            if let Some(dialog) = weak.upgrade() {
                dialog.close();
            }
            return glib::Propagation::Stop;
        }
        let step = match keyval {
            K::Up | K::KP_Up => -1,
            K::Down | K::KP_Down => 1,
            _ => return glib::Propagation::Proceed,
        };
        // The focus stays in the entry, so typing goes on refining the query.
        let count = i64::from(selection.n_items());
        if count > 0 {
            let position = (i64::from(selection.selected()) + step).rem_euclid(count);
            view.scroll_to(position as u32, gtk::ListScrollFlags::SELECT, None);
        }
        glib::Propagation::Stop
    });
    search.add_controller(keys);
    dialog.present(Some(parent));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bindings::{FontSize, Scroll};
    use mux::{KeyCode, Mods};

    fn sorted(mut entries: Vec<Entry>) -> Vec<Entry> {
        entries.sort_by(compare);
        entries
    }

    fn titles(entries: &[Entry], query: &str) -> Vec<String> {
        filter(entries, query).into_iter().map(|i| entries[i].title.clone()).collect()
    }

    fn entry(title: &str) -> Entry {
        commands().into_iter().find(|e| e.title == title).unwrap_or_else(|| panic!("{title}"))
    }

    #[test]
    fn no_two_entries_share_a_title_or_what_they_run() {
        let entries = commands();
        for (i, a) in entries.iter().enumerate() {
            for b in &entries[i + 1..] {
                assert_ne!(a.title, b.title);
                assert_ne!(a.run, b.run, "{} and {}", a.title, b.title);
                assert_ne!(a.action, b.action, "{} and {}", a.title, b.title);
            }
        }
    }

    #[test]
    fn every_terminal_binding_has_an_entry_but_the_palette_itself() {
        let bound: Vec<Binding> = commands()
            .iter()
            .filter_map(|e| match e.run {
                Run::Binding(binding) => Some(binding),
                _ => None,
            })
            .collect();
        let scrolls =
            [Scroll::PageUp, Scroll::PageDown, Scroll::Top, Scroll::Bottom, Scroll::PreviousPrompt, Scroll::NextPrompt];
        let sizes = [FontSize::Increase, FontSize::Decrease, FontSize::Reset];
        let all = [
            Binding::StartSearch,
            Binding::Copy { performable: true },
            Binding::Paste { primary: false },
            Binding::Paste { primary: true },
            Binding::SelectAll,
        ]
        .into_iter()
        .chain(scrolls.map(Binding::Scroll))
        .chain(sizes.map(Binding::FontSize));
        for binding in all {
            assert!(bound.contains(&binding), "{binding:?}");
        }
        assert!(!bound.contains(&Binding::ToggleCommandPalette));
    }

    /// Every command of the prefix table the model implements is an entry,
    /// but select-window by index, which ghostty leaves out (goto_tab) for
    /// the jump commands, and the alert windows the model never has.
    #[test]
    fn every_implemented_prefix_binding_has_an_entry() {
        let shown: Vec<String> = commands().into_iter().map(|e| e.action).collect();
        let mut keys = Vec::new();
        for (ctrl, meta, shift) in (0..8).map(|m| (m & 1 != 0, m & 2 != 0, m & 4 != 0)) {
            let mods = Mods { ctrl, meta, shift };
            if !shift {
                keys.extend((' '..='~').map(|c| mux::Key { code: KeyCode::Char(c), mods }));
            }
            let codes = [KeyCode::Up, KeyCode::Down, KeyCode::Left, KeyCode::Right, KeyCode::PageUp, KeyCode::PageDown];
            let more = [KeyCode::Home, KeyCode::End, KeyCode::Insert, KeyCode::Delete, KeyCode::Enter, KeyCode::Tab];
            keys.extend(codes.into_iter().chain(more).map(|code| mux::Key { code, mods }));
        }
        let mut found = 0;
        for key in keys {
            let Some(command) = mux::prefix_command(key) else { continue };
            found += 1;
            if command.starts_with("select-window -t :=") || command.ends_with("-window -a") {
                continue;
            }
            assert!(shown.contains(&command), "{key:?}: {command}");
        }
        assert!(found > 50, "{found}");
    }

    #[test]
    fn prefix_entries_run_the_key_a_press_gives() {
        assert!(mux::is_prefix(input::from_gdk(PREFIX.1, PREFIX.0).unwrap().0));
        let char_key = |c| Run::Prefix(mux::Key { code: KeyCode::Char(c), mods: Mods::default() });
        assert_eq!(entry("Split Right").run, char_key('|'));
        assert_eq!(entry("Resize Split: Left").run, char_key('H'));
        assert_eq!(entry("Copy to Clipboard").run, Run::Binding(Binding::Copy { performable: true }));
        assert_eq!(entry("Quit").run, Run::Action("app.quit"));
    }

    #[test]
    fn bindings_show_as_accelerators_and_prefix_bindings_as_sequences() {
        assert_eq!(entry("Copy to Clipboard").accel, "<Shift><Control>c");
        assert_eq!(entry("Increase Font Size").accel, "<Control>plus");
        assert_eq!(entry("Paste from Selection").accel, "<Shift>Insert");
        assert_eq!(entry("Split Right").accel, "<Control>b+bar");
        assert_eq!(entry("Resize Split: Left").accel, "<Control>b+<Shift>h");
        assert_eq!(entry("Resize Split: Left by One Cell").accel, "<Control>b+<Control>Left");
        assert_eq!(entry("Select Layout: Tiled").accel, "<Control>b+<Alt>5");
        assert_eq!(entry("Send Prefix Key").accel, "<Control>b+<Control>b");
        assert_eq!(entry("Quit").accel, "");
        assert_eq!(entry("Split Right").action, "split-window -h -c \"#{pane_current_path}\"");
    }

    #[test]
    fn an_empty_query_lists_all_in_ghosttys_order() {
        let mut entries = commands();
        entries.extend([focus(7, "vim", None), focus(3, "vim", Some("/tmp")), focus(5, "", None)]);
        let entries = sorted(entries);
        let all = titles(&entries, " ");
        assert_eq!(all.len(), entries.len());
        let lower: Vec<String> = all.iter().map(|t| t.to_lowercase().replace(':', "\t")).collect();
        assert!(lower.is_sorted());
        let at = |title: &str| all.iter().position(|t| t == title).unwrap();
        assert!(at("Focus: vim") < at("Focus Split: Down"));
        assert!(at("Focus: Untitled") < at("Focus: vim"));
        let vims: Vec<u64> = filter(&entries, "focus: vim").into_iter().map(|i| entries[i].order).collect();
        assert_eq!(vims, [3, 7]);
    }

    #[test]
    fn a_jump_shows_the_directory_its_title_does_not() {
        assert_eq!(focus(1, "vim", Some("/tmp")).action, "/tmp");
        assert_eq!(focus(1, "/tmp", Some("/tmp")).action, "");
        assert_eq!(focus(1, "", Some("/tmp")).title, "Focus: Untitled");
    }

    #[test]
    fn matches_ignore_case_and_rank_title_starts_first() {
        let entries = sorted(commands());
        let found = titles(&entries, "SPLIT");
        let first = ["Split Down", "Split Left", "Split Right", "Split Tab Down", "Split Tab Right", "Split Up"];
        assert_eq!(found[..6], first);
        assert_eq!(found[6], "Focus Split: Down");
        for i in filter(&entries, "split") {
            let e = &entries[i];
            assert!(e.title.to_lowercase().contains("split") || e.action.contains("split"), "{}", e.title);
        }
        assert!(filter(&entries, "Xyzzy").is_empty());
    }

    #[test]
    fn words_match_in_any_order_and_as_starts_of_title_words() {
        let entries = sorted(commands());
        assert_eq!(titles(&entries, "right focus")[0], "Focus Split: Right");
        assert_eq!(titles(&entries, "fsr")[0], "Focus Split: Right");
        assert_eq!(titles(&entries, "sr")[0], "Split Right");
        assert_eq!(titles(&entries, "incfont")[0], "Increase Font Size");
        // Not a subsequence: "ft" is no start of a word of Increase Font Size.
        assert!(!titles(&entries, "ft").contains(&"Increase Font Size".to_owned()));
    }

    #[test]
    fn actions_match_after_titles() {
        let entries = sorted(commands());
        assert_eq!(titles(&entries, "kill-pane"), ["Close Terminal"]);
        assert_eq!(titles(&entries, "new_split"), ["Split Left", "Split Up"]);
        // Equal ranks keep the alphabetical order.
        assert_eq!(titles(&entries, "select-layout")[0], "Select Layout: Even Horizontal");
    }

    #[test]
    fn word_starts_take_each_word_once_in_order() {
        let words: Vec<Vec<char>> = ["focus", "split", "right"].iter().map(|w| w.chars().collect()).collect();
        let starts = |s: &str| word_starts(&s.chars().collect::<Vec<_>>(), &words);
        assert!(starts("fsr"));
        assert!(starts("focspr"));
        assert!(starts("sr"));
        assert!(!starts("rs"));
        assert!(!starts("ff"));
        assert!(!starts("fx"));
    }
}
