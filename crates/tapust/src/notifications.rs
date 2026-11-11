//! ghostty's desktop_notifications.zig: sends and withdraws the desktop
//! notifications of one surface.

use std::cell::RefCell;
use std::hash::Hasher;
use std::rc::Rc;
use std::time::Duration;

use gtk::prelude::*;
use gtk::{gio, glib};

use crate::theme;

/// Notifications kept per surface, since GIO does not report dismissals.
const LIMIT: usize = 64;
/// Notifications sent while the surface is focused expire after this.
const FOCUSED_TIMEOUT: Duration = Duration::from_secs(3);

struct Entry {
    hash: u64,
    timeout: Option<glib::SourceId>,
}

pub struct DesktopNotifications {
    surface_id: u64,
    entries: RefCell<Vec<Entry>>,
}

/// ghostty's notificationHash: title and body as one byte string (ghostty
/// uses Wyhash; any hash keeps the same identities).
pub fn hash(title: &str, body: &str) -> u64 {
    let mut hasher = std::hash::DefaultHasher::new();
    hasher.write(title.as_bytes());
    hasher.write(body.as_bytes());
    hasher.finish()
}

/// ghostty's formatId: the same content from another surface stays separate.
fn id(surface_id: u64, hash: u64) -> String {
    format!("tapust-surface-{surface_id:x}-{hash:x}")
}

impl DesktopNotifications {
    pub fn new(surface_id: u64) -> Rc<DesktopNotifications> {
        Rc::new(DesktopNotifications { surface_id, entries: RefCell::new(Vec::new()) })
    }

    /// Sends a notification, replacing one with the same title and body.
    pub fn send(self: &Rc<Self>, focused: bool, title: &str, body: &str) {
        let Some(app) = gio::Application::default() else { return };
        let notification = gio::Notification::new(if title.is_empty() { "tapust" } else { title });
        notification.set_body(Some(body));
        notification.set_icon(&gio::ThemedIcon::new(theme::ICON));
        notification.set_default_action_and_target_value("app.present-surface", Some(&self.surface_id.to_variant()));
        let hash = hash(title, body);
        if let Some(evicted) = self.track(hash) {
            self.clear_removed(evicted);
        }
        app.send_notification(Some(&id(self.surface_id, hash)), &notification);
        if focused {
            self.schedule_timeout(hash);
        }
    }

    /// Withdraws every notification and cancels their timers.
    pub fn clear(&self) {
        let entries = self.entries.take();
        for entry in entries {
            self.clear_removed(entry);
        }
    }

    /// Adds `hash` as the newest entry; returns the oldest when it no
    /// longer fits.
    fn track(&self, hash: u64) -> Option<Entry> {
        if let Some(removed) = self.remove(hash) {
            if let Some(timeout) = removed.timeout {
                timeout.remove();
            }
            self.entries.borrow_mut().push(Entry { hash, timeout: None });
            return None;
        }
        let mut entries = self.entries.borrow_mut();
        let evicted = (entries.len() >= LIMIT).then(|| entries.remove(0));
        entries.push(Entry { hash, timeout: None });
        evicted
    }

    /// Takes the entry out without keeping the others' order.
    fn remove(&self, hash: u64) -> Option<Entry> {
        let mut entries = self.entries.borrow_mut();
        let i = entries.iter().position(|e| e.hash == hash)?;
        Some(entries.swap_remove(i))
    }

    fn clear_removed(&self, removed: Entry) {
        if let Some(timeout) = removed.timeout {
            timeout.remove();
        }
        self.withdraw(removed.hash);
    }

    fn withdraw(&self, hash: u64) {
        if let Some(app) = gio::Application::default() {
            app.withdraw_notification(&id(self.surface_id, hash));
        }
    }

    fn schedule_timeout(self: &Rc<Self>, hash: u64) {
        let weak = Rc::downgrade(self);
        let source = glib::timeout_add_local_once(FOCUSED_TIMEOUT, move || {
            let Some(n) = weak.upgrade() else { return };
            // This source is the entry's timeout and ends by itself.
            if let Some(removed) = n.remove(hash) {
                n.withdraw(removed.hash);
            }
        });
        if let Some(entry) = self.entries.borrow_mut().iter_mut().find(|e| e.hash == hash) {
            entry.timeout = Some(source);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashes_follow_title_and_body_bytes() {
        assert_eq!(hash("Title", "Body"), hash("Title", "Body"));
        assert_ne!(hash("Title", "Body"), hash("Other", "Body"));
        assert_ne!(hash("Title", "Body"), hash("Title", "Other"));
        // Title and body are hashed as concatenated bytes.
        assert_eq!(hash("ab", "c"), hash("a", "bc"));
        assert_ne!(id(1, 5), id(2, 5));
    }

    #[test]
    fn replacing_moves_to_the_end_and_the_oldest_is_evicted() {
        let n = DesktopNotifications::new(1);
        for h in 0..LIMIT as u64 {
            assert!(n.track(h).is_none());
        }
        assert!(n.track(0).is_none());
        let hashes = |n: &DesktopNotifications| n.entries.borrow().iter().map(|e| e.hash).collect::<Vec<_>>();
        // The last entry took the replaced one's place.
        assert_eq!(hashes(&n)[0], LIMIT as u64 - 1);
        assert_eq!(hashes(&n)[LIMIT - 1], 0);
        assert_eq!(n.track(100).map(|e| e.hash), Some(LIMIT as u64 - 1));
        assert_eq!(hashes(&n).len(), LIMIT);
        assert!(n.remove(100).is_some() && n.remove(100).is_none());
    }
}
