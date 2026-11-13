//! The application windows as ghostty builds them with GTK and libadwaita
//! (window.blp, tab.blp, split_tree.zig), driven by the tmux model of the
//! `mux` crate: every GTK window shows one tmux session, every tmux window
//! of it is a tab, every tab holds the split tree. Tabs dragged between
//! windows move their tmux windows between the sessions.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::ffi::OsString;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::rc::{Rc, Weak};
use std::time::{Duration, Instant};

use adw::prelude::*;
use gtk::{gdk, gio, glib};
use mux::{
    Command, ConfirmTarget, Copied, CopyResult, Effect, InputKind, KeyResult, ModeKeys, NewAt, Node, Overlay, PaneId,
    Rect, Server, Session,
};
use term::Rgb;

use crate::attention::Attention;
use crate::bindings::{Binding, Press, Scroll};
use crate::config::{
    self, Command as ConfigCommand, Config, ConfirmClose, Decoration, Diagnostic, NewTabPosition, ShowTabBar, Subtitle,
    TabsLocation, Theme, TitlebarStyle, WindowTheme, WorkingDirectory,
};
use crate::font;
use crate::input;
use crate::notifications;
use crate::palette::{self, Run};
use crate::pty::{ChildExit, PaneIo, Program, Pty};
use crate::surface::{self, Event, Surface};
use crate::theme;

/// How often foreground process names are checked (tmux `NAME_INTERVAL`).
const NAME_INTERVAL: Duration = Duration::from_millis(500);

thread_local! {
    static APP: RefCell<Option<Rc<App>>> = const { RefCell::new(None) };
}

struct Pane {
    surface: Rc<Surface>,
    io: Option<PaneIo<term::Terminal>>,
    /// The directory the shell starts in.
    cwd: PathBuf,
    /// What the pane runs instead of the configured command (`-e`).
    program: Option<Vec<OsString>>,
    /// The terminal title (OSC 0/2) last seen; empty until the program sets one.
    title: String,
    /// ghostty's surface title override (Change Terminal Title).
    title_override: Option<String>,
    /// The working directory the terminal reported (OSC 7), ghostty's pwd.
    pwd: Option<String>,
    /// ghostty's child_exited: the exit was handled and the pane stayed.
    child_exited: bool,
}

/// A tab: ghostty's Tab (a vertical box) holding a SplitTree whose tree_bin
/// shows the widgets built from the window's split tree.
struct Tab {
    page: adw::TabPage,
    tree_bin: adw::Bin,
    /// The tree and zoomed pane the widgets were built from.
    built: Option<(Node, Option<PaneId>)>,
    splits: Vec<Rc<SplitWidget>>,
    /// libadwaita took the page out of the tab view for a drag, and has
    /// not attached it to a tab view again yet.
    detached: bool,
}

struct State {
    session: Session,
    panes: HashMap<PaneId, Pane>,
    /// Tabs by tmux window id.
    tabs: HashMap<u32, Tab>,
    /// The prompt or message on screen, as a dialog or a toast.
    shown: Option<Overlay>,
    window_title_override: Option<String>,
    tick: Option<glib::SourceId>,
}

/// The application: tmux's server, with one window per session.
pub struct App {
    application: adw::Application,
    server: Server,
    windows: RefCell<Vec<Rc<Window>>>,
    /// The window showing each pane, which gets the pane's events.
    owners: RefCell<HashMap<PaneId, Weak<Window>>>,
    /// The directory tapust started in (working-directory = inherit).
    cwd: PathBuf,
    /// What the next new pane runs instead of the configured command: `-e`
    /// for the first pane, the editor for Open Configuration in New Window.
    command: RefCell<Option<Vec<OsString>>>,
    config: RefCell<Rc<Config>>,
    /// The configuration was read for a dark style (ghostty's conditional
    /// theme state).
    theme_dark: Cell<bool>,
    /// The stylesheet, whose runtime rules follow the configuration.
    css: gtk::CssProvider,
    /// The gtk-custom-css files, over the stylesheet.
    custom_css: RefCell<Vec<gtk::CssProvider>>,
    /// ghostty's config errors dialog while it is open, with its text. A
    /// dialog presented as a window does not tell when it closes: it is gone
    /// once its window is.
    errors: RefCell<Option<(glib::WeakRef<adw::AlertDialog>, glib::WeakRef<gtk::TextView>)>>,
    /// When the last desktop notification was shown and its hash (ghostty's
    /// last_notification_time and last_notification_digest).
    last_notification: Cell<Option<(Instant, u64)>>,
    attention: Rc<Attention>,
}

/// A GTK window (ghostty's Window) showing one tmux session; it is the
/// session's client.
struct Window {
    app: Rc<App>,
    state: RefCell<State>,
    window: adw::ApplicationWindow,
    tab_view: adw::TabView,
    toasts: adw::ToastOverlay,
    title: adw::WindowTitle,
    header: adw::HeaderBar,
    tab_bar: adw::TabBar,
    toolbar: adw::ToolbarView,
    /// The header bar's buttons and the window's at the start and end of
    /// the tab bar, for gtk-titlebar-style = tabs.
    tab_bar_buttons: [gtk::Box; 2],
    /// Where the tab bar is now.
    tabs_location: Cell<TabsLocation>,
    /// Set while the application changes GTK, so its own changes do not
    /// come back as user actions.
    syncing: Cell<bool>,
    reconcile_pending: Cell<bool>,
    /// The keycode of the last key pressed and not released yet.
    held_key: Cell<Option<u32>>,
    restore_focus_pending: Cell<bool>,
    /// The window's minimum size was set from the first surface.
    min_size_set: Cell<bool>,
    /// The tab whose context menu is open (ghostty's context_menu_page).
    context_page: RefCell<Option<adw::TabPage>>,
    overview: adw::TabOverview,
    overview_focus: RefCell<Option<glib::SourceId>>,
    /// The session ended while the tab overview was open: the window
    /// stays (ghostty's tabViewNPages) and the next new tab starts a session.
    session_ended: Cell<bool>,
    /// The window is going away: GTK no longer shows the model in it.
    closing: Cell<bool>,
}

/// ghostty's SplitTreeSplit: Adw.Bin > Adw.Bin > GtkPaned, keeping the
/// widget's ratio in sync with the tree (syncSplitRatio).
struct SplitWidget {
    bin: adw::Bin,
    paned: gtk::Paned,
    window: u32,
    /// In-order index in the tree, which changes when splits come or go.
    index: Cell<usize>,
    /// ghostty's priv.ratio: the tree's when built, then the last one written.
    ratio: Cell<f64>,
    /// The position was set to follow the tree's ratio (after a limit
    /// changed or the tree got a new ratio), not dragged.
    followed: Cell<bool>,
    pos_changed: Cell<bool>,
    idle: RefCell<Option<glib::SourceId>>,
    app: Weak<App>,
}

#[derive(Clone, Copy, PartialEq)]
enum Sync {
    TreeToWidget,
    WidgetToTree,
    ClampedToTree,
}

impl SplitWidget {
    fn new(app: &Rc<App>, window: u32, index: usize, horizontal: bool, ratio: f64, start: &gtk::Widget, end: &gtk::Widget) -> Rc<SplitWidget> {
        let orientation = if horizontal { gtk::Orientation::Horizontal } else { gtk::Orientation::Vertical };
        let paned = gtk::Paned::builder().orientation(orientation).shrink_start_child(false).shrink_end_child(false).build();
        paned.set_start_child(Some(start));
        paned.set_end_child(Some(end));
        let bin = theme::styled(adw::Bin::builder().child(&adw::Bin::builder().child(&paned).build()).build(), &["split"]);
        let split = Rc::new(SplitWidget {
            bin,
            paned,
            window,
            index: Cell::new(index),
            ratio: Cell::new(ratio),
            followed: Cell::new(false),
            pos_changed: Cell::new(false),
            idle: RefCell::new(None),
            app: Rc::downgrade(app),
        });
        for property in ["min-position", "max-position"] {
            let weak = Rc::downgrade(&split);
            split.paned.connect_notify_local(Some(property), move |_, _| {
                if let Some(s) = weak.upgrade() {
                    // A resize of the paned or of a child's minimum: the
                    // position follows the ratio right away.
                    s.followed.set(true);
                    s.sync(Sync::TreeToWidget);
                    s.queue_idle();
                }
            });
        }
        let weak = Rc::downgrade(&split);
        split.paned.connect_notify_local(Some("position"), move |_, _| {
            if let Some(s) = weak.upgrade() {
                s.pos_changed.set(true);
                s.queue_idle();
            }
        });
        split
    }

    fn queue_idle(self: &Rc<Self>) {
        if self.idle.borrow().is_some() {
            return;
        }
        let weak = Rc::downgrade(self);
        let id = glib::idle_add_local_once(move || {
            let Some(s) = weak.upgrade() else { return };
            s.idle.take();
            let (followed, pos) = (s.followed.replace(false), s.pos_changed.replace(false));
            // Only the position changed: the user dragged the divider.
            if !followed && pos {
                s.sync(Sync::WidgetToTree);
            }
        });
        *self.idle.borrow_mut() = Some(id);
    }

    /// The tree got a new ratio (a resize by key, or a split kept by a
    /// rebuild): the divider moves to it.
    fn follow(self: &Rc<Self>, ratio: f64) {
        self.ratio.set(ratio);
        self.followed.set(true);
        self.sync(Sync::TreeToWidget);
        self.queue_idle();
    }

    /// ghostty's syncSplitRatio.
    fn sync(&self, direction: Sync) {
        let p = &self.paned;
        let (pos, min, max) = (p.position(), p.min_position(), p.max_position());
        if max == i32::MAX {
            return;
        }
        let end_min = p.end_child().map_or(0, |c| c.measure(p.orientation(), -1).0);
        let size = max + end_min;
        if size == 0 {
            return;
        }
        let current = f64::from(pos) / f64::from(size);
        let desired = self.ratio.get();
        if (current - desired).abs() <= 0.001 {
            return;
        }
        let d = f64::from(size) * desired;
        let desired_pos = d.round() as i32;
        let clamped = desired_pos.clamp(min, min.max(max));
        match direction {
            Sync::TreeToWidget => p.set_position(clamped),
            Sync::WidgetToTree => self.set_tree_ratio(current),
            Sync::ClampedToTree => {
                if clamped != desired_pos {
                    self.set_tree_ratio(f64::from(clamped) / f64::from(size));
                }
            }
        }
    }

    /// ghostty's setTreeRatio: the tree keeps the ratio as f16.
    fn set_tree_ratio(&self, ratio: f64) {
        self.ratio.set(ratio);
        if let Some(app) = self.app.upgrade() {
            app.set_ratio(self.window, self.index.get(), ratio);
        }
    }
}

/// Takes the keyboard focus from inside `widget` before it leaves its
/// parent. GTK would move the focus once the widget is gone, and a GtkPaned
/// that stays in the window then warns that it cannot find the focused
/// widget; the rebuild gives the focus to the active pane again.
fn release_focus(widget: &gtk::Widget) {
    if let Some(root) = widget.root()
        && let Some(focus) = root.focus()
        && (focus == *widget || focus.is_ancestor(widget))
    {
        root.set_focus(gtk::Widget::NONE);
    }
}

/// Makes `child` the start or end child of `paned`, moving it only when it
/// is not there already.
fn put(paned: &gtk::Paned, start: bool, child: &gtk::Widget) {
    let current = if start { paned.start_child() } else { paned.end_child() };
    if current.as_ref() == Some(child) {
        return;
    }
    if let Some(current) = &current {
        release_focus(current);
    }
    detach(child);
    if start {
        paned.set_start_child(Some(child));
    } else {
        paned.set_end_child(Some(child));
    }
}

/// Removes a reused widget from its old parent (ghostty's detachWidget).
fn detach(widget: &gtk::Widget) {
    let Some(parent) = widget.parent() else { return };
    release_focus(widget);
    if let Some(paned) = parent.downcast_ref::<gtk::Paned>() {
        if paned.start_child().as_ref() == Some(widget) {
            paned.set_start_child(gtk::Widget::NONE);
        } else {
            paned.set_end_child(gtk::Widget::NONE);
        }
    } else if let Some(bin) = parent.downcast_ref::<adw::Bin>() {
        bin.set_child(gtk::Widget::NONE);
    } else {
        widget.unparent();
    }
}

fn menu_item(menu: &gio::Menu, label: &str, action: &str) {
    menu.append(Some(label), Some(action));
}

/// ghostty's split_menu.
fn split_menu() -> gio::Menu {
    let menu = gio::Menu::new();
    menu_item(&menu, "Split Up", "win.split-up");
    menu_item(&menu, "Split Down", "win.split-down");
    menu_item(&menu, "Split Left", "win.split-left");
    menu_item(&menu, "Split Right", "win.split-right");
    menu
}

/// ghostty's header bar buttons: New Tab with the split menu, View Open
/// Tabs and the main menu; `flat` in the tab bar.
fn header_buttons(overview: &adw::TabOverview, flat: bool) -> (adw::SplitButton, gtk::ToggleButton, gtk::MenuButton) {
    let new_tab = adw::SplitButton::builder()
        .icon_name("tab-new-symbolic")
        .tooltip_text("New Tab")
        .dropdown_tooltip("New Split")
        .menu_model(&split_menu())
        .can_focus(false)
        .focus_on_click(false)
        .build();
    let overview_toggle = gtk::ToggleButton::builder()
        .icon_name("view-grid-symbolic")
        .tooltip_text("View Open Tabs")
        .can_focus(false)
        .focus_on_click(false)
        .build();
    overview.bind_property("open", &overview_toggle, "active").bidirectional().sync_create().build();
    let menu = gtk::MenuButton::builder()
        .icon_name("open-menu-symbolic")
        .menu_model(&main_menu())
        .tooltip_text("Main Menu")
        .can_focus(false)
        .build();
    if flat {
        new_tab.add_css_class("flat");
        overview_toggle.add_css_class("flat");
        menu.add_css_class("flat");
    }
    (new_tab, overview_toggle, menu)
}

