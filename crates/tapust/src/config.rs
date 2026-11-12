//! The configuration file, `$XDG_CONFIG_HOME/tapust/config`, read as
//! ghostty reads its own (cli/args.zig, config/Config.zig): `key = value`
//! lines and `#` comments, with ghostty's names and values for the options
//! ghostty has and tapust's for the tmux ones. A line that cannot be applied
//! is a diagnostic at its place and leaves its option as it was; the other
//! lines still apply.

use std::collections::{HashMap, HashSet};
use std::ffi::CStr;
use std::fmt;
use std::os::unix::ffi::OsStrExt;
use std::path::{Component, Path, PathBuf};
use std::sync::OnceLock;

use mux::{Key, ModeKeys};
use term::{CursorShape, Palette, Rgb};

use crate::bindings::{Keybinds, MAX_POINTS, MIN_POINTS};
use crate::theme;

/// ghostty's TerminalColor: a color, or the colors of the cell.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TerminalColor {
    Rgb(Rgb),
    CellForeground,
    CellBackground,
}

/// ghostty's Command: a command line for `/bin/sh -c`, or with `direct:`
/// words run as they are.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    Shell(String),
    Direct(Vec<String>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WorkingDirectory {
    /// The directory tapust was started in.
    Inherit,
    Home,
    Path(PathBuf),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CopyOnSelect {
    None,
    Primary,
    Clipboard,
    Both,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConfirmClose {
    False,
    True,
    Always,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Config {
    /// `font-family`: one family, as tapust has no fallback families.
    pub font_family: Option<String>,
    pub font_size: f32,
    pub background: Rgb,
    pub foreground: Rgb,
    pub palette: [Rgb; 256],
    pub cursor_color: Option<TerminalColor>,
    pub cursor_text: Option<TerminalColor>,
    pub cursor_style: CursorShape,
    pub cursor_style_blink: Option<bool>,
    pub selection_foreground: Option<TerminalColor>,
    pub selection_background: Option<TerminalColor>,
    /// `command`: unset, the login shell as a login shell.
    pub command: Option<Command>,
    pub working_directory: WorkingDirectory,
    pub copy_on_select: CopyOnSelect,
    pub confirm_close_surface: ConfirmClose,
    pub keybind: Keybinds,
    /// `scrollback-lines`, tapust's: ghostty's scrollback-limit counts bytes.
    pub scrollback_lines: usize,
    /// `tmux-prefix`, tmux's `prefix`; `None` is no prefix key.
    pub tmux_prefix: Option<Key>,
    /// `tmux-mode-keys`, tmux's `mode-keys`; unset, tmux's rule from
    /// $VISUAL and $EDITOR.
    pub tmux_mode_keys: Option<ModeKeys>,
}

impl Default for Config {
    fn default() -> Config {
        Config {
            font_family: None,
            font_size: theme::FONT_POINTS,
            background: theme::BACKGROUND,
            foreground: theme::FOREGROUND,
            palette: theme::palette(),
            cursor_color: None,
            cursor_text: None,
            cursor_style: CursorShape::Block,
            cursor_style_blink: None,
            selection_foreground: None,
            selection_background: None,
            command: None,
            working_directory: WorkingDirectory::Inherit,
            copy_on_select: CopyOnSelect::Primary,
            confirm_close_surface: ConfirmClose::True,
            keybind: Keybinds::default(),
            scrollback_lines: 10_000,
            tmux_prefix: Some(mux::DEFAULT_PREFIX),
            tmux_mode_keys: None,
        }
    }
}

impl Config {
    pub fn font_family(&self) -> &str {
        self.font_family.as_deref().unwrap_or(theme::FONT_FAMILY)
    }

    /// The terminal's configured colors; a cursor color of the cell's own
    /// colors is drawn, not a color of the terminal.
    pub fn term_palette(&self) -> Palette {
        let cursor = match self.cursor_color {
            Some(TerminalColor::Rgb(c)) => Some(c),
            _ => None,
        };
        Palette { indexed: self.palette, foreground: self.foreground, background: self.background, cursor }
    }
}

/// A line that could not be applied, or a file that could not be read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diagnostic {
    /// The file and the line in it.
    pub location: Option<(PathBuf, usize)>,
    pub key: String,
    pub message: String,
}

/// ghostty's Diagnostic.format: `path:line:key: message`.
impl fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some((path, line)) = &self.location {
            write!(f, "{}:{line}:", path.display())?;
        }
        if !self.key.is_empty() {
            write!(f, "{}: ", self.key)?;
        } else if self.location.is_some() {
            f.write_str(" ")?;
        }
        f.write_str(&self.message)
    }
}

/// The configuration file: `$XDG_CONFIG_HOME/tapust/config`, with
/// `~/.config` for an unset or empty XDG_CONFIG_HOME.
pub fn path() -> Result<PathBuf, String> {
    match std::env::var_os("XDG_CONFIG_HOME").filter(|v| !v.is_empty()) {
        Some(dir) => Ok(PathBuf::from(dir).join("tapust/config")),
        None => {
            let home = home().ok_or("neither XDG_CONFIG_HOME nor a home directory is known")?;
            Ok(home.join(".config/tapust/config"))
        }
    }
}

/// $HOME, else the password database's home directory (ghostty's
/// internal_os.home).
pub fn home() -> Option<PathBuf> {
    if let Some(home) = std::env::var_os("HOME").filter(|h| !h.is_empty()) {
        return Some(PathBuf::from(home));
    }
    let pw = unsafe { libc::getpwuid(libc::getuid()) };
    if pw.is_null() || unsafe { (*pw).pw_dir }.is_null() {
        return None;
    }
    let dir = unsafe { CStr::from_ptr((*pw).pw_dir) };
    Some(PathBuf::from(std::ffi::OsStr::from_bytes(dir.to_bytes())))
}

/// Reads the configuration file, which may be missing, and the files its
/// `config-file` lines name; `font_exists` tells the font families there are.
pub fn load(font_exists: &dyn Fn(&str) -> bool) -> (Config, Vec<Diagnostic>) {
    let mut loader = Loader::new(font_exists);
    match path() {
        Ok(path) => loader.load(&path),
        Err(message) => loader.error(message),
    }
    (loader.config, loader.diagnostics)
}

#[derive(Debug)]
enum Error {
    Unknown,
    Required,
    Invalid,
    /// An invalid value, and why.
    Hint(String),
    Message(String),
}

struct Loader<'a> {
    config: Config,
    diagnostics: Vec<Diagnostic>,
    /// The `config-file` paths, and whether each is optional; the files
    /// from `next_include` on are still to be read.
    includes: Vec<(PathBuf, bool)>,
    next_include: usize,
    font_exists: &'a dyn Fn(&str) -> bool,
}

