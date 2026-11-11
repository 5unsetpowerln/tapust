//! ghostty's default link matcher: the URL and path regex of its
//! config/url.zig, run with Oniguruma (as ghostty does) over the logical
//! line under the pointer.

use std::sync::OnceLock;

use onig::Regex;
use term::{Terminal, Width};

use crate::selection::{self, Point, Selection};

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

/// ghostty's linkAtPos for the default URL matcher: the match on the
/// logical line under `p` (Screen.selectLine) that covers `p`, and its text.
pub fn link_at(term: &Terminal, p: Point) -> Option<(Selection, String)> {
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
    fn link_at_finds_the_url_under_the_pointer() {
        let mut t = Terminal::new(20, 3, 10);
        t.feed(b"see https://example.com/abc now");
        let top = t.view_top();
        let (sel, text) = link_at(&t, Point { line: top, col: 6 }).unwrap();
        assert_eq!(text, "https://example.com/abc");
        assert_eq!((sel.start, sel.end), (Point { line: top, col: 4 }, Point { line: top + 1, col: 6 }));
        assert!(link_at(&t, Point { line: top, col: 1 }).is_none());
    }
}