fn section(items: &[(&str, &str)]) -> gio::Menu {
    let s = gio::Menu::new();
    for (label, action) in items {
        menu_item(&s, label, action);
    }
    s
}

fn targeted(label: &str, action: &str, target: &str) -> gio::MenuItem {
    let item = gio::MenuItem::new(Some(label), None);
    item.set_action_and_target_value(Some(action), Some(&target.to_variant()));
    item
}

/// ghostty's Config submenu.
fn config_menu() -> gio::Menu {
    let config = gio::Menu::new();
    config.append_item(&targeted("Open Configuration in OS Editor", "app.open-config", "os-open"));
    config.append_item(&targeted("Open Configuration in New Window", "app.open-config", "new-window"));
    menu_item(&config, "Reload Configuration", "app.reload-config");
    config
}

/// ghostty's context_menu_model for the surface.
pub fn context_menu() -> gio::Menu {
    let menu = gio::Menu::new();
    menu.append_section(
        None,
        &section(&[
            ("Copy", "win.copy"),
            ("Paste", "win.paste"),
            ("Notify on Next Command Finish", "surface.notify-on-next-command-finish"),
        ]),
    );
    menu.append_section(None, &section(&[("Clear", "win.clear"), ("Reset", "win.reset")]));
    let split = section(&[
        ("Change Title\u{2026}", "win.prompt-surface-title"),
        ("Split Up", "win.split-up"),
        ("Split Down", "win.split-down"),
        ("Split Left", "win.split-left"),
        ("Split Right", "win.split-right"),
        ("Close Split", "win.close-split"),
    ]);
    let tab = section(&[("Change Tab Title\u{2026}", "win.prompt-tab-title"), ("New Tab", "win.new-tab")]);
    tab.append_item(&targeted("Close Tab", "win.close-tab", "this"));
    let window = section(&[
        ("Change Window Title\u{2026}", "win.prompt-window-title"),
        ("New Window", "win.new-window"),
        ("Close Window", "win.close"),
    ]);
    let submenus = gio::Menu::new();
    submenus.append_submenu(Some("Split"), &split);
    submenus.append_submenu(Some("Tab"), &tab);
    submenus.append_submenu(Some("Window"), &window);
    menu.append_section(None, &submenus);
    let config = gio::Menu::new();
    config.append_submenu(Some("Config"), &config_menu());
    menu.append_section(None, &config);
    menu
}

/// ghostty's main_menu.
fn main_menu() -> gio::Menu {
    let menu = gio::Menu::new();
    menu.append_section(None, &section(&[("Copy", "win.copy"), ("Paste", "win.paste")]));
    menu.append_section(
        None,
        &section(&[("Change Window Title\u{2026}", "win.prompt-window-title"), ("New Window", "win.new-window"), ("Close Window", "win.close")]),
    );
    let tab = section(&[("Change Tab Title\u{2026}", "win.prompt-tab-title"), ("New Tab", "win.new-tab")]);
    tab.append_item(&targeted("Close Tab", "win.close-tab", "this"));
    menu.append_section(None, &tab);
    let split = section(&[
        ("Change Title\u{2026}", "win.prompt-surface-title"),
        ("Split Up", "win.split-up"),
        ("Split Down", "win.split-down"),
        ("Split Left", "win.split-left"),
        ("Split Right", "win.split-right"),
    ]);
    let splits = gio::Menu::new();
    splits.append_submenu(Some("Split"), &split);
    menu.append_section(None, &splits);
    menu.append_section(None, &section(&[("Clear", "win.clear"), ("Reset", "win.reset")]));
    let tools = section(&[("Command Palette", "win.toggle-command-palette"), ("Terminal Inspector", "win.toggle-inspector")]);
    tools.append_submenu(Some("Config"), &config_menu());
    menu.append_section(None, &tools);
    menu.append_section(None, &section(&[("About tapust", "win.about"), ("Quit", "app.quit")]));
    menu
}