impl Loader<'_> {
    fn new(font_exists: &dyn Fn(&str) -> bool) -> Loader<'_> {
        let config = Config::default();
        Loader { config, diagnostics: Vec::new(), includes: Vec::new(), next_include: 0, font_exists }
    }

    fn error(&mut self, message: String) {
        self.diagnostics.push(Diagnostic { location: None, key: String::new(), message });
    }

    /// The file at `path`, if there is one, then the files `config-file`
    /// names, each after the file that names it was read (ghostty's
    /// loadRecursiveFiles), each once.
    fn load(&mut self, path: &Path) {
        match std::fs::read(path) {
            Ok(bytes) => self.text(path, &bytes),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => self.error(format!("error reading config file {}: {err}", path.display())),
        }
        let mut read = HashSet::new();
        if let Ok(path) = real_path(path) {
            read.insert(path);
        }
        while let Some((include, optional)) = self.includes.get(self.next_include).cloned() {
            self.next_include += 1;
            if !read.insert(include.clone()) {
                self.error(format!("config-file {}: cycle detected", include.display()));
                continue;
            }
            match std::fs::read(&include) {
                Ok(bytes) => self.text(&include, &bytes),
                Err(err) if optional && err.kind() == std::io::ErrorKind::NotFound => {}
                Err(err) => self.error(format!("error opening config-file {}: {err}", include.display())),
            }
        }
    }

    /// ghostty's LineIterator: lines trimmed of blanks, without the blank
    /// ones and the comments; the key before the first `=`, the value after
    /// it without the quotes around it.
    fn text(&mut self, path: &Path, bytes: &[u8]) {
        let bytes = bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(bytes);
        for (n, raw) in bytes.split(|&b| b == b'\n').enumerate() {
            let location = Some((path.to_owned(), n + 1));
            let Ok(line) = std::str::from_utf8(raw) else {
                let message = "the line is not valid UTF-8".to_owned();
                self.diagnostics.push(Diagnostic { location, key: String::new(), message });
                continue;
            };
            let line = line.trim_matches([' ', '\t', '\r']);
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let (key, value) = match line.split_once('=') {
                Some((key, value)) => (key.trim_matches([' ', '\t']), Some(unquote(value.trim_matches([' ', '\t'])))),
                None => (line, None),
            };
            let dir = path.parent().unwrap_or(Path::new("/"));
            let Err(err) = self.set(key, value, dir) else { continue };
            let values = valid_values(key);
            let message = match err {
                Error::Unknown => "unknown field".to_owned(),
                Error::Required => format!("value required{values}"),
                Error::Invalid => format!("invalid value \"{}\"{values}", value.unwrap_or_default()),
                Error::Hint(hint) => format!("invalid value \"{}\", {hint}", value.unwrap_or_default()),
                Error::Message(message) => message,
            };
            self.diagnostics.push(Diagnostic { location, key: key.to_owned(), message });
        }
    }

    /// Applies one line; an empty value resets the option to its default
    /// (ghostty's parseIntoField), and a key alone sets a boolean.
    fn set(&mut self, key: &str, value: Option<&str>, dir: &Path) -> Result<(), Error> {
        let default = Config::default();
        let c = &mut self.config;
        match key {
            "font-family" => match required(value)? {
                "" => c.font_family = None,
                _ if c.font_family.is_some() => {
                    return Err(Error::Message(
                        "tapust has no fallback font families: set font-family = \"\" before another family".to_owned(),
                    ));
                }
                family if !(self.font_exists)(family) => {
                    return Err(Error::Hint("no font family has that name".to_owned()));
                }
                family => c.font_family = Some(family.to_owned()),
            },
            "font-size" => c.font_size = or_default(value, default.font_size, font_size)?,
            "background" => c.background = or_default(value, default.background, color)?,
            "foreground" => c.foreground = or_default(value, default.foreground, color)?,
            "palette" => match required(value)? {
                "" => c.palette = default.palette,
                entry => {
                    let (index, rgb) = palette_entry(entry)?;
                    c.palette[index] = rgb;
                }
            },
            "cursor-color" => c.cursor_color = optional(value, terminal_color)?,
            "cursor-text" => c.cursor_text = optional(value, terminal_color)?,
            "selection-foreground" => c.selection_foreground = optional(value, terminal_color)?,
            "selection-background" => c.selection_background = optional(value, terminal_color)?,
            "cursor-style" => c.cursor_style = or_default(value, default.cursor_style, |v| one_of(v, CURSOR_STYLES))?,
            "cursor-style-blink" => {
                c.cursor_style_blink = match value {
                    None => Some(true),
                    Some("") => None,
                    Some(v) => Some(boolean(v)?),
                }
            }
            "command" => c.command = optional(value, command)?,
            "working-directory" => {
                c.working_directory = or_default(value, default.working_directory, working_directory)?
            }
            "copy-on-select" => {
                // ghostty's compatCopyOnSelect: true is primary on Linux.
                c.copy_on_select = or_default(value, default.copy_on_select, |v| match v {
                    "true" => Ok(CopyOnSelect::Primary),
                    "false" => Ok(CopyOnSelect::None),
                    _ => one_of(v, COPY_ON_SELECT),
                })?
            }
            "confirm-close-surface" => {
                let parse = |v: &str| one_of(v, CONFIRM_CLOSE);
                c.confirm_close_surface = or_default(value, default.confirm_close_surface, parse)?
            }
            "keybind" => match required(value)? {
                "" => c.keybind = default.keybind,
                "clear" => c.keybind.clear(),
                entry => c.keybind.parse(entry).ok_or(Error::Invalid)?,
            },
            "scrollback-lines" => {
                c.scrollback_lines = or_default(value, default.scrollback_lines, |v| {
                    integer(v).map(|lines| lines as usize).ok_or(Error::Invalid)
                })?
            }
            "tmux-prefix" => {
                c.tmux_prefix = or_default(value, default.tmux_prefix, |v| mux::parse_key(v).map_err(Error::Message))?
            }
            "tmux-mode-keys" => c.tmux_mode_keys = optional(value, |v| one_of(v, MODE_KEYS))?,
            "config-file" => match required(value)? {
                "" => self.includes.truncate(self.next_include),
                file => {
                    let (optional, file) = match file.strip_prefix('?') {
                        Some(file) => (true, unquote(file)),
                        None => (false, file),
                    };
                    // ghostty ignores an empty path.
                    if !file.is_empty() {
                        let path = real_path(&dir.join(expand_home(file)?)).map_err(|err| {
                            Error::Message(format!("error resolving file path {file}: {err}"))
                        })?;
                        self.includes.push((path, optional));
                    }
                }
            },
            _ => return Err(Error::Unknown),
        }
        Ok(())
    }
}

