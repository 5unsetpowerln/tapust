//! The layout file: a running tapust's windows, tabs and split trees with
//! each pane's working directory, which another tapust restores
//! (`--save-layout`, `--load-layout`). The programs in the panes are not
//! kept.
//!
//! One item per line, the trees in preorder:
//!
//! ```text
//! tapust-layout 1
//! window
//! tab
//! split horizontal 0.5
//! pane /home/me/src
//! pane /tmp
//! tab
//! pane
//! ```
//!
//! `split` is followed by its first and second subtree; `pane` without a
//! directory is a pane whose directory was not known. A directory's `%` and
//! newline bytes are written as `%25` and `%0A`, as in OSC 7's URLs.

use std::ffi::OsString;
use std::io::Write;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};

use mux::{Node, PaneId, Split};

const HEADER: &[u8] = b"tapust-layout 1";

#[derive(Clone, Debug, PartialEq)]
pub struct Layout {
    /// The GTK windows, each with its tabs in order.
    pub windows: Vec<Vec<Tree>>,
}

/// A tab's split tree.
#[derive(Clone, Debug, PartialEq)]
pub enum Tree {
    /// A pane, in its working directory when that was known.
    Pane(Option<PathBuf>),
    Split { horizontal: bool, ratio: f64, first: Box<Tree>, second: Box<Tree> },
}

impl Tree {
    /// The tree of `node`, with the directory `cwd` knows for each pane.
    pub fn of(node: &Node, cwd: &impl Fn(PaneId) -> Option<PathBuf>) -> Tree {
        match node {
            Node::Leaf(pane) => Tree::Pane(cwd(*pane)),
            Node::Split(split) => Tree::Split {
                horizontal: split.horizontal,
                ratio: split.ratio,
                first: Box::new(Tree::of(&split.first, cwd)),
                second: Box::new(Tree::of(&split.second, cwd)),
            },
        }
    }

    /// The split tree of mux for restoring this one, its leaves numbered
    /// in order from the length of `dirs`, to which their directories are
    /// added.
    pub fn to_node(&self, dirs: &mut Vec<Option<PathBuf>>) -> Node {
        match self {
            Tree::Pane(dir) => {
                dirs.push(dir.clone());
                Node::Leaf(dirs.len() as PaneId - 1)
            }
            Tree::Split { horizontal, ratio, first, second } => Node::Split(Box::new(Split {
                horizontal: *horizontal,
                ratio: *ratio,
                first: first.to_node(dirs),
                second: second.to_node(dirs),
            })),
        }
    }

    fn write(&self, out: &mut Vec<u8>) {
        match self {
            Tree::Pane(dir) => {
                out.extend_from_slice(b"pane");
                if let Some(dir) = dir {
                    out.push(b' ');
                    encode(dir.as_os_str().as_bytes(), out);
                }
                out.push(b'\n');
            }
            Tree::Split { horizontal, ratio, first, second } => {
                let orientation = if *horizontal { "horizontal" } else { "vertical" };
                out.extend_from_slice(format!("split {orientation} {ratio}\n").as_bytes());
                first.write(out);
                second.write(out);
            }
        }
    }
}