impl App {
    pub fn build(application: &adw::Application, command: Option<Vec<OsString>>) -> Rc<App> {
        let display = gdk::Display::default().expect("display");
        let css = gtk::CssProvider::new();
        gtk::style_context_add_provider_for_display(&display, &css, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
        let app = Rc::new(App {
            application: application.clone(),
            server: Server::new(),
            windows: RefCell::new(Vec::new()),
            owners: RefCell::new(HashMap::new()),
            cwd: std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/")),
            command: RefCell::new(command),
            config: RefCell::new(Rc::new(Config::default())),
            theme_dark: Cell::new(false),
            css,
            custom_css: RefCell::new(Vec::new()),
            errors: RefCell::new(None),
            last_notification: Cell::new(None),
            attention: Attention::new(&display),
        });
        APP.with(|a| *a.borrow_mut() = Some(app.clone()));
        let (config, mut diagnostics) = app.read_config();
        *app.config.borrow_mut() = Rc::new(config);
        diagnostics.extend(app.apply_config());
        app.add_actions();
        app.follow_style();
        app.new_window(None);
        // ghostty shows the errors of the configuration it starts with on
        // their own, not knowing whether a window will come.
        app.show_errors(&diagnostics);
        app
    }

    pub fn config(&self) -> Rc<Config> {
        self.config.borrow().clone()
    }

    /// The configuration file read over the theme for the style it asks
    /// for: a light and dark theme pair takes the side the desktop is on,
    /// unless window-theme forces one.
    fn read_config(&self) -> (Config, Vec<Diagnostic>) {
        let style = self.application.style_manager();
        let dark = style.is_dark();
        let (config, diagnostics) = config::load(&font::family_exists, dark);
        self.theme_dark.set(dark);
        if config.theme.as_ref().is_some_and(Theme::conditional) {
            style.set_color_scheme(color_scheme(&config));
            if style.is_dark() != dark {
                self.theme_dark.set(!dark);
                return config::load(&font::family_exists, !dark);
            }
        }
        (config, diagnostics)
    }

    /// What the configuration sets for the whole application: ghostty's
    /// window-theme, its runtime stylesheet, the gtk-custom-css files, and
    /// tmux's prefix and mode-keys. Returns the stylesheets' errors.
    fn apply_config(&self) -> Vec<Diagnostic> {
        let config = self.config();
        self.application.style_manager().set_color_scheme(color_scheme(&config));
        self.sync_css_scheme();
        self.css.load_from_string(&css(&config));
        let errors = self.load_custom_css(&config);
        self.server.set_prefix(config.tmux_prefix);
        self.server.set_mode_keys(mode_keys(&config));
        errors
    }

    /// ghostty's loadCustomCss: each gtk-custom-css file in a stylesheet over
    /// tapust's own, with its parsing errors, which ghostty only logs.
    fn load_custom_css(&self, config: &Config) -> Vec<Diagnostic> {
        let display = gdk::Display::default().expect("display");
        for provider in self.custom_css.take() {
            gtk::style_context_remove_provider_for_display(&display, &provider);
        }
        let errors = Rc::new(RefCell::new(Vec::new()));
        let scheme = css_scheme(&self.application.style_manager());
        let mut providers = Vec::new();
        for sheet in &config.gtk_custom_css {
            let provider = gtk::CssProvider::builder().prefers_color_scheme(scheme).build();
            let (found, path) = (Rc::downgrade(&errors), sheet.path.clone());
            // The errors are told once: not again when the style changes and
            // GTK parses the file anew.
            provider.connect_parsing_error(move |_, section, error| {
                let Some(found) = found.upgrade() else { return };
                let start = section.start_location();
                let message = format!("css parsing failed at column {}: {}", start.line_chars() + 1, error.message());
                let location = Some((path.clone(), start.lines() + 1));
                found.borrow_mut().push(Diagnostic { location, key: String::new(), message });
            });
            provider.load_from_string(&sheet.css);
            gtk::style_context_add_provider_for_display(&display, &provider, gtk::STYLE_PROVIDER_PRIORITY_USER);
            providers.push(provider);
        }
        *self.custom_css.borrow_mut() = providers;
        errors.take()
    }

    /// ghostty's handleStyleManagerDark for the stylesheets: their
    /// `prefers-color-scheme` media queries follow the style.
    fn sync_css_scheme(&self) {
        let scheme = css_scheme(&self.application.style_manager());
        for provider in std::iter::once(&self.css).chain(self.custom_css.borrow().iter()) {
            provider.set_prefers_color_scheme(scheme);
        }
    }

    /// A light and dark theme pair follows the desktop: once its style
    /// changes, the configuration is read again (ghostty's soft reload).
    fn follow_style(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        self.application.style_manager().connect_dark_notify(move |style| {
            let Some(app) = weak.upgrade() else { return };
            app.sync_css_scheme();
            let conditional = app.config().theme.as_ref().is_some_and(Theme::conditional);
            if conditional && style.is_dark() != app.theme_dark.get() {
                // Not while the color scheme that changed it is being set.
                let weak = Rc::downgrade(&app);
                glib::idle_add_local_once(move || {
                    if let Some(app) = weak.upgrade()
                        && app.application.style_manager().is_dark() != app.theme_dark.get()
                    {
                        app.reload_config();
                    }
                });
            }
        });
    }

    /// ghostty's reload_config: the file read again, and what it sets
    /// applied to the application, every window and every pane, whose
    /// programs go on.
    fn reload_config(self: &Rc<Self>) {
        let (config, mut diagnostics) = self.read_config();
        let config = Rc::new(config);
        *self.config.borrow_mut() = config.clone();
        diagnostics.extend(self.apply_config());
        let keys = self.server.mode_keys();
        for window in self.windows() {
            window.sync_appearance();
            window.queue_reconcile();
            let surfaces: Vec<Rc<Surface>> = window.state.borrow().panes.values().map(|p| p.surface.clone()).collect();
            for surface in surfaces {
                surface.apply_config(config.clone());
                surface.set_mode_keys(keys);
            }
        }
        self.show_errors(&diagnostics);
    }

    /// ghostty's ConfigErrorsDialog: the errors listed, to ignore or to
    /// reload the configuration after fixing them. An open dialog shows the
    /// new errors, and goes away when there are none.
    fn show_errors(self: &Rc<Self>, diagnostics: &[Diagnostic]) {
        let text: String = diagnostics.iter().map(|d| format!("{d}\n")).collect();
        let open = self.errors.borrow().as_ref().and_then(|(dialog, view)| dialog.upgrade().zip(view.upgrade()));
        match open {
            Some((dialog, _)) if diagnostics.is_empty() => {
                self.errors.borrow_mut().take();
                dialog.force_close();
            }
            Some((_, view)) => view.buffer().set_text(&text),
            None if diagnostics.is_empty() => {}
            None => {
                let dialog = adw::AlertDialog::new(
                    Some("Configuration Errors"),
                    Some(concat!(
                        "One or more configuration errors were found. Please review the errors below, ",
                        "and either reload your configuration or ignore these errors."
                    )),
                );
                dialog.add_response("ignore", "Ignore");
                dialog.add_response("reload", "Reload Configuration");
                dialog.set_response_appearance("reload", adw::ResponseAppearance::Suggested);
                let view = gtk::TextView::builder()
                    .editable(false)
                    .cursor_visible(false)
                    .top_margin(8)
                    .bottom_margin(8)
                    .left_margin(8)
                    .right_margin(8)
                    .build();
                view.buffer().set_text(&text);
                let scrolled =
                    gtk::ScrolledWindow::builder().min_content_width(500).min_content_height(100).child(&view).build();
                dialog.set_extra_child(Some(&scrolled));
                let weak = Rc::downgrade(self);
                dialog.connect_response(Some("reload"), move |_, _| {
                    if let Some(app) = weak.upgrade() {
                        // The dialog closes after its response: errors of the
                        // reload need a new one.
                        app.errors.borrow_mut().take();
                        app.reload_config();
                    }
                });
                *self.errors.borrow_mut() = Some((dialog.downgrade(), view.downgrade()));
                dialog.present(None::<&gtk::Widget>);
            }
        }
    }

    /// ghostty's open_config: the configuration file, made with a short
    /// header if missing, in the desktop's editor for it, or in a new window
    /// running $VISUAL or $EDITOR (vi without them).
    fn open_config(self: &Rc<Self>, new_window: bool) {
        let path = match config::path().and_then(|path| create_config(&path).map(|()| path)) {
            Ok(path) => path,
            Err(err) => {
                self.toast(&err);
                return;
            }
        };
        if !new_window {
            surface::xdg_open(path.as_os_str());
            return;
        }
        let editor = ["VISUAL", "EDITOR"].iter().find_map(|v| std::env::var_os(v).filter(|e| !e.is_empty()));
        let mut line = editor.unwrap_or_else(|| "vi".into());
        line.push(" ");
        line.push(shell_quote(path.as_os_str()));
        *self.command.borrow_mut() = Some(vec!["/bin/sh".into(), "-c".into(), line]);
        self.new_window(None);
    }

    /// A toast in the active window, or the first one.
    fn toast(&self, text: &str) {
        let windows = self.windows();
        if let Some(window) = windows.iter().find(|w| w.window.is_active()).or(windows.first()) {
            window.toast(text);
        }
    }

    /// The directory of a pane that does not take its parent's: ghostty's
    /// working-directory.
    fn default_cwd(&self) -> PathBuf {
        match &self.config().working_directory {
            WorkingDirectory::Inherit => self.cwd.clone(),
            // ghostty inherits when there is no home directory.
            WorkingDirectory::Home => config::home().unwrap_or_else(|| self.cwd.clone()),
            WorkingDirectory::Path(path) => path.clone(),
        }
    }

    /// new-session in a new window (ghostty's new_window): the first pane
    /// takes the directory and font size of `parent`.
    fn new_window(self: &Rc<Self>, parent: Option<PaneId>) {
        let (session, effects) = Session::new(&self.server, parent);
        let window = self.add_window(session);
        window.apply(effects);
        window.reconcile();
        window.window.present();
    }

    fn add_window(self: &Rc<Self>, session: Session) -> Rc<Window> {
        let window = Window::build(self, session);
        let (weak, gone) = (Rc::downgrade(self), Rc::downgrade(&window));
        window.window.connect_destroy(move |_| {
            if let Some(app) = weak.upgrade() {
                app.windows.borrow_mut().retain(|w| Rc::as_ptr(w) != gone.as_ptr());
            }
        });
        self.windows.borrow_mut().push(window.clone());
        window
    }

    fn add_actions(self: &Rc<Self>) {
        let present = gio::SimpleAction::new("present-surface", Some(glib::VariantTy::UINT64));
        let weak = Rc::downgrade(self);
        present.connect_activate(move |_, parameter| {
            if let (Some(app), Some(id)) = (weak.upgrade(), parameter.and_then(|p| p.get::<u64>())) {
                app.present_surface(id);
            }
        });
        self.application.add_action(&present);
        let quit = gio::SimpleAction::new("quit", None);
        let weak = Rc::downgrade(self);
        quit.connect_activate(move |_, _| {
            if let Some(app) = weak.upgrade() {
                app.quit();
            }
        });
        self.application.add_action(&quit);
        // ghostty's app.open-config with the target os-open or new-window.
        let open = gio::SimpleAction::new("open-config", Some(glib::VariantTy::STRING));
        let weak = Rc::downgrade(self);
        open.connect_activate(move |_, parameter| {
            if let (Some(app), Some(target)) = (weak.upgrade(), parameter.and_then(|p| p.get::<String>())) {
                app.open_config(target == "new-window");
            }
        });
        self.application.add_action(&open);
        let reload = gio::SimpleAction::new("reload-config", None);
        let weak = Rc::downgrade(self);
        reload.connect_activate(move |_, _| {
            if let Some(app) = weak.upgrade() {
                app.reload_config();
            }
        });
        self.application.add_action(&reload);
        // A request to stop from outside ends the sessions like an
        // unconfirmed quit, so the panes hang up their programs.
        for signal in [libc::SIGTERM, libc::SIGHUP, libc::SIGINT] {
            let weak = Rc::downgrade(self);
            glib::unix_signal_add_local(signal, move || {
                if let Some(app) = weak.upgrade() {
                    app.close_all();
                }
                glib::ControlFlow::Continue
            });
        }
    }

    fn windows(&self) -> Vec<Rc<Window>> {
        self.windows.borrow().clone()
    }

    fn owner(&self, pane: PaneId) -> Option<Rc<Window>> {
        self.owners.borrow().get(&pane).and_then(Weak::upgrade)
    }

    fn set_owner(&self, pane: PaneId, window: &Rc<Window>) {
        self.owners.borrow_mut().insert(pane, Rc::downgrade(window));
    }

    /// A surface's event goes to the window that shows its pane.
    fn handle(&self, pane: PaneId, event: Event) -> bool {
        self.owner(pane).is_some_and(|window| window.handle(event))
    }

    fn wake(&self, pane: PaneId) {
        if let Some(window) = self.owner(pane) {
            window.wake(pane);
        }
    }

    /// What a new pane takes from the pane it was made from, in whichever
    /// window that is: the font size, and with `inherit_cwd` the directory
    /// of its foreground process; else the configured ones.
    fn origin(&self, parent: Option<PaneId>, inherit_cwd: bool) -> (f32, PathBuf) {
        let window = parent.and_then(|p| self.owner(p));
        let st = window.as_ref().map(|w| w.state.borrow());
        let parent = st.as_ref().zip(parent).and_then(|(st, p)| st.panes.get(&p));
        let points = parent.map_or(self.config().font_size, |p| p.surface.font_points());
        let cwd = parent
            .filter(|_| inherit_cwd)
            .and_then(|p| p.io.as_ref())
            .and_then(|io| io.pty.foreground_cwd())
            .unwrap_or_else(|| self.default_cwd());
        (points, cwd)
    }

    /// A split widget of tmux window `window` writes its ratio back, to the
    /// window that shows it now.
    fn set_ratio(&self, window: u32, index: usize, ratio: f64) {
        for w in self.windows() {
            w.set_ratio(window, index, ratio);
        }
    }

    /// The window whose state has the tab of `page`, and the tab's tmux window.
    fn tab_owner(&self, page: &adw::TabPage) -> Option<(Rc<Window>, u32)> {
        self.windows().into_iter().find_map(|w| {
            let id = w.tab_of(page)?;
            Some((w, id))
        })
    }

    /// ghostty's Surface.showDesktopNotification: across surfaces, at most
    /// one notification a second and the same one at most every 5 seconds.
    fn desktop_notification(&self, surface: &Surface, title: &str, body: &str) {
        let now = Instant::now();
        let digest = notifications::hash(title, body);
        if let Some((last, last_digest)) = self.last_notification.get() {
            let since = now - last;
            if since < Duration::from_secs(1) || (last_digest == digest && since < Duration::from_secs(5)) {
                return;
            }
        }
        self.last_notification.set(Some((now, digest)));
        surface.send_desktop_notification(title, body);
    }

    /// ghostty's app.present-surface: the window with the notification's
    /// surface presents it.
    fn present_surface(&self, id: u64) {
        if id == 0 {
            return;
        }
        let window = self.windows().into_iter().find(|w| w.state.borrow().panes.values().any(|p| p.surface.id == id));
        if let Some(window) = window {
            window.present_surface(id);
        }
    }

    /// ghostty's Application.quit, asking once when a pane of any window
    /// needs it: every window closes with its session (kill-server), and
    /// the application ends with the last one.
    fn quit(self: &Rc<Self>) {
        if !self.windows().iter().any(|w| w.needs_confirm()) {
            self.close_all();
            return;
        }
        let dialog = close_dialog("Quit tapust?", "All terminal sessions will be terminated.");
        let weak = Rc::downgrade(self);
        dialog.connect_response(None, move |_, response| {
            if let Some(app) = weak.upgrade()
                && response == "close"
            {
                app.close_all();
            }
        });
        dialog.present(self.application.active_window().as_ref());
    }

    fn close_all(&self) {
        for window in self.windows() {
            window.close();
        }
    }

    /// ghostty's jump commands for the command palette: one for every
    /// terminal of every window, under its title override or title.
    fn focus_entries(&self) -> Vec<palette::Entry> {
        let mut entries = Vec::new();
        for window in self.windows() {
            let st = window.state.borrow();
            // A pane hidden by a zoomed split cannot be presented.
            for p in st.panes.values().filter(|p| p.surface.outer.is_ancestor(&window.window)) {
                let title = p.title_override.as_deref().unwrap_or(&p.title);
                entries.push(palette::focus(p.surface.id, title, p.pwd.as_deref()));
            }
        }
        entries
    }
}

impl Window {
    fn build(app: &Rc<App>, session: Session) -> Rc<Window> {
        let tab_view = adw::TabView::builder().shortcuts(adw::TabViewShortcuts::empty()).build();
        let toasts = adw::ToastOverlay::new();
        toasts.set_child(Some(&tab_view));
        let content = gtk::Box::builder().orientation(gtk::Orientation::Vertical).build();
        content.append(&toasts);

        let title = adw::WindowTitle::new("tapust", "");
        let header = adw::HeaderBar::builder().title_widget(&title).build();
        let overview = adw::TabOverview::builder().view(&tab_view).enable_new_tab(true).build();
        let (new_tab, overview_toggle, menu) = header_buttons(&overview, false);
        header.pack_start(&new_tab);
        let end = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        end.append(&overview_toggle);
        end.append(&menu);
        header.pack_end(&end);

        // ghostty's tab bar, which takes the header bar's place with
        // gtk-titlebar-style = tabs: its buttons, flat, and the window's.
        let tab_bar = adw::TabBar::builder().view(&tab_view).autohide(true).expand_tabs(true).build();
        let (tab_new_tab, tab_overview_toggle, tab_menu) = header_buttons(&overview, true);
        let start = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        start.append(&gtk::WindowControls::new(gtk::PackType::Start));
        start.append(&tab_new_tab);
        let end = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        end.append(&tab_overview_toggle);
        end.append(&tab_menu);
        end.append(&gtk::WindowControls::new(gtk::PackType::End));

        let toolbar = adw::ToolbarView::builder()
            .top_bar_style(adw::ToolbarStyle::Raised)
            .bottom_bar_style(adw::ToolbarStyle::Raised)
            .build();
        toolbar.add_top_bar(&header);
        toolbar.add_top_bar(&tab_bar);
        toolbar.set_content(Some(&content));
        overview.set_child(Some(&toolbar));

        let window = adw::ApplicationWindow::builder()
            .application(&app.application)
            .default_width(800)
            .default_height(600)
            .content(&overview)
            .build();
        window.set_property("handle-menubar-accel", false);
        window.add_css_class("window");
        window.set_icon_name(Some(theme::ICON));
        // ghostty's handleActiveState: an active window needs no attention.
        let attention = app.attention.clone();
        window.connect_is_active_notify(move |w| {
            if w.is_active() {
                attention.cancel(w);
            }
        });
        let attention = app.attention.clone();
        window.connect_unrealize(move |w| attention.cancel(w));

        let win = Rc::new(Window {
            app: app.clone(),
            state: RefCell::new(State {
                session,
                panes: HashMap::new(),
                tabs: HashMap::new(),
                shown: None,
                window_title_override: None,
                tick: None,
            }),
            window,
            tab_view,
            toasts,
            title,
            header,
            tab_bar,
            toolbar,
            tab_bar_buttons: [start, end],
            tabs_location: Cell::new(TabsLocation::Top),
            syncing: Cell::new(false),
            reconcile_pending: Cell::new(false),
            held_key: Cell::new(None),
            restore_focus_pending: Cell::new(false),
            min_size_set: Cell::new(false),
            context_page: RefCell::new(None),
            overview: overview.clone(),
            overview_focus: RefCell::new(None),
            session_ended: Cell::new(false),
            closing: Cell::new(false),
        });

        // A key released while the window is not active ends its repeats too.
        let weak = Rc::downgrade(&win);
        win.window.connect_is_active_notify(move |w| {
            if !w.is_active()
                && let Some(win) = weak.upgrade()
            {
                win.held_key.set(None);
            }
        });

        for button in [new_tab, tab_new_tab] {
            let weak = Rc::downgrade(&win);
            button.connect_clicked(move |_| {
                if let Some(win) = weak.upgrade() {
                    win.new_tab();
                }
            });
        }
        // ghostty's syncAppearance follows the window's state too.
        let weak = Rc::downgrade(&win);
        win.window.connect_fullscreened_notify(move |_| {
            if let Some(win) = weak.upgrade() {
                win.sync_appearance();
            }
        });
        let weak = Rc::downgrade(&win);
        win.window.connect_maximized_notify(move |_| {
            if let Some(win) = weak.upgrade() {
                win.sync_appearance();
            }
        });
        win.sync_appearance();
        win.connect_tabs(&overview);
        win.connect_overview();
        let weak = Rc::downgrade(&win);
        win.window.connect_close_request(move |_| weak.upgrade().map_or(glib::Propagation::Proceed, |win| win.close_request()));
        win.add_actions();
        win.start_timers();
        win
    }

    /// ghostty's syncAppearance and the window properties its blueprint
    /// binds: the decorations, the header bar, the tab bar and its place,
    /// the bars' style, and the window's background behind a translucent
    /// terminal.
    fn sync_appearance(&self) {
        let config = self.app.config();
        let window = &self.window;
        for class in ["ssd", "csd", "solid-csd", "no-border-radius"] {
            window.remove_css_class(class);
        }
        let csd = config.window_decoration != Decoration::None;
        window.set_decorated(csd);
        if csd {
            // GTK's own test for a compositor that shows the shadows.
            let display = WidgetExt::display(window);
            window.add_css_class(if display.is_rgba() && display.is_composited() { "csd" } else { "solid-csd" });
        } else {
            window.add_css_class("ssd");
            window.add_css_class("no-border-radius");
        }
        let hidden_maximized = window.is_maximized() && config.gtk_titlebar_hide_when_maximized;
        let tabs_title = config.gtk_titlebar_style == TitlebarStyle::Tabs;
        let header = csd && !window.is_fullscreen() && !hidden_maximized && !tabs_title && config.gtk_titlebar;
        self.header.set_visible(header);
        let (autohide, visible) = match config.window_show_tab_bar {
            _ if tabs_title => (false, !hidden_maximized),
            ShowTabBar::Always => (false, true),
            ShowTabBar::Auto => (true, true),
            ShowTabBar::Never => (true, false),
        };
        self.tab_bar.set_autohide(autohide);
        self.tab_bar.set_visible(visible);
        self.tab_bar.set_expand_tabs(config.gtk_wide_tabs);
        self.tab_bar.set_inverted(config.gtk_tabs_inverted);
        // ghostty hides the boxes, whose places keep libadwaita's padding.
        let [start, end] = &self.tab_bar_buttons;
        self.tab_bar.set_start_action_widget(tabs_title.then_some(start));
        self.tab_bar.set_end_action_widget(tabs_title.then_some(end));
        self.overview.set_inverted(config.gtk_tabs_inverted);
        self.overview.set_show_start_title_buttons(csd);
        self.overview.set_show_end_title_buttons(csd);
        self.toolbar.set_top_bar_style(config.gtk_toolbar_style);
        self.toolbar.set_bottom_bar_style(config.gtk_toolbar_style);
        if config.background_opacity >= 1.0 {
            window.add_css_class("background");
        } else {
            window.remove_css_class("background");
        }
        if self.tabs_location.replace(config.gtk_tabs_location) != config.gtk_tabs_location {
            self.toolbar.remove(&self.tab_bar);
            match config.gtk_tabs_location {
                TabsLocation::Top => self.toolbar.add_top_bar(&self.tab_bar),
                TabsLocation::Bottom => self.toolbar.add_bottom_bar(&self.tab_bar),
            }
        }
    }

    /// ghostty's new tab, at window-new-tab-position.
    fn new_tab(self: &Rc<Self>) {
        let at = match self.app.config().window_new_tab_position {
            NewTabPosition::Current => NewAt::AfterCurrent,
            NewTabPosition::End => NewAt::AfterLast,
        };
        self.command(Command::NewWindow { cwd_of_active: true, at });
    }

