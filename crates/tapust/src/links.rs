//! The links under the pointer: OSC 8 hyperlinks, and ghostty's default
//! link matcher: the URL and path regex of its config/url.zig, run with
//! Oniguruma (as ghostty does) over the logical line under the pointer.

use std::sync::OnceLock;

use onig::Regex;
use term::{LinkId, Point, Selection, Terminal, Width};

use crate::selection;

const URL_SCHEMES: &str =
    r"https?://|mailto:|ftp://|file:|ssh:|git://|ssh://|tel:|magnet:|ipfs://|ipns://|gemini://|gopher://|news:";
const IPV6_URL_PATTERN: &str = r"(?:\[[:0-9a-fA-F]+(?:[:0-9a-fA-F]*)+\](?::[0-9]+)?)";
const SCHEME_URL_CHARS: &str = r"[\w\-.~:/?#@!$&*+,;=%]";
const PATH_CHARS: &str = r"[\w\-.~:\/?#@!$&*+;=%]";
const OPTIONAL_BRACKETED_WORD_SUFFIX: &str = r"(?:[\(\[]\w*[\)\]])?";
const NO_TRAILING_PUNCTUATION: &str = r"(?<![,.])";
const NO_TRAILING_COLON: &str = r"(?<!:)";
const DOTTED_PATH_LOOKAHEAD: &str = r"(?=[\w\-.~:\/?#@!$&*+;=%]*\.)";
const NON_DOTTED_PATH_LOOKAHEAD: &str = r"(?![\w\-.~:\/?#@!$&*+;=%]*\.)";
const DOTTED_PATH_SPACE_SEGMENTS: &str = r"(?:(?<!:) (?!\w+:\/\/)(?!\.{0,2}\/)(?!~\/)[\w\-.~:\/?#@!$&*+;=%]*[\/.])*";
const ANY_PATH_SPACE_SEGMENTS: &str = r"(?:(?<!:) (?!\w+:\/\/)(?!\.{0,2}\/)(?!~\/)[\w\-.~:\/?#@!$&*+;=%]+)*";
const ROOTED_OR_RELATIVE_PATH_PREFIX: &str =
    r"(?:\.\.\/|\.\/|(?<!\w)~\/|(?:[\w][\w\-.]*\/)*(?<!\w)\$[A-Za-z_]\w*\/|\.[\w][\w\-.]*\/|(?<![\w~\/])\/(?!\/))";
const BARE_RELATIVE_PATH_PREFIX: &str = r"(?<!\$\d*)(?<!\w)[\w][\w\-.]*\/";

fn url_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| {
        let scheme_url_branch = format!(
            "(?:{URL_SCHEMES})(?:{IPV6_URL_PATTERN}|{SCHEME_URL_CHARS}+{OPTIONAL_BRACKETED_WORD_SUFFIX})+{NO_TRAILING_PUNCTUATION}"
        );
        let rooted_or_relative_path_branch = format!(
            "{ROOTED_OR_RELATIVE_PATH_PREFIX}(?:{DOTTED_PATH_LOOKAHEAD}{PATH_CHARS}+{DOTTED_PATH_SPACE_SEGMENTS}{NO_TRAILING_COLON}|{NON_DOTTED_PATH_LOOKAHEAD}{PATH_CHARS}+{ANY_PATH_SPACE_SEGMENTS}{NO_TRAILING_COLON})"
        );
        let bare_relative_path_branch =
            format!("{DOTTED_PATH_LOOKAHEAD}{BARE_RELATIVE_PATH_PREFIX}{PATH_CHARS}+{NO_TRAILING_COLON}");
        Regex::new(&format!("{scheme_url_branch}|{rooted_or_relative_path_branch}|{bare_relative_path_branch}"))
            .expect("ghostty's URL regex")
    })
}

/// A link under the pointer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Link {
    /// The link's cells around the pointer: a match, or the run of a
    /// hyperlink's cells.
    pub cells: Selection,
    /// The hyperlink, all of whose visible cells are underlined.
    pub id: Option<LinkId>,
    /// What opening the link opens and the URL label shows: a hyperlink's
    /// URI, or the matched text.
    pub target: String,
}

/// ghostty's linkAtPos: the OSC 8 hyperlink of the cell at `p`, or else the
/// URL regex match there.
pub fn link_at(term: &Terminal, p: Point) -> Option<Link> {
    let Some(id) = term.line(p.line)?[p.col as usize].link() else {
        return url_at(term, p).map(|(cells, target)| Link { cells, id: None, target });
    };
    let same = |q: Point| term.line(q.line).is_some_and(|row| row[q.col as usize].link() == Some(id));
    Some(Link { cells: selection::run(term, p, same), id: Some(id), target: term.link_uri(id)?.to_owned() })
}

