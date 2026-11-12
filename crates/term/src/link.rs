//! The per-terminal table of OSC 8 hyperlinks that cells reference by id.

use std::collections::HashMap;

/// The longest URI kept, as ghostty's OSC 8 buffer.
pub(crate) const MAX_URI: usize = 2048;
/// The longest id kept, as VTE's.
pub(crate) const MAX_ID: usize = 250;
/// The most bytes of URIs and ids held.
const MAX_BYTES: usize = 16 << 20;
/// The bytes held before the first sweep.
const SWEEP_MIN: usize = 1 << 20;
/// New links asked for between two sweeps while the table is full, so that
/// a table full of links still in use costs one sweep per batch of links
/// it refuses rather than one per link.
const SWEEP_BATCH: usize = 4096;

/// Hyperlinks by id; id 0 is no link. A start with an id shares the link
/// of an earlier start with the same id and URI; a start without one
/// makes a new link. Nothing is freed while printing: the terminal sweeps
/// the ids no cell uses once the held bytes pass twice those left by the
/// last sweep (and 1 MiB), or when the ids or bytes run out.
pub(crate) struct LinkTable {
    /// The URI of each id in use.
    uris: Vec<Option<Box<str>>>,
    free: Vec<u16>,
    /// Links started with an id, by id and URI.
    named: HashMap<(Box<[u8]>, Box<str>), u16>,
    /// Bytes of the URIs and ids held, in `uris` and `named`.
    bytes: usize,
    /// `bytes` past which a new link sweeps first.
    sweep_at: usize,
    /// New links asked for since the last sweep.
    asked: usize,
}

/// The bytes a link holds: its URI, and a named link's id and URI again as
/// its key in `named`.
fn size(uri: &str, id: Option<&[u8]>) -> usize {
    uri.len() + id.map_or(0, |id| id.len() + uri.len())
}

impl LinkTable {
    pub fn new() -> LinkTable {
        LinkTable {
            uris: vec![None],
            free: Vec::new(),
            named: HashMap::new(),
            bytes: 0,
            sweep_at: SWEEP_MIN,
            asked: 0,
        }
    }

    pub fn uri(&self, id: u16) -> Option<&str> {
        self.uris.get(id as usize)?.as_deref()
    }

    /// One more than the largest id.
    pub fn len(&self) -> usize {
        self.uris.len()
    }

    /// The link of an earlier start with `id` and `uri`.
    pub fn find(&self, uri: &str, id: Option<&[u8]>) -> Option<u16> {
        self.named.get(&(Box::from(id?), Box::from(uri))).copied()
    }

    fn full(&self, size: usize) -> bool {
        (self.free.is_empty() && self.uris.len() > u16::MAX as usize) || self.bytes + size > MAX_BYTES
    }

    /// Whether to sweep before adding a link to `uri` with `id`.
    pub fn wants_sweep(&self, uri: &str, id: Option<&[u8]>) -> bool {
        let size = size(uri, id);
        if self.full(size) { self.asked >= SWEEP_BATCH } else { self.bytes + size > self.sweep_at }
    }

    /// Adds a link to `uri` with `id`; 0 when the table is full.
    pub fn insert(&mut self, uri: &str, id: Option<&[u8]>) -> u16 {
        self.asked += 1;
        let size = size(uri, id);
        if self.full(size) {
            return 0;
        }
        let n = self.free.pop().unwrap_or_else(|| {
            self.uris.push(None);
            (self.uris.len() - 1) as u16
        });
        self.uris[n as usize] = Some(uri.into());
        if let Some(id) = id {
            self.named.insert((id.into(), uri.into()), n);
        }
        self.bytes += size;
        n
    }

    /// Frees the ids not marked in `live`.
    pub fn sweep(&mut self, live: &[bool]) {
        for (n, uri) in self.uris.iter_mut().enumerate().skip(1) {
            if !live[n] && let Some(uri) = uri.take() {
                self.bytes -= uri.len();
                self.free.push(n as u16);
            }
        }
        let bytes = &mut self.bytes;
        self.named.retain(|(id, uri), n| {
            let keep = live[*n as usize];
            if !keep {
                *bytes -= id.len() + uri.len();
            }
            keep
        });
        self.sweep_at = (2 * self.bytes).max(SWEEP_MIN);
        self.asked = 0;
    }

    /// The links held.
    #[cfg(test)]
    pub fn held(&self) -> usize {
        self.uris.iter().filter(|u| u.is_some()).count()
    }
}