    fn connect_tabs(self: &Rc<Self>, overview: &adw::TabOverview) {
        let menu = gio::Menu::new();
        menu.append(Some("Change Tab Title\u{2026}"), Some("win.prompt-context-tab-title"));
        self.tab_view.set_menu_model(Some(&menu));
        let weak = Rc::downgrade(self);
        self.tab_view.connect_setup_menu(move |_, page| {
            if let Some(win) = weak.upgrade() {
                *win.context_page.borrow_mut() = page.cloned();
            }
        });
        let weak = Rc::downgrade(self);
        self.tab_view.connect_close_page(move |view, page| {
            let Some(win) = weak.upgrade() else { return glib::Propagation::Proceed };
            if win.syncing.get() {
                return glib::Propagation::Proceed;
            }
            // The tab goes away when its window does (reconcile).
            view.close_page_finish(page, false);
            if let Some(id) = win.tab_of(page) {
                win.close_tab(id);
            }
            glib::Propagation::Stop
        });
        let weak = Rc::downgrade(self);
        self.tab_view.connect_selected_page_notify(move |view| {
            let Some(win) = weak.upgrade() else { return };
            let Some(page) = view.selected_page() else { return };
            // ghostty unmarks a tab that needed attention once selected,
            // however it got selected.
            page.set_needs_attention(false);
            if win.syncing.get() {
                return;
            }
            if let Some(index) = win.window_of_page(&page) {
                win.command(Command::SelectWindow(index));
            }
        });
        let weak = Rc::downgrade(self);
        self.tab_view.connect_page_reordered(move |_, page, position| {
            let Some(win) = weak.upgrade() else { return };
            if win.syncing.get() {
                return;
            }
            if let Some(id) = win.tab_of(page) {
                win.place_window(id, position as usize);
            }
        });
        // A tab dragged out of the tab bar leaves the tab view until it is
        // dropped (libadwaita's AdwTabBox begin_drag).
        let weak = Rc::downgrade(self);
        self.tab_view.connect_page_detached(move |_, page, _| {
            let Some(win) = weak.upgrade() else { return };
            if win.syncing.get() {
                return;
            }
            if let Some(tab) = win.state.borrow_mut().tabs.values_mut().find(|t| &t.page == page) {
                tab.detached = true;
            }
        });
        let weak = Rc::downgrade(self);
        self.tab_view.connect_page_attached(move |_, page, position| {
            let Some(win) = weak.upgrade() else { return };
            if !win.syncing.get() {
                win.page_attached(page, position as usize);
            }
        });
        let weak = Rc::downgrade(self);
        self.tab_view.connect_create_window(move |_| weak.upgrade().and_then(|win| win.create_window()));
        let weak = Rc::downgrade(self);
        overview.connect_create_tab(move |_| {
            let win = weak.upgrade().expect("window");
            if win.session_ended.replace(false) {
                let (session, effects) = Session::new(&win.app.server, None);
                win.state.borrow_mut().session = session;
                win.apply(effects);
            } else {
                win.new_tab();
            }
            win.reconcile();
            win.tab_view.selected_page().expect("new tab")
        });
    }

    /// The tmux window of the tab of `page`, if this window has the tab.
    fn tab_of(&self, page: &adw::TabPage) -> Option<u32> {
        self.state.borrow().tabs.iter().find(|(_, t)| &t.page == page).map(|(&id, _)| id)
    }

    /// Puts tmux window `id` where its tab is now: tmux moves a window one
    /// place at a time (swap-window -d -t +1 or -1).
    fn place_window(self: &Rc<Self>, id: u32, position: usize) {
        let windows = self.state.borrow().session.windows();
        let Some(from) = windows.iter().position(|w| w.id == id) else { return };
        self.command(Command::SelectWindow(windows[from].index));
        let step = if position > from { 1 } else { -1 };
        for _ in 0..position.abs_diff(from) {
            self.command(Command::SwapWindow(step));
        }
    }

    /// libadwaita attached a dragged tab to this window's tab view at
    /// `position`: back where it came from, or from another window, whose
    /// session moves the tmux window to this one (move-window).
    fn page_attached(self: &Rc<Self>, page: &adw::TabPage, position: usize) {
        let Some((owner, id)) = self.app.tab_owner(page) else { return };
        if !Rc::ptr_eq(&owner, self) {
            let effects = owner.move_window(id, self, position);
            owner.apply(effects);
            owner.queue_reconcile();
        }
        if let Some(tab) = self.state.borrow_mut().tabs.get_mut(&id) {
            tab.detached = false;
        }
        self.place_window(id, position);
        self.queue_reconcile();
    }

    /// ghostty's tabViewCreateWindow for the tab dropped outside every tab
    /// bar: its tmux window moves to a new session in a new window.
    fn create_window(self: &Rc<Self>) -> Option<adw::TabView> {
        let id = self.state.borrow().tabs.iter().find(|(_, t)| t.detached).map(|(&id, _)| id)?;
        let (session, effects) = self.state.borrow_mut().session.move_window_new(id)?;
        let window = self.app.add_window(session);
        self.hand_over(id, &window);
        self.apply(effects);
        self.queue_reconcile();
        window.window.present();
        Some(window.tab_view.clone())
    }

    /// move-window of tmux window `id` from this window's session to the
    /// session of `dst`, at `position`; a session that ended in `dst`
    /// gives way to a new one. Returns this session's effects.
    fn move_window(self: &Rc<Self>, id: u32, dst: &Rc<Window>, position: usize) -> Vec<Effect> {
        let effects = {
            let mut st = self.state.borrow_mut();
            if !dst.session_ended.get() {
                st.session.move_window(id, &mut dst.state.borrow_mut().session, position)
            } else if let Some((session, effects)) = st.session.move_window_new(id) {
                dst.session_ended.set(false);
                dst.state.borrow_mut().session = session;
                effects
            } else {
                Vec::new()
            }
        };
        self.hand_over(id, dst);
        effects
    }

    /// Gives the tab of tmux window `id` to `dst`, whose session has the
    /// window now, with its panes: their events go to `dst` from now on.
    fn hand_over(&self, id: u32, dst: &Rc<Window>) {
        let mut surfaces = Vec::new();
        {
            let mut st = self.state.borrow_mut();
            let mut to = dst.state.borrow_mut();
            let Some(tab) = st.tabs.remove(&id) else { return };
            to.tabs.insert(id, tab);
            let windows = to.session.windows();
            for pane in windows.into_iter().filter(|w| w.id == id).flat_map(|w| w.panes).map(|p| p.pane) {
                let Some(p) = st.panes.remove(&pane) else { continue };
                surfaces.push(p.surface.clone());
                to.panes.insert(pane, p);
                self.app.set_owner(pane, dst);
            }
        }
        // A prompt answered by keys belongs to the client it was opened in.
        for surface in surfaces {
            surface.set_copy_prompt(None);
        }
    }

    /// ghostty's tabOverviewOpen: focus can get lost when the overview
    /// closes (libadwaita#670), so the active surface takes it again after
    /// the 400ms animation.
    fn connect_overview(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        self.overview.connect_open_notify(move |overview| {
            let Some(win) = weak.upgrade() else { return };
            if overview.is_open() {
                return;
            }
            if let Some(old) = win.overview_focus.take() {
                old.remove();
            }
            let weak = Rc::downgrade(&win);
            let id = glib::timeout_add_local_once(theme::OVERVIEW_FOCUS, move || {
                if let Some(win) = weak.upgrade() {
                    win.overview_focus.take();
                    if let Some(surface) = win.active_surface() {
                        surface.grab_focus();
                    }
                }
            });
            *win.overview_focus.borrow_mut() = Some(id);
        });
    }

    fn add_actions(self: &Rc<Self>) {
        let add = |name: &str, f: Action| {
            let action = gio::SimpleAction::new(name, None);
            let weak = Rc::downgrade(self);
            action.connect_activate(move |_, _| {
                if let Some(win) = weak.upgrade() {
                    f(&win);
                }
            });
            self.window.add_action(&action);
        };
        for (name, horizontal, before) in
            [("split-up", false, true), ("split-down", false, false), ("split-left", true, true), ("split-right", true, false)]
        {
            add(name, Box::new(move |win| win.command(Command::SplitWindow { horizontal, before, full: false, cwd_of_active: true })));
        }
        add("new-tab", Box::new(|win| win.new_tab()));
        add("new-window", Box::new(|win| {
            let parent = win.state.borrow().session.active_pane();
            win.app.new_window(Some(parent));
        }));
        add("close", Box::new(|win| win.window.close()));
        add("toggle-fullscreen", Box::new(|win| win.window.set_fullscreened(!win.window.is_fullscreen())));
        add("toggle-tab-overview", Box::new(|win| win.overview.set_open(!win.overview.is_open())));
        add("toggle-command-palette", Box::new(|win| {
            let pane = win.state.borrow().session.active_pane();
            win.open_palette(pane);
        }));
        add("close-split", Box::new(|win| win.confirm_close_split()));
        add("copy", Box::new(|win| {
            if let Some(surface) = win.active_surface() {
                win.copy(&surface);
            }
        }));
        add("paste", Box::new(|win| win.paste(win.window.clipboard())));
        add("reset", Box::new(|win| win.reset()));
        add("clear", Box::new(|win| win.clear()));
        add("prompt-tab-title", Box::new(|win| win.title_dialog(TitleTarget::Tab(win.current_window_id()))));
        add(
            "prompt-context-tab-title",
            Box::new(|win| {
                let page = win.context_page.borrow().clone();
                if let Some(id) = page.and_then(|page| win.tab_of(&page)) {
                    win.title_dialog(TitleTarget::Tab(id));
                }
            }),
        );
        add("prompt-window-title", Box::new(|win| win.title_dialog(TitleTarget::Window)));
        add("prompt-surface-title", Box::new(|win| win.title_dialog(TitleTarget::Surface)));
        add("about", Box::new(|win| {
            let about = adw::AboutDialog::builder()
                .application_name("tapust")
                .developer_name("tapust Developers")
                .application_icon(theme::ICON)
                .version(env!("CARGO_PKG_VERSION"))
                .build();
            about.present(Some(&win.window));
        }));
        let close_tab = gio::SimpleAction::new("close-tab", Some(glib::VariantTy::STRING));
        let weak = Rc::downgrade(self);
        close_tab.connect_activate(move |_, _| {
            if let Some(win) = weak.upgrade() {
                let id = win.current_window_id();
                win.close_tab(id);
            }
        });
        self.window.add_action(&close_tab);
    }

    fn start_timers(self: &Rc<Self>) {
        // ghostty's termios timer: polls each PTY for a password prompt.
        let weak = Rc::downgrade(self);
        glib::timeout_add_local(theme::TERMIOS_POLL, move || {
            let Some(win) = weak.upgrade() else { return glib::ControlFlow::Break };
            for p in win.state.borrow().panes.values() {
                if let Some(io) = &p.io {
                    p.surface.set_password_input(io.shared.password_input());
                }
            }
            glib::ControlFlow::Continue
        });
        let weak = Rc::downgrade(self);
        glib::timeout_add_local(NAME_INTERVAL, move || {
            let Some(win) = weak.upgrade() else { return glib::ControlFlow::Break };
            win.update_names();
            glib::ControlFlow::Continue
        });
    }

    /// Runs a tmux command on the current window.
    fn command(self: &Rc<Self>, cmd: Command) {
        let out = self.state.borrow_mut().session.command(&cmd);
        if let Some(error) = out.error {
            self.toast(&error);
        }
        self.apply(out.effects);
        self.queue_reconcile();
    }

    fn current_window_id(&self) -> u32 {
        self.state.borrow().session.windows().into_iter().find(|w| w.active).expect("the current window").id
    }

    /// ghostty's ringBell with the default bell-features (attention, title):
    /// the tab of an unselected window needs attention, a window that is not
    /// active asks the compositor for attention, and the surface rings unless
    /// it has focus.
    fn ring_bell(self: &Rc<Self>, pane: PaneId, surface: &Surface) {
        let page = {
            let st = self.state.borrow();
            let window = st.session.windows().into_iter().find(|w| w.panes.iter().any(|p| p.pane == pane));
            window.and_then(|w| st.tabs.get(&w.id).map(|t| t.page.clone()))
        };
        if let Some(page) = page
            && !page.is_selected()
        {
            page.set_needs_attention(true);
        }
        if !self.window.is_active() {
            self.app.attention.request(&self.window);
        }
        surface.set_bell_ringing(true);
    }

    /// ghostty's initial_size at the first surface's init: window-width x
    /// window-height cells and the padding, in logical pixels, and the bars
    /// above and below the tabs, which ghostty's estimate leaves out so that
    /// its grid comes out smaller than configured.
    fn initial_size(&self, (cw, ch): (u32, u32), scale: f64, (pad_x, pad_y): (u32, u32)) {
        let config = self.app.config();
        if config.window_width == 0 || config.window_height == 0 {
            return;
        }
        let logical = |cells: u32, cell: u32, pad: u32| (f64::from(cells * cell + pad) / scale).ceil() as i32;
        // The toolbar view stacks its bars and its content.
        let natural = |widget: &gtk::Widget| widget.measure(gtk::Orientation::Vertical, -1).1;
        let content = self.toolbar.content().expect("the toolbar's content");
        let bars = natural(self.toolbar.upcast_ref()) - natural(&content);
        let (width, height) = (logical(config.window_width, cw, pad_x), logical(config.window_height, ch, pad_y));
        self.window.set_default_size(width, height + bars);
    }

    fn window_of_page(&self, page: &adw::TabPage) -> Option<u32> {
        let id = self.tab_of(page)?;
        self.state.borrow().session.windows().into_iter().find(|w| w.id == id).map(|w| w.index)
    }