/// ghostty's Path.expand: the real path of a file, or of a missing one the
/// path with `.` and `..` folded, so that a cycle is seen whatever the
/// paths that make it.
fn real_path(path: &Path) -> std::io::Result<PathBuf> {
    match std::fs::canonicalize(path) {
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            let mut resolved = PathBuf::new();
            for part in path.components() {
                match part {
                    Component::ParentDir => {
                        resolved.pop();
                    }
                    Component::CurDir => {}
                    part => resolved.push(part),
                }
            }
            Ok(resolved)
        }
        real => real,
    }
}

fn unquote(value: &str) -> &str {
    match value.strip_prefix('"').and_then(|v| v.strip_suffix('"')) {
        Some(inner) if value.len() >= 2 => inner,
        _ => value,
    }
}

fn required(value: Option<&str>) -> Result<&str, Error> {
    value.ok_or(Error::Required)
}

/// The value parsed, or the option's default for an empty value.
fn or_default<T>(value: Option<&str>, default: T, parse: impl FnOnce(&str) -> Result<T, Error>) -> Result<T, Error> {
    match required(value)? {
        "" => Ok(default),
        v => parse(v),
    }
}

/// An option that is unset by default.
fn optional<T>(value: Option<&str>, parse: impl FnOnce(&str) -> Result<T, Error>) -> Result<Option<T>, Error> {
    or_default(value, None, |v| parse(v).map(Some))
}

