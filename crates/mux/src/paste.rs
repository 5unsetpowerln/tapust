//! Paste buffers, ported from tmux 3.6 paste.c, and what paste-buffer
//! writes (cmd-paste-buffer.c).

/// The `buffer-limit` option.
pub(crate) const BUFFER_LIMIT: usize = 50;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Buffer {
    pub name: String,
    pub data: Vec<u8>,
    /// Made by paste_add (a copy) rather than named by paste_set.
    pub automatic: bool,
    /// paste_next_order when it was made; the newest is the largest.
    pub order: u32,
}

/// The buffers, oldest first (paste_by_time walked backwards).
#[derive(Debug, Default)]
pub(crate) struct Buffers {
    list: Vec<Buffer>,
    next_index: u32,
    next_order: u32,
}

impl Buffers {
    fn get_name(&self, name: &str) -> Option<usize> {
        self.list.iter().position(|b| b.name == name)
    }

    /// paste_get_top: the newest automatic buffer.
    pub fn top(&self) -> Option<&Buffer> {
        self.list.iter().rev().find(|b| b.automatic)
    }

    /// paste_free of the buffer named `name`.
    pub fn free(&mut self, name: &str) {
        if let Some(i) = self.get_name(name) {
            self.list.remove(i);
        }
    }

    /// paste_add: a new automatic buffer named `buffer<N>`, freeing the
    /// oldest automatic buffers beyond buffer-limit. Empty data adds nothing.
    pub fn add(&mut self, data: Vec<u8>) {
        if data.is_empty() {
            return;
        }
        let mut automatic = self.list.iter().filter(|b| b.automatic).count();
        let mut i = 0;
        while i < self.list.len() && automatic >= BUFFER_LIMIT {
            if self.list[i].automatic {
                self.list.remove(i);
                automatic -= 1;
            } else {
                i += 1;
            }
        }
        let name = loop {
            let name = format!("buffer{}", self.next_index);
            self.next_index += 1;
            if self.get_name(&name).is_none() {
                break name;
            }
        };
        let order = self.next_order;
        self.next_order += 1;
        self.list.push(Buffer { name, data, automatic: true, order });
    }

    /// paste_set with a name: a buffer that is not automatic, replacing the
    /// one of that name. Empty data sets nothing.
    pub fn set(&mut self, data: Vec<u8>, name: &str) {
        if data.is_empty() {
            return;
        }
        let order = self.next_order;
        self.next_order += 1;
        self.free(name);
        self.list.push(Buffer { name: name.to_owned(), data, automatic: false, order });
    }

    /// window_copy_append_selection: `data` after the newest automatic
    /// buffer, set under its name (so no longer automatic), or a new buffer.
    pub fn append(&mut self, data: Vec<u8>) {
        match self.top() {
            Some(top) => {
                let name = top.name.clone();
                let mut joined = top.data.clone();
                joined.extend_from_slice(&data);
                self.set(joined, &name);
            }
            None => self.add(data),
        }
    }

    #[cfg(test)]
    pub fn names(&self) -> Vec<&str> {
        self.list.iter().map(|b| b.name.as_str()).collect()
    }
}

/// cmd_paste_buffer_exec without `-s` or `-r`: every line feed of the
/// buffer becomes the separator, a carriage return.
pub(crate) fn paste_data(data: &[u8]) -> Vec<u8> {
    data.iter().map(|&b| if b == b'\n' { b'\r' } else { b }).collect()
}