    fn handle(self: &Rc<Self>, event: Event) -> bool {
        match event {
            Event::Key { pane, press, text } => {
                // GTK marks no automatic repeat: it is a press without a
                // release, of a key the window's keyboard holds down, so
                // that a binding moving the focus to another pane keeps it.
                let repeat = self.held_key.replace(Some(press.keycode)) == Some(press.keycode);
                self.key(pane, press, text, repeat)
            }
            Event::KeyReleased { keycode } => {
                if self.held_key.get() == Some(keycode) {
                    self.held_key.set(None);
                }
                false
            }
            Event::Text { pane, text } => {
                for c in text.chars() {
                    let key = mux::Key { code: mux::KeyCode::Char(c), mods: mux::Mods::default() };
                    self.send_key(pane, key, Some(c.to_string()), false);
                }
                true
            }
            Event::Resized { pane, cols, rows, cell } => {
                self.resized(pane, cols, rows, cell);
                true
            }
            Event::PastePrimary => {
                self.paste(self.window.primary_clipboard());
                true
            }
            Event::SwipeTab { next } => {
                if next {
                    self.tab_view.select_next_page();
                } else {
                    self.tab_view.select_previous_page();
                }
                true
            }
            Event::ContextMenu { pane, x, y } => {
                // ghostty's menu signal: actions are synced before it opens.
                self.sync_copy_action();
                if let Some(p) = self.state.borrow().panes.get(&pane) {
                    p.surface.popup_menu(x, y);
                }
                true
            }
            Event::SelectionChanged => {
                self.sync_copy_action();
                true
            }
            Event::SelectionCopied => {
                self.toast("Copied to clipboard");
                true
            }
            Event::BellChanged => {
                self.queue_reconcile();
                true
            }
            Event::CellSize { cell: (cw, ch), scale, padding } => {
                // ghostty's size_limit at the first surface's init: ten
                // columns and four rows of device pixels, used as logical.
                if !self.min_size_set.replace(true) {
                    self.window.set_size_request((cw * 10) as i32, (ch * 4) as i32);
                    self.initial_size((cw, ch), scale, padding);
                }
                true
            }
            Event::Mapped => {
                // ghostty's propSurfaceMapped: surfaces of nested splits can
                // be unmapped at a zero size and lose focus, so focus goes
                // back to the active surface once they are mapped again.
                if !self.restore_focus_pending.replace(true) {
                    let weak = Rc::downgrade(self);
                    glib::idle_add_local_once(move || {
                        if let Some(win) = weak.upgrade() {
                            win.restore_focus_pending.set(false);
                            if let Some(surface) = win.active_surface().filter(|s| s.area.is_mapped()) {
                                surface.grab_focus();
                            }
                        }
                    });
                }
                true
            }
            Event::Focused { pane } => {
                let Ok(mut st) = self.state.try_borrow_mut() else { return true };
                if st.session.active_pane() != pane && !self.syncing.get() {
                    let effects = st.session.focus_pane(pane);
                    drop(st);
                    self.apply(effects);
                    self.queue_reconcile();
                }
                true
            }
            Event::Dropped { source, target, dir } => {
                let effects = self.state.borrow_mut().session.move_pane(source, target, dir);
                self.apply(effects);
                self.queue_reconcile();
                true
            }
            Event::ChildExitedClose { pane } => {
                self.close_pane(pane);
                true
            }
            Event::WheelKeys { pane, up, count } => {
                let code = if up { mux::KeyCode::Up } else { mux::KeyCode::Down };
                for _ in 0..count {
                    self.send_key(pane, mux::Key { code, mods: mux::Mods::default() }, None, false);
                }
                true
            }
        }
    }

    /// A key press; `commit` is the input method's text for it, which is
    /// typed as it is (a dead key or a direct input method).
    fn key(self: &Rc<Self>, pane: PaneId, press: Press, commit: Option<String>, repeat: bool) -> bool {
        // ghostty's bindings come before the tmux key tables. A performable
        // one that could not be performed is no binding, and an unconsumed
        // one lets the key go on too.
        let bound = self.app.config().keybind.lookup(&press);
        if let Some(bound) = bound {
            let performed = self.perform(pane, bound.binding);
            if bound.consumed && (performed || !bound.performable) {
                return true;
            }
        }
        let (key, text) = match commit {
            Some(text) => {
                for c in text.chars() {
                    let key = mux::Key { code: mux::KeyCode::Char(c), mods: mux::Mods::default() };
                    self.send_key(pane, key, Some(c.to_string()), repeat);
                }
                return true;
            }
            None => match input::from_gdk(press.keyval, press.state) {
                Some(key) => key,
                None => return false,
            },
        };
        self.send_key(pane, key, text, repeat);
        true
    }

    /// Carries out a binding on the pane; returns whether it could
    /// (ghostty's performable: a copy needs a selection).
    fn perform(self: &Rc<Self>, pane: PaneId, binding: Binding) -> bool {
        let Some(surface) = self.state.borrow().panes.get(&pane).map(|p| p.surface.clone()) else { return false };
        match binding {
            Binding::StartSearch => surface.start_search(),
            Binding::Copy => return self.copy(&surface),
            Binding::Paste { primary: false } => self.paste(self.window.clipboard()),
            Binding::Paste { primary: true } => self.paste(self.window.primary_clipboard()),
            Binding::Scroll(scroll) => surface.scroll(scroll),
            Binding::SelectAll => surface.select_all(),
            Binding::FontSize(change) => surface.change_font_size(change),
            Binding::ToggleCommandPalette => self.open_palette(pane),
            Binding::OpenConfig { new_window } => self.app.open_config(new_window),
            Binding::ReloadConfig => self.app.reload_config(),
        }
        true
    }

    /// ghostty's toggleCommandPalette, which opens it here: ctrl+shift+p in
    /// the palette closes it. Its entries run on `pane`, where it was opened
    /// (ghostty runs them on the window's active surface then).
    fn open_palette(self: &Rc<Self>, pane: PaneId) {
        let config = self.app.config();
        let mut entries = palette::commands(&config, self.app.server.prefix());
        entries.extend(self.app.focus_entries());
        let weak = Rc::downgrade(self);
        palette::present(&self.window, entries, config.keybind.clone(), move |run| {
            if let Some(win) = weak.upgrade() {
                win.run_entry(pane, run);
            }
        });
    }

    /// Runs a command palette entry as its key binding or menu item does;
    /// nothing runs on a pane that went away while the palette was open.
    fn run_entry(self: &Rc<Self>, pane: PaneId, run: Run) {
        let Some(surface) = self.state.borrow().panes.get(&pane).map(|p| p.surface.clone()) else { return };
        match run {
            Run::Binding(binding) => {
                self.perform(pane, binding);
            }
            Run::Prefix(key) => {
                let effects = self.state.borrow_mut().session.run_prefix_binding(key, Instant::now());
                self.apply(effects);
                self.queue_reconcile();
                self.schedule_tick();
            }
            // The surface's widget reaches its own actions, the window's and the application's.
            Run::Action(name) => surface.area.activate_action(name, None).expect("an action of the palette"),
            Run::Focus(id) => self.app.present_surface(id),
        }
    }

    /// `repeat` is an automatic repeat of a key held down.
    fn send_key(self: &Rc<Self>, pane: PaneId, key: mux::Key, text: Option<String>, repeat: bool) {
        // Any key the terminal uses stops the bell (ghostty's keyEvent).
        let surface = self.state.borrow().panes.get(&pane).map(|p| p.surface.clone());
        if let Some(surface) = surface {
            surface.set_bell_ringing(false);
        }
        let mut st = self.state.borrow_mut();
        let result = if repeat { st.session.key_repeat(key, Instant::now()) } else { st.session.key(key, Instant::now()) };
        match result {
            KeyResult::Forward(id) => {
                let target = if st.panes.contains_key(&id) { id } else { pane };
                let mut typed = None;
                let mut close = false;
                if let Some(p) = st.panes.get(&target)
                    && let Some(io) = &p.io
                {
                    let app_cursor = io.shared.term.lock().modes().app_cursor;
                    let mut bytes = Vec::new();
                    input::encode(key, text.as_deref(), app_cursor, &mut bytes);
                    // ghostty's keyCallback: a key that encodes to something
                    // closes a surface whose child exited.
                    if p.child_exited && !bytes.is_empty() {
                        close = true;
                    } else {
                        io.shared.write(&bytes);
                        if !bytes.is_empty() {
                            typed = Some(p.surface.clone());
                        }
                    }
                }
                drop(st);
                if close {
                    self.close_pane(target);
                }
                // ghostty's selection-clear-on-typing and scroll-to-bottom =
                // keystroke, once the state is no longer borrowed (clearing
                // the selection updates the Copy action).
                if let Some(surface) = typed {
                    surface.clear_selection();
                    surface.scroll(Scroll::Bottom);
                }
            }
            KeyResult::Handled(effects) => {
                drop(st);
                self.apply(effects);
                self.queue_reconcile();
            }
        }
        self.schedule_tick();
    }

    /// GTK gave a pane its size: start its shell, or resize it.
    fn resized(self: &Rc<Self>, pane: PaneId, cols: u16, rows: u16, cell: (u32, u32)) {
        let mut st = self.state.borrow_mut();
        let st = &mut *st;
        let Some(p) = st.panes.get_mut(&pane) else { return };
        match &p.io {
            Some(io) => {
                io.shared.term.lock().resize(cols, rows);
                let _ = io.pty.resize(cols, rows, cell);
                p.surface.resize_copy_mode(cols, rows);
            }
            None => {
                let config = self.app.config();
                let program = match (p.program.take(), &config.command) {
                    (Some(argv), _) => Program::Argv(argv),
                    (None, None) => Program::LoginShell,
                    (None, Some(ConfigCommand::Shell(line))) => Program::ShellCommand(line.clone()),
                    (None, Some(ConfigCommand::Direct(words))) => {
                        Program::Argv(words.iter().map(OsString::from).collect())
                    }
                };
                let blink = config.cursor_style_blink.unwrap_or(true);
                let pty = match Pty::spawn(program, blink, cols, rows, cell, &p.cwd) {
                    Ok(pty) => pty,
                    Err(err) => {
                        eprintln!("tapust: cannot start a shell: {err}");
                        return;
                    }
                };
                let mut terminal = term::Terminal::new(cols, rows, config.scrollback_lines);
                terminal.set_default_palette(config.term_palette());
                terminal.set_default_cursor(config.cursor_style, config.cursor_style_blink);
                let io = match PaneIo::start(pty, terminal, move || {
                    glib::idle_add_once(move || wake(pane));
                }) {
                    Ok(io) => io,
                    Err(err) => {
                        eprintln!("tapust: cannot start the pane's I/O: {err}");
                        return;
                    }
                };
                if let Some(name) = io.pty.foreground_name() {
                    st.session.set_auto_name(pane, &name);
                }
                p.surface.attach(io.shared.clone());
                p.io = Some(io);
            }
        }
        self.queue_geometry();
    }

    /// The pane's terminal changed or its process exited.
    fn wake(self: &Rc<Self>, pane: PaneId) {
        let mut st = self.state.borrow_mut();
        let Some(p) = st.panes.get_mut(&pane) else { return };
        let Some(io) = &p.io else { return };
        // News from here on wakes this again.
        io.shared.news.store(false, std::sync::atomic::Ordering::Release);
        let exit = io.shared.exited.get().copied().filter(|_| !p.child_exited);
        let (title, pwd, bell, messages) = {
            let mut term = io.shared.term.lock();
            (term.title().to_owned(), term.reported_cwd().map(str::to_owned), term.take_bell(), term.take_messages())
        };
        p.child_exited |= exit.is_some();
        p.surface.output();
        p.surface.refresh_search();
        p.surface.area.queue_render();
        p.surface.sync_scrollbar();
        p.surface.update_cursor();
        let surface = p.surface.clone();
        let mut changed = pwd != p.pwd;
        p.pwd = pwd;
        if title != p.title {
            p.title = title.clone();
            st.session.set_pane_title(pane, &title);
            changed = true;
        }
        drop(st);
        if bell {
            self.ring_bell(pane, &surface);
        }
        for message in messages {
            match message {
                term::Message::DesktopNotification { title, body } => {
                    self.app.desktop_notification(&surface, &title, &body)
                }
                term::Message::ProgressReport(report) => surface.set_progress_report(report),
                term::Message::StartCommand => surface.start_command(),
                term::Message::StopCommand(_) => {
                    if surface.stop_command() {
                        self.ring_bell(pane, &surface);
                    }
                }
            }
        }
        if changed {
            self.queue_reconcile();
        }
        if let Some(exit) = exit {
            self.child_exited(pane, &surface, exit);
        }
    }

    /// ghostty's Surface.childExited with wait-after-command = false: a
    /// failing child that exited within abnormal-command-exit-runtime keeps
    /// its surface with the banner; any other exit closes the surface.
    fn child_exited(self: &Rc<Self>, pane: PaneId, surface: &Surface, exit: ChildExit) {
        surface.show_child_exited(exit.code);
        if exit.runtime.as_millis() <= theme::ABNORMAL_EXIT_RUNTIME.as_millis() && exit.code != 0 {
            return;
        }
        self.close_pane(pane);
    }

    /// ghostty's close-request of a surface whose child exited: it closes
    /// without confirmation.
    fn close_pane(self: &Rc<Self>, pane: PaneId) {
        let effects = self.state.borrow_mut().session.pane_exited(pane);
        self.apply(effects);
        self.queue_reconcile();
    }

    /// ghostty's surfacePresentRequest: the notification's surface gets its
    /// tab selected, the focus and the window raised.
    fn present_surface(&self, id: u64) {
        let found = {
            let st = self.state.borrow();
            let surface = st.panes.values().find(|p| p.surface.id == id).map(|p| p.surface.clone());
            surface.and_then(|surface| {
                let window =
                    st.session.windows().into_iter().find(|w| w.panes.iter().any(|q| q.pane == surface.pane))?;
                Some((surface, st.tabs.get(&window.id)?.page.clone()))
            })
        };
        // A pane hidden by a zoomed split is not in the window.
        let Some((surface, page)) = found.filter(|(s, _)| s.outer.is_ancestor(&self.window)) else { return };
        self.tab_view.set_selected_page(&page);
        surface.grab_focus();
        self.window.present();
    }

