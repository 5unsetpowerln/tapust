//! A surface's scrollback search: ghostty's search overlay
//! (search-overlay.blp) and which of the query's matches is current.

use std::time::Instant;

use adw::prelude::*;
use gtk::{gdk, glib};
use term::{Selection, Terminal};

use crate::theme;

/// ghostty's default start_search trigger, ctrl+shift+f.
pub fn is_start_search(keyval: gdk::Key, state: gdk::ModifierType) -> bool {
    use gdk::ModifierType as M;
    let held = state & (M::CONTROL_MASK | M::SHIFT_MASK | M::ALT_MASK | M::SUPER_MASK | M::HYPER_MASK | M::META_MASK);
    held == M::CONTROL_MASK | M::SHIFT_MASK && keyval.to_lower() == gdk::Key::f
}

/// ghostty's GhosttySearchOverlay: an entry, the match count and buttons at
/// the top right of the surface, shown while a search is open.
pub struct Overlay {
    pub bin: adw::Bin,
    pub entry: gtk::SearchEntry,
    pub count: gtk::Label,
    /// Goes up to the next older match (ghostty's prev_button, which emits
    /// next-match).
    pub up: gtk::Button,
    pub down: gtk::Button,
    pub close: gtk::Button,
}

impl Overlay {
    pub fn new() -> Overlay {
        let entry = gtk::SearchEntry::builder().placeholder_text("Find\u{2026}").width_chars(20).hexpand(true).build();
        let count = theme::styled(gtk::Label::builder().width_chars(6).xalign(1.0).build(), &["dim-label"]);
        let button = |icon: &str, tooltip: &str| {
            let button = gtk::Button::builder().icon_name(icon).tooltip_text(tooltip).build();
            button.set_cursor_from_name(Some("pointer"));
            button
        };
        let (up, down) = (button("go-up-symbolic", "Previous Match"), button("go-down-symbolic", "Next Match"));
        let close = button("window-close-symbolic", "Close");
        let buttons = theme::styled(
            gtk::Box::builder().orientation(gtk::Orientation::Horizontal).spacing(1).build(),
            &["linked"],
        );
        buttons.append(&up);
        buttons.append(&down);
        let container = theme::styled(
            gtk::Box::builder().orientation(gtk::Orientation::Horizontal).spacing(6).build(),
            &["background", "search-overlay"],
        );
        container.append(&entry);
        container.append(&count);
        container.append(&buttons);
        container.append(&close);
        let bin = adw::Bin::builder()
            .visible(false)
            .halign(gtk::Align::End)
            .valign(gtk::Align::Start)
            .child(&adw::Bin::builder().child(&container).build())
            .build();
        Overlay { bin, entry, count, up, down, close }
    }

    pub fn is_open(&self) -> bool {
        self.bin.is_visible()
    }
}

/// The search's state: the query searched for, its matches and the current one.
#[derive(Default)]
pub struct Found {
    /// The query last searched for; empty while nothing is searched for.
    pub query: String,
    /// Top to bottom, as `Terminal::search` returns them.
    pub matches: Vec<Selection>,
    pub current: Option<usize>,
    /// The search entry had the focus last, so the surface gives it back.
    pub focused: bool,
    /// When the last search ran, and the timer that runs it again after output.
    pub ran: Option<Instant>,
    pub timer: Option<glib::SourceId>,
}

impl Found {
    /// Takes the matches of `query`; returns whether the query changed. The
    /// current match stays on the same cells when the query is the same and
    /// they still match; otherwise it becomes the newest match that starts at
    /// or above line `bottom` (the viewport's last), or the newest of all when
    /// every match is below it (searching up from there wraps around).
    pub fn update(&mut self, query: &str, matches: Vec<Selection>, bottom: u64) -> bool {
        let changed = query != self.query;
        let old = self.current_match().filter(|_| !changed);
        let kept = old.and_then(|old| {
            matches.binary_search_by(|m| m.start.cmp(&old.start)).ok().filter(|&i| matches[i] == old)
        });
        self.current = kept.or_else(|| match matches.partition_point(|m| m.start.line <= bottom) {
            0 => matches.len().checked_sub(1),
            n => Some(n - 1),
        });
        self.query = query.to_owned();
        self.matches = matches;
        changed
    }

    /// Moves to the next older match (up) or newer one, wrapping around.
    pub fn step(&mut self, older: bool) {
        let n = self.matches.len();
        self.current = self.current.map(|i| if older { (i + n - 1) % n } else { (i + 1) % n });
    }

    pub fn current_match(&self) -> Option<Selection> {
        self.current.map(|i| self.matches[i])
    }

    /// The count label: the current match's position from the oldest one
    /// and the total, nothing before a query is searched for.
    pub fn label(&self) -> String {
        match self.current {
            _ if self.query.is_empty() => String::new(),
            Some(i) => format!("{}/{}", i + 1, self.matches.len()),
            None => "0/0".to_owned(),
        }
    }
}

