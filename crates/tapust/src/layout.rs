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

#[cfg(test)]
mod tests {
    use super::*;

    fn pane(dir: &str) -> Tree {
        Tree::Pane(Some(PathBuf::from(dir)))
    }

    fn split(horizontal: bool, ratio: f64, first: Tree, second: Tree) -> Tree {
        Tree::Split { horizontal, ratio, first: Box::new(first), second: Box::new(second) }
    }

    #[test]
    fn layouts_are_read_back_as_written() {
        let layout = Layout {
            windows: vec![
                vec![
                    split(true, 0.25, pane("/home/me/src"), split(false, 0.333251953125, pane("/tmp"), Tree::Pane(None))),
                    pane("/home/me/a b"),
                ],
                vec![pane("/odd/100%\ndir")],
            ],
        };
        let bytes = layout.to_bytes();
        let text = "tapust-layout 1\nwindow\ntab\nsplit horizontal 0.25\npane /home/me/src\n\
                    split vertical 0.333251953125\npane /tmp\npane\ntab\npane /home/me/a b\n\
                    window\ntab\npane /odd/100%25%0Adir\n";
        assert_eq!(String::from_utf8(bytes.clone()).unwrap(), text);
        assert_eq!(Layout::parse(&bytes), Ok(layout));
    }

    #[test]
    fn bad_layouts_say_where() {
        let parse = |text: &str| Layout::parse(text.as_bytes());
        assert_eq!(parse("tapust-layout 2\nwindow\ntab\npane\n"), Err("not a tapust layout file".to_owned()));
        assert_eq!(parse("tapust-layout 1\n"), Err("the layout has no window".to_owned()));
        assert_eq!(parse("tapust-layout 1\ntab\npane\n"), Err("line 2: a window was expected".to_owned()));
        assert_eq!(parse("tapust-layout 1\nwindow\nwindow\n"), Err("line 2: the window has no tab".to_owned()));
        assert_eq!(parse("tapust-layout 1\nwindow\ntab\nsplit diagonal 0.5\n").unwrap_err(), "line 4: a split is horizontal or vertical");
        assert_eq!(
            parse("tapust-layout 1\nwindow\ntab\nsplit vertical 1.5\npane\npane\n").unwrap_err(),
            "line 4: a split's ratio is a number from 0 to 1"
        );
        assert_eq!(parse("tapust-layout 1\nwindow\ntab\nsplit vertical 0.5\npane\n").unwrap_err(), "the layout ends in a tab's tree");
        assert_eq!(parse("tapust-layout 1\nwindow\ntab\nwindow\n").unwrap_err(), "line 4: a pane or split was expected");
        assert_eq!(parse("tapust-layout 1\nwindow\ntab\npane /a%2\n").unwrap_err(), "line 4: a bad % escape in the directory");
    }

    /// A tab's tree goes to mux with its leaves numbered in order and back
    /// with the directories of the panes mux made.
    #[test]
    fn trees_go_to_mux_and_back() {
        let tree = split(false, 0.5, pane("/a"), split(true, 0.75, Tree::Pane(None), pane("/c")));
        let mut dirs = vec![Some(PathBuf::from("/before"))];
        let node = tree.to_node(&mut dirs);
        let leaf = Node::Leaf;
        let mux_split = |horizontal, ratio, first, second| Node::Split(Box::new(Split { horizontal, ratio, first, second }));
        assert_eq!(node, mux_split(false, 0.5, leaf(1), mux_split(true, 0.75, leaf(2), leaf(3))));
        assert_eq!(dirs, [Some("/before".into()), Some("/a".into()), None, Some("/c".into())]);
        let back = Tree::of(&node, &|pane| dirs[pane as usize].clone());
        assert_eq!(back, tree);
    }

    #[test]
    fn layouts_are_written_whole() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/test-layout-write");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("layout");
        let layout = Layout { windows: vec![vec![pane("/x")]] };
        layout.write(&file).unwrap();
        assert_eq!(Layout::read(&file), Ok(layout));
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1, "no temporary file is left");
        assert!(Layout::read(&dir.join("missing")).unwrap_err().starts_with("cannot read "));
    }
}