    fn update_names(self: &Rc<Self>) {
        let mut st = self.state.borrow_mut();
        let before = st.session.generation();
        let names: Vec<(PaneId, String)> = st
            .panes
            .iter()
            .filter_map(|(&id, p)| p.io.as_ref().and_then(|io| io.pty.foreground_name()).map(|n| (id, n)))
            .collect();
        for (id, name) in names {
            st.session.set_auto_name(id, &name);
        }
        let changed = st.session.generation() != before;
        drop(st);
        if changed {
            self.queue_reconcile();
        }
    }

    fn schedule_tick(self: &Rc<Self>) {
        let mut st = self.state.borrow_mut();
        if let Some(old) = st.tick.take() {
            old.remove();
        }
        let Some(deadline) = st.session.deadline() else { return };
        let weak = Rc::downgrade(self);
        let delay = deadline.saturating_duration_since(Instant::now());
        st.tick = Some(glib::timeout_add_local_once(delay, move || {
            let Some(win) = weak.upgrade() else { return };
            let mut st = win.state.borrow_mut();
            st.tick = None;
            st.session.tick(Instant::now());
            drop(st);
            win.queue_reconcile();
            win.schedule_tick();
        }));
    }

    /// Carries out the model's effects.
    fn apply(self: &Rc<Self>, effects: Vec<Effect>) {
        for effect in effects {
            match effect {
                Effect::Spawn { pane, parent, inherit_cwd } => {
                    let (points, cwd) = self.app.origin(parent, inherit_cwd);
                    self.app.set_owner(pane, self);
                    let weak = Rc::downgrade(&self.app);
                    let handler: crate::surface::Handler =
                        Rc::new(move |event| weak.upgrade().is_some_and(|app| app.handle(pane, event)));
                    let surface = Surface::new(pane, self.app.config(), points, handler);
                    let program = self.app.command.take();
                    self.state.borrow_mut().panes.insert(
                        pane,
                        Pane {
                            surface,
                            io: None,
                            cwd,
                            program,
                            title: String::new(),
                            title_override: None,
                            pwd: None,
                            child_exited: false,
                        },
                    );
                }
                Effect::Destroy { pane } => {
                    self.app.owners.borrow_mut().remove(&pane);
                    let removed = self.state.borrow_mut().panes.remove(&pane);
                    if let Some(p) = removed {
                        detach(p.surface.outer.upcast_ref());
                    }
                }
                Effect::Key { pane, key } => {
                    if let Some(io) = self.state.borrow().panes.get(&pane).and_then(|p| p.io.as_ref()) {
                        let app_cursor = io.shared.term.lock().modes().app_cursor;
                        let mut bytes = Vec::new();
                        input::encode(key, None, app_cursor, &mut bytes);
                        io.shared.write(&bytes);
                    }
                }
                Effect::ResizeSplit { pane, horizontal, cells } => self.resize_split(pane, horizontal, cells),
                Effect::CopyMode { pane, search } => {
                    let surface = self.state.borrow().panes.get(&pane).map(|p| p.surface.clone());
                    let keys = self.app.server.mode_keys();
                    // A pane GTK has not sized yet has no screen to copy.
                    if !surface.is_some_and(|s| s.enter_copy_mode(keys, search)) {
                        let result = CopyResult { cancel: true, ..CopyResult::default() };
                        self.state.borrow_mut().session.copy_result(pane, result);
                    }
                }
                Effect::CopyCommand { pane, command, count } => {
                    let surface = self.state.borrow().panes.get(&pane).map(|p| p.surface.clone());
                    let Some(result) = surface.and_then(|s| s.copy_command(&command, count)) else { continue };
                    // tmux's set-clipboard external: the copy reaches the
                    // outer terminal's clipboard too.
                    if let Some(Copied::New(data) | Copied::Append(data)) = &result.copied {
                        self.set_clipboard(&String::from_utf8_lossy(data));
                    }
                    self.state.borrow_mut().session.copy_result(pane, result);
                }
                Effect::Paste { pane, data, bracket } => self.paste_buffer(pane, &data, bracket),
                // ghostty's tabViewNPages: the window closes with its last
                // tab, unless the tab overview is open.
                Effect::Exit => {
                    if self.overview.is_open() {
                        self.session_ended.set(true);
                    } else {
                        self.close_later();
                    }
                }
            }
        }
    }

    /// ghostty's resizeSplit: resize from the ratio a clamped split shows,
    /// then move by the amount as a share of the tree's size.
    fn resize_split(self: &Rc<Self>, pane: PaneId, horizontal: bool, cells: i32) {
        let (surface, tree_bin, splits) = {
            let st = self.state.borrow();
            let Some(p) = st.panes.get(&pane) else { return };
            let Some(index) = st.session.windows().into_iter().find(|w| w.panes.iter().any(|q| q.pane == pane)).map(|w| w.id) else {
                return;
            };
            let Some(tab) = st.tabs.get(&index) else { return };
            (p.surface.clone(), tab.tree_bin.clone(), tab.splits.clone())
        };
        let orientation = if horizontal { gtk::Orientation::Horizontal } else { gtk::Orientation::Vertical };
        let mut widget = surface.outer.parent();
        while let Some(w) = widget {
            if let Some(paned) = w.downcast_ref::<gtk::Paned>()
                && paned.orientation() == orientation
            {
                if let Some(split) = splits.iter().find(|s| &s.paned == paned) {
                    split.sync(Sync::ClampedToTree);
                }
                break;
            }
            widget = w.parent();
        }
        let (width, height) = (tree_bin.width(), tree_bin.height());
        if width == 0 || height == 0 {
            return;
        }
        // tmux counts cells; ghostty moves by logical pixels.
        let scale = surface.area.native().and_then(|n| n.surface()).map_or(1.0, |s| s.scale());
        let (cw, ch) = surface.cell_size().unwrap_or((1, 1));
        let cell = if horizontal { f64::from(cw) } else { f64::from(ch) } / scale;
        let amount = (f64::from(cells) * cell).round();
        let delta = amount / f64::from(if horizontal { width } else { height });
        let effects = self.state.borrow_mut().session.resize_split(pane, horizontal, delta);
        self.apply(effects);
        self.queue_reconcile();
    }

    fn set_ratio(&self, window: u32, index: usize, ratio: f64) {
        let Ok(mut st) = self.state.try_borrow_mut() else { return };
        let Some(info) = st.session.windows().into_iter().find(|w| w.id == window) else { return };
        st.session.set_ratio(info.index, index, ratio);
        // The widgets stay: remember the tree as it now is.
        let tree = st.session.tree(info.index).map(|(t, z, _)| (t.clone(), z));
        if let Some(tab) = st.tabs.get_mut(&window) {
            tab.built = tree;
        }
    }

    fn queue_reconcile(self: &Rc<Self>) {
        if self.reconcile_pending.replace(true) {
            return;
        }
        let weak = Rc::downgrade(self);
        glib::idle_add_local_once(move || {
            if let Some(win) = weak.upgrade() {
                win.reconcile_pending.set(false);
                win.reconcile();
            }
        });
    }

    fn queue_geometry(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        glib::idle_add_local_once(move || {
            if let Some(win) = weak.upgrade() {
                win.report_geometry();
            }
        });
    }

    /// Tells the model where GTK put the current window's panes (tmux's
    /// directional selection uses them).
    fn report_geometry(&self) {
        let Ok(mut st) = self.state.try_borrow_mut() else { return };
        let index = st.session.current();
        let Some(info) = st.session.windows().into_iter().find(|w| w.index == index) else { return };
        let Some(tab) = st.tabs.get(&info.id) else { return };
        let tree_bin = tab.tree_bin.clone();
        let area = Rect { x: 0, y: 0, w: tree_bin.width().max(0) as u32, h: tree_bin.height().max(0) as u32 };
        let active = info.panes.iter().find(|p| p.active).and_then(|p| st.panes.get(&p.pane));
        // The pane gets its fonts when GTK first sizes it, which reports again.
        let Some(grid) = active.and_then(|p| p.surface.grid(area.w, area.h)) else { return };
        let mut panes = Vec::new();
        for p in &info.panes {
            let Some(pane) = st.panes.get(&p.pane) else { continue };
            if !pane.surface.outer.is_mapped() {
                continue;
            }
            let Some(b) = pane.surface.bounds(&tree_bin) else { continue };
            let rect = Rect { x: b.x().max(0.0) as u32, y: b.y().max(0.0) as u32, w: b.width() as u32, h: b.height() as u32 };
            let (cols, rows) = pane.io.as_ref().map_or((0, 0), |io| {
                let t = io.shared.term.lock();
                (t.cols(), t.rows())
            });
            panes.push((p.pane, rect, cols, rows));
        }
        st.session.set_geometry(index, area, grid, &panes);
    }

    /// Makes GTK show the model: one tab per window in index order, each with
    /// the widgets of its split tree, the current window selected and its
    /// active pane focused, and the prompt or message.
    fn reconcile(self: &Rc<Self>) {
        // Closing the tabs of a window on its way out makes libadwaita
        // render their thumbnails without a renderer.
        if self.closing.get() {
            return;
        }
        self.syncing.set(true);
        let windows = self.state.borrow().session.windows();
        // Tabs of windows that are gone; a tab being dragged goes once it
        // is dropped.
        let gone: Vec<u32> = self
            .state
            .borrow()
            .tabs
            .iter()
            .filter(|(id, t)| !t.detached && !windows.iter().any(|w| w.id == **id))
            .map(|(&id, _)| id)
            .collect();
        for id in gone {
            let tab = self.state.borrow_mut().tabs.remove(&id);
            if let Some(tab) = tab {
                self.tab_view.close_page(&tab.page);
            }
        }
        // The position among the pages in the tab view, where a dragged
        // tab is not.
        let mut position = 0;
        for info in &windows {
            let exists = self.state.borrow().tabs.contains_key(&info.id);
            if !exists {
                let tree_bin = adw::Bin::builder().hexpand(true).vexpand(true).build();
                let split_tree = gtk::Box::builder().orientation(gtk::Orientation::Vertical).build();
                split_tree.append(&tree_bin);
                let tab = theme::styled(
                    gtk::Box::builder().orientation(gtk::Orientation::Vertical).hexpand(true).vexpand(true).build(),
                    &["tab"],
                );
                tab.append(&split_tree);
                let page = self.tab_view.append(&tab);
                let tab = Tab { page, tree_bin, built: None, splits: Vec::new(), detached: false };
                self.state.borrow_mut().tabs.insert(info.id, tab);
            }
            let (page, detached) = {
                let tab = &self.state.borrow().tabs[&info.id];
                (tab.page.clone(), tab.detached)
            };
            if !detached {
                if self.tab_view.page_position(&page) != position {
                    self.tab_view.reorder_page(&page, position);
                }
                position += 1;
            }
            self.rebuild(info.index, info.id);
            let title = self.tab_title(info);
            page.set_title(&title);
            // ghostty binds the tab's tooltip to its active surface's pwd.
            let pwd = {
                let st = self.state.borrow();
                info.panes.iter().find(|p| p.active).and_then(|p| st.panes.get(&p.pane)).and_then(|p| p.pwd.clone())
            };
            page.set_tooltip(pwd.as_deref().unwrap_or(""));
            if info.active {
                if !detached {
                    self.tab_view.set_selected_page(&page);
                }
                // ghostty's computed_subtitle.
                let subtitle = match self.app.config().window_subtitle {
                    Subtitle::False => None,
                    Subtitle::WorkingDirectory => pwd.as_deref(),
                };
                self.title.set_subtitle(subtitle.unwrap_or(""));
                // The override only replaces the header bar's title; the
                // window keeps the tab's title (ghostty's closureTitle).
                let shown = self.state.borrow().window_title_override.clone().unwrap_or_else(|| title.clone());
                self.title.set_title(&shown);
                self.window.set_title(Some(&title));
            }
        }
        // Replacing split widgets makes GTK move the focus to a focusable
        // ancestor after the next paint unless it is grabbed again
        // (ghostty's onRebuild grabs the last focused surface).
        if let Some(surface) = self.active_surface() {
            surface.grab_focus();
        }
        self.syncing.set(false);
        self.show_overlay();
        self.sync_copy_action();
        self.queue_geometry();
    }

    /// Makes the tab's widgets show its tree when it changed (ghostty's
    /// onRebuild). A split widget stays where the tree keeps a split the
    /// same way, and a widget moves only when its place changed: moving one
    /// realizes the GL areas of its panes again.
    fn rebuild(self: &Rc<Self>, index: u32, id: u32) {
        let (tree, zoomed) = {
            let st = self.state.borrow();
            let Some((tree, zoomed, _)) = st.session.tree(index) else { return };
            (tree.clone(), zoomed)
        };
        let (old, tree_bin) = {
            let st = self.state.borrow();
            let tab = &st.tabs[&id];
            let old = match &tab.built {
                Some((built, z)) if *z == zoomed && *built == tree => return,
                Some((built, z)) if *z == zoomed => {
                    let mut by_index = vec![None; built.split_count()];
                    for split in &tab.splits {
                        by_index[split.index.get()] = Some(split.clone());
                    }
                    Some((built.clone(), by_index))
                }
                _ => None,
            };
            (old, tab.tree_bin.clone())
        };
        let mut splits = Vec::new();
        let widget = match zoomed {
            Some(pane) => self.leaf(pane),
            None => {
                let (old_tree, reuse) = old.as_ref().map_or((None, &[][..]), |(t, r)| (Some((t, 0)), &r[..]));
                self.place(&tree, old_tree, reuse, id, &mut 0, &mut splits)
            }
        };
        let split = matches!(tree, Node::Split(_));
        let mut leaves = Vec::new();
        tree.leaves(&mut leaves);
        let mut st = self.state.borrow_mut();
        for pane in &leaves {
            if let Some(p) = st.panes.get(pane) {
                p.surface.set_split(split);
            }
        }
        let tab = st.tabs.get_mut(&id).expect("tab");
        tab.built = Some((tree, zoomed));
        tab.splits = splits;
        drop(st);
        if tree_bin.child() != widget {
            if let Some(current) = tree_bin.child() {
                release_focus(&current);
            }
            if let Some(w) = &widget {
                detach(w);
            }
            tree_bin.set_child(widget.as_ref());
        }
    }

