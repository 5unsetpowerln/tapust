//! The themes tapust ships: ghostty's theme collection (iTerm2-Color-Schemes,
//! MIT, see themes/LICENSE), embedded as the gzipped tar ghostty pins and
//! read when a theme is not among the user's own.

use std::io::Read;

use gtk::prelude::*;
use gtk::{gio, glib};

static ARCHIVE: &[u8] = include_bytes!("../themes/ghostty-themes.tgz");

/// The archive's directory of themes.
const DIR: &str = "ghostty/";

/// The file of the bundled theme `name`, if there is one.
pub fn bundled(name: &str) -> Option<Vec<u8>> {
    let tar = gunzip(ARCHIVE);
    files(&tar).find(|(path, _)| path.strip_prefix(DIR) == Some(name)).map(|(_, data)| data.to_vec())
}

/// The names of the bundled themes.
#[cfg(test)]
pub fn names() -> Vec<String> {
    let tar = gunzip(ARCHIVE);
    files(&tar).filter_map(|(path, _)| path.strip_prefix(DIR).map(str::to_owned)).collect()
}

fn gunzip(data: &'static [u8]) -> Vec<u8> {
    let input = gio::MemoryInputStream::from_bytes(&glib::Bytes::from_static(data));
    let decompressor = gio::ZlibDecompressor::new(gio::ZlibCompressorFormat::Gzip);
    let stream = gio::ConverterInputStream::new(&input, &decompressor);
    let mut tar = Vec::new();
    stream.into_read().read_to_end(&mut tar).expect("the bundled themes decompress");
    tar
}

/// The regular files of a tar archive (ustar or GNU headers), by name.
fn files(tar: &[u8]) -> impl Iterator<Item = (&str, &[u8])> {
    let mut at = 0;
    std::iter::from_fn(move || {
        loop {
            let header = tar.get(at..at + 512)?;
            if header.iter().all(|&b| b == 0) {
                return None;
            }
            let field = |range: std::ops::Range<usize>| {
                let bytes = &header[range];
                &bytes[..bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len())]
            };
            let size = std::str::from_utf8(field(124..136)).ok()?;
            let size = usize::from_str_radix(size.trim(), 8).ok()?;
            let data = tar.get(at + 512..at + 512 + size)?;
            at += 512 + size.div_ceil(512) * 512;
            if matches!(header[156], b'0' | 0) {
                return Some((std::str::from_utf8(field(0..100)).ok()?, data));
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_bundled_themes_are_ghosttys() {
        let names = names();
        assert_eq!(names.len(), 890);
        assert!(names.iter().all(|n| n.is_ascii() && !n.contains('/')), "{names:?}");
        let theme = bundled("Tomorrow Night").expect("Tomorrow Night");
        let text = String::from_utf8(theme).unwrap();
        assert!(text.contains("background = #1d1f21\n"), "{text}");
        assert_eq!(bundled("tomorrow night"), None);
        assert_eq!(bundled("Nope"), None);
    }
}
