//! The configuration file, `$XDG_CONFIG_HOME/tapust/config`, read as
//! ghostty reads its own (cli/args.zig, config/Config.zig): `key = value`
//! lines and `#` comments, with ghostty's names and values for the options
//! ghostty has and tapust's for the tmux ones. A line that cannot be applied
//! is a diagnostic at its place and leaves its option as it was; the other
//! lines still apply. A `theme` is read first, the user's lines over it.

use std::collections::{HashMap, HashSet};
use std::ffi::CStr;
use std::fmt;
use std::os::unix::ffi::OsStrExt;
use std::path::{Component, Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

use mux::{Key, ModeKeys};
use term::{CursorShape, Palette, Rgb};

use crate::bindings::{Keybinds, MAX_POINTS, MIN_POINTS};
use crate::{theme, themes};

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

/// ghostty's Theme: the theme for a light and for a dark desktop, the same
/// one unless `light:` and `dark:` name two.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Theme {
    pub light: String,
    pub dark: String,
}

impl Theme {
    /// The theme follows the desktop's light or dark style.
    pub fn conditional(&self) -> bool {
        self.light != self.dark
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WindowTheme {
    /// Light or dark as the terminal background is.
    Auto,
    System,
    Light,
    Dark,
    /// As auto, with the window in the terminal's colors.
    Ghostty,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TitlebarStyle {
    Native,
    /// The tab bar is the title bar.
    Tabs,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TabsLocation {
    Top,
    Bottom,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShowTabBar {
    Always,
    /// With two tabs or more.
    Auto,
    Never,
}

/// ghostty's window-decoration without `server`: GTK announces client-side
/// decorations for a libadwaita window, and tapust does not ask the
/// compositor for its own as ghostty does with KDE's protocol.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decoration {
    Auto,
    Client,
    None,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Subtitle {
    False,
    WorkingDirectory,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NewTabPosition {
    /// After the current tab.
    Current,
    End,
}

/// ghostty's window-padding-balance: leftover space split around the grid,
/// with `true` keeping the top padding near the side padding.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PaddingBalance {
    False,
    True,
    Equal,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PaddingColor {
    Background,
    /// The nearest cell's background, unless ghostty's heuristics say no.
    Extend,
    ExtendAlways,
}

/// A `gtk-custom-css` file and its text, read with the configuration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Stylesheet {
    pub path: PathBuf,
    pub css: String,
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
    /// The theme the colors above were read over, if any.
    pub theme: Option<Theme>,
    pub window_theme: WindowTheme,
    /// ghostty's window-titlebar-background and -foreground for
    /// window-theme = ghostty; unset, the terminal's colors.
    pub window_titlebar_background: Option<Rgb>,
    pub window_titlebar_foreground: Option<Rgb>,
    pub background_opacity: f64,
    pub background_opacity_cells: bool,
    pub unfocused_split_opacity: f64,
    /// Unset, the background.
    pub unfocused_split_fill: Option<Rgb>,
    /// Unset, libadwaita's window background.
    pub split_divider_color: Option<Rgb>,
    pub gtk_titlebar: bool,
    pub gtk_titlebar_hide_when_maximized: bool,
    pub gtk_titlebar_style: TitlebarStyle,
    pub gtk_tabs_location: TabsLocation,
    pub gtk_tabs_inverted: bool,
    pub gtk_wide_tabs: bool,
    pub gtk_toolbar_style: adw::ToolbarStyle,
    pub window_show_tab_bar: ShowTabBar,
    pub window_decoration: Decoration,
    pub window_subtitle: Subtitle,
    pub window_new_tab_position: NewTabPosition,
    /// The initial grid of a new window, in cells; 0 leaves the size to GTK.
    pub window_width: u32,
    pub window_height: u32,
    /// `window-padding-x` (left, right) and `window-padding-y` (top,
    /// bottom), in points.
    pub window_padding_x: (u32, u32),
    pub window_padding_y: (u32, u32),
    pub window_padding_balance: PaddingBalance,
    pub window_padding_color: PaddingColor,
    pub gtk_custom_css: Vec<Stylesheet>,
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
    /// `split-resize-transition`, tapust's: how long a divider takes to move
    /// to a ratio a key gave (resize-pane, a layout); zero moves it at once.
    pub split_resize_transition: Duration,
    /// `split-resize-step`, tapust's: the logical pixels a resize by key of
    /// five cells (pain-control's step) moves a divider, in either
    /// direction; unset, five cell widths.
    pub split_resize_step: Option<f64>,
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
            theme: None,
            window_theme: WindowTheme::Auto,
            window_titlebar_background: None,
            window_titlebar_foreground: None,
            background_opacity: 1.0,
            background_opacity_cells: false,
            unfocused_split_opacity: 0.7,
            unfocused_split_fill: None,
            split_divider_color: None,
            gtk_titlebar: true,
            gtk_titlebar_hide_when_maximized: false,
            gtk_titlebar_style: TitlebarStyle::Native,
            gtk_tabs_location: TabsLocation::Top,
            gtk_tabs_inverted: false,
            gtk_wide_tabs: true,
            gtk_toolbar_style: adw::ToolbarStyle::Raised,
            window_show_tab_bar: ShowTabBar::Auto,
            window_decoration: Decoration::Auto,
            window_subtitle: Subtitle::False,
            window_new_tab_position: NewTabPosition::Current,
            window_width: 0,
            window_height: 0,
            window_padding_x: (2, 2),
            window_padding_y: (2, 2),
            window_padding_balance: PaddingBalance::False,
            window_padding_color: PaddingColor::Background,
            gtk_custom_css: Vec::new(),
            command: None,
            working_directory: WorkingDirectory::Inherit,
            copy_on_select: CopyOnSelect::Primary,
            confirm_close_surface: ConfirmClose::True,
            keybind: Keybinds::default(),
            scrollback_lines: 10_000,
            tmux_prefix: Some(mux::DEFAULT_PREFIX),
            tmux_mode_keys: None,
            split_resize_transition: Duration::ZERO,
            split_resize_step: None,
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
/// `config-file` lines name, over the theme they name (its dark one when
/// `dark`); `font_exists` tells the font families there are.
pub fn load(font_exists: &dyn Fn(&str) -> bool, dark: bool) -> (Config, Vec<Diagnostic>) {
    let mut loader = Loader::new(font_exists);
    match path() {
        Ok(path) => {
            loader.load(&path);
            loader.load_theme(&path.with_file_name("themes"), dark);
        }
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

/// A line that applied, to apply again over a theme (ghostty's replay step).
struct Step {
    key: String,
    value: Option<String>,
    dir: PathBuf,
    location: (PathBuf, usize),
}

struct Loader<'a> {
    config: Config,
    diagnostics: Vec<Diagnostic>,
    /// The `config-file` paths, and whether each is optional; the files
    /// from `next_include` on are still to be read.
    includes: Vec<(PathBuf, bool)>,
    next_include: usize,
    font_exists: &'a dyn Fn(&str) -> bool,
    steps: Vec<Step>,
    /// The theme file is being read: it cannot name another theme or file.
    in_theme: bool,
}

impl Loader<'_> {
    fn new(font_exists: &dyn Fn(&str) -> bool) -> Loader<'_> {
        let config = Config::default();
        Loader {
            config,
            diagnostics: Vec::new(),
            includes: Vec::new(),
            next_include: 0,
            font_exists,
            steps: Vec::new(),
            in_theme: false,
        }
    }

    /// ghostty's loadTheme: the configuration starts over from the theme's
    /// file, found by name in `themes` and then among the bundled themes,
    /// and the lines read so far apply again over it, so that they win
    /// wherever they are.
    fn load_theme(&mut self, themes: &Path, dark: bool) {
        let Some(theme) = self.config.theme.clone() else { return };
        let name = if dark { &theme.dark } else { &theme.light };
        let (path, text) = match find_theme(name, themes) {
            Ok(found) => found,
            Err(messages) => {
                // The last theme line is the one that took effect.
                let location = self.steps.iter().rev().find(|s| s.key == "theme").map(|s| s.location.clone());
                for message in messages {
                    self.diagnostics.push(Diagnostic { location: location.clone(), key: "theme".to_owned(), message });
                }
                return;
            }
        };
        let steps = std::mem::take(&mut self.steps);
        self.config = Config::default();
        self.in_theme = true;
        self.text(&path, &text);
        self.in_theme = false;
        for step in &steps {
            self.apply(&step.key, step.value.as_deref(), &step.dir, step.location.clone());
        }
        self.steps = steps;
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
            let location = (path.to_owned(), n + 1);
            if self.apply(key, value, dir, location.clone()) && !self.in_theme && key != "config-file" {
                let value = value.map(str::to_owned);
                self.steps.push(Step { key: key.to_owned(), value, dir: dir.to_owned(), location });
            }
        }
    }

    /// Applies one line, or tells why it could not at `location`.
    fn apply(&mut self, key: &str, value: Option<&str>, dir: &Path, location: (PathBuf, usize)) -> bool {
        let Err(err) = self.set(key, value, dir) else { return true };
        let values = valid_values(key);
        let message = match err {
            Error::Unknown => "unknown field".to_owned(),
            Error::Required => format!("value required{values}"),
            Error::Invalid => format!("invalid value \"{}\"{values}", value.unwrap_or_default()),
            Error::Hint(hint) => format!("invalid value \"{}\", {hint}", value.unwrap_or_default()),
            Error::Message(message) => message,
        };
        self.diagnostics.push(Diagnostic { location: Some(location), key: key.to_owned(), message });
        false
    }

    /// Applies one line; an empty value resets the option to its default
    /// (ghostty's parseIntoField), and a key alone sets a boolean.
    fn set(&mut self, key: &str, value: Option<&str>, dir: &Path) -> Result<(), Error> {
        // ghostty ignores these in a theme without a word.
        if self.in_theme && matches!(key, "theme" | "config-file") {
            return Err(Error::Message(format!("a theme file cannot set {key}")));
        }
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
            "split-resize-step" => c.split_resize_step = optional(value, pixels)?,
            "split-resize-transition" => {
                c.split_resize_transition = or_default(value, default.split_resize_transition, duration)?
            }
            "theme" => c.theme = optional(value, theme)?,
            "window-theme" => c.window_theme = or_default(value, default.window_theme, |v| one_of(v, WINDOW_THEMES))?,
            "window-titlebar-background" => c.window_titlebar_background = optional(value, color)?,
            "window-titlebar-foreground" => c.window_titlebar_foreground = optional(value, color)?,
            "background-opacity" => {
                c.background_opacity = or_default(value, default.background_opacity, |v| fraction(v, 0.0))?
            }
            "background-opacity-cells" => c.background_opacity_cells = flag(value, default.background_opacity_cells)?,
            "unfocused-split-opacity" => {
                // ghostty's lowest: a split any fainter is hard to make out.
                let parse = |v: &str| fraction(v, 0.15);
                c.unfocused_split_opacity = or_default(value, default.unfocused_split_opacity, parse)?
            }
            "unfocused-split-fill" => c.unfocused_split_fill = optional(value, color)?,
            "split-divider-color" => c.split_divider_color = optional(value, color)?,
            "gtk-titlebar" => c.gtk_titlebar = flag(value, default.gtk_titlebar)?,
            "gtk-titlebar-hide-when-maximized" => {
                c.gtk_titlebar_hide_when_maximized = flag(value, default.gtk_titlebar_hide_when_maximized)?
            }
            "gtk-titlebar-style" => {
                c.gtk_titlebar_style = or_default(value, default.gtk_titlebar_style, |v| one_of(v, TITLEBAR_STYLES))?
            }
            "gtk-tabs-location" => {
                c.gtk_tabs_location = or_default(value, default.gtk_tabs_location, |v| one_of(v, TABS_LOCATIONS))?
            }
            "gtk-tabs-inverted" => c.gtk_tabs_inverted = flag(value, default.gtk_tabs_inverted)?,
            "gtk-wide-tabs" => c.gtk_wide_tabs = flag(value, default.gtk_wide_tabs)?,
            "gtk-toolbar-style" => {
                c.gtk_toolbar_style = or_default(value, default.gtk_toolbar_style, |v| one_of(v, TOOLBAR_STYLES))?
            }
            "window-show-tab-bar" => {
                c.window_show_tab_bar = or_default(value, default.window_show_tab_bar, |v| one_of(v, SHOW_TAB_BAR))?
            }
            "window-decoration" => {
                c.window_decoration = match value {
                    None => Decoration::Auto,
                    value => or_default(value, default.window_decoration, decoration)?,
                }
            }
            "window-subtitle" => {
                c.window_subtitle = or_default(value, default.window_subtitle, |v| one_of(v, SUBTITLES))?
            }
            "window-new-tab-position" => {
                let parse = |v: &str| one_of(v, NEW_TAB_POSITIONS);
                c.window_new_tab_position = or_default(value, default.window_new_tab_position, parse)?
            }
            "window-width" => c.window_width = or_default(value, default.window_width, |v| cells(v, 10, "wide"))?,
            "window-height" => c.window_height = or_default(value, default.window_height, |v| cells(v, 4, "high"))?,
            "window-padding-x" => c.window_padding_x = or_default(value, default.window_padding_x, padding)?,
            "window-padding-y" => c.window_padding_y = or_default(value, default.window_padding_y, padding)?,
            "window-padding-balance" => {
                c.window_padding_balance = or_default(value, default.window_padding_balance, |v| match v {
                    "true" => Ok(PaddingBalance::True),
                    "false" => Ok(PaddingBalance::False),
                    _ => one_of(v, PADDING_BALANCE),
                })?
            }
            "window-padding-color" => {
                let parse = |v: &str| one_of(v, PADDING_COLORS);
                c.window_padding_color = or_default(value, default.window_padding_color, parse)?
            }
            "gtk-custom-css" => match required(value)? {
                "" => c.gtk_custom_css.clear(),
                file => {
                    if let Some((path, optional)) = file_path(file, dir)?
                        && let Some(css) = stylesheet(&path, optional)?
                    {
                        c.gtk_custom_css.push(Stylesheet { path, css });
                    }
                }
            },
            "config-file" => match required(value)? {
                "" => self.includes.truncate(self.next_include),
                file => {
                    if let Some(include) = file_path(file, dir)? {
                        self.includes.push(include);
                    }
                }
            },
            _ => return Err(Error::Unknown),
        }
        Ok(())
    }
}

/// ghostty's Path.parse and expand: a file named relative to `dir` or the
/// home directory, optional with a leading `?`; an empty name, which ghostty
/// ignores, is none.
fn file_path(value: &str, dir: &Path) -> Result<Option<(PathBuf, bool)>, Error> {
    let (optional, file) = match value.strip_prefix('?') {
        Some(file) => (true, unquote(file)),
        None => (false, value),
    };
    if file.is_empty() {
        return Ok(None);
    }
    let path = real_path(&dir.join(expand_home(file)?))
        .map_err(|err| Error::Message(format!("error resolving file path {file}: {err}")))?;
    Ok(Some((path, optional)))
}

/// ghostty's limit for one gtk-custom-css file.
const CSS_LIMIT: u64 = 5 * 1024 * 1024;

/// The text of a gtk-custom-css file; none for a missing optional one.
fn stylesheet(path: &Path, optional: bool) -> Result<Option<String>, Error> {
    let fail = |err: std::io::Error| {
        Error::Message(format!("error opening gtk-custom-css file {}: {err}", path.display()))
    };
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(err) if optional && err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(fail(err)),
    };
    let mut bytes = Vec::new();
    std::io::Read::read_to_end(&mut std::io::Read::take(file, CSS_LIMIT + 1), &mut bytes).map_err(fail)?;
    if bytes.len() as u64 > CSS_LIMIT {
        return Err(Error::Message(format!("gtk-custom-css file {} is larger than 5 MiB", path.display())));
    }
    let css = String::from_utf8(bytes)
        .map_err(|_| Error::Message(format!("gtk-custom-css file {} is not valid UTF-8", path.display())))?;
    Ok(Some(css))
}

/// ghostty's Theme.parseCLI: one name or path, or `light:NAME,dark:NAME`
/// in any order (a `,`, `:` or `=` makes a pair), with `~/` expanded.
fn theme(value: &str) -> Result<Theme, Error> {
    let name = |v: &str| -> Result<String, Error> {
        let v = unquote(v.trim_matches([' ', '\t']));
        match v.strip_prefix("~/") {
            Some(_) => Ok(expand_home(v)?.to_string_lossy().into_owned()),
            None => Ok(v.to_owned()),
        }
    };
    if !value.contains([',', ':', '=']) {
        let theme = name(value)?;
        return Ok(Theme { light: theme.clone(), dark: theme });
    }
    let (mut light, mut dark) = (None, None);
    for entry in value.split(',') {
        let (key, theme) = entry.split_once(':').ok_or(Error::Invalid)?;
        match key.trim_matches([' ', '\t']) {
            "light" => light = Some(name(theme)?),
            "dark" => dark = Some(name(theme)?),
            _ => return Err(Error::Invalid),
        }
    }
    match (light, dark) {
        (Some(light), Some(dark)) => Ok(Theme { light, dark }),
        _ => Err(Error::Hint("a theme pair needs both light: and dark:".to_owned())),
    }
}

/// ghostty's theme.open: an absolute path, or a name looked up in the
/// user's `themes` directory, then among the bundled themes. Returns the
/// file's path (shown in its errors) and contents, or why there is none.
fn find_theme(name: &str, themes: &Path) -> Result<(PathBuf, Vec<u8>), Vec<String>> {
    let failed = |path: &Path, err| Some(format!("failed to load theme from \"{}\": {err}", path.display()));
    let read = |path: &Path| match std::fs::metadata(path) {
        Ok(meta) if !meta.is_file() => {
            Err(Some(format!("not reading theme from \"{}\": it is not a file", path.display())))
        }
        Ok(_) => std::fs::read(path).map_err(|err| failed(path, err)),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Err(None),
        Err(err) => Err(failed(path, err)),
    };
    if Path::new(name).is_absolute() {
        let path = PathBuf::from(name);
        return match read(&path) {
            Ok(text) => Ok((path, text)),
            Err(Some(message)) => Err(vec![message]),
            Err(None) => Err(vec![format!("failed to load theme from the path \"{name}\"")]),
        };
    }
    if name.contains('/') {
        return Err(vec![format!("theme \"{name}\" cannot include path separators unless it is an absolute path")]);
    }
    let user = themes.join(name);
    match read(&user) {
        Ok(text) => return Ok((user, text)),
        Err(Some(message)) => return Err(vec![message]),
        Err(None) => {}
    }
    match themes::bundled(name) {
        Some(text) => Ok((Path::new(BUNDLED).join(name), text)),
        None => Err(vec![
            format!("theme \"{name}\" not found, tried path \"{}\"", user.display()),
            format!("theme \"{name}\" not found among the themes bundled with tapust"),
        ]),
    }
}

/// Where the errors of a bundled theme say they are.
const BUNDLED: &str = "(tapust's themes)";

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
const WINDOW_THEMES: &[(&str, WindowTheme)] = &[
    ("auto", WindowTheme::Auto),
    ("system", WindowTheme::System),
    ("light", WindowTheme::Light),
    ("dark", WindowTheme::Dark),
    ("ghostty", WindowTheme::Ghostty),
];
const TITLEBAR_STYLES: &[(&str, TitlebarStyle)] = &[("native", TitlebarStyle::Native), ("tabs", TitlebarStyle::Tabs)];
const TABS_LOCATIONS: &[(&str, TabsLocation)] = &[("top", TabsLocation::Top), ("bottom", TabsLocation::Bottom)];
const TOOLBAR_STYLES: &[(&str, adw::ToolbarStyle)] = &[
    ("flat", adw::ToolbarStyle::Flat),
    ("raised", adw::ToolbarStyle::Raised),
    ("raised-border", adw::ToolbarStyle::RaisedBorder),
];
const SHOW_TAB_BAR: &[(&str, ShowTabBar)] =
    &[("always", ShowTabBar::Always), ("auto", ShowTabBar::Auto), ("never", ShowTabBar::Never)];
const DECORATIONS: &[(&str, Decoration)] =
    &[("auto", Decoration::Auto), ("client", Decoration::Client), ("none", Decoration::None)];
const SUBTITLES: &[(&str, Subtitle)] = &[("false", Subtitle::False), ("working-directory", Subtitle::WorkingDirectory)];
const NEW_TAB_POSITIONS: &[(&str, NewTabPosition)] =
    &[("current", NewTabPosition::Current), ("end", NewTabPosition::End)];
const PADDING_BALANCE: &[(&str, PaddingBalance)] =
    &[("false", PaddingBalance::False), ("true", PaddingBalance::True), ("equal", PaddingBalance::Equal)];
const PADDING_COLORS: &[(&str, PaddingColor)] = &[
    ("background", PaddingColor::Background),
    ("extend", PaddingColor::Extend),
    ("extend-always", PaddingColor::ExtendAlways),
];

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
        "window-theme" => names(WINDOW_THEMES),
        "gtk-titlebar-style" => names(TITLEBAR_STYLES),
        "gtk-tabs-location" => names(TABS_LOCATIONS),
        "gtk-toolbar-style" => names(TOOLBAR_STYLES),
        "window-show-tab-bar" => names(SHOW_TAB_BAR),
        "window-decoration" => names(DECORATIONS),
        "window-subtitle" => names(SUBTITLES),
        "window-new-tab-position" => names(NEW_TAB_POSITIONS),
        "window-padding-balance" => names(PADDING_BALANCE),
        "window-padding-color" => names(PADDING_COLORS),
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

/// A boolean option: alone it is true, and an empty value is its default.
fn flag(value: Option<&str>, default: bool) -> Result<bool, Error> {
    match value {
        None => Ok(true),
        value => or_default(value, default, boolean),
    }
}

/// ghostty's WindowDecoration.parseCLI, which takes booleans too.
fn decoration(value: &str) -> Result<Decoration, Error> {
    if value == "server" {
        let hint = "tapust cannot ask the compositor to draw the decorations, valid values are: auto, client, none";
        return Err(Error::Hint(hint.to_owned()));
    }
    match boolean(value) {
        Ok(true) => Ok(Decoration::Auto),
        Ok(false) => Ok(Decoration::None),
        Err(_) => one_of(value, DECORATIONS),
    }
}

/// An opacity from `min` to 1, which ghostty would clamp to that range
/// without a word.
/// A positive length in pixels: `40px` or `40`.
fn pixels(value: &str) -> Result<f64, Error> {
    let v: f64 = value.strip_suffix("px").unwrap_or(value).parse().map_err(|_| Error::Invalid)?;
    if !(v > 0.0 && v.is_finite()) {
        return Err(Error::Hint("it must be more than 0".to_owned()));
    }
    Ok(v)
}

/// A duration in ghostty's syntax, with the units that make sense for a
/// transition: `150ms`, `1s`, or `0`.
fn duration(value: &str) -> Result<Duration, Error> {
    if value == "0" {
        return Ok(Duration::ZERO);
    }
    let (digits, unit) = value.split_at(value.find(|c: char| !c.is_ascii_digit()).unwrap_or(value.len()));
    let n: u64 = digits.parse().map_err(|_| Error::Invalid)?;
    match unit {
        "ms" => Ok(Duration::from_millis(n)),
        "s" => Ok(Duration::from_secs(n)),
        _ => Err(Error::Hint("it must be a duration such as 150ms".to_owned())),
    }
}

fn fraction(value: &str, min: f64) -> Result<f64, Error> {
    let v: f64 = value.parse().map_err(|_| Error::Invalid)?;
    if !(min..=1.0).contains(&v) {
        return Err(Error::Hint(format!("it must be from {min} to 1")));
    }
    Ok(v)
}

/// window-width or -height: 0 for none, else at least `min` cells, which
/// ghostty would make of a smaller size without a word.
fn cells(value: &str, min: u32, side: &str) -> Result<u32, Error> {
    let cells = integer(value).ok_or(Error::Invalid)?;
    if cells != 0 && cells < min {
        return Err(Error::Hint(format!("a window is at least {min} cells {side}")));
    }
    Ok(cells)
}

/// ghostty's WindowPadding: one number of points for both sides, or two
/// separated by a comma.
fn padding(value: &str) -> Result<(u32, u32), Error> {
    let points = |v: &str| v.trim_matches([' ', '\t']).parse::<u32>().map_err(|_| Error::Invalid);
    match value.split_once(',') {
        Some((start, end)) => Ok((points(start)?, points(end)?)),
        None => {
            let both = points(value)?;
            Ok((both, both))
        }
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
    fn split_resize_transition() {
        use std::time::Duration;
        assert_eq!(Config::default().split_resize_transition, Duration::ZERO);
        assert_eq!(ok("split-resize-transition = 150ms").split_resize_transition, Duration::from_millis(150));
        assert_eq!(ok("split-resize-transition = 1s").split_resize_transition, Duration::from_secs(1));
        assert_eq!(ok("split-resize-transition = 150ms\nsplit-resize-transition = 0").split_resize_transition, Duration::ZERO);
        assert_eq!(ok("split-resize-transition = 150ms\nsplit-resize-transition =").split_resize_transition, Duration::ZERO);
        assert_eq!(errors("split-resize-transition = ms"), ["/c/config:1:split-resize-transition: invalid value \"ms\""]);
        assert_eq!(errors("split-resize-transition = 2m").len(), 1);
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

    /// A fresh directory under target/ for a test's files.
    fn test_dir(name: &str) -> PathBuf {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target").join(name);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("themes")).unwrap();
        std::fs::canonicalize(dir).unwrap()
    }

    fn write(dir: &Path, name: &str, text: &str) {
        std::fs::write(dir.join(name), text).unwrap();
    }

    /// The configuration `dir/config` loads, with `dir/themes` for the
    /// user's themes, for a light or `dark` desktop.
    fn load_dir(dir: &Path, dark: bool) -> (Config, Vec<String>) {
        let mut loader = Loader::new(&fonts);
        loader.load(&dir.join("config"));
        loader.load_theme(&dir.join("themes"), dark);
        (loader.config, loader.diagnostics.iter().map(ToString::to_string).collect())
    }

    #[test]
    fn title_bar_and_tab_options() {
        let c = ok("gtk-titlebar = false\ngtk-titlebar-hide-when-maximized\ngtk-titlebar-style = tabs\n\
                    gtk-tabs-location = bottom\ngtk-tabs-inverted = true\ngtk-wide-tabs = false\n\
                    gtk-toolbar-style = raised-border\nwindow-show-tab-bar = always");
        assert!(!c.gtk_titlebar && c.gtk_titlebar_hide_when_maximized && c.gtk_tabs_inverted && !c.gtk_wide_tabs);
        assert_eq!((c.gtk_titlebar_style, c.gtk_tabs_location), (TitlebarStyle::Tabs, TabsLocation::Bottom));
        assert_eq!((c.gtk_toolbar_style, c.window_show_tab_bar), (adw::ToolbarStyle::RaisedBorder, ShowTabBar::Always));
        assert!(ok("gtk-titlebar = false\ngtk-titlebar").gtk_titlebar);
        assert!(ok("gtk-wide-tabs = false\ngtk-wide-tabs =").gtk_wide_tabs);
        assert_eq!(ok("gtk-toolbar-style = flat").gtk_toolbar_style, adw::ToolbarStyle::Flat);
        assert_eq!(ok("window-show-tab-bar = never").window_show_tab_bar, ShowTabBar::Never);
        assert_eq!(
            errors("gtk-titlebar-style = hidden\ngtk-tabs-location = left\ngtk-titlebar = no\nwindow-show-tab-bar"),
            [
                "/c/config:1:gtk-titlebar-style: invalid value \"hidden\", valid values are: native, tabs",
                "/c/config:2:gtk-tabs-location: invalid value \"left\", valid values are: top, bottom",
                "/c/config:3:gtk-titlebar: invalid value \"no\"",
                "/c/config:4:window-show-tab-bar: value required, valid values are: always, auto, never",
            ]
        );
    }

    #[test]
    fn window_colors_and_opacity() {
        let c = ok("window-theme = ghostty\nbackground-opacity = 0.8\nbackground-opacity-cells\n\
                    unfocused-split-opacity = 0.4\nunfocused-split-fill = #102030\nsplit-divider-color = red\n\
                    window-titlebar-background = #000\nwindow-titlebar-foreground = white");
        assert_eq!((c.window_theme, c.background_opacity), (WindowTheme::Ghostty, 0.8));
        assert!(c.background_opacity_cells);
        assert_eq!((c.unfocused_split_opacity, c.unfocused_split_fill), (0.4, Some(rgb(0x102030))));
        assert_eq!(c.split_divider_color, Some(rgb(0xff0000)));
        assert_eq!((c.window_titlebar_background, c.window_titlebar_foreground), (Some(rgb(0)), Some(rgb(0xffffff))));
        let themes = [("auto", WindowTheme::Auto), ("system", WindowTheme::System), ("light", WindowTheme::Light)];
        for (text, theme) in themes {
            assert_eq!(ok(&format!("window-theme = {text}")).window_theme, theme);
        }
        assert_eq!(ok("background-opacity = 0\nunfocused-split-opacity = 1").background_opacity, 0.0);
        assert_eq!(ok("background-opacity = 0.5\nbackground-opacity =").background_opacity, 1.0);
        assert_eq!(ok("split-divider-color = red\nsplit-divider-color =").split_divider_color, None);
        assert_eq!(
            errors("background-opacity = 1.5\nunfocused-split-opacity = 0.1\nbackground-opacity = NaN\nwindow-theme=X"),
            [
                "/c/config:1:background-opacity: invalid value \"1.5\", it must be from 0 to 1",
                "/c/config:2:unfocused-split-opacity: invalid value \"0.1\", it must be from 0.15 to 1",
                "/c/config:3:background-opacity: invalid value \"NaN\", it must be from 0 to 1",
                "/c/config:4:window-theme: invalid value \"X\", valid values are: auto, system, light, dark, ghostty",
            ]
        );
    }

    #[test]
    fn window_decoration_takes_booleans_but_not_server() {
        for (text, decoration) in [
            ("window-decoration", Decoration::Auto),
            ("window-decoration = true", Decoration::Auto),
            ("window-decoration = false", Decoration::None),
            ("window-decoration = client", Decoration::Client),
            ("window-decoration = none\nwindow-decoration =", Decoration::Auto),
        ] {
            assert_eq!(ok(text).window_decoration, decoration, "{text}");
        }
        assert_eq!(
            errors("window-decoration = server\nwindow-decoration = ssd"),
            [
                concat!(
                    "/c/config:1:window-decoration: invalid value \"server\", tapust cannot ask the compositor to ",
                    "draw the decorations, valid values are: auto, client, none"
                ),
                "/c/config:2:window-decoration: invalid value \"ssd\", valid values are: auto, client, none",
            ]
        );
    }

    #[test]
    fn window_size_padding_subtitle_and_new_tabs() {
        let c = ok("window-width = 100\nwindow-height = 0x20\nwindow-padding-x = 4\nwindow-padding-y = 1, 6\n\
                    window-padding-balance = true\nwindow-padding-color = extend-always\n\
                    window-subtitle = working-directory\nwindow-new-tab-position = end");
        assert_eq!((c.window_width, c.window_height), (100, 32));
        assert_eq!((c.window_padding_x, c.window_padding_y), ((4, 4), (1, 6)));
        assert_eq!(c.window_padding_balance, PaddingBalance::True);
        assert_eq!(c.window_padding_color, PaddingColor::ExtendAlways);
        assert_eq!((c.window_subtitle, c.window_new_tab_position), (Subtitle::WorkingDirectory, NewTabPosition::End));
        assert_eq!(ok("window-padding-balance = equal").window_padding_balance, PaddingBalance::Equal);
        assert_eq!(ok("window-width = 0\nwindow-height = 4").window_height, 4);
        assert_eq!(ok("window-padding-x = 9\nwindow-padding-x =").window_padding_x, (2, 2));
        assert_eq!(
            errors("window-width = 9\nwindow-height = 3\nwindow-padding-x = 1,x\nwindow-padding-y = -1\n\
                    window-padding-color = extended\nwindow-new-tab-position = start"),
            [
                "/c/config:1:window-width: invalid value \"9\", a window is at least 10 cells wide",
                "/c/config:2:window-height: invalid value \"3\", a window is at least 4 cells high",
                "/c/config:3:window-padding-x: invalid value \"1,x\"",
                "/c/config:4:window-padding-y: invalid value \"-1\"",
                concat!(
                    "/c/config:5:window-padding-color: invalid value \"extended\", ",
                    "valid values are: background, extend, extend-always"
                ),
                "/c/config:6:window-new-tab-position: invalid value \"start\", valid values are: current, end",
            ]
        );
    }

    #[test]
    fn themes_parse_as_a_name_or_a_light_and_dark_pair() {
        let same = |name: &str| Some(Theme { light: name.to_owned(), dark: name.to_owned() });
        assert_eq!(parse("theme = Tomorrow Night").0.theme, same("Tomorrow Night"));
        assert_eq!(parse("theme = \"  Nord  \"").0.theme, same("Nord"));
        assert_eq!(parse("theme = /t/x").0.theme, same("/t/x"));
        let home = home().unwrap().join("t").to_string_lossy().into_owned();
        assert_eq!(parse("theme = ~/t").0.theme, same(&home));
        let pair = Theme { light: "Day".to_owned(), dark: "Night Owl".to_owned() };
        assert!(pair.conditional() && !Theme { light: "x".to_owned(), dark: "x".to_owned() }.conditional());
        assert_eq!(parse("theme = dark : Night Owl,  light:Day ").0.theme, Some(pair));
        assert_eq!(parse("theme = Nord\ntheme =").0.theme, None);
        assert_eq!(
            parse("theme = light:Day\ntheme = light:Day,dusk:Night\ntheme = Day=Night").1,
            [
                "/c/config:1:theme: invalid value \"light:Day\", a theme pair needs both light: and dark:",
                "/c/config:2:theme: invalid value \"light:Day,dusk:Night\"",
                "/c/config:3:theme: invalid value \"Day=Night\"",
            ]
        );
    }

    #[test]
    fn a_bundled_theme_sets_its_colors_and_the_configuration_wins() {
        let dir = test_dir("test-theme-bundled");
        // Lines before and after the theme line both override it.
        write(&dir, "config", "background = #123456\ntheme = Tomorrow Night\npalette = 1=#010101\ncursor-color =\n");
        let (c, d) = load_dir(&dir, false);
        assert!(d.is_empty(), "{d:?}");
        assert_eq!((c.background, c.foreground), (rgb(0x123456), rgb(0xc5c8c6)));
        assert_eq!((c.palette[0], c.palette[1], c.palette[8]), (rgb(0), rgb(0x010101), rgb(0x4c4c4c)));
        assert_eq!(c.palette[16], theme::palette()[16]);
        assert_eq!(c.cursor_color, None);
        assert_eq!(c.selection_background, Some(TerminalColor::Rgb(rgb(0x373b41))));
        // An empty palette line resets the theme's palette too, as in ghostty.
        write(&dir, "config", "theme = Tomorrow Night\npalette =\n");
        assert_eq!(load_dir(&dir, false).0.palette, theme::palette());
    }

    #[test]
    fn the_users_themes_come_before_the_bundled_ones() {
        let dir = test_dir("test-theme-user");
        write(&dir, "themes/Tomorrow Night", "background = #000001\n");
        write(&dir, "config", "theme = Tomorrow Night\n");
        let (c, d) = load_dir(&dir, false);
        assert!(d.is_empty(), "{d:?}");
        assert_eq!((c.background, c.foreground), (rgb(0x000001), Config::default().foreground));
        write(&dir, "elsewhere", "background = #000002\n");
        write(&dir, "config", &format!("theme = {}/elsewhere\n", dir.display()));
        assert_eq!(load_dir(&dir, false).0.background, rgb(0x000002));
    }

    #[test]
    fn a_theme_pair_takes_the_desktops_side() {
        let dir = test_dir("test-theme-pair");
        write(&dir, "themes/Mine", "background = #000003\n");
        write(&dir, "config", "theme = light:Mine,dark:Tomorrow Night\n");
        assert_eq!(load_dir(&dir, false).0.background, rgb(0x000003));
        let (c, d) = load_dir(&dir, true);
        assert!(d.is_empty(), "{d:?}");
        assert_eq!(c.background, rgb(0x1d1f21));
        assert!(c.theme.unwrap().conditional());
    }

    #[test]
    fn theme_errors() {
        let dir = test_dir("test-theme-errors");
        write(&dir, "config", "font-size = 13\ntheme = Nope\ntheme = Missing\n");
        let (c, d) = load_dir(&dir, false);
        let (config, themes) = (dir.join("config"), dir.join("themes"));
        let at = format!("{}:3:theme: theme \"Missing\" not found", config.display());
        assert_eq!(
            d,
            [
                format!("{at}, tried path \"{}/Missing\"", themes.display()),
                format!("{at} among the themes bundled with tapust"),
            ]
        );
        assert_eq!((c.font_size, c.background), (13.0, Config::default().background));
        write(&dir, "config", "theme = sub/Nope\n");
        let separator = "theme: theme \"sub/Nope\" cannot include path separators unless it is an absolute path";
        assert_eq!(load_dir(&dir, false).1, [format!("{}:1:{separator}", config.display())]);
        write(&dir, "config", &format!("theme = {}/none\n", dir.display()));
        let absent = format!("{}:1:theme: failed to load theme from the path", config.display());
        assert_eq!(load_dir(&dir, false).1, [format!("{absent} \"{}/none\"", dir.display())]);
        // A theme cannot name another theme or file; its own errors name it.
        write(&dir, "themes/Bad", "theme = Nord\nconfig-file = x\nbackground = nope\nforeground = #010101\n");
        write(&dir, "config", "theme = Bad\n");
        let (c, d) = load_dir(&dir, false);
        let bad = themes.join("Bad");
        assert_eq!(
            d,
            [
                format!("{}:1:theme: a theme file cannot set theme", bad.display()),
                format!("{}:2:config-file: a theme file cannot set config-file", bad.display()),
                format!("{}:3:background: invalid value \"nope\"", bad.display()),
            ]
        );
        assert_eq!(c.foreground, rgb(0x010101));
    }

    #[test]
    fn custom_css_files_read_with_the_configuration() {
        let dir = test_dir("test-custom-css");
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        write(&dir, "style.css", "window { opacity: 0.5; }");
        write(&dir, "sub/more.css", "label { color: red; }");
        write(&dir, "sub/config", "gtk-custom-css = more.css\n");
        write(
            &dir,
            "config",
            "gtk-custom-css = ./style.css\ngtk-custom-css = ?absent.css\ngtk-custom-css = gone.css\n\
             gtk-custom-css = ?\nconfig-file = sub/config\n",
        );
        let (c, d) = load_dir(&dir, false);
        let sheets: Vec<(PathBuf, &str)> = c.gtk_custom_css.iter().map(|s| (s.path.clone(), s.css.as_str())).collect();
        assert_eq!(
            sheets,
            [(dir.join("style.css"), "window { opacity: 0.5; }"), (dir.join("sub/more.css"), "label { color: red; }")]
        );
        assert_eq!(
            d,
            [format!(
                "{}/config:3:gtk-custom-css: error opening gtk-custom-css file {}/gone.css: {}",
                dir.display(),
                dir.display(),
                "No such file or directory (os error 2)"
            )]
        );
        write(&dir, "config", "gtk-custom-css = style.css\ngtk-custom-css =\n");
        assert_eq!(load_dir(&dir, false).0.gtk_custom_css, []);
    }
}