/// ghostty's linkAtPin for the default URL matcher: the match on the
/// logical line under `p` (Screen.selectLine) that covers `p`, and its text.
pub fn url_at(term: &Terminal, p: Point) -> Option<(Selection, String)> {
    let line = selection::line(term, p, true)?;
    // The line's text with the cell each byte came from.
    let mut text = String::new();
    let mut cells: Vec<Point> = Vec::new();
    let mut q = line.start;
    loop {
        if let Some(row) = term.line(q.line) {
            let cell = row[q.col as usize];
            if !matches!(cell.width(), Width::SpacerTail | Width::SpacerHead) {
                let start = text.len();
                match (term.line_grapheme(q.line, q.col), cell.char().filter(|&c| c != '\0')) {
                    (Some(cluster), _) => text.push_str(cluster),
                    (None, Some(c)) => text.push(c),
                    (None, None) => text.push(' '),
                }
                cells.resize(text.len(), q);
                debug_assert!(cells.len() > start);
            }
        }
        if q == line.end {
            break;
        }
        q = if q.col + 1 < term.cols() { Point { line: q.line, col: q.col + 1 } } else { Point { line: q.line + 1, col: 0 } };
    }
    for (start, end) in url_regex().find_iter(&text) {
        if end == 0 {
            continue;
        }
        let (first, last) = (cells[start], cells[end - 1]);
        if first <= p && p <= last {
            return Some((Selection { start: first, end: last, rect: false }, text[start..end].to_owned()));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Cases from ghostty's url.zig test: the first match of each input.
    #[test]
    fn regex_matches_ghostty_cases() {
        let cases = [
            ("hello https://example.com world", "https://example.com"),
            ("https://example.com/foo(bar) more", "https://example.com/foo(bar)"),
            ("https://example.com/foo(bar)baz more", "https://example.com/foo(bar)baz"),
            ("Link inside (https://example.com) parens", "https://example.com"),
            ("Link period https://example.com. More text.", "https://example.com"),
            ("Link trailing comma https://example.com, more text.", "https://example.com"),
            ("Link in single quotes 'https://example.com' and more", "https://example.com"),
            ("match with query url https://example.com?query=1&other=2 and more text.", "https://example.com?query=1&other=2"),
            ("dot.http://example.com", "http://example.com"),
            ("square brackets https://example.com/[foo] and more", "https://example.com/[foo]"),
            ("/tmp/test.txt http://www.google.com", "/tmp/test.txt"),
            ("match tel:+18005551234 tel links", "tel:+18005551234"),
            ("/Users/ghostty.user/code/../example.py hello world", "/Users/ghostty.user/code/../example.py"),
            ("first time ../example.py contributor ", "../example.py"),
            ("[link](/home/user/ghostty.user/example)", "/home/user/ghostty.user/example"),
            ("Serving HTTP on :: port 8000 (http://[::]:8000/)", "http://[::]:8000/"),
            ("IPv6 address https://[2001:db8::1]:8080/path", "https://[2001:db8::1]:8080/path"),
        ];
        for (input, expect) in cases {
            let (start, end) = url_regex().find(input).unwrap_or_else(|| panic!("no match in {input:?}"));
            assert_eq!(&input[start..end], expect, "{input:?}");
        }
    }

    #[test]
    fn url_at_finds_the_url_under_the_pointer() {
        let mut t = Terminal::new(20, 3, 10);
        t.feed(b"see https://example.com/abc now");
        let top = t.view_top();
        let (sel, text) = url_at(&t, Point { line: top, col: 6 }).unwrap();
        assert_eq!(text, "https://example.com/abc");
        assert_eq!((sel.start, sel.end), (Point { line: top, col: 4 }, Point { line: top + 1, col: 6 }));
        assert!(url_at(&t, Point { line: top, col: 1 }).is_none());
    }

    #[test]
    fn a_hyperlink_comes_before_a_url_and_shows_its_uri() {
        let mut t = Terminal::new(10, 4, 10);
        // A hyperlink over text that looks like a URL, soft-wrapped onto the
        // next row, then the same link again after unlinked text.
        t.feed(b"\x1b]8;id=a;gemini://a/b\x1b\\x https://c\x1b]8;;\x1b\\ y\r\n");
        t.feed(b"\x1b]8;id=a;gemini://a/b\x1b\\z\x1b]8;;\x1b\\ https://d");
        let top = t.view_top();
        let at = |line: u64, col: u16| link_at(&t, Point { line: top + line, col });
        let link = at(0, 5).unwrap();
        assert_eq!(link.target, "gemini://a/b");
        assert_eq!((link.cells.start, link.cells.end), (Point { line: top, col: 0 }, Point { line: top + 1, col: 0 }));
        assert_eq!(at(1, 0), Some(link.clone()));
        // The same link elsewhere: the same id, its own cells.
        let other = at(2, 0).unwrap();
        assert_eq!((other.id, other.target.as_str()), (link.id, "gemini://a/b"));
        let z = Point { line: top + 2, col: 0 };
        assert_eq!((other.cells.start, other.cells.end), (z, z));
        // Outside the hyperlinks, the regex.
        assert_eq!(at(2, 3).map(|l| (l.id, l.target)), Some((None, "https://d".to_owned())));
        assert_eq!(at(1, 3), None);
    }
}