    fn leaf(&self, pane: PaneId) -> Option<gtk::Widget> {
        Some(self.state.borrow().panes.get(&pane)?.surface.outer.clone().upcast())
    }

    /// The widget for `node`. `old` is the node the built tree had in the
    /// same place, with the in-order index of its first split; a split that
    /// was a split the same way keeps its widget from `reuse`.
    fn place(
        self: &Rc<Self>,
        node: &Node,
        old: Option<(&Node, usize)>,
        reuse: &[Option<Rc<SplitWidget>>],
        window: u32,
        counter: &mut usize,
        splits: &mut Vec<Rc<SplitWidget>>,
    ) -> Option<gtk::Widget> {
        let s = match node {
            Node::Leaf(pane) => return self.leaf(*pane),
            Node::Split(s) => s,
        };
        let old = match old {
            Some((Node::Split(o), base)) if o.horizontal == s.horizontal => Some((o, base, base + o.first.split_count())),
            _ => None,
        };
        let first = self.place(&s.first, old.map(|(o, base, _)| (&o.first, base)), reuse, window, counter, splits)?;
        let index = *counter;
        *counter += 1;
        let second = self.place(&s.second, old.map(|(o, _, at)| (&o.second, at + 1)), reuse, window, counter, splits)?;
        let split = match old.and_then(|(_, _, at)| reuse[at].clone()) {
            Some(split) => {
                split.index.set(index);
                put(&split.paned, true, &first);
                put(&split.paned, false, &second);
                split.follow(s.ratio);
                split
            }
            None => {
                detach(&first);
                detach(&second);
                SplitWidget::new(&self.app, window, index, s.horizontal, s.ratio, &first, &second)
            }
        };
        let widget = split.bin.clone().upcast();
        splits.push(split);
        Some(widget)
    }

    /// ghostty's tab title: the title override, else the active pane's
    /// terminal title, else the application name; zoomed tabs get a magnifier.
    fn tab_title(&self, info: &mux::WindowInfo) -> String {
        let st = self.state.borrow();
        let active = info.panes.iter().find(|p| p.active).and_then(|p| st.panes.get(&p.pane));
        let title = Some(info.name.as_str())
            .filter(|n| info.renamed && !n.is_empty())
            .or_else(|| active.and_then(|p| p.title_override.as_deref()))
            .or_else(|| active.map(|p| p.title.as_str()).filter(|t| !t.is_empty()))
            .unwrap_or("tapust");
        let bell = active.is_some_and(|p| p.surface.bell_ringing());
        let mut shown = String::new();
        if bell {
            shown.push_str("\u{1F514} ");
        }
        if info.zoomed {
            shown.push_str("\u{1F50D} ");
        }
        shown.push_str(title);
        shown
    }

    fn toast(&self, text: &str) {
        let toast = adw::Toast::builder().title(text).timeout(3).build();
        self.toasts.add_toast(toast);
    }

    /// Shows tmux's prompt or message the way ghostty asks and tells:
    /// confirmations and the title and copy-mode prompts as dialogs,
    /// messages as toasts, and a prompt answered by keys in the active
    /// pane's copy-mode label.
    fn show_overlay(self: &Rc<Self>) {
        let overlay = self.state.borrow().session.overlay();
        if self.state.borrow().shown == overlay {
            return;
        }
        self.state.borrow_mut().shown = overlay.clone();
        let keys = match &overlay {
            Some(Overlay::Keys { prompt, text }) => Some(format!("{prompt}{text}")),
            _ => None,
        };
        let surfaces: Vec<(bool, Rc<Surface>)> = {
            let st = self.state.borrow();
            let active = st.session.active_pane();
            st.panes.iter().map(|(&id, p)| (id == active, p.surface.clone())).collect()
        };
        for (active, surface) in surfaces {
            surface.set_copy_prompt(if active { keys.clone() } else { None });
        }
        match overlay {
            Some(Overlay::Message(text)) => self.toast(&text),
            Some(Overlay::Confirm { target, .. }) => {
                let (heading, body) = match target {
                    ConfirmTarget::Pane => ("Close Split?", "The currently running process in this split will be terminated."),
                    ConfirmTarget::Window => ("Close Tab?", "All terminal sessions in this tab will be terminated."),
                };
                let dialog = close_dialog(heading, body);
                let weak = Rc::downgrade(self);
                dialog.connect_response(None, move |_, response| {
                    if let Some(win) = weak.upgrade() {
                        win.answer((response == "close").then_some("y"));
                    }
                });
                dialog.present(Some(&self.window));
            }
            Some(Overlay::Input { text, kind, .. }) => {
                let (dialog, entry) = match kind {
                    InputKind::RenameSession => title_dialog_widgets("Rename Session", &text),
                    InputKind::RenameWindow => {
                        let title = self.title_initial(TitleTarget::Tab(self.current_window_id()));
                        title_dialog_widgets("Change Tab Title", &title)
                    }
                    InputKind::Search { forward, incremental, last } => {
                        let heading = if forward { "Search Down" } else { "Search Up" };
                        let (dialog, entry) = prompt_dialog_widgets(heading, &text);
                        if incremental {
                            self.connect_incremental(&entry, last);
                        }
                        (dialog, entry)
                    }
                    InputKind::GotoLine => prompt_dialog_widgets("Go to Line", &text),
                };
                let weak = Rc::downgrade(self);
                dialog.connect_response(None, move |_, response| {
                    if let Some(win) = weak.upgrade() {
                        let text = entry.text();
                        win.answer((response == "ok").then_some(text.as_str()));
                    }
                });
                dialog.present(Some(&self.window));
            }
            Some(Overlay::Keys { .. }) | None => {}
        }
    }

    /// An incremental search prompt in a dialog: each change of the text
    /// searches again from where the search started, and C-s and C-r in it
    /// go to the next match down or up, or bring back the pane's last
    /// search (`last`) into an empty prompt, as tmux's prompt does.
    fn connect_incremental(self: &Rc<Self>, entry: &gtk::Entry, last: String) {
        let weak = Rc::downgrade(self);
        entry.connect_changed(move |entry| {
            if let Some(win) = weak.upgrade() {
                win.prompt_changed(&entry.text(), '=');
            }
        });
        let keys = gtk::EventControllerKey::builder().propagation_phase(gtk::PropagationPhase::Capture).build();
        let (weak, target) = (Rc::downgrade(self), entry.clone());
        keys.connect_key_pressed(move |_, keyval, _, state| {
            use gdk::ModifierType as M;
            let held = state & (M::CONTROL_MASK | M::SHIFT_MASK | M::ALT_MASK);
            let prefix = match keyval {
                gdk::Key::s if held == M::CONTROL_MASK => '+',
                gdk::Key::r if held == M::CONTROL_MASK => '-',
                _ => return glib::Propagation::Proceed,
            };
            let Some(win) = weak.upgrade() else { return glib::Propagation::Proceed };
            let text = target.text();
            if text.is_empty() {
                target.set_text(&last);
                target.set_position(-1);
            } else {
                win.prompt_changed(&text, prefix);
            }
            glib::Propagation::Stop
        });
        entry.add_controller(keys);
    }

    fn prompt_changed(self: &Rc<Self>, text: &str, prefix: char) {
        let effects = self.state.borrow_mut().session.prompt_changed(text, prefix, Instant::now());
        self.apply(effects);
        self.queue_reconcile();
    }

    fn answer(self: &Rc<Self>, reply: Option<&str>) {
        let effects = self.state.borrow_mut().session.answer(reply, Instant::now());
        self.apply(effects);
        self.queue_reconcile();
    }

    /// ghostty's tabViewClosePage: closes the tab of window `id` at once
    /// unless one of its panes needs confirmation.
    fn close_tab(self: &Rc<Self>, id: u32) {
        if !self.window_needs_confirm(id) {
            self.kill_window(id);
            return;
        }
        let dialog = close_dialog("Close Tab?", "All terminal sessions in this tab will be terminated.");
        let weak = Rc::downgrade(self);
        dialog.connect_response(None, move |_, response| {
            if let Some(win) = weak.upgrade()
                && response == "close"
            {
                win.kill_window(id);
            }
        });
        dialog.present(Some(&self.window));
    }

    fn kill_window(self: &Rc<Self>, id: u32) {
        let effects = self.state.borrow_mut().session.kill_window_id(id);
        self.apply(effects);
        self.queue_reconcile();
    }

    /// ghostty's needsConfirmQuit: a live process, which with
    /// confirm-close-surface = true is not sitting at its shell prompt.
    fn pane_needs_confirm(&self, pane: PaneId) -> bool {
        let confirm = self.app.config().confirm_close_surface;
        let st = self.state.borrow();
        st.panes.get(&pane).and_then(|p| p.io.as_ref()).is_some_and(|io| {
            let alive = io.shared.exited.get().is_none();
            match confirm {
                ConfirmClose::False => false,
                ConfirmClose::Always => alive,
                ConfirmClose::True => alive && !io.shared.term.lock().cursor_at_prompt(),
            }
        })
    }

    fn window_needs_confirm(&self, id: u32) -> bool {
        let windows = self.state.borrow().session.windows();
        let panes: Vec<PaneId> = windows.iter().filter(|w| w.id == id).flat_map(|w| w.panes.iter().map(|p| p.pane)).collect();
        panes.into_iter().any(|p| self.pane_needs_confirm(p))
    }

    fn needs_confirm(&self) -> bool {
        let panes: Vec<PaneId> = self.state.borrow().panes.keys().copied().collect();
        panes.into_iter().any(|p| self.pane_needs_confirm(p))
    }

    /// ghostty's close-split: asks only when the pane needs it.
    fn confirm_close_split(self: &Rc<Self>) {
        let pane = self.state.borrow().session.active_pane();
        if !self.pane_needs_confirm(pane) {
            self.command(Command::KillPane);
            return;
        }
        let dialog = close_dialog("Close Split?", "The currently running process in this split will be terminated.");
        let weak = Rc::downgrade(self);
        dialog.connect_response(None, move |_, response| {
            if let Some(win) = weak.upgrade()
                && response == "close"
            {
                win.command(Command::KillPane);
            }
        });
        dialog.present(Some(&self.window));
    }

    /// ghostty's windowCloseRequest: asks before closing a window with a
    /// pane that needs it. The window closes with its session.
    fn close_request(self: &Rc<Self>) -> glib::Propagation {
        if !self.needs_confirm() {
            self.close();
            return glib::Propagation::Stop;
        }
        let dialog = close_dialog("Close Window?", "All terminal sessions in this window will be terminated.");
        let weak = Rc::downgrade(self);
        dialog.connect_response(None, move |_, response| {
            if let Some(win) = weak.upgrade()
                && response == "close"
            {
                win.close();
            }
        });
        dialog.present(Some(&self.window));
        glib::Propagation::Stop
    }

    /// kill-session, and the window goes even if its session had ended.
    fn close(self: &Rc<Self>) {
        let effects = self.state.borrow_mut().session.kill();
        self.apply(effects.into_iter().filter(|e| *e != Effect::Exit).collect());
        self.close_later();
    }

    /// Destroys the window once the signal being handled is over: a drag
    /// that emptied the session is still using the tab view.
    fn close_later(self: &Rc<Self>) {
        self.closing.set(true);
        let weak = Rc::downgrade(self);
        glib::idle_add_local_once(move || {
            if let Some(win) = weak.upgrade() {
                win.window.destroy();
            }
        });
    }

    /// ghostty's TitleDialog initial value: the target's title override, else
    /// the title it shows now.
    fn title_initial(&self, target: TitleTarget) -> String {
        match target {
            TitleTarget::Tab(id) => {
                let windows = self.state.borrow().session.windows();
                let Some(info) = windows.iter().find(|w| w.id == id) else { return String::new() };
                if info.renamed && !info.name.is_empty() { info.name.clone() } else { self.tab_title(info) }
            }
            TitleTarget::Window => {
                let over = self.state.borrow().window_title_override.clone();
                over.unwrap_or_else(|| self.window.title().map(Into::into).unwrap_or_default())
            }
            TitleTarget::Surface => {
                let st = self.state.borrow();
                let pane = &st.panes[&st.session.active_pane()];
                pane.title_override.clone().unwrap_or_else(|| pane.title.clone())
            }
        }
    }

    /// ghostty's TitleDialog for a tab, the window or the focused surface.
    fn title_dialog(self: &Rc<Self>, target: TitleTarget) {
        let heading = match target {
            TitleTarget::Tab(_) => "Change Tab Title",
            TitleTarget::Window => "Change Window Title",
            TitleTarget::Surface => "Change Terminal Title",
        };
        let (dialog, entry) = title_dialog_widgets(heading, &self.title_initial(target));
        let weak = Rc::downgrade(self);
        dialog.connect_response(None, move |_, response| {
            let Some(win) = weak.upgrade() else { return };
            if response != "ok" {
                return;
            }
            let text = entry.text().to_string();
            let value = Some(text.clone()).filter(|t| !t.is_empty());
            match target {
                TitleTarget::Tab(id) => {
                    win.state.borrow_mut().session.rename_window_id(id, &text);

                    win.queue_reconcile();
                }
                TitleTarget::Window => {
                    win.state.borrow_mut().window_title_override = value;
                    win.queue_reconcile();
                }
                TitleTarget::Surface => {
                    let mut st = win.state.borrow_mut();
                    let active = st.session.active_pane();
                    if let Some(p) = st.panes.get_mut(&active) {
                        p.title_override = value;
                    }
                    drop(st);
                    win.queue_reconcile();
                }
            }
        });
        dialog.present(Some(&self.window));
    }

    fn active_io<R>(&self, f: impl FnOnce(&PaneIo<term::Terminal>) -> R) -> Option<R> {
        let st = self.state.borrow();
        st.panes.get(&st.session.active_pane()).and_then(|p| p.io.as_ref()).map(f)
    }

    fn active_surface(&self) -> Option<Rc<Surface>> {
        let st = self.state.borrow();
        st.panes.get(&st.session.active_pane()).map(|p| p.surface.clone())
    }