/// Scrolls the view so that `m` is visible: not at all when it is, else with
/// its first line near the middle. Returns whether the view moved.
pub fn reveal(term: &mut Terminal, m: Selection) -> bool {
    let (top, rows) = (term.view_top(), u64::from(term.rows()));
    if m.start.line >= top && m.end.line < top + rows {
        return false;
    }
    let target = m.start.line.saturating_sub(rows / 2);
    term.scroll_view(top as isize - target as isize);
    term.view_top() != top
}

#[cfg(test)]
mod tests {
    use super::*;
    use term::Point;

    fn span(start: (u64, u16), end: (u64, u16)) -> Selection {
        Selection { start: Point { line: start.0, col: start.1 }, end: Point { line: end.0, col: end.1 }, rect: false }
    }

    fn at(line: u64, col: u16, len: u16) -> Selection {
        span((line, col), (line, col + len - 1))
    }

    #[test]
    fn a_new_query_starts_at_the_newest_match_above_the_bottom() {
        let mut f = Found::default();
        let matches = vec![at(1, 0, 2), at(5, 3, 2), at(5, 8, 2), at(9, 0, 2)];
        assert!(f.update("ab", matches.clone(), 6));
        assert_eq!((f.current, f.label()), (Some(2), "3/4".to_owned()));
        // A match starting on the bottom line counts as above it.
        assert!(f.update("abc", matches.clone(), 9));
        assert_eq!(f.current, Some(3));
        // Every match below the view: searching up wraps to the newest.
        assert!(f.update("ab", matches, 0));
        assert_eq!(f.current, Some(3));
    }

    #[test]
    fn the_same_query_keeps_the_current_match_while_it_still_matches() {
        let mut f = Found::default();
        f.update("ab", vec![at(1, 0, 2), at(5, 3, 2), at(9, 0, 2)], 6);
        assert_eq!(f.current, Some(1));
        // New output added a match above and one below.
        assert!(!f.update("ab", vec![at(0, 0, 2), at(1, 0, 2), at(5, 3, 2), at(9, 0, 2), at(12, 0, 2)], 20));
        assert_eq!(f.current_match(), Some(at(5, 3, 2)));
        // The current match's text changed: back to the newest above the bottom.
        assert!(!f.update("ab", vec![at(1, 0, 2), at(9, 0, 2), at(12, 0, 2)], 10));
        assert_eq!(f.current, Some(1));
    }

    #[test]
    fn a_search_closed_and_reopened_starts_anew_with_the_same_query() {
        let mut f = Found::default();
        let matches = vec![at(1, 0, 2), at(5, 3, 2), at(9, 0, 2)];
        f.update("ab", matches.clone(), 20);
        f.step(true);
        assert_eq!(f.current, Some(1));
        // Closing forgets the search; the entry keeps "ab" and reopening runs it.
        f = Found::default();
        assert!(f.update("ab", matches, 20));
        assert_eq!(f.current, Some(2));
    }

    #[test]
    fn stepping_wraps_around_and_labels_count_from_the_oldest() {
        let mut f = Found::default();
        assert_eq!(f.label(), "");
        f.update("x", Vec::new(), 0);
        assert_eq!((f.current, f.label()), (None, "0/0".to_owned()));
        f.step(true);
        assert_eq!(f.current, None);
        f.update("ab", vec![at(1, 0, 2), at(2, 0, 2), at(3, 0, 2)], 1);
        assert_eq!(f.label(), "1/3");
        f.step(true);
        assert_eq!(f.label(), "3/3");
        f.step(false);
        assert_eq!(f.label(), "1/3");
        f.step(false);
        assert_eq!(f.label(), "2/3");
    }

    #[test]
    fn reveal_scrolls_only_to_a_match_out_of_view() {
        let mut t = Terminal::new(10, 4, 100);
        for i in 0..30 {
            t.feed(format!("{i}\r\n").as_bytes());
        }
        let top = t.view_top();
        assert!(!reveal(&mut t, at(top + 3, 0, 1)));
        assert_eq!(t.view_top(), top);
        // Above the view: its line goes to the middle (two rows down).
        assert!(reveal(&mut t, at(top - 10, 0, 1)));
        assert_eq!(t.view_top(), top - 12);
        // Below the view: the view cannot pass the bottom.
        assert!(reveal(&mut t, at(top + 3, 0, 1)));
        assert_eq!(t.view_top(), top);
        // A match crossing the bottom of the view is not visible.
        t.scroll_view(5);
        let top = t.view_top();
        assert!(reveal(&mut t, span((top + 3, 0), (top + 4, 0))));
        assert_eq!(t.view_top(), top + 1);
    }
}