const CURSOR_STYLES: &[(&str, CursorShape)] = &[
    ("block", CursorShape::Block),
    ("bar", CursorShape::Bar),
    ("underline", CursorShape::Underline),
    ("block_hollow", CursorShape::BlockHollow),
];
const COPY_ON_SELECT: &[(&str, CopyOnSelect)] = &[
    ("none", CopyOnSelect::None),
    ("primary", CopyOnSelect::Primary),
    ("clipboard", CopyOnSelect::Clipboard),
    ("both", CopyOnSelect::Both),
];
const CONFIRM_CLOSE: &[(&str, ConfirmClose)] =
    &[("false", ConfirmClose::False), ("true", ConfirmClose::True), ("always", ConfirmClose::Always)];
const MODE_KEYS: &[(&str, ModeKeys)] = &[("emacs", ModeKeys::Emacs), ("vi", ModeKeys::Vi)];

fn one_of<T: Copy>(value: &str, values: &[(&str, T)]) -> Result<T, Error> {
    values.iter().find(|(name, _)| *name == value).map(|&(_, v)| v).ok_or(Error::Invalid)
}

/// ghostty's formatValues: the names of an enumerated option's values.
fn valid_values(key: &str) -> String {
    fn names<T>(values: &[(&'static str, T)]) -> Vec<&'static str> {
        values.iter().map(|&(name, _)| name).collect()
    }
    let names = match key {
        "cursor-style" => names(CURSOR_STYLES),
        "copy-on-select" => names(COPY_ON_SELECT),
        "confirm-close-surface" => names(CONFIRM_CLOSE),
        "tmux-mode-keys" => names(MODE_KEYS),
        _ => return String::new(),
    };
    format!(", valid values are: {}", names.join(", "))
}

/// ghostty's parseBool.
fn boolean(value: &str) -> Result<bool, Error> {
    match value {
        "1" | "t" | "T" | "true" => Ok(true),
        "0" | "f" | "F" | "false" => Ok(false),
        _ => Err(Error::Invalid),
    }
}

/// Zig's parseInt with base 0: decimal, or `0x`, `0o` or `0b` digits, with
/// underscores between digits.
fn integer(value: &str) -> Option<u32> {
    let value = value.strip_prefix('+').unwrap_or(value);
    let (radix, digits) = match value.get(..2).map(str::to_ascii_lowercase).as_deref() {
        Some("0x") => (16, &value[2..]),
        Some("0o") => (8, &value[2..]),
        Some("0b") => (2, &value[2..]),
        _ => (10, value),
    };
    let well_formed = !digits.is_empty()
        && digits.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        && !digits.starts_with('_')
        && !digits.ends_with('_')
        && !digits.contains("__");
    well_formed.then(|| u32::from_str_radix(&digits.replace('_', ""), radix).ok()).flatten()
}

fn font_size(value: &str) -> Result<f32, Error> {
    let points: f32 = value.parse().map_err(|_| Error::Invalid)?;
    if !(MIN_POINTS..=MAX_POINTS).contains(&points) {
        return Err(Error::Hint(format!("tapust's font sizes are from {MIN_POINTS} to {MAX_POINTS} points")));
    }
    Ok(points)
}

fn color(value: &str) -> Result<Rgb, Error> {
    parse_rgb(value).ok_or(Error::Invalid)
}

fn terminal_color(value: &str) -> Result<TerminalColor, Error> {
    Ok(match value {
        "cell-foreground" => TerminalColor::CellForeground,
        "cell-background" => TerminalColor::CellBackground,
        _ => TerminalColor::Rgb(color(value)?),
    })
}

/// ghostty's parsePaletteEntry: `N=COLOR`.
fn palette_entry(entry: &str) -> Result<(usize, Rgb), Error> {
    let (index, rgb) = entry.split_once('=').ok_or(Error::Invalid)?;
    let index = integer(index.trim_matches([' ', '\t'])).filter(|&i| i < 256).ok_or(Error::Invalid)?;
    Ok((index as usize, color(rgb)?))
}

/// ghostty's Command.parseCLI.
fn command(value: &str) -> Result<Command, Error> {
    let value = value.trim_matches(' ');
    let (direct, line) = match value.split_once(':') {
        Some(("direct", line)) => (true, line),
        Some(("shell", line)) => (false, line),
        _ => (false, value),
    };
    let line = line.trim_matches(' ');
    if line.is_empty() {
        return Err(Error::Required);
    }
    Ok(match direct {
        true => Command::Direct(line.split(' ').filter(|w| !w.is_empty()).map(str::to_owned).collect()),
        false => Command::Shell(line.to_owned()),
    })
}

/// ghostty's WorkingDirectory: `home`, `inherit`, or an absolute path or
/// one under `~/`.
fn working_directory(value: &str) -> Result<WorkingDirectory, Error> {
    Ok(match unquote(value.trim()) {
        "home" => WorkingDirectory::Home,
        "inherit" => WorkingDirectory::Inherit,
        path => {
            let path = expand_home(path)?;
            if !path.is_absolute() {
                let hint = "it must be home, inherit, an absolute path or one that starts with ~/";
                return Err(Error::Hint(hint.to_owned()));
            }
            WorkingDirectory::Path(path)
        }
    })
}

/// A path with a leading `~/` in the home directory.
fn expand_home(path: &str) -> Result<PathBuf, Error> {
    match path.strip_prefix("~/") {
        Some(rest) => Ok(home().ok_or(Error::Message("no home directory to expand ~/ in".to_owned()))?.join(rest)),
        None => Ok(PathBuf::from(path)),
    }
}

/// ghostty's RGB.parse: `#rgb`, `#rrggbb`, `#rrrgggbbb`, `#rrrrggggbbbb`,
/// an X11 color name, `rgb` or `rrggbb`, `rgb:r/g/b` (1 to 4 hex digits
/// each) or `rgbi:r/g/b` (intensities from 0 to 1).
pub fn parse_rgb(value: &str) -> Option<Rgb> {
    let input = value.trim_matches([' ', '\t']);
    if input.is_empty() {
        return None;
    }
    if let Some(hex) = input.strip_prefix('#') {
        return matches!(hex.len(), 3 | 6 | 9 | 12).then(|| hex_triplet(hex)).flatten();
    }
    if let Some(rgb) = x11_color(input) {
        return Some(rgb);
    }
    if matches!(input.len(), 3 | 6) {
        return hex_triplet(input);
    }
    let (intensity, parts) = match (input.strip_prefix("rgbi:"), input.strip_prefix("rgb:")) {
        (Some(parts), _) => (true, parts),
        (_, Some(parts)) => (false, parts),
        _ => return None,
    };
    let mut parts = parts.splitn(3, '/');
    let mut component = || {
        let part = parts.next()?;
        if intensity { from_intensity(part) } else { from_hex(part) }
    };
    Some(Rgb { r: component()?, g: component()?, b: component()? })
}

/// Three equally long groups of hex digits.
fn hex_triplet(hex: &str) -> Option<Rgb> {
    if !hex.is_ascii() {
        return None;
    }
    let n = hex.len() / 3;
    Some(Rgb { r: from_hex(&hex[..n])?, g: from_hex(&hex[n..2 * n])?, b: from_hex(&hex[2 * n..])? })
}

/// ghostty's RGB.fromHex: 1 to 4 hex digits scaled to 8 bits.
fn from_hex(digits: &str) -> Option<u8> {
    if digits.is_empty() || digits.len() > 4 || !digits.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let value = u32::from_str_radix(digits, 16).ok()?;
    let max = (1u32 << (4 * digits.len())) - 1;
    Some((value * 255 / max) as u8)
}

fn from_intensity(value: &str) -> Option<u8> {
    let v: f64 = value.parse().ok()?;
    (0.0..=1.0).contains(&v).then_some((v * 255.0) as u8)
}

/// X11's rgb.txt (MIT/X11 license, as ghostty ships it), by name in any case.
fn x11_color(name: &str) -> Option<Rgb> {
    static COLORS: OnceLock<HashMap<String, Rgb>> = OnceLock::new();
    let colors = COLORS.get_or_init(|| {
        let mut colors = HashMap::new();
        for line in include_str!("rgb.txt").lines().filter(|l| l.len() > 12) {
            let level = |at: usize| line[at..at + 3].trim().parse().expect("an rgb.txt level");
            let rgb = Rgb { r: level(0), g: level(4), b: level(8) };
            colors.entry(line[12..].trim().to_ascii_lowercase()).or_insert(rgb);
        }
        colors
    });
    colors.get(&name.to_ascii_lowercase()).copied()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bindings::{Binding, FontSize, Press};
    use gtk::gdk;

    fn fonts(name: &str) -> bool {
        matches!(name, "JetBrains Mono" | "Fira Code" | "monospace")
    }

    /// The configuration of `text` read as the file `/c/config`, and its
    /// diagnostics as the error dialog lists them.
    fn parse_bytes(text: &[u8]) -> (Config, Vec<String>) {
        let mut loader = Loader::new(&fonts);
        loader.text(Path::new("/c/config"), text);
        (loader.config, loader.diagnostics.iter().map(ToString::to_string).collect())
    }

    fn parse(text: &str) -> (Config, Vec<String>) {
        parse_bytes(text.as_bytes())
    }

    fn ok(text: &str) -> Config {
        let (config, diagnostics) = parse(text);
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        config
    }

    fn errors(text: &str) -> Vec<String> {
        parse(text).1
    }

    fn rgb(v: u32) -> Rgb {
        Rgb { r: (v >> 16) as u8, g: (v >> 8) as u8, b: v as u8 }
    }

    #[test]
    fn an_empty_file_is_the_default() {
        assert_eq!(ok(""), Config::default());
        assert_eq!(ok("\n# only a comment\n\n"), Config::default());
        let c = Config::default();
        assert_eq!((c.font_family(), c.font_size), ("JetBrains Mono", 12.0));
        assert_eq!((c.background, c.foreground), (rgb(0x282c34), rgb(0xffffff)));
        assert_eq!(c.palette[1], rgb(0xcc6666));
        assert_eq!(c.tmux_prefix, Some(mux::DEFAULT_PREFIX));
        assert_eq!(c.term_palette().cursor, None);
    }

    #[test]
    fn lines_read_as_ghostty_reads_them() {
        let c = ok(concat!(
            "\u{feff}# a comment\n",
            "\n",
            "  font-size=14  \r\n",
            "\tbackground =  \"#102030\"\n",
            "   # an indented comment\n",
            "font-family = \"Fira Code\"\n",
            "cursor-style-blink\n",
        ));
        assert_eq!(c.font_size, 14.0);
        assert_eq!(c.background, rgb(0x102030));
        assert_eq!(c.font_family.as_deref(), Some("Fira Code"));
        assert_eq!(c.cursor_style_blink, Some(true));
        // Only a value between quotes loses them.
        assert_eq!(errors("foreground = \"#fff"), ["/c/config:1:foreground: invalid value \"\"#fff\""]);
    }

    #[test]
    fn errors_name_their_line_and_the_rest_applies() {
        let (c, d) = parse_bytes(
            b"font-size = 16\nfoo = bar\nbackground = nope\ncursor-style = beam\nforeground\n\
              background = #fff\ntmux-prefix = X-a\nfont-size\nkeybind = ctrl+a=new_tab\n\xff = 1\n",
        );
        assert_eq!(
            d,
            [
                "/c/config:2:foo: unknown field",
                "/c/config:3:background: invalid value \"nope\"",
                "/c/config:4:cursor-style: invalid value \"beam\", valid values are: block, bar, underline, block_hollow",
                "/c/config:5:foreground: value required",
                "/c/config:7:tmux-prefix: bad key: X-a",
                "/c/config:8:font-size: value required",
                "/c/config:9:keybind: invalid value \"ctrl+a=new_tab\"",
                "/c/config:10: the line is not valid UTF-8",
            ]
        );
        assert_eq!((c.font_size, c.background), (16.0, rgb(0xffffff)));
        assert_eq!(c.foreground, Config::default().foreground);
    }

    #[test]
    fn an_empty_value_resets_the_option() {
        let c = ok(
            "font-size = 20\nfont-size =\nbackground = #000\nbackground = \"\"\ncursor-style = bar\ncursor-style =",
        );
        assert_eq!(c, Config::default());
    }

    #[test]
    fn colors_read_as_ghosttys_rgb_parse() {
        for (text, v) in [
            ("#102030", 0x102030),
            ("102030", 0x102030),
            ("#fff", 0xffffff),
            ("#345", 0x334455),
            ("abc", 0xaabbcc),
            ("#123456789", 0x124578),
            ("#1234abcdef01", 0x12abee),
            ("rgb:f/80/abc", 0xff80ab),
            ("rgbi:1/0.5/0", 0xff7f00),
            ("black", 0),
            ("Ghost White", 0xf8f8ff),
            ("  red\t", 0xff0000),
            ("DarkSlateGray", 0x2f4f4f),
        ] {
            assert_eq!(parse_rgb(text), Some(rgb(v)), "{text:?}");
        }
        let bad = ["", "#12", "#ggg", "12345", "rgb:1/2", "rgbi:2/0/0", "notacolor", "rgb:12345/0/0", "#\u{e9}\u{e9}"];
        for bad in bad {
            assert_eq!(parse_rgb(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn palette_entries_repeat_and_reset() {
        let c = ok(
            "palette = 0=#010203\npalette = 0x0f = red\npalette=  255 =#fff\npalette = 0b11=black\npalette = 1_0=#0a0a0a",
        );
        assert_eq!((c.palette[0], c.palette[15], c.palette[255]), (rgb(0x010203), rgb(0xff0000), rgb(0xffffff)));
        assert_eq!((c.palette[3], c.palette[10]), (rgb(0), rgb(0x0a0a0a)));
        assert_eq!(c.palette[1], theme::palette()[1]);
        let c = ok("palette = 1=#000\npalette =\npalette = 2=#fff");
        assert_eq!((c.palette[1], c.palette[2]), (theme::palette()[1], rgb(0xffffff)));
        let invalid = |entry: &str| format!("/c/config:1:palette: invalid value \"{entry}\"");
        for entry in ["256=#fff", "x=#fff", "1", "1=nope", "-1=#fff", "_1=#fff"] {
            assert_eq!(errors(&format!("palette = {entry}")), [invalid(entry)]);
        }
    }

    #[test]
    fn one_font_family_that_fontconfig_has() {
        assert_eq!(ok("font-family = Fira Code").font_family(), "Fira Code");
        assert_eq!(ok("font-family = monospace").font_family(), "monospace");
        let (c, d) = parse(
            "font-family = Fira Code\nfont-family = JetBrains Mono\nfont-family = \"\"\n\
             font-family = JetBrains Mono\nfont-family = \"\"\nfont-family = Comic Sans",
        );
        assert_eq!(c.font_family, None);
        assert_eq!(
            d,
            [
                "/c/config:2:font-family: tapust has no fallback font families: set font-family = \"\" before another family",
                "/c/config:6:font-family: invalid value \"Comic Sans\", no font family has that name",
            ]
        );
    }

    #[test]
    fn font_sizes_within_what_tapust_draws() {
        assert_eq!(ok("font-size = 13.5").font_size, 13.5);
        assert_eq!(ok("font-size = 4\nfont-size = 72").font_size, 72.0);
        for size in ["3.9", "73", "big", "NaN", "inf"] {
            let message = match size {
                "big" => format!("/c/config:1:font-size: invalid value \"{size}\""),
                _ => format!(
                    "/c/config:1:font-size: invalid value \"{size}\", tapust's font sizes are from 4 to 72 points"
                ),
            };
            assert_eq!(errors(&format!("font-size = {size}")), [message]);
        }
    }

    #[test]
    fn cursor_and_selection_colors() {
        let c = ok("cursor-color = cell-foreground\ncursor-text = #123\nselection-foreground = cell-background\n\
                    selection-background = navy");
        assert_eq!(c.cursor_color, Some(TerminalColor::CellForeground));
        assert_eq!(c.cursor_text, Some(TerminalColor::Rgb(rgb(0x112233))));
        assert_eq!(c.selection_foreground, Some(TerminalColor::CellBackground));
        assert_eq!(c.selection_background, Some(TerminalColor::Rgb(rgb(0x000080))));
        assert_eq!(c.term_palette().cursor, None);
        let c = ok("cursor-color = #fff");
        assert_eq!(c.term_palette().cursor, Some(rgb(0xffffff)));
        assert_eq!(ok("cursor-color = #fff\ncursor-color =").cursor_color, None);
        assert_eq!(errors("cursor-text = cell-text"), ["/c/config:1:cursor-text: invalid value \"cell-text\""]);
    }

    #[test]
    fn cursor_style_and_blink() {
        let c = ok("cursor-style = block_hollow\ncursor-style-blink = false");
        assert_eq!((c.cursor_style, c.cursor_style_blink), (CursorShape::BlockHollow, Some(false)));
        for (text, blink) in [("t", Some(true)), ("1", Some(true)), ("F", Some(false)), ("0", Some(false))] {
            assert_eq!(ok(&format!("cursor-style-blink = {text}")).cursor_style_blink, blink);
        }
        assert_eq!(ok("cursor-style-blink = true\ncursor-style-blink =").cursor_style_blink, None);
        assert_eq!(errors("cursor-style-blink = yes"), ["/c/config:1:cursor-style-blink: invalid value \"yes\""]);
        let shapes = [("bar", CursorShape::Bar), ("underline", CursorShape::Underline), ("block", CursorShape::Block)];
        for (text, shape) in shapes {
            assert_eq!(ok(&format!("cursor-style = {text}")).cursor_style, shape);
        }
    }

    #[test]
    fn commands_as_ghostty_takes_them() {
        let shell = |line: &str| Some(Command::Shell(line.to_owned()));
        assert_eq!(ok("command = fish -l").command, shell("fish -l"));
        assert_eq!(ok("command = shell: nvim 'a b'").command, shell("nvim 'a b'"));
        assert_eq!(ok("command = foo:bar").command, shell("foo:bar"));
        let direct = ok("command = direct:nvim  a b").command;
        assert_eq!(direct, Some(Command::Direct(vec!["nvim".into(), "a".into(), "b".into()])));
        assert_eq!(ok("command = fish\ncommand =").command, None);
        assert_eq!(errors("command = direct:"), ["/c/config:1:command: value required"]);
    }

    #[test]
    fn working_directories() {
        assert_eq!(ok("working-directory = home").working_directory, WorkingDirectory::Home);
        assert_eq!(ok("working-directory = inherit").working_directory, WorkingDirectory::Inherit);
        assert_eq!(ok("working-directory = \"/tmp\"").working_directory, WorkingDirectory::Path("/tmp".into()));
        let under_home = ok("working-directory = ~/src").working_directory;
        assert_eq!(under_home, WorkingDirectory::Path(home().unwrap().join("src")));
        assert_eq!(
            errors("working-directory = src"),
            [concat!(
                "/c/config:1:working-directory: invalid value \"src\", ",
                "it must be home, inherit, an absolute path or one that starts with ~/"
            )]
        );
    }

    #[test]
    fn copy_on_select_and_confirm_close_surface() {
        for (text, mode) in [
            ("none", CopyOnSelect::None),
            ("primary", CopyOnSelect::Primary),
            ("clipboard", CopyOnSelect::Clipboard),
            ("both", CopyOnSelect::Both),
            ("true", CopyOnSelect::Primary),
            ("false", CopyOnSelect::None),
        ] {
            assert_eq!(ok(&format!("copy-on-select = {text}")).copy_on_select, mode);
        }
        assert_eq!(
            errors("copy-on-select = yes\ncopy-on-select"),
            [
                "/c/config:1:copy-on-select: invalid value \"yes\", valid values are: none, primary, clipboard, both",
                "/c/config:2:copy-on-select: value required, valid values are: none, primary, clipboard, both",
            ]
        );
        let confirms = [("false", ConfirmClose::False), ("true", ConfirmClose::True), ("always", ConfirmClose::Always)];
        for (text, confirm) in confirms {
            assert_eq!(ok(&format!("confirm-close-surface = {text}")).confirm_close_surface, confirm);
        }
        assert_eq!(
            errors("confirm-close-surface = never"),
            ["/c/config:1:confirm-close-surface: invalid value \"never\", valid values are: false, true, always"]
        );
    }

    #[test]
    fn scrollback_lines() {
        assert_eq!(ok("scrollback-lines = 0").scrollback_lines, 0);
        assert_eq!(ok("scrollback-lines = 1_000_000").scrollback_lines, 1_000_000);
        assert_eq!(ok("scrollback-lines = 0x100").scrollback_lines, 256);
        for bad in ["-1", "1.5", "4294967296", "1__0", "lots"] {
            let invalid = format!("/c/config:1:scrollback-lines: invalid value \"{bad}\"");
            assert_eq!(errors(&format!("scrollback-lines = {bad}")), [invalid]);
        }
    }

    #[test]
    fn tmux_options() {
        let ctrl_a = mux::parse_key("C-a").unwrap();
        assert_eq!(ok("tmux-prefix = C-a").tmux_prefix, ctrl_a);
        assert_eq!(ok("tmux-prefix = None").tmux_prefix, None);
        assert_eq!(ok("tmux-prefix = C-a\ntmux-prefix =").tmux_prefix, Some(mux::DEFAULT_PREFIX));
        assert_eq!(ok("tmux-mode-keys = vi").tmux_mode_keys, Some(ModeKeys::Vi));
        assert_eq!(ok("tmux-mode-keys = emacs").tmux_mode_keys, Some(ModeKeys::Emacs));
        assert_eq!(ok("tmux-mode-keys = vi\ntmux-mode-keys =").tmux_mode_keys, None);
        let invalid = "/c/config:1:tmux-mode-keys: invalid value \"Vi\", valid values are: emacs, vi";
        assert_eq!(errors("tmux-mode-keys = Vi"), [invalid]);
    }

    #[test]
    fn keybinds_repeat_clear_and_reset() {
        let press = |c: &Config, keyval, state| {
            let press = Press { keyval, keycode: 0, state, consumed: gdk::ModifierType::empty(), unshifted: None };
            c.keybind.lookup(&press).map(|b| b.binding)
        };
        let ctrl_shift = gdk::ModifierType::CONTROL_MASK | gdk::ModifierType::SHIFT_MASK;
        let c = ok("keybind = ctrl+shift+k=increase_font_size:2\nkeybind = ctrl+shift+c=unbind");
        assert_eq!(press(&c, gdk::Key::K, ctrl_shift), Some(Binding::FontSize(FontSize::Increase(2.0))));
        assert_eq!(press(&c, gdk::Key::C, ctrl_shift), None);
        assert_eq!(press(&c, gdk::Key::V, ctrl_shift), Some(Binding::Paste { primary: false }));
        let c = ok("keybind = clear\nkeybind = ctrl+shift+k=select_all");
        assert_eq!(press(&c, gdk::Key::V, ctrl_shift), None);
        assert_eq!(press(&c, gdk::Key::K, ctrl_shift), Some(Binding::SelectAll));
        assert_eq!(ok("keybind = clear\nkeybind =").keybind, Keybinds::default());
    }

    #[test]
    fn config_files_load_after_the_file_that_names_them() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/test-config-files");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        let write = |name: &str, text: &str| std::fs::write(dir.join(name), text).unwrap();
        write(
            "config",
            "config-file = sub/a\nfont-size = 14\nconfig-file = ?missing\nconfig-file = gone\nbackground = #111\n",
        );
        write("sub/a", "font-size = 15\nconfig-file = \"./b\"\nconfig-file = ../config\nconfig-file = ?\n");
        write("sub/b", "background = #222\nbogus\n");
        let mut loader = Loader::new(&fonts);
        loader.load(&dir.join("config"));
        assert_eq!((loader.config.font_size, loader.config.background), (15.0, rgb(0x222222)));
        let shown: Vec<String> = loader.diagnostics.iter().map(ToString::to_string).collect();
        let dir = std::fs::canonicalize(dir).unwrap();
        let dir = dir.display();
        assert_eq!(
            shown,
            [
                format!("error opening config-file {dir}/gone: No such file or directory (os error 2)"),
                format!("{dir}/sub/b:2:bogus: unknown field"),
                format!("config-file {dir}/config: cycle detected"),
            ]
        );
    }

    #[test]
    fn a_missing_file_is_no_error_and_an_unreadable_one_is() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/test-config-missing");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("config")).unwrap();
        let mut loader = Loader::new(&fonts);
        loader.load(&dir.join("absent"));
        assert!(loader.diagnostics.is_empty());
        loader.load(&dir.join("config"));
        let error = format!("error reading config file {}/config: Is a directory (os error 21)", dir.display());
        assert_eq!(loader.diagnostics.iter().map(ToString::to_string).collect::<Vec<_>>(), [error]);
    }
}