    /// ghostty's copy_to_clipboard: the surface's selection to the
    /// clipboard as plain text and HTML, with ghostty's toast; returns
    /// whether it had one.
    fn copy(&self, surface: &Surface) -> bool {
        let copied = surface.copy_selection(&self.window.clipboard());
        if copied {
            self.toast("Copied to clipboard");
        }
        copied
    }

    /// ghostty's clipboard write for a program's OSC 52, plain text only,
    /// with its toast (app-notifications clipboard-copy): a copy-mode copy
    /// reaches the clipboard as tmux's set-clipboard sends it with OSC 52.
    fn set_clipboard(&self, text: &str) {
        self.window.clipboard().set_text(text);
        self.toast("Copied to clipboard");
    }

    /// tmux's paste-buffer: the buffer as it is to the pane's PTY, fenced
    /// when `bracket` (`-p`) and the pane's program asked for bracketed paste.
    fn paste_buffer(&self, pane: PaneId, data: &[u8], bracket: bool) {
        let st = self.state.borrow();
        let Some(p) = st.panes.get(&pane) else { return };
        // window_pane_exited
        if p.child_exited {
            drop(st);
            self.toast("Target pane has exited");
            return;
        }
        let Some(io) = &p.io else { return };
        let fenced = bracket && io.shared.term.lock().modes().bracketed_paste;
        let mut bytes = Vec::with_capacity(data.len() + 12);
        if fenced {
            bytes.extend_from_slice(b"\x1b[200~");
        }
        bytes.extend_from_slice(data);
        if fenced {
            bytes.extend_from_slice(b"\x1b[201~");
        }
        io.shared.write(&bytes);
    }

    /// ghostty's syncActions: Copy is enabled while the surface has a
    /// selection.
    fn sync_copy_action(&self) {
        let selected = self.active_surface().is_some_and(|s| s.has_selection());
        if let Some(action) = self.window.lookup_action("copy").and_then(|a| a.downcast::<gio::SimpleAction>().ok()) {
            action.set_enabled(selected);
        }
    }

    /// ghostty's paste_from_clipboard, or paste_from_selection with the
    /// primary selection.
    fn paste(self: &Rc<Self>, clipboard: gdk::Clipboard) {
        let weak = Rc::downgrade(self);
        clipboard.read_text_async(gio::Cancellable::NONE, move |text| {
            let (Some(win), Ok(Some(text))) = (weak.upgrade(), text) else { return };
            win.paste_text(&text, false);
        });
    }

    /// ghostty's completeClipboardPaste: with clipboard-paste-protection, an
    /// unbracketed paste with a line break (or a bracketed one with the end
    /// fence) waits for confirmation; the text is then encoded as xterm does
    /// and the view returns to the bottom.
    fn paste_text(self: &Rc<Self>, text: &str, allow_unsafe: bool) {
        let Some(bracketed) = self.active_io(|io| io.shared.term.lock().modes().bracketed_paste) else { return };
        let unsafe_paste = if bracketed { text.contains("\x1b[201~") } else { text.contains(['\r', '\n']) };
        if unsafe_paste && !allow_unsafe {
            self.confirm_paste(text);
            return;
        }
        let bytes = encode_paste(text, bracketed);
        self.active_io(|io| {
            let mut term = io.shared.term.lock();
            let offset = term.view_offset() as isize;
            term.scroll_view(-offset);
            drop(term);
            io.shared.write(&bytes);
        });
    }

    /// ghostty's ClipboardConfirmationDialog for an unsafe paste.
    fn confirm_paste(self: &Rc<Self>, text: &str) {
        let dialog = adw::AlertDialog::new(
            Some("Warning: Potentially Unsafe Paste"),
            Some("Pasting this text into the terminal may be dangerous as it looks like some commands may be executed."),
        );
        dialog.add_css_class("clipboard-confirmation-dialog");
        dialog.add_response("cancel", "Deny");
        dialog.add_response("ok", "Allow");
        dialog.set_response_appearance("cancel", adw::ResponseAppearance::Suggested);
        dialog.set_response_appearance("ok", adw::ResponseAppearance::Destructive);
        dialog.set_default_response(Some("cancel"));
        dialog.set_close_response("cancel");

        let view = gtk::TextView::builder().cursor_visible(false).editable(false).monospace(true).build();
        view.buffer().set_text(text);
        let scroll = gtk::ScrolledWindow::builder().child(&view).build();
        let stack = theme::styled(
            gtk::Stack::builder().width_request(500).height_request(200).build(),
            &["clipboard-contents"],
        );
        stack.add_titled(&scroll, Some("part-0"), "text/plain;charset=utf-8");
        let overlay = theme::styled(gtk::Overlay::builder().child(&stack).build(), &["osd", "clipboard-overlay"]);
        let parts = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(6).build();
        parts.append(&overlay);
        // The remember switch is hidden for pastes but stays in the list,
        // where it keeps the parts row from being the last row.
        let remember = theme::styled(
            adw::SwitchRow::builder()
                .title("Remember choice for this split")
                .subtitle("Reload configuration to show this prompt again")
                .visible(false)
                .build(),
            &["card"],
        );
        let list = gtk::ListBox::builder().selection_mode(gtk::SelectionMode::None).build();
        list.append(&parts);
        list.append(&remember);
        dialog.set_extra_child(Some(&list));

        let weak = Rc::downgrade(self);
        let text = text.to_owned();
        dialog.connect_response(None, move |_, response| {
            if let Some(win) = weak.upgrade()
                && response == "ok"
            {
                win.paste_text(&text, true);
            }
        });
        dialog.present(Some(&self.window));
    }

    fn reset(&self) {
        // ghostty's reset action: a full reset, which also drops the selection.
        self.active_io(|io| io.shared.term.lock().full_reset());
        self.sync_copy_action();
        if let Some(surface) = self.active_surface() {
            surface.refresh_search();
            surface.area.queue_render();
        }
    }

    /// ghostty's clear_screen: erase the screen and the scrollback, and let
    /// the shell redraw its prompt.
    fn clear(&self) {
        self.active_io(|io| {
            io.shared.term.lock().feed(b"\x1b[H\x1b[2J\x1b[3J");
            io.shared.write(b"\x0c");
        });
        if let Some(surface) = self.active_surface() {
            surface.clear_selection();
        }
    }
}

type Action = Box<dyn Fn(&Rc<Window>)>;

#[derive(Clone, Copy)]
enum TitleTarget {
    /// The tab of the window with this tmux id.
    Tab(u32),
    Window,
    Surface,
}

/// ghostty's CloseConfirmationDialog.
fn close_dialog(heading: &str, body: &str) -> adw::AlertDialog {
    let dialog = adw::AlertDialog::new(Some(heading), Some(body));
    dialog.add_response("cancel", "Cancel");
    dialog.add_response("close", "Close");
    dialog.set_response_appearance("close", adw::ResponseAppearance::Destructive);
    dialog.set_close_response("cancel");
    dialog
}

/// ghostty's TitleDialog.
fn title_dialog_widgets(heading: &str, text: &str) -> (adw::AlertDialog, gtk::Entry) {
    let (dialog, entry) = prompt_dialog_widgets(heading, text);
    dialog.set_body("Leave blank to restore the default title.");
    (dialog, entry)
}

/// A dialog with an entry for tmux's prompts: ghostty's TitleDialog
/// without its body.
fn prompt_dialog_widgets(heading: &str, text: &str) -> (adw::AlertDialog, gtk::Entry) {
    let dialog = adw::AlertDialog::new(Some(heading), None);
    dialog.add_response("cancel", "Cancel");
    dialog.add_response("ok", "OK");
    dialog.set_response_appearance("ok", adw::ResponseAppearance::Suggested);
    dialog.set_default_response(Some("ok"));
    let entry = gtk::Entry::builder().activates_default(true).text(text).build();
    dialog.set_extra_child(Some(&entry));
    dialog.set_property("focus-widget", &entry);
    (dialog, entry)
}

/// ghostty's input/paste encode (after xterm): bytes that could inject
/// commands become spaces, a bracketed paste is fenced, and an unbracketed
/// one sends line feeds as carriage returns.
fn encode_paste(text: &str, bracketed: bool) -> Vec<u8> {
    const STRIP: [u8; 16] = [0x00, 0x08, 0x05, 0x04, 0x1B, 0x7F, 0x03, 0x1C, 0x15, 0x1A, 0x11, 0x13, 0x17, 0x16, 0x12, 0x0F];
    let mut bytes: Vec<u8> = text.bytes().map(|b| if STRIP.contains(&b) { b' ' } else { b }).collect();
    if bracketed {
        bytes.splice(0..0, *b"\x1b[200~");
        bytes.extend_from_slice(b"\x1b[201~");
    } else {
        for b in &mut bytes {
            if *b == b'\n' {
                *b = b'\r';
            }
        }
    }
    bytes
}

/// Runs on the main thread when a pane's I/O thread has news.
fn wake(pane: PaneId) {
    let app = APP.with(|a| a.borrow().clone());
    if let Some(app) = app {
        app.wake(pane);
    }
}

/// ghostty's colorScheme: the window's light or dark style. The terminal
/// background decides for auto and ghostty, except with a light and dark
/// theme pair, whose side follows the desktop.
fn color_scheme(config: &Config) -> adw::ColorScheme {
    match config.window_theme {
        WindowTheme::Auto | WindowTheme::Ghostty if config.theme.as_ref().is_some_and(Theme::conditional) => {
            adw::ColorScheme::PreferLight
        }
        WindowTheme::Auto | WindowTheme::Ghostty => {
            // Perceived luminance.
            let Rgb { r, g, b } = config.background;
            let luminance = (0.299 * f64::from(r) + 0.587 * f64::from(g) + 0.114 * f64::from(b)) / 255.0;
            if luminance > 0.5 { adw::ColorScheme::PreferLight } else { adw::ColorScheme::PreferDark }
        }
        WindowTheme::System => adw::ColorScheme::PreferLight,
        WindowTheme::Light => adw::ColorScheme::ForceLight,
        WindowTheme::Dark => adw::ColorScheme::ForceDark,
    }
}

fn css_scheme(style: &adw::StyleManager) -> gtk::InterfaceColorScheme {
    if style.is_dark() { gtk::InterfaceColorScheme::Dark } else { gtk::InterfaceColorScheme::Light }
}

/// ghostty's style.css and the runtime CSS rules of loadRuntimeCss that
/// apply to tapust.
fn css(config: &Config) -> String {
    let rgb = |Rgb { r, g, b }: Rgb| format!("rgb({r},{g},{b})");
    let mut css = format!("{}\n", theme::GHOSTTY_CSS);
    if config.window_theme == WindowTheme::Ghostty {
        let fg = rgb(config.window_titlebar_foreground.unwrap_or(config.foreground));
        let bg = rgb(config.window_titlebar_background.unwrap_or(config.background));
        css.push_str(&format!(
            ":root {{
  --ghostty-fg: {fg};
  --ghostty-bg: {bg};
  --headerbar-fg-color: var(--ghostty-fg);
  --headerbar-bg-color: var(--ghostty-bg);
  --headerbar-backdrop-color: oklab(from var(--headerbar-bg-color) calc(l * 0.9) a b / alpha);
  --overview-fg-color: var(--ghostty-fg);
  --overview-bg-color: var(--ghostty-bg);
  --popover-fg-color: var(--ghostty-fg);
  --popover-bg-color: var(--ghostty-bg);
  --window-fg-color: var(--ghostty-fg);
  --window-bg-color: var(--ghostty-bg);
}}
listview.command-palette-view {{
  --view-fg-color: var(--ghostty-fg);
  --view-bg-color: var(--ghostty-bg);
}}
windowhandle {{
  background-color: var(--headerbar-bg-color);
  color: var(--headerbar-fg-color);
}}
windowhandle:backdrop {{
 background-color: var(--headerbar-backdrop-color);
}}
"
        ));
    }
    let fill = rgb(config.unfocused_split_fill.unwrap_or(config.background));
    let opacity = 1.0 - config.unfocused_split_opacity;
    css.push_str(&format!("widget.unfocused-split {{\n opacity: {opacity:.2};\n background-color: {fill};\n}}\n"));
    if let Some(color) = config.split_divider_color {
        let color = rgb(color);
        css.push_str(&format!(".window .split paned > separator {{\n  color: {color};\n  background: {color};\n}}\n"));
    }
    css
}

/// tmux-mode-keys, else tmux's main's choice from $VISUAL or $EDITOR.
fn mode_keys(config: &Config) -> ModeKeys {
    config.tmux_mode_keys.unwrap_or_else(|| {
        let (visual, editor) = (std::env::var_os("VISUAL"), std::env::var_os("EDITOR"));
        ModeKeys::from_env(visual.as_deref(), editor.as_deref())
    })
}

/// The header of a configuration file Open Configuration makes.
const CONFIG_HEADER: &str = "# tapust configuration, in ghostty's syntax: one `key = value` per line.
# Reload Configuration (in the menu or the command palette) applies changes.
";

/// Makes the configuration file with its header unless there is one.
fn create_config(path: &Path) -> Result<(), String> {
    let make = || -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        match std::fs::OpenOptions::new().write(true).create_new(true).open(path) {
            Ok(mut file) => std::io::Write::write_all(&mut file, CONFIG_HEADER.as_bytes()),
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
            Err(err) => Err(err),
        }
    };
    make().map_err(|err| format!("Cannot create {}: {err}", path.display()))
}

/// `word` quoted for /bin/sh.
fn shell_quote(word: &std::ffi::OsStr) -> OsString {
    let mut quoted = b"'".to_vec();
    for &b in word.as_bytes() {
        if b == b'\'' {
            quoted.extend_from_slice(b"'\\''");
        } else {
            quoted.push(b);
        }
    }
    quoted.push(b'\'');
    OsString::from_vec(quoted)
}

#[cfg(test)]
mod tests {
    use super::encode_paste;

    #[test]
    fn paste_encoding_follows_ghostty() {
        assert_eq!(encode_paste("a\nb\x1bc\x03", false), b"a\rb c ");
        assert_eq!(encode_paste("a\nb", true), b"\x1b[200~a\nb\x1b[201~");
    }
}