impl Layout {
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = HEADER.to_vec();
        out.push(b'\n');
        for tabs in &self.windows {
            out.extend_from_slice(b"window\n");
            for tree in tabs {
                out.extend_from_slice(b"tab\n");
                tree.write(&mut out);
            }
        }
        out
    }

    pub fn parse(bytes: &[u8]) -> Result<Layout, String> {
        let bytes = bytes.strip_suffix(b"\n").unwrap_or(bytes);
        let mut lines = Lines { lines: bytes.split(|&b| b == b'\n').collect(), next: 0 };
        match lines.next() {
            Some((_, HEADER)) => {}
            _ => return Err("not a tapust layout file".to_owned()),
        }
        let mut windows = Vec::new();
        while let Some((n, line)) = lines.next() {
            if line != b"window" {
                return Err(at(n, "a window was expected"));
            }
            let mut tabs = Vec::new();
            while lines.peek() == Some(&b"tab"[..]) {
                lines.next();
                tabs.push(parse_tree(&mut lines)?);
            }
            if tabs.is_empty() {
                return Err(at(n, "the window has no tab"));
            }
            windows.push(tabs);
        }
        if windows.is_empty() {
            return Err("the layout has no window".to_owned());
        }
        Ok(Layout { windows })
    }

    pub fn read(path: &Path) -> Result<Layout, String> {
        let bytes = std::fs::read(path).map_err(|err| format!("cannot read {}: {err}", path.display()))?;
        Layout::parse(&bytes).map_err(|err| format!("{}: {err}", path.display()))
    }

    /// Writes the file whole: a reader never sees part of it.
    pub fn write(&self, path: &Path) -> Result<(), String> {
        let mut temporary = path.as_os_str().to_owned();
        temporary.push(format!(".{}.tmp", std::process::id()));
        let written = std::fs::File::create(&temporary)
            .and_then(|mut file| file.write_all(&self.to_bytes()))
            .and_then(|()| std::fs::rename(&temporary, path));
        written.map_err(|err| {
            let _ = std::fs::remove_file(&temporary);
            format!("cannot write {}: {err}", path.display())
        })
    }
}

struct Lines<'a> {
    lines: Vec<&'a [u8]>,
    next: usize,
}

impl<'a> Lines<'a> {
    /// The next line with its number, counted from 1.
    fn next(&mut self) -> Option<(usize, &'a [u8])> {
        let line = self.lines.get(self.next)?;
        self.next += 1;
        Some((self.next, line))
    }

    fn peek(&self) -> Option<&'a [u8]> {
        self.lines.get(self.next).copied()
    }
}

fn at(line: usize, message: &str) -> String {
    format!("line {line}: {message}")
}

fn parse_tree(lines: &mut Lines) -> Result<Tree, String> {
    let Some((n, line)) = lines.next() else { return Err("the layout ends in a tab's tree".to_owned()) };
    if line == b"pane" {
        return Ok(Tree::Pane(None));
    }
    if let Some(dir) = line.strip_prefix(b"pane ") {
        let dir = decode(dir).ok_or_else(|| at(n, "a bad % escape in the directory"))?;
        return Ok(Tree::Pane(Some(PathBuf::from(OsString::from_vec(dir)))));
    }
    let Some(split) = line.strip_prefix(b"split ") else { return Err(at(n, "a pane or split was expected")) };
    let split = std::str::from_utf8(split).map_err(|_| at(n, "a bad split"))?;
    let horizontal = match split.split_once(' ') {
        Some(("horizontal", _)) => true,
        Some(("vertical", _)) => false,
        _ => return Err(at(n, "a split is horizontal or vertical")),
    };
    let ratio = split.split_once(' ').and_then(|(_, r)| r.parse::<f64>().ok()).filter(|r| (0.0..=1.0).contains(r));
    let ratio = ratio.ok_or_else(|| at(n, "a split's ratio is a number from 0 to 1"))?;
    let first = Box::new(parse_tree(lines)?);
    let second = Box::new(parse_tree(lines)?);
    Ok(Tree::Split { horizontal, ratio, first, second })
}

pub(crate) fn encode(bytes: &[u8], out: &mut Vec<u8>) {
    for &b in bytes {
        match b {
            b'%' => out.extend_from_slice(b"%25"),
            b'\n' => out.extend_from_slice(b"%0A"),
            b => out.push(b),
        }
    }
}

pub(crate) fn decode(bytes: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(bytes.len());
    let mut rest = bytes;
    while let Some((&b, tail)) = rest.split_first() {
        if b == b'%' {
            let hex = std::str::from_utf8(tail.get(..2)?).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            rest = &tail[2..];
        } else {
            out.push(b);
            rest = tail;
        }
    }
    Some(out)
}
