//! The application window as ghostty builds it with GTK and libadwaita
//! (window.blp, tab.blp, split_tree.zig), driven by the tmux model of the
//! `mux` crate: every tmux window is a tab, every tab holds the split tree.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::{Rc, Weak};
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use adw::prelude::*;
use gtk::{gio, glib};
use mux::{Command, ConfirmTarget, Effect, KeyResult, Node, Overlay, PaneId, Rect, Session};

use crate::compose;
use crate::input;
use crate::pty::{PaneIo, Pty};
use crate::surface::{Event, Surface};
use crate::theme;

const SCROLLBACK: usize = 10_000;
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
    /// The terminal title (OSC 0/2) last seen; empty until the program sets one.
    title: String,
    /// ghostty's surface title override (Change Terminal Title).
    title_override: Option<String>,
    /// The working directory the terminal reported (OSC 7), ghostty's pwd.
    pwd: Option<String>,
}

/// A tab: ghostty's Tab (a vertical box) holding a SplitTree whose tree_bin
/// shows the widgets built from the window's split tree.
struct Tab {
    page: adw::TabPage,
    tree_bin: adw::Bin,
    /// The tree and zoomed pane the widgets were built from.
    built: Option<(Node, Option<PaneId>)>,
    splits: Vec<Rc<SplitWidget>>,
}

struct State {
    session: Session,
    panes: HashMap<PaneId, Pane>,
    /// Tabs by tmux window id.
    tabs: HashMap<u32, Tab>,
    cwd: PathBuf,
    /// Program for the first pane (`-e`); later panes run the login shell.
    command: Option<Vec<String>>,
    palette: term::Palette,
    term_bg: [u8; 4],
    /// The prompt or message on screen, as a dialog or a toast.
    shown: Option<Overlay>,
    window_title_override: Option<String>,
    tick: Option<glib::SourceId>,
}

pub struct App {
    state: RefCell<State>,
    window: adw::ApplicationWindow,
    tab_view: adw::TabView,
    toasts: adw::ToastOverlay,
    title: adw::WindowTitle,
    /// Set while the application changes GTK, so its own changes do not
    /// come back as user actions.
    syncing: Cell<bool>,
    reconcile_pending: Cell<bool>,
    restore_focus_pending: Cell<bool>,
    /// The window's minimum size was set from the first surface.
    min_size_set: Cell<bool>,
    /// The tab whose context menu is open (ghostty's context_menu_page).
    context_page: RefCell<Option<adw::TabPage>>,
    overview: adw::TabOverview,
    overview_focus: RefCell<Option<glib::SourceId>>,
    /// The last window closed while the tab overview was open: the window
    /// stays (ghostty's tabViewNPages) and the next new tab starts a session.
    session_ended: Cell<bool>,
}

/// ghostty's SplitTreeSplit: Adw.Bin > Adw.Bin > GtkPaned, keeping the
/// widget's ratio in sync with the tree (syncSplitRatio).
struct SplitWidget {
    bin: adw::Bin,
    paned: gtk::Paned,
    window: u32,
    index: usize,
    /// ghostty's priv.ratio: the tree's when built, then the last one written.
    ratio: Cell<f64>,
    limits_changed: Cell<bool>,
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
        let bin = adw::Bin::builder().css_classes(["split"]).child(&adw::Bin::builder().child(&paned).build()).build();
        let split = Rc::new(SplitWidget {
            bin,
            paned,
            window,
            index,
            ratio: Cell::new(ratio),
            limits_changed: Cell::new(false),
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
                    s.limits_changed.set(true);
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
            let (limits, pos) = (s.limits_changed.replace(false), s.pos_changed.replace(false));
            // Only the position changed: the user dragged the divider.
            if !limits && pos {
                s.sync(Sync::WidgetToTree);
            }
        });
        *self.idle.borrow_mut() = Some(id);
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
            app.set_ratio(self.window, self.index, ratio);
        }
    }
}

/// Removes a reused widget from its old parent (ghostty's detachWidget).
fn detach(widget: &gtk::Widget) {
    let Some(parent) = widget.parent() else { return };
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

/// ghostty's main_menu.
fn main_menu() -> gio::Menu {
    let menu = gio::Menu::new();
    let section = |items: &[(&str, &str)]| {
        let s = gio::Menu::new();
        for (label, action) in items {
            menu_item(&s, label, action);
        }
        s
    };
    menu.append_section(None, &section(&[("Copy", "win.copy"), ("Paste", "win.paste")]));
    menu.append_section(
        None,
        &section(&[("Change Window Title\u{2026}", "win.prompt-window-title"), ("New Window", "win.new-window"), ("Close Window", "win.close")]),
    );
    let tab = section(&[("Change Tab Title\u{2026}", "win.prompt-tab-title"), ("New Tab", "win.new-tab")]);
    let close_tab = gio::MenuItem::new(Some("Close Tab"), None);
    close_tab.set_action_and_target_value(Some("win.close-tab"), Some(&"this".to_variant()));
    tab.append_item(&close_tab);
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
    let config = gio::Menu::new();
    let os_open = gio::MenuItem::new(Some("Open Configuration in OS Editor"), None);
    os_open.set_action_and_target_value(Some("app.open-config"), Some(&"os-open".to_variant()));
    config.append_item(&os_open);
    let new_window = gio::MenuItem::new(Some("Open Configuration in New Window"), None);
    new_window.set_action_and_target_value(Some("app.open-config"), Some(&"new-window".to_variant()));
    config.append_item(&new_window);
    menu_item(&config, "Reload Configuration", "app.reload-config");
    tools.append_submenu(Some("Config"), &config);
    menu.append_section(None, &tools);
    menu.append_section(None, &section(&[("About tapust", "win.about"), ("Quit", "app.quit")]));
    menu
}

impl App {
    pub fn build(application: &adw::Application, command: Option<Vec<String>>) -> Rc<App> {
        let tab_view = adw::TabView::builder().shortcuts(adw::TabViewShortcuts::empty()).build();
        let toasts = adw::ToastOverlay::new();
        toasts.set_child(Some(&tab_view));
        let content = gtk::Box::builder().orientation(gtk::Orientation::Vertical).build();
        content.append(&toasts);

        let title = adw::WindowTitle::new("tapust", "");
        let header = adw::HeaderBar::builder().title_widget(&title).build();
        let new_tab = adw::SplitButton::builder()
            .icon_name("tab-new-symbolic")
            .tooltip_text("New Tab")
            .dropdown_tooltip("New Split")
            .menu_model(&split_menu())
            .can_focus(false)
            .focus_on_click(false)
            .build();
        header.pack_start(&new_tab);
        let end = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        let overview_toggle = gtk::ToggleButton::builder()
            .icon_name("view-grid-symbolic")
            .tooltip_text("View Open Tabs")
            .can_focus(false)
            .focus_on_click(false)
            .build();
        let menu = gtk::MenuButton::builder()
            .icon_name("open-menu-symbolic")
            .menu_model(&main_menu())
            .tooltip_text("Main Menu")
            .can_focus(false)
            .build();
        end.append(&overview_toggle);
        end.append(&menu);
        header.pack_end(&end);

        let tab_bar = adw::TabBar::builder().view(&tab_view).autohide(true).expand_tabs(true).build();
        let toolbar = adw::ToolbarView::builder().top_bar_style(adw::ToolbarStyle::Raised).bottom_bar_style(adw::ToolbarStyle::Raised).build();
        toolbar.add_top_bar(&header);
        toolbar.add_top_bar(&tab_bar);
        toolbar.set_content(Some(&content));
        let overview = adw::TabOverview::builder().view(&tab_view).enable_new_tab(true).child(&toolbar).build();
        overview.bind_property("open", &overview_toggle, "active").bidirectional().sync_create().build();

        let window = adw::ApplicationWindow::builder()
            .application(application)
            .default_width(800)
            .default_height(600)
            .content(&overview)
            .build();
        window.set_property("handle-menubar-accel", false);
        window.add_css_class("window");
        window.set_icon_name(Some(theme::ICON));
        // ghostty's headerbar-visible: never shown while fullscreen.
        window.connect_fullscreened_notify(move |w| header.set_visible(!w.is_fullscreen()));

        let palette = theme::palette();
        let term_bg = compose::resolve(term::Color::Default, &palette, palette.background);
        // ghostty's window-theme = auto: the scheme follows how light the
        // terminal background is (perceived luminance).
        let [r, g, b, _] = term_bg.map(|c| f64::from(c) / 255.0);
        let luminance = 0.299 * r + 0.587 * g + 0.114 * b;
        application.style_manager().set_color_scheme(if luminance > 0.5 {
            adw::ColorScheme::PreferLight
        } else {
            adw::ColorScheme::PreferDark
        });
        let (session, effects) = Session::new();
        let app = Rc::new(App {
            state: RefCell::new(State {
                session,
                panes: HashMap::new(),
                tabs: HashMap::new(),
                cwd: std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/")),
                command,
                palette,
                term_bg,
                shown: None,
                window_title_override: None,
                tick: None,
            }),
            window,
            tab_view,
            toasts,
            title,
            syncing: Cell::new(false),
            reconcile_pending: Cell::new(false),
            restore_focus_pending: Cell::new(false),
            min_size_set: Cell::new(false),
            context_page: RefCell::new(None),
            overview: overview.clone(),
            overview_focus: RefCell::new(None),
            session_ended: Cell::new(false),
        });
        APP.with(|a| *a.borrow_mut() = Some(app.clone()));

        let weak = Rc::downgrade(&app);
        new_tab.connect_clicked(move |_| {
            if let Some(app) = weak.upgrade() {
                app.command(Command::NewWindow { cwd_of_active: true, after: true });
            }
        });
        app.connect_tabs(&overview);
        app.connect_overview();
        app.add_actions(application);
        app.apply(effects);
        app.reconcile();
        app.start_timers();
        app.window.present();
        app
    }

    fn connect_tabs(self: &Rc<Self>, overview: &adw::TabOverview) {
        let menu = gio::Menu::new();
        menu.append(Some("Change Tab Title\u{2026}"), Some("win.prompt-context-tab-title"));
        self.tab_view.set_menu_model(Some(&menu));
        let weak = Rc::downgrade(self);
        self.tab_view.connect_setup_menu(move |_, page| {
            if let Some(app) = weak.upgrade() {
                *app.context_page.borrow_mut() = page.cloned();
            }
        });
        let weak = Rc::downgrade(self);
        self.tab_view.connect_close_page(move |view, page| {
            let Some(app) = weak.upgrade() else { return glib::Propagation::Proceed };
            if app.syncing.get() {
                return glib::Propagation::Proceed;
            }
            // Closing a tab goes through tmux's kill-window confirmation.
            view.close_page_finish(page, false);
            if let Some(index) = app.window_of_page(page) {
                app.command(Command::SelectWindow(index));
                app.confirm_close_tab();
            }
            glib::Propagation::Stop
        });
        let weak = Rc::downgrade(self);
        self.tab_view.connect_selected_page_notify(move |view| {
            let Some(app) = weak.upgrade() else { return };
            if app.syncing.get() {
                return;
            }
            if let Some(index) = view.selected_page().and_then(|p| app.window_of_page(&p)) {
                app.command(Command::SelectWindow(index));
            }
        });
        let weak = Rc::downgrade(self);
        self.tab_view.connect_page_reordered(move |_, page, position| {
            let Some(app) = weak.upgrade() else { return };
            if app.syncing.get() {
                return;
            }
            // tmux moves a window one place at a time (swap-window -d -t +1 or -1).
            let Some(index) = app.window_of_page(page) else { return };
            let from = app.state.borrow().session.windows().iter().position(|w| w.index == index).unwrap_or(0) as i32;
            app.command(Command::SelectWindow(index));
            let step = if position > from { 1 } else { -1 };
            for _ in 0..(position - from).abs() {
                app.command(Command::SwapWindow(step));
            }
        });
        self.tab_view.connect_create_window(|_| None);
        let weak = Rc::downgrade(self);
        overview.connect_create_tab(move |_| {
            let app = weak.upgrade().expect("app");
            if app.session_ended.replace(false) {
                let (session, effects) = Session::new();
                app.state.borrow_mut().session = session;
                app.apply(effects);
            } else {
                app.command(Command::NewWindow { cwd_of_active: true, after: true });
            }
            app.reconcile();
            app.tab_view.selected_page().expect("new tab")
        });
    }

    /// ghostty's tabOverviewOpen: focus can get lost when the overview
    /// closes (libadwaita#670), so the active surface takes it again after
    /// the 400ms animation.
    fn connect_overview(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        self.overview.connect_open_notify(move |overview| {
            let Some(app) = weak.upgrade() else { return };
            if overview.is_open() {
                return;
            }
            if let Some(old) = app.overview_focus.take() {
                old.remove();
            }
            let weak = Rc::downgrade(&app);
            let id = glib::timeout_add_local_once(theme::OVERVIEW_FOCUS, move || {
                if let Some(app) = weak.upgrade() {
                    app.overview_focus.take();
                    if let Some(area) = app.active_area() {
                        area.grab_focus();
                    }
                }
            });
            *app.overview_focus.borrow_mut() = Some(id);
        });
    }

    fn add_actions(self: &Rc<Self>, application: &adw::Application) {
        let add = |name: &str, f: Action| {
            let action = gio::SimpleAction::new(name, None);
            let weak = Rc::downgrade(self);
            action.connect_activate(move |_, _| {
                if let Some(app) = weak.upgrade() {
                    f(&app);
                }
            });
            self.window.add_action(&action);
        };
        for (name, horizontal, before) in
            [("split-up", false, true), ("split-down", false, false), ("split-left", true, true), ("split-right", true, false)]
        {
            add(name, Box::new(move |app| app.command(Command::SplitWindow { horizontal, before, full: false, cwd_of_active: true })));
        }
        add("new-tab", Box::new(|app| app.command(Command::NewWindow { cwd_of_active: true, after: true })));
        add("new-window", Box::new(|_| {
            if let Ok(exe) = std::env::current_exe() {
                let _ = std::process::Command::new(exe).spawn();
            }
        }));
        add("close", Box::new(|app| app.confirm_close_window()));
        add("paste", Box::new(|app| app.paste()));
        add("reset", Box::new(|app| app.reset()));
        add("clear", Box::new(|app| app.clear()));
        add("prompt-tab-title", Box::new(|app| app.title_dialog(TitleTarget::Tab(app.current_window_id()))));
        add(
            "prompt-context-tab-title",
            Box::new(|app| {
                let page = app.context_page.borrow().clone();
                let id = page.and_then(|page| app.state.borrow().tabs.iter().find(|(_, t)| t.page == page).map(|(&id, _)| id));
                if let Some(id) = id {
                    app.title_dialog(TitleTarget::Tab(id));
                }
            }),
        );
        add("prompt-window-title", Box::new(|app| app.title_dialog(TitleTarget::Window)));
        add("prompt-surface-title", Box::new(|app| app.title_dialog(TitleTarget::Surface)));
        add("about", Box::new(|app| {
            let about = adw::AboutDialog::builder()
                .application_name("tapust")
                .developer_name("tapust Developers")
                .application_icon(theme::ICON)
                .version(env!("CARGO_PKG_VERSION"))
                .build();
            about.present(Some(&app.window));
        }));
        let close_tab = gio::SimpleAction::new("close-tab", Some(glib::VariantTy::STRING));
        let weak = Rc::downgrade(self);
        close_tab.connect_activate(move |_, _| {
            if let Some(app) = weak.upgrade() {
                app.confirm_close_tab();
            }
        });
        self.window.add_action(&close_tab);
        let quit = gio::SimpleAction::new("quit", None);
        let weak = Rc::downgrade(self);
        quit.connect_activate(move |_, _| {
            if let Some(app) = weak.upgrade()
                && let Some(application) = app.window.application()
            {
                application.quit();
            }
        });
        application.add_action(&quit);
    }

    fn start_timers(self: &Rc<Self>) {
        // ghostty's termios timer: polls each PTY for a password prompt.
        let weak = Rc::downgrade(self);
        glib::timeout_add_local(theme::TERMIOS_POLL, move || {
            let Some(app) = weak.upgrade() else { return glib::ControlFlow::Break };
            for p in app.state.borrow().panes.values() {
                if let Some(io) = &p.io {
                    p.surface.set_password_input(io.shared.password_input());
                }
            }
            glib::ControlFlow::Continue
        });
        let weak = Rc::downgrade(self);
        glib::timeout_add_local(NAME_INTERVAL, move || {
            let Some(app) = weak.upgrade() else { return glib::ControlFlow::Break };
            app.update_names();
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

    fn window_of_page(&self, page: &adw::TabPage) -> Option<u32> {
        let st = self.state.borrow();
        let id = st.tabs.iter().find(|(_, t)| &t.page == page).map(|(&id, _)| id)?;
        st.session.windows().into_iter().find(|w| w.id == id).map(|w| w.index)
    }

    pub fn handle(self: &Rc<Self>, event: Event) -> bool {
        match event {
            Event::Key { pane, keyval, state } => self.key(pane, keyval, state),
            Event::Resized { pane, cols, rows, cell } => {
                self.resized(pane, cols, rows, cell);
                true
            }
            Event::CellSize { cell: (cw, ch) } => {
                // ghostty's size_limit at the first surface's init: ten
                // columns and four rows of device pixels, used as logical.
                if !self.min_size_set.replace(true) {
                    self.window.set_size_request((cw * 10) as i32, (ch * 4) as i32);
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
                        if let Some(app) = weak.upgrade() {
                            app.restore_focus_pending.set(false);
                            if let Some(area) = app.active_area().filter(|a| a.is_mapped()) {
                                area.grab_focus();
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
        }
    }

    fn key(self: &Rc<Self>, pane: PaneId, keyval: gtk::gdk::Key, state: gtk::gdk::ModifierType) -> bool {
        let Some((key, text)) = input::from_gdk(keyval, state) else { return false };
        let mut st = self.state.borrow_mut();
        match st.session.key(key, Instant::now()) {
            KeyResult::Forward(id) => {
                let target = if st.panes.contains_key(&id) { id } else { pane };
                if let Some(io) = st.panes.get(&target).and_then(|p| p.io.as_ref()) {
                    let app_cursor = io.shared.term.lock().modes().app_cursor;
                    let mut bytes = Vec::new();
                    input::encode(key, text.as_deref(), app_cursor, &mut bytes);
                    io.shared.write(&bytes);
                }
                drop(st);
            }
            KeyResult::Handled(effects) => {
                drop(st);
                self.apply(effects);
                self.queue_reconcile();
            }
        }
        self.schedule_tick();
        true
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
            }
            None => {
                let pty = match st.command.take() {
                    Some(argv) => {
                        let mut command = std::process::Command::new(&argv[0]);
                        command.args(&argv[1..]);
                        Pty::spawn_command(command, cols, rows, cell, &p.cwd)
                    }
                    None => Pty::spawn(cols, rows, cell, &p.cwd),
                };
                let pty = match pty {
                    Ok(pty) => pty,
                    Err(err) => {
                        eprintln!("tapust: cannot start a shell: {err}");
                        return;
                    }
                };
                let mut terminal = term::Terminal::new(cols, rows, SCROLLBACK);
                terminal.set_default_palette(st.palette.clone());
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
        if io.shared.exited.load(Ordering::Acquire) {
            let effects = st.session.pane_exited(pane);
            drop(st);
            self.apply(effects);
            self.queue_reconcile();
            return;
        }
        let (title, pwd) = {
            let term = io.shared.term.lock();
            (term.title().to_owned(), term.reported_cwd().map(str::to_owned))
        };
        p.surface.output();
        p.surface.area.queue_render();
        let mut changed = pwd != p.pwd;
        p.pwd = pwd;
        if title != p.title {
            p.title = title.clone();
            st.session.set_pane_title(pane, &title);
            changed = true;
        }
        drop(st);
        if changed {
            self.queue_reconcile();
        }
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
            let Some(app) = weak.upgrade() else { return };
            let mut st = app.state.borrow_mut();
            st.tick = None;
            st.session.tick(Instant::now());
            drop(st);
            app.queue_reconcile();
            app.schedule_tick();
        }));
    }

    /// Carries out the model's effects.
    fn apply(self: &Rc<Self>, effects: Vec<Effect>) {
        for effect in effects {
            match effect {
                Effect::Spawn { pane, cwd_of } => {
                    let mut st = self.state.borrow_mut();
                    let cwd = cwd_of
                        .and_then(|p| st.panes.get(&p))
                        .and_then(|p| p.io.as_ref())
                        .and_then(|io| io.pty.foreground_cwd())
                        .unwrap_or_else(|| st.cwd.clone());
                    let weak = Rc::downgrade(self);
                    let handler: crate::surface::Handler =
                        Rc::new(move |event| weak.upgrade().is_some_and(|app| app.handle(event)));
                    let surface = Surface::new(pane, st.term_bg, handler);
                    st.panes.insert(pane, Pane { surface, io: None, cwd, title: String::new(), title_override: None, pwd: None });
                }
                Effect::Destroy { pane } => {
                    let removed = self.state.borrow_mut().panes.remove(&pane);
                    if let Some(p) = removed {
                        detach(p.surface.outer.upcast_ref());
                    }
                }
                Effect::Write { pane, bytes } => {
                    if let Some(io) = self.state.borrow().panes.get(&pane).and_then(|p| p.io.as_ref()) {
                        io.shared.write(&bytes);
                    }
                }
                Effect::ResizeSplit { pane, horizontal, cells } => self.resize_split(pane, horizontal, cells),
                Effect::Exit => {
                    if self.overview.is_open() {
                        self.session_ended.set(true);
                    } else if let Some(app) = self.window.application() {
                        app.quit();
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
            if let Some(app) = weak.upgrade() {
                app.reconcile_pending.set(false);
                app.reconcile();
            }
        });
    }

    fn queue_geometry(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        glib::idle_add_local_once(move || {
            if let Some(app) = weak.upgrade() {
                app.report_geometry();
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
        st.session.set_geometry(index, area, &panes);
    }

    /// Makes GTK show the model: one tab per window in index order, each with
    /// the widgets of its split tree, the current window selected and its
    /// active pane focused, and the prompt or message.
    fn reconcile(self: &Rc<Self>) {
        self.syncing.set(true);
        let windows = self.state.borrow().session.windows();
        // Tabs of windows that are gone.
        let gone: Vec<u32> =
            self.state.borrow().tabs.keys().copied().filter(|id| !windows.iter().any(|w| w.id == *id)).collect();
        for id in gone {
            let tab = self.state.borrow_mut().tabs.remove(&id);
            if let Some(tab) = tab {
                self.tab_view.close_page(&tab.page);
            }
        }
        for (position, info) in windows.iter().enumerate() {
            let exists = self.state.borrow().tabs.contains_key(&info.id);
            if !exists {
                let tree_bin = adw::Bin::builder().hexpand(true).vexpand(true).build();
                let split_tree = gtk::Box::builder().orientation(gtk::Orientation::Vertical).build();
                split_tree.append(&tree_bin);
                let tab = gtk::Box::builder().orientation(gtk::Orientation::Vertical).hexpand(true).vexpand(true).css_classes(["tab"]).build();
                tab.append(&split_tree);
                let page = self.tab_view.append(&tab);
                self.state.borrow_mut().tabs.insert(info.id, Tab { page, tree_bin, built: None, splits: Vec::new() });
            }
            let page = self.state.borrow().tabs[&info.id].page.clone();
            if self.tab_view.page_position(&page) != position as i32 {
                self.tab_view.reorder_page(&page, position as i32);
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
                self.tab_view.set_selected_page(&page);
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
        if let Some(area) = self.active_area() {
            area.grab_focus();
        }
        self.syncing.set(false);
        self.show_overlay();
        self.queue_geometry();
    }

    fn active_area(&self) -> Option<gtk::GLArea> {
        let st = self.state.borrow();
        st.panes.get(&st.session.active_pane()).map(|p| p.surface.area.clone())
    }

    /// Builds the tab's widgets again when its tree changed (ghostty's
    /// onRebuild): new split widgets, the same surfaces.
    fn rebuild(self: &Rc<Self>, index: u32, id: u32) {
        let (tree, zoomed) = {
            let st = self.state.borrow();
            let Some((tree, zoomed, _)) = st.session.tree(index) else { return };
            (tree.clone(), zoomed)
        };
        let unchanged = self.state.borrow().tabs.get(&id).and_then(|t| t.built.as_ref()) == Some(&(tree.clone(), zoomed));
        if unchanged {
            return;
        }
        let mut splits = Vec::new();
        let mut counter = 0;
        let widget = match zoomed {
            Some(pane) => self.build_leaf(pane),
            None => self.build_node(&tree, id, &mut counter, &mut splits),
        };
        let split = matches!(tree, Node::Split(_));
        let mut leaves = Vec::new();
        collect_leaves(&tree, &mut leaves);
        let mut st = self.state.borrow_mut();
        for pane in &leaves {
            if let Some(p) = st.panes.get(pane) {
                p.surface.set_split(split);
            }
        }
        let tab = st.tabs.get_mut(&id).expect("tab");
        tab.built = Some((tree, zoomed));
        tab.splits = splits;
        let tree_bin = tab.tree_bin.clone();
        drop(st);
        if let Some(w) = &widget {
            detach(w);
        }
        tree_bin.set_child(widget.as_ref());
    }

    fn build_leaf(&self, pane: PaneId) -> Option<gtk::Widget> {
        let st = self.state.borrow();
        let outer: gtk::Widget = st.panes.get(&pane)?.surface.outer.clone().upcast();
        drop(st);
        detach(&outer);
        Some(outer)
    }

    fn build_node(self: &Rc<Self>, node: &Node, window: u32, counter: &mut usize, splits: &mut Vec<Rc<SplitWidget>>) -> Option<gtk::Widget> {
        match node {
            Node::Leaf(pane) => self.build_leaf(*pane),
            Node::Split(s) => {
                let first = self.build_node(&s.first, window, counter, splits)?;
                let index = *counter;
                *counter += 1;
                let second = self.build_node(&s.second, window, counter, splits)?;
                let split = SplitWidget::new(self, window, index, s.horizontal, s.ratio, &first, &second);
                let widget: gtk::Widget = split.bin.clone().upcast();
                splits.push(split);
                Some(widget)
            }
        }
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
        if info.zoomed { format!("\u{1F50D} {title}") } else { title.to_owned() }
    }

    fn toast(&self, text: &str) {
        let toast = adw::Toast::builder().title(text).timeout(3).build();
        self.toasts.add_toast(toast);
    }

    /// Shows tmux's prompt or message the way ghostty asks and tells:
    /// confirmations and the title prompt as dialogs, messages as toasts.
    fn show_overlay(self: &Rc<Self>) {
        let overlay = self.state.borrow().session.overlay();
        if self.state.borrow().shown == overlay {
            return;
        }
        self.state.borrow_mut().shown = overlay.clone();
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
                    if let Some(app) = weak.upgrade() {
                        app.answer((response == "close").then_some("y"));
                    }
                });
                dialog.present(Some(&self.window));
            }
            Some(Overlay::Input { prompt, text, .. }) => {
                let (heading, text) = if prompt.contains("session") {
                    ("Rename Session", text)
                } else {
                    ("Change Tab Title", self.title_initial(TitleTarget::Tab(self.current_window_id())))
                };
                let (dialog, entry) = title_dialog_widgets(heading, &text);
                let weak = Rc::downgrade(self);
                dialog.connect_response(None, move |_, response| {
                    if let Some(app) = weak.upgrade() {
                        let text = entry.text();
                        app.answer((response == "ok").then_some(text.as_str()));
                    }
                });
                dialog.present(Some(&self.window));
            }
            None => {}
        }
    }

    fn answer(self: &Rc<Self>, reply: Option<&str>) {
        let effects = self.state.borrow_mut().session.answer(reply, Instant::now());
        self.apply(effects);
        self.queue_reconcile();
    }

    fn confirm_close_tab(self: &Rc<Self>) {
        let dialog = close_dialog("Close Tab?", "All terminal sessions in this tab will be terminated.");
        let weak = Rc::downgrade(self);
        dialog.connect_response(None, move |_, response| {
            if let Some(app) = weak.upgrade()
                && response == "close"
            {
                app.command(Command::KillWindow);
            }
        });
        dialog.present(Some(&self.window));
    }

    fn confirm_close_window(self: &Rc<Self>) {
        let dialog = close_dialog("Close Window?", "All terminal sessions in this window will be terminated.");
        let weak = Rc::downgrade(self);
        dialog.connect_response(None, move |_, response| {
            if let Some(app) = weak.upgrade()
                && response == "close"
                && let Some(application) = app.window.application()
            {
                application.quit();
            }
        });
        dialog.present(Some(&self.window));
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
            let Some(app) = weak.upgrade() else { return };
            if response != "ok" {
                return;
            }
            let text = entry.text().to_string();
            let value = Some(text.clone()).filter(|t| !t.is_empty());
            match target {
                TitleTarget::Tab(id) => {
                    app.state.borrow_mut().session.rename_window_id(id, &text);

                    app.queue_reconcile();
                }
                TitleTarget::Window => {
                    app.state.borrow_mut().window_title_override = value;
                    app.queue_reconcile();
                }
                TitleTarget::Surface => {
                    let mut st = app.state.borrow_mut();
                    let active = st.session.active_pane();
                    if let Some(p) = st.panes.get_mut(&active) {
                        p.title_override = value;
                    }
                    drop(st);
                    app.queue_reconcile();
                }
            }
        });
        dialog.present(Some(&self.window));
    }

    fn active_io<R>(&self, f: impl FnOnce(&PaneIo<term::Terminal>) -> R) -> Option<R> {
        let st = self.state.borrow();
        st.panes.get(&st.session.active_pane()).and_then(|p| p.io.as_ref()).map(f)
    }

    fn paste(self: &Rc<Self>) {
        let clipboard = self.window.clipboard();
        let weak = Rc::downgrade(self);
        clipboard.read_text_async(gio::Cancellable::NONE, move |text| {
            let (Some(app), Ok(Some(text))) = (weak.upgrade(), text) else { return };
            app.active_io(|io| {
                let bracketed = io.shared.term.lock().modes().bracketed_paste;
                let mut bytes = Vec::new();
                if bracketed {
                    bytes.extend_from_slice(b"\x1b[200~");
                }
                bytes.extend_from_slice(text.as_bytes());
                if bracketed {
                    bytes.extend_from_slice(b"\x1b[201~");
                }
                io.shared.write(&bytes);
            });
        });
    }

    fn reset(&self) {
        // RIS, as ghostty's reset action does.
        self.active_io(|io| io.shared.term.lock().feed(b"\x1bc"));
        let st = self.state.borrow();
        if let Some(p) = st.panes.get(&st.session.active_pane()) {
            p.surface.area.queue_render();
        }
    }

    /// ghostty's clear_screen: erase the screen and the scrollback, and let
    /// the shell redraw its prompt.
    fn clear(&self) {
        self.active_io(|io| {
            io.shared.term.lock().feed(b"\x1b[H\x1b[2J\x1b[3J");
            io.shared.write(b"\x0c");
        });
    }
}

type Action = Box<dyn Fn(&Rc<App>)>;

#[derive(Clone, Copy)]
enum TitleTarget {
    /// The tab of the window with this tmux id.
    Tab(u32),
    Window,
    Surface,
}

fn collect_leaves(node: &Node, out: &mut Vec<PaneId>) {
    match node {
        Node::Leaf(p) => out.push(*p),
        Node::Split(s) => {
            collect_leaves(&s.first, out);
            collect_leaves(&s.second, out);
        }
    }
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
    let dialog = adw::AlertDialog::new(Some(heading), Some("Leave blank to restore the default title."));
    dialog.add_response("cancel", "Cancel");
    dialog.add_response("ok", "OK");
    dialog.set_response_appearance("ok", adw::ResponseAppearance::Suggested);
    dialog.set_default_response(Some("ok"));
    let entry = gtk::Entry::builder().activates_default(true).text(text).build();
    dialog.set_extra_child(Some(&entry));
    dialog.set_property("focus-widget", &entry);
    (dialog, entry)
}

/// Runs on the main thread when a pane's I/O thread has news.
fn wake(pane: PaneId) {
    let app = APP.with(|a| a.borrow().clone());
    if let Some(app) = app {
        app.wake(pane);
    }
}

/// ghostty's style.css and runtime CSS rules that apply to tapust.
pub fn css() -> String {
    let [r, g, b] = theme::UNFOCUSED_FILL;
    format!(
        "{}\nwidget.unfocused-split {{\n opacity: {:.2};\n background-color: rgb({r},{g},{b});\n}}\n",
        theme::GHOSTTY_CSS,
        theme::UNFOCUSED_ALPHA
    )
}
