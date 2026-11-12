//! A pane's widgets, as ghostty builds them (surface-scrolled-window.blp and
//! surface.blp): an Adw.Bin with a GtkScrolledWindow around an Overlay whose
//! child is the GLArea that draws the terminal, with ghostty's progress bar,
//! child exited banner, resize overlay, search overlay, unfocused-split veil,
//! drag handle and drop overlay on top.

use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};
use std::sync::Arc;
use std::time::{Duration, Instant};

use adw::prelude::*;
use gtk::{gdk, gio, glib, graphene};
use mux::{Direction, PaneId};
use term::{Point, Selection};

use crate::bindings::{self, Binding, Scroll};
use crate::compose;
use crate::font::Fonts;
use crate::frame::{Frame, Grid};
use crate::pty::Shared;
use crate::renderer::Renderer;
use crate::scroll::SurfaceBin;
use crate::links;
use crate::mouse;
use crate::notifications::DesktopNotifications;
use crate::search;
use crate::selection::{self, DragPoint};
use crate::theme;

/// What a surface tells the application.
pub enum Event {
    /// GTK gave the GLArea a new size: the grid it holds.
    Resized { pane: PaneId, cols: u16, rows: u16, cell: (u32, u32) },
    Focused { pane: PaneId },
    /// A key press; returns whether it was used. `text` is what the input
    /// method committed for it, if anything.
    Key { pane: PaneId, keyval: gdk::Key, state: gdk::ModifierType, text: Option<String> },
    /// Text the input method committed outside a plain key press (a
    /// finished composition).
    Text { pane: PaneId, text: String },
    /// A pane's drag handle was dropped on the `dir` side of this pane.
    Dropped { source: PaneId, target: PaneId, dir: Direction },
    /// The GLArea was mapped (ghostty restores focus after this).
    Mapped,
    /// The bell-ringing state changed (the tab title shows it).
    BellChanged,
    /// The middle button asks to paste the primary selection.
    PastePrimary,
    /// The surface's selection appeared or went away (the Copy action).
    SelectionChanged,
    /// A touchpad swipe asks for the next (or previous) tab.
    SwipeTab { next: bool },
    /// A right click not reported to the program opens the context menu.
    ContextMenu { pane: PaneId, x: f64, y: f64 },
    /// The cell size in device pixels is known or changed (ghostty's
    /// cell_size action).
    CellSize { cell: (u32, u32) },
    /// The child exited banner's Close button: close without confirmation.
    ChildExitedClose { pane: PaneId },
}

pub type Handler = Rc<dyn Fn(Event) -> bool>;

/// ghostty's in_keyevent: whether a key press is being filtered through
/// the input method, and whether it was composing then.
#[derive(Clone, Copy, PartialEq, Eq)]
enum InKeyEvent {
    No,
    Composing,
    NotComposing,
}

/// ghostty's SelectionGesture for the left button.
#[derive(Default)]
struct Gesture {
    /// Clicks in the current series (1 cell, 2 word, 3 line or output).
    count: u8,
    /// The third click held ctrl: it and its drag select whole outputs
    /// (ghostty's left_click_behavior .output).
    output: bool,
    last_press: Option<Instant>,
    /// Where the series started, in device pixels.
    first_px: (f64, f64),
    click: Option<DragPoint>,
    left_down: bool,
    /// The press became a selection drag (ghostty's left_click_dragged).
    dragged: bool,
}

/// The drawing state, borrowed only by the GLArea's handlers.
struct Draw {
    renderer: Option<Renderer>,
    frame: Frame,
    shared: Option<Arc<Shared<term::Terminal>>>,
    term_bg: [u8; 4],
    /// Draw every row on the next frame.
    full: bool,
    blink_on: bool,
}

pub struct Surface {
    pub pane: PaneId,
    /// ghostty's surface id, the target of a notification's default action.
    pub id: u64,
    /// The leaf of the split tree (ghostty's SurfaceScrolledWindow).
    pub outer: adw::Bin,
    /// The surface (ghostty's terminal_page overlay), the drop target.
    pub overlay: gtk::Overlay,
    pub area: gtk::GLArea,
    veil: gtk::Revealer,
    drag_handle: gtk::CenterBox,
    drop_overlay: gtk::DrawingArea,
    resize_label: gtk::Label,
    resize_bin: adw::Bin,
    url_left: gtk::Label,
    url_right: gtk::Label,
    search: search::Overlay,
    found: RefCell<search::Found>,
    progress: gtk::ProgressBar,
    progress_timer: RefCell<Option<glib::SourceId>>,
    /// ghostty's SurfaceChildExited and its banner.
    child_exited: adw::Bin,
    banner: adw::Banner,
    notifications: Rc<DesktopNotifications>,
    /// When the running command started (OSC 133;C), ghostty's command_timer.
    command_timer: Cell<Option<Instant>>,
    /// ghostty's surface.notify-on-next-command-finish: its state tells
    /// whether the next command to finish notifies.
    notify_next_command: gio::SimpleAction,
    resize_hide: RefCell<Option<glib::SourceId>>,
    resize_idle: Cell<bool>,
    blink_timer: RefCell<Option<glib::SourceId>>,
    /// The PTY looks like a password prompt (ghostty's password_input).
    password: Cell<bool>,
    /// ghostty's bell-ringing: set by BEL while unfocused, cleared by focus,
    /// a key or a click.
    bell: Cell<bool>,
    /// ghostty's IMMulticontext for the surface and its state.
    im: gtk::IMMulticontext,
    im_composing: Cell<bool>,
    in_keyevent: Cell<InKeyEvent>,
    /// Text committed during the current key press while not composing.
    im_commit: RefCell<Option<String>>,
    /// The input method's preedit string, drawn at the cursor.
    preedit: RefCell<Option<String>>,
    /// ghostty's GhosttySurface: scrollable, so the scrollbar shows the
    /// scrollback.
    scroll_bin: SurfaceBin,
    /// The pointer over the GLArea, in logical pixels.
    pointer: Cell<(f64, f64)>,
    precision_scroll: Cell<bool>,
    /// Wheel movement not yet worth a row (ghostty's pending_scroll_y).
    pending_scroll: Cell<f64>,
    /// Horizontal touchpad movement toward a tab switch, and the timer that
    /// forgets it.
    pending_swipe: Cell<f64>,
    swipe_reset: RefCell<Option<glib::SourceId>>,
    gesture: RefCell<Gesture>,
    /// Mouse buttons held (bit per button number), for motion reports.
    buttons: Cell<u8>,
    /// A left click that only focused the surface; its release is ignored.
    suppress_left_release: Cell<bool>,
    /// Scrolls while a selection is dragged above or below the surface.
    autoscroll: RefCell<Option<(glib::SourceId, bool)>>,
    /// Modifiers held (from the key controller), for the pointer's shape.
    mods: Cell<mouse::Mods>,
    /// The CSS cursor name last given to the GLArea.
    cursor: Cell<&'static str>,
    /// The link under the pointer while ctrl is held.
    hover: RefCell<Option<links::Link>>,
    /// ghostty's context_menu, made on the first right click.
    menu: RefCell<Option<gtk::PopoverMenu>>,
    last_blink_reset: Cell<Option<Instant>>,
    /// The pane's font size (ghostty's font_size), which its fonts are
    /// loaded at, multiplied by `font_scale`.
    font_points: Cell<f32>,
    font_scale: Cell<f64>,
    cell_idle: Cell<bool>,
    handler: Handler,
    draw: RefCell<Draw>,
    created: Instant,
    pub focused: Cell<bool>,
    split: Cell<bool>,
    size: Cell<(u16, u16)>,
    /// The GLArea's size in device pixels.
    pixels: Cell<(i32, i32)>,
}

/// Zig's std.fs.path.resolve for an absolute `dir`: `path` joined to it
/// with `.` and `..` segments and repeated slashes folded away.
fn resolve_path(dir: &str, path: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for part in dir.split('/').chain(path.split('/')) {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            _ => parts.push(part),
        }
    }
    format!("/{}", parts.join("/"))
}

impl Drop for Surface {
    fn drop(&mut self) {
        if let Some(menu) = self.menu.take() {
            menu.unparent();
        }
        self.notifications.clear();
    }
}

/// ghostty's surface id: random and never 0.
fn surface_id() -> u64 {
    loop {
        let mut id = 0u64;
        let n = unsafe { libc::getrandom((&mut id as *mut u64).cast(), 8, 0) };
        assert_eq!(n, 8, "getrandom: {}", std::io::Error::last_os_error());
        if id != 0 {
            return id;
        }
    }
}

/// ghostty's internal_os.open on Linux.
fn xdg_open(url: &str) {
    if let Err(err) = std::process::Command::new("xdg-open").arg(url).stdout(std::process::Stdio::null()).spawn() {
        eprintln!("tapust: cannot run xdg-open: {err}");
    }
}

fn mods_of(state: gdk::ModifierType) -> mouse::Mods {
    mouse::Mods {
        shift: state.contains(gdk::ModifierType::SHIFT_MASK),
        alt: state.contains(gdk::ModifierType::ALT_MASK),
        ctrl: state.contains(gdk::ModifierType::CONTROL_MASK),
    }
}

fn button_of(number: u32) -> Option<mouse::Button> {
    match number {
        1 => Some(mouse::Button::Left),
        2 => Some(mouse::Button::Middle),
        3 => Some(mouse::Button::Right),
        _ => None,
    }
}

fn load_fonts(points: f32, scale: f64) -> Option<Fonts> {
    Fonts::new(theme::FONT_FAMILY, points, scale)
        .inspect_err(|err| eprintln!("tapust: cannot load fonts: {err}"))
        .ok()
}

fn padding(scale: f64) -> u32 {
    // ghostty converts the padding from points at 96 dpi and floors it.
    (theme::PADDING_POINTS * 96.0 / 72.0 * scale).floor() as u32
}

/// ghostty's calcDropDirection: the side nearest to (`x`, `y`), compared
/// left, right, top, bottom.
fn drop_direction(x: f64, y: f64, w: f64, h: f64) -> Direction {
    let (l, t) = (x / w, y / h);
    let (r, b) = (1.0 - l, 1.0 - t);
    let min = l.min(t).min(r).min(b);
    if min == l {
        Direction::Left
    } else if min == r {
        Direction::Right
    } else if min == t {
        Direction::Up
    } else {
        Direction::Down
    }
}

impl Surface {
    pub fn new(pane: PaneId, term_bg: [u8; 4], font_points: f32, handler: Handler) -> Rc<Surface> {
        let area = gtk::GLArea::builder().hexpand(true).vexpand(true).focusable(true).focus_on_click(true).build();
        area.set_allowed_apis(gdk::GLAPI::GL);
        area.set_required_version(3, 3);
        area.set_auto_render(false);
        area.set_cursor_from_name(Some("text"));
        let page = gtk::Box::builder().hexpand(true).vexpand(true).build();
        page.append(&area);
        let overlay = gtk::Overlay::builder().focusable(false).focus_on_click(false).child(&page).build();

        let progress = theme::styled(
            gtk::ProgressBar::builder().visible(false).halign(gtk::Align::Fill).valign(gtk::Align::Start).build(),
            &["osd"],
        );
        overlay.add_overlay(&progress);
        // ghostty's SurfaceChildExited: a revealed banner at the bottom,
        // shown once the child exited.
        let banner = adw::Banner::builder()
            .revealed(true)
            .button_label("Close")
            .halign(gtk::Align::Fill)
            .valign(gtk::Align::End)
            .build();
        let child_exited = theme::styled(
            adw::Bin::builder().visible(false).child(&adw::Bin::builder().child(&banner).build()).build(),
            &["child-exited"],
        );
        overlay.add_overlay(&child_exited);

        // ghostty's ResizeOverlay: a centered `background` label shown 750 ms.
        let resize_label = theme::styled(
            gtk::Label::builder()
                .can_focus(false)
                .can_target(false)
                .focusable(false)
                .focus_on_click(false)
                .selectable(false)
                .justify(gtk::Justification::Center)
                .halign(gtk::Align::Center)
                .valign(gtk::Align::Center)
                .build(),
            &["background", "resize-overlay"],
        );
        let inner = adw::Bin::builder().child(&resize_label).build();
        let resize_bin = adw::Bin::builder()
            .visible(false)
            .can_focus(false)
            .can_target(false)
            .focusable(false)
            .focus_on_click(false)
            .child(&inner)
            .build();
        overlay.add_overlay(&resize_bin);
        // ghostty's url_left and url_right: the hovered link's target.
        let url_label = |align| {
            theme::styled(
                gtk::Label::builder().visible(false).halign(align).valign(gtk::Align::End).build(),
                &["background", "url-overlay"],
            )
        };
        let (url_left, url_right) = (url_label(gtk::Align::Start), url_label(gtk::Align::End));
        let url_motion = gtk::EventControllerMotion::new();
        let right = url_right.clone();
        url_motion.connect_enter(move |_, _, _| right.set_visible(true));
        let right = url_right.clone();
        url_motion.connect_leave(move |_| right.set_visible(false));
        url_left.add_controller(url_motion);
        overlay.add_overlay(&url_left);
        overlay.add_overlay(&url_right);
        let search = search::Overlay::new();
        overlay.add_overlay(&search.bin);

        let veil = gtk::Revealer::builder().transition_duration(0).can_focus(false).can_target(false).focusable(false).build();
        veil.set_child(Some(&theme::styled(gtk::DrawingArea::builder().build(), &["unfocused-split"])));
        overlay.add_overlay(&veil);

        let drag_handle = theme::styled(
            gtk::CenterBox::builder().halign(gtk::Align::Fill).valign(gtk::Align::Start).visible(false).build(),
            &["drag-handle"],
        );
        drag_handle.set_center_widget(Some(&gtk::Image::from_icon_name("view-more-horizontal-symbolic")));
        drag_handle.set_cursor_from_name(Some("grab"));
        overlay.add_overlay(&drag_handle);

        let drop_overlay = theme::styled(
            gtk::DrawingArea::builder()
                .halign(gtk::Align::Fill)
                .valign(gtk::Align::Fill)
                .can_focus(false)
                .can_target(false)
                .focusable(false)
                .build(),
            &["drop-overlay"],
        );
        overlay.add_overlay(&drop_overlay);

        let scroll_bin = SurfaceBin::new(&overlay);
        // ghostty's surface action group, which the context menu targets.
        let notify_next_command =
            gio::SimpleAction::new_stateful("notify-on-next-command-finish", None, &false.to_variant());
        let actions = gio::SimpleActionGroup::new();
        actions.add_action(&notify_next_command);
        scroll_bin.insert_action_group("surface", Some(&actions));
        let scrolled = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .child(&scroll_bin)
            .build();
        let outer = adw::Bin::builder().child(&adw::Bin::builder().child(&scrolled).build()).build();

        let id = surface_id();
        let surface = Rc::new(Surface {
            pane,
            id,
            outer,
            overlay,
            area,
            veil,
            drag_handle,
            drop_overlay,
            resize_label,
            resize_bin,
            url_left,
            url_right,
            search,
            found: RefCell::new(search::Found::default()),
            progress,
            progress_timer: RefCell::new(None),
            child_exited,
            banner,
            notifications: DesktopNotifications::new(id),
            command_timer: Cell::new(None),
            notify_next_command,
            resize_hide: RefCell::new(None),
            resize_idle: Cell::new(false),
            blink_timer: RefCell::new(None),
            password: Cell::new(false),
            bell: Cell::new(false),
            im: gtk::IMMulticontext::new(),
            im_composing: Cell::new(false),
            in_keyevent: Cell::new(InKeyEvent::No),
            im_commit: RefCell::new(None),
            preedit: RefCell::new(None),
            scroll_bin,
            pointer: Cell::new((0.0, 0.0)),
            precision_scroll: Cell::new(false),
            pending_scroll: Cell::new(0.0),
            pending_swipe: Cell::new(0.0),
            swipe_reset: RefCell::new(None),
            gesture: RefCell::new(Gesture::default()),
            buttons: Cell::new(0),
            suppress_left_release: Cell::new(false),
            autoscroll: RefCell::new(None),
            mods: Cell::new(mouse::Mods::default()),
            cursor: Cell::new("text"),
            hover: RefCell::new(None),
            menu: RefCell::new(None),
            last_blink_reset: Cell::new(None),
            font_points: Cell::new(font_points),
            font_scale: Cell::new(0.0),
            cell_idle: Cell::new(false),
            handler: handler.clone(),
            draw: RefCell::new(Draw {
                renderer: None,
                frame: Frame::new(term_bg),
                shared: None,
                term_bg,
                full: true,
                blink_on: true,
            }),
            created: Instant::now(),
            focused: Cell::new(false),
            split: Cell::new(false),
            size: Cell::new((0, 0)),
            pixels: Cell::new((0, 0)),
        });
        surface.connect(handler);
        surface
    }

    fn connect(self: &Rc<Self>, handler: Handler) {
        self.connect_search(handler.clone());
        let weak = Rc::downgrade(self);
        self.area.connect_realize(move |area| {
            let Some(s) = weak.upgrade() else { return };
            area.make_current();
            if area.error().is_some() {
                eprintln!("tapust: no OpenGL context: {:?}", area.error());
                return;
            }
            let scale = area.native().and_then(|n| n.surface()).map_or(1.0, |s| s.scale());
            let Some(fonts) = load_fonts(s.font_points.get(), scale) else { return };
            s.draw.borrow_mut().renderer = Some(Renderer::new(crate::gl::context(), fonts));
            s.im.set_client_widget(Some(area));
            s.font_scale.set(scale);
            s.schedule_cell_size();
        });
        let weak = Rc::downgrade(self);
        self.area.connect_unrealize(move |area| {
            if let Some(s) = weak.upgrade() {
                area.make_current();
                s.draw.borrow_mut().renderer = None;
                s.im.set_client_widget(None::<&gtk::Widget>);
            }
        });
        let weak = Rc::downgrade(self);
        self.area.connect_resize(move |_, width, height| {
            if let Some(s) = weak.upgrade() {
                s.resized(width, height);
            }
        });
        let weak = Rc::downgrade(self);
        self.area.connect_render(move |_, _| {
            if let Some(s) = weak.upgrade() {
                s.render();
            }
            glib::Propagation::Stop
        });

        let h = handler.clone();
        self.area.connect_map(move |_| {
            h(Event::Mapped);
        });
        let (pane, h) = (self.pane, handler.clone());
        self.banner.connect_button_clicked(move |_| {
            h(Event::ChildExitedClose { pane });
        });

        // ghostty's GestureClick for every button.
        let click = gtk::GestureClick::builder().button(0).build();
        let weak = Rc::downgrade(self);
        click.connect_pressed(move |g, _, x, y| {
            if let Some(s) = weak.upgrade() {
                s.button_pressed(g.current_button(), x, y, g.current_event_state());
            }
        });
        let weak = Rc::downgrade(self);
        click.connect_released(move |g, _, x, y| {
            if let Some(s) = weak.upgrade() {
                s.button_released(g.current_button(), x, y, g.current_event_state());
            }
        });
        self.area.add_controller(click);

        let focus = gtk::EventControllerFocus::new();
        let (weak, h) = (Rc::downgrade(self), handler.clone());
        focus.connect_enter(move |_| {
            if let Some(s) = weak.upgrade() {
                s.found.borrow_mut().focused = false;
                s.im.focus_in();
                s.set_focused(true);
                h(Event::Focused { pane: s.pane });
            }
        });
        let weak = Rc::downgrade(self);
        focus.connect_leave(move |_| {
            if let Some(s) = weak.upgrade() {
                s.im.focus_out();
                s.set_focused(false);
            }
        });
        self.area.add_controller(focus);

        let keys = gtk::EventControllerKey::new();
        let (weak, h) = (Rc::downgrade(self), handler.clone());
        keys.connect_key_pressed(move |keys, keyval, _, state| {
            let Some(s) = weak.upgrade() else { return glib::Propagation::Proceed };
            let Some(event) = keys.current_event() else { return glib::Propagation::Proceed };
            // ghostty's keyEvent: the input method sees the key first.
            s.update_ime_position();
            s.in_keyevent.set(if s.im_composing.get() { InKeyEvent::Composing } else { InKeyEvent::NotComposing });
            let handled = s.im.filter_keypress(&event);
            let was = s.in_keyevent.replace(InKeyEvent::No);
            let text = s.im_commit.take();
            if handled && (s.im_composing.get() || was == InKeyEvent::Composing || text.is_none()) {
                return glib::Propagation::Stop;
            }
            if !h(Event::Key { pane: s.pane, keyval, state, text }) {
                return glib::Propagation::Proceed;
            }
            // A key the terminal used ends a composition.
            if s.im_composing.get() {
                s.im.reset();
                s.set_preedit(None);
            }
            glib::Propagation::Stop
        });
        let weak = Rc::downgrade(self);
        keys.connect_modifiers(move |_, state| {
            if let Some(s) = weak.upgrade() {
                s.mods.set(mods_of(state));
                s.update_hover();
                s.update_cursor();
            }
            glib::Propagation::Proceed
        });
        let weak = Rc::downgrade(self);
        keys.connect_key_released(move |keys, _, _, _| {
            if let (Some(s), Some(event)) = (weak.upgrade(), keys.current_event()) {
                s.im.filter_keypress(&event);
            }
        });
        self.area.add_controller(keys);

        let weak = Rc::downgrade(self);
        self.scroll_bin.connect_scroll(move |value| {
            if let Some(s) = weak.upgrade() {
                s.scroll_to_row(value);
            }
        });
        let motion = gtk::EventControllerMotion::new();
        let weak = Rc::downgrade(self);
        motion.connect_motion(move |m, x, y| {
            if let Some(s) = weak.upgrade() {
                s.pointer.set((x, y));
                s.pointer_moved(m.current_event_state());
            }
        });
        self.area.add_controller(motion);
        let wheel = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::VERTICAL);
        let weak = Rc::downgrade(self);
        wheel.connect_scroll_begin(move |_| {
            if let Some(s) = weak.upgrade() {
                s.precision_scroll.set(true);
            }
        });
        let weak = Rc::downgrade(self);
        wheel.connect_scroll_end(move |_| {
            if let Some(s) = weak.upgrade() {
                s.precision_scroll.set(false);
            }
        });
        let weak = Rc::downgrade(self);
        wheel.connect_scroll(move |wheel, _, dy| {
            let Some(s) = weak.upgrade() else { return glib::Propagation::Proceed };
            s.wheel(dy, wheel.current_event_state());
            glib::Propagation::Stop
        });
        self.area.add_controller(wheel);
        let tilt = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::HORIZONTAL);
        let weak = Rc::downgrade(self);
        tilt.connect_scroll(move |tilt, dx, _| {
            let Some(s) = weak.upgrade() else { return glib::Propagation::Proceed };
            s.horizontal_scroll(dx, tilt.unit(), tilt.current_event_state());
            glib::Propagation::Stop
        });
        self.area.add_controller(tilt);

        let weak = Rc::downgrade(self);
        self.im.connect_preedit_start(move |_| {
            if let Some(s) = weak.upgrade() {
                s.im_composing.set(true);
                s.im_commit.take();
                s.update_ime_position();
            }
        });
        let weak = Rc::downgrade(self);
        self.im.connect_preedit_changed(move |im| {
            if let Some(s) = weak.upgrade() {
                s.im_composing.set(true);
                let (text, _, _) = im.preedit_string();
                s.set_preedit(Some(text.to_string()));
                s.update_ime_position();
            }
        });
        let weak = Rc::downgrade(self);
        self.im.connect_preedit_end(move |_| {
            if let Some(s) = weak.upgrade() {
                s.im_composing.set(false);
                s.set_preedit(None);
            }
        });
        let (weak, h) = (Rc::downgrade(self), handler.clone());
        self.im.connect_commit(move |_, text| {
            let Some(s) = weak.upgrade() else { return };
            // Text committed by a key press that was not composing goes out
            // with that key (ghostty's im_buf).
            if s.in_keyevent.get() == InKeyEvent::NotComposing {
                *s.im_commit.borrow_mut() = Some(text.to_owned());
                return;
            }
            s.im_composing.set(false);
            s.set_preedit(None);
            h(Event::Text { pane: s.pane, text: text.to_owned() });
        });

        // Drag a pane by its drag handle (ghostty's surface_drag_source).
        let source = gtk::DragSource::builder().actions(gdk::DragAction::MOVE).build();
        let pane = self.pane;
        source.connect_prepare(move |_, _, _| Some(gdk::ContentProvider::for_value(&u64::from(pane).to_value())));
        let weak = Rc::downgrade(self);
        source.connect_drag_begin(move |src, _| {
            let Some(s) = weak.upgrade() else { return };
            // A snapshot of the surface at 0.2, held at its middle.
            let widget = s.overlay.upcast_ref::<gtk::Widget>();
            let paintable = gtk::WidgetPaintable::new(Some(widget));
            let (w, h) = (widget.width() as f32, widget.height() as f32);
            let scale = theme::DRAG_PREVIEW_SCALE as f32;
            let snap = gtk::Snapshot::new();
            snap.scale(scale, scale);
            paintable.snapshot(&snap, f64::from(w), f64::from(h));
            let hot = ((w / 2.0 * scale) as i32, (h / 2.0 * scale) as i32);
            src.set_icon(snap.to_paintable(None).as_ref(), hot.0, hot.1);
        });
        self.drag_handle.add_controller(source);

        // Panes accept other panes (ghostty's surface_drop_target).
        let target = gtk::DropTarget::new(glib::Type::U64, gdk::DragAction::MOVE);
        target.set_preload(true);
        let weak = Rc::downgrade(self);
        target.connect_value_notify(move |t| {
            let (Some(s), Some(v)) = (weak.upgrade(), t.value()) else { return };
            if v.get::<u64>().ok() == Some(u64::from(s.pane)) {
                t.reject();
            }
        });
        let weak = Rc::downgrade(self);
        target.connect_motion(move |_, x, y| {
            if let Some(s) = weak.upgrade() {
                s.set_drop(Some(s.direction(x, y)));
            }
            gdk::DragAction::MOVE
        });
        let weak = Rc::downgrade(self);
        target.connect_leave(move |_| {
            if let Some(s) = weak.upgrade() {
                s.set_drop(None);
            }
        });
        let (weak, h) = (Rc::downgrade(self), handler);
        target.connect_drop(move |_, value, x, y| {
            let Some(s) = weak.upgrade() else { return false };
            s.set_drop(None);
            let Ok(source) = value.get::<u64>() else { return false };
            let dir = s.direction(x, y);
            h(Event::Dropped { source: source as PaneId, target: s.pane, dir })
        });
        self.overlay.add_controller(target);
    }

    /// Wires the search overlay as ghostty's surface does: up and Enter go to
    /// older matches (ghostty's next-match), down and Shift+Enter to newer
    /// ones, Escape and the close button end the search.
    fn connect_search(self: &Rc<Self>, handler: Handler) {
        /// A signal handler that runs `f` on the surface while it lives.
        fn on<W: 'static>(weak: &Weak<Surface>, f: fn(&Surface)) -> impl Fn(&W) + 'static {
            let weak = weak.clone();
            move |_| {
                if let Some(s) = weak.upgrade() {
                    f(&s);
                }
            }
        }
        let weak = Rc::downgrade(self);
        let entry = &self.search.entry;
        entry.connect_search_changed(on(&weak, |s| {
            if s.search.is_open() {
                s.run_search(&s.search.entry.text());
            }
        }));
        entry.connect_activate(on(&weak, |s| s.navigate_search(true)));
        entry.connect_next_match(on(&weak, |s| s.navigate_search(true)));
        entry.connect_previous_match(on(&weak, |s| s.navigate_search(false)));
        entry.connect_stop_search(on(&weak, Surface::end_search));
        self.search.up.connect_clicked(on(&weak, |s| s.navigate_search(true)));
        self.search.down.connect_clicked(on(&weak, |s| s.navigate_search(false)));
        self.search.close.connect_clicked(on(&weak, Surface::end_search));
        // Ahead of the entry: Shift+Enter, which does not activate it, and the
        // binding that opens the search, which selects the query again.
        let keys = gtk::EventControllerKey::builder().propagation_phase(gtk::PropagationPhase::Capture).build();
        let w = weak.clone();
        keys.connect_key_pressed(move |_, keyval, _, state| {
            let Some(s) = w.upgrade() else { return glib::Propagation::Proceed };
            let enter = matches!(keyval, gdk::Key::Return | gdk::Key::KP_Enter | gdk::Key::ISO_Enter);
            if enter && state.contains(gdk::ModifierType::SHIFT_MASK) {
                s.navigate_search(false);
            } else if bindings::binding(keyval, state) == Some(Binding::StartSearch) {
                s.start_search();
            } else {
                return glib::Propagation::Proceed;
            }
            glib::Propagation::Stop
        });
        entry.add_controller(keys);
        let focus = gtk::EventControllerFocus::new();
        focus.connect_enter(move |_| {
            if let Some(s) = weak.upgrade() {
                s.found.borrow_mut().focused = true;
                handler(Event::Focused { pane: s.pane });
            }
        });
        entry.add_controller(focus);
    }

    fn direction(&self, x: f64, y: f64) -> Direction {
        let w = self.overlay.width() as f64;
        let h = self.overlay.height() as f64;
        drop_direction(x, y, w, h)
    }

    /// ghostty's setDropOverlayDirection.
    fn set_drop(&self, dir: Option<Direction>) {
        for class in ["drop-top", "drop-left", "drop-right", "drop-bottom"] {
            self.drop_overlay.remove_css_class(class);
        }
        if let Some(dir) = dir {
            self.drop_overlay.add_css_class(match dir {
                Direction::Up => "drop-top",
                Direction::Left => "drop-left",
                Direction::Right => "drop-right",
                Direction::Down => "drop-bottom",
            });
        }
    }

    /// ghostty's Surface.syncSplitMinSize: the leaf never gets smaller than
    /// one cell (in logical pixels, rounded up) and the padding. Runs on idle
    /// since the scale is noticed while GTK draws.
    fn schedule_cell_size(self: &Rc<Self>) {
        if self.cell_idle.replace(true) {
            return;
        }
        let weak = Rc::downgrade(self);
        glib::idle_add_local_once(move || {
            let Some(s) = weak.upgrade() else { return };
            s.cell_idle.set(false);
            let Some((cw, ch)) = s.cell_size() else { return };
            let scale = s.font_scale.get();
            let pad = 2 * padding(1.0) as i32;
            let logical = |px: u32| (f64::from(px) / scale).ceil() as i32;
            s.outer.set_size_request(logical(cw) + pad, logical(ch) + pad);
            (s.handler)(Event::CellSize { cell: (cw, ch) });
        });
    }

    /// The grid that fits `width` x `height` device pixels of the GLArea
    /// with the cell size of its fonts, told to the application. GTK asks
    /// for this with the GL context current.
    fn resized(self: &Rc<Self>, width: i32, height: i32) {
        let scale = self.area.native().and_then(|n| n.surface()).map_or(1.0, |s| s.scale());
        let (cols, rows, cell) = {
            let mut d = self.draw.borrow_mut();
            let Some(renderer) = d.renderer.as_mut() else { return };
            // ghostty's content scale callback: a new scale needs fonts
            // rasterized for it.
            if scale != self.font_scale.get()
                && let Some(fonts) = load_fonts(self.font_points.get(), scale)
            {
                renderer.set_fonts(fonts);
                self.font_scale.set(scale);
                self.schedule_cell_size();
            }
            renderer.resize(width as u32, height as u32);
            let (cw, ch) = renderer.cell_size();
            let pad = padding(scale);
            let fit = |size: i32, cell: u32| ((size.max(0) as u32).saturating_sub(2 * pad) / cell.max(1)).max(1) as u16;
            let (cols, rows) = (fit(width, cw), fit(height, ch));
            let bg = d.term_bg;
            d.frame.grids = vec![Grid::new(self.pane, (0, 0, width as u32, height as u32), (pad as i32, pad as i32), (cols, rows), bg)];
            d.full = true;
            (cols, rows, (cw, ch))
        };
        self.size.set((cols, rows));
        // ghostty shows the overlay whenever the pixel size changes,
        // except for the first size.
        let old = self.pixels.replace((width, height));
        if old != (width, height) && old != (0, 0) && self.created.elapsed() >= theme::RESIZE_OVERLAY_DELAY {
            self.schedule_resize_overlay();
        }
        (self.handler)(Event::Resized { pane: self.pane, cols, rows, cell });
        self.refresh_search();
    }

    pub fn font_points(&self) -> f32 {
        self.font_points.get()
    }

    /// ghostty's setFontSize: the fonts at `points`, and the grid that fits
    /// the same pixels with their cell size; the split keeps its size.
    pub fn set_font_points(self: &Rc<Self>, points: f32) {
        if points == self.font_points.get() {
            return;
        }
        let Some(fonts) = load_fonts(points, self.font_scale.get()) else { return };
        {
            let mut d = self.draw.borrow_mut();
            let Some(renderer) = d.renderer.as_mut() else { return };
            self.area.make_current();
            renderer.set_fonts(fonts);
        }
        self.font_points.set(points);
        self.schedule_cell_size();
        let (width, height) = self.pixels.get();
        self.resized(width, height);
        self.area.queue_render();
        self.sync_scrollbar();
    }

    /// ghostty's ResizeOverlay.schedule: GtkGLArea emits `resize` while the
    /// window is being drawn, so the label changes on the next idle tick with
    /// the latest grid size.
    fn schedule_resize_overlay(self: &Rc<Self>) {
        if self.resize_idle.replace(true) {
            return;
        }
        let weak = Rc::downgrade(self);
        glib::idle_add_local_once(move || {
            let Some(s) = weak.upgrade() else { return };
            s.resize_idle.set(false);
            if let Some(old) = s.resize_hide.take() {
                old.remove();
            }
            let (cols, rows) = s.size.get();
            s.resize_label.set_label(&format!("{cols} x {rows}"));
            s.resize_bin.set_visible(true);
            let weak = Rc::downgrade(&s);
            let id = glib::timeout_add_local_once(theme::RESIZE_OVERLAY, move || {
                if let Some(s) = weak.upgrade() {
                    s.resize_hide.take();
                    s.resize_bin.set_visible(false);
                }
            });
            *s.resize_hide.borrow_mut() = Some(id);
        });
    }

    fn set_focused(self: &Rc<Self>, focused: bool) {
        self.focused.set(focused);
        // ghostty's focusGained: DEC 1004 reports focus changes.
        if let Some(shared) = self.draw.borrow().shared.clone() {
            let mut term = shared.term.lock();
            term.set_focused(focused);
            let report = term.modes().focus_events;
            drop(term);
            if report {
                shared.write(if focused { b"\x1b[I" } else { b"\x1b[O" });
            }
        }
        self.sync_veil();
        if focused {
            // ghostty's updateFocus: focus clears the bell and the notifications.
            self.set_bell_ringing(false);
            self.notifications.clear();
            // Focus shows the cursor and starts blinking unless it already does.
            if self.blink_timer.borrow().is_none() {
                self.start_blink();
            }
        } else if let Some(timer) = self.blink_timer.take() {
            timer.remove();
        }
        self.area.queue_render();
    }

    /// The window is split (ghostty's `is-split`): the veil may show and the
    /// drag handle is there.
    pub fn set_split(&self, split: bool) {
        self.split.set(split);
        self.drag_handle.set_visible(split);
        self.sync_veil();
    }

    /// ghostty's unfocused-split veil: over an unfocused split while no
    /// search is open.
    fn sync_veil(&self) {
        self.veil.set_reveal_child(self.split.get() && !self.focused.get() && !self.search.is_open());
    }

    pub fn attach(&self, shared: Arc<Shared<term::Terminal>>) {
        let mut d = self.draw.borrow_mut();
        d.shared = Some(shared);
        d.full = true;
        drop(d);
        self.area.queue_render();
    }

    /// The cursor blinks while the surface has the focus.
    /// ghostty's renderer cursor timer: the blink phase flips every 600ms
    /// while the surface has focus.
    fn start_blink(self: &Rc<Self>) {
        if let Some(old) = self.blink_timer.take() {
            old.remove();
        }
        self.draw.borrow_mut().blink_on = true;
        self.area.queue_render();
        let weak = Rc::downgrade(self);
        let id = glib::timeout_add_local(theme::BLINK, move || {
            let Some(s) = weak.upgrade() else { return glib::ControlFlow::Break };
            let mut d = s.draw.borrow_mut();
            d.blink_on = !d.blink_on;
            drop(d);
            s.area.queue_render();
            glib::ControlFlow::Continue
        });
        *self.blink_timer.borrow_mut() = Some(id);
    }

    /// ghostty's reset_cursor_blink on PTY output, at most every 500ms: the
    /// cursor shows again and the blink timer starts over.
    pub fn output(self: &Rc<Self>) {
        let now = Instant::now();
        if self.last_blink_reset.get().is_some_and(|last| now - last <= theme::BLINK_RESET) {
            return;
        }
        self.last_blink_reset.set(Some(now));
        if self.blink_timer.borrow().is_some() {
            self.start_blink();
        } else {
            self.draw.borrow_mut().blink_on = true;
        }
    }

    /// Sets the terminal's selection for the user; SelectionChanged tells
    /// the application when one appeared or went away.
    fn set_selection(&self, selection: Option<Selection>) {
        let Some(shared) = self.draw.borrow().shared.clone() else { return };
        let mut term = shared.term.lock();
        let old = term.selection();
        if old == selection {
            return;
        }
        term.set_selection(selection);
        drop(term);
        self.area.queue_render();
        if old.is_some() != selection.is_some() {
            (self.handler)(Event::SelectionChanged);
        }
    }

    pub fn clear_selection(&self) {
        self.set_selection(None);
    }

    pub fn has_selection(&self) -> bool {
        self.draw.borrow().shared.as_ref().is_some_and(|s| s.term.lock().selection().is_some())
    }

    /// The selected text as ghostty copies it.
    pub fn selection_text(&self) -> Option<String> {
        let shared = self.draw.borrow().shared.clone()?;
        let term = shared.term.lock();
        let text = selection::text(&term, &term.selection()?);
        (!text.is_empty()).then_some(text)
    }

    /// ghostty's select_all, copied to the primary selection like a
    /// selection made with the mouse.
    pub fn select_all(&self) {
        let Some(shared) = self.draw.borrow().shared.clone() else { return };
        let all = selection::all(&shared.term.lock());
        if all.is_some() {
            self.set_selection(all);
            self.copy_primary();
        }
    }

    /// ghostty's copy-on-select = primary.
    fn copy_primary(&self) {
        if let Some(text) = self.selection_text() {
            self.area.primary_clipboard().set_text(&text);
        }
    }

    /// The cell under the logical point (`x`, `y`), clamped to the grid,
    /// with x in device pixels from the grid's left edge.
    fn drag_point(&self, term: &term::Terminal, x: f64, y: f64) -> Option<DragPoint> {
        let (cw, ch) = self.cell_size()?;
        let scale = self.font_scale.get();
        let pad = f64::from(padding(scale));
        let (px, py) = (x * scale - pad, y * scale - pad);
        let col = ((px / f64::from(cw)).floor().max(0.0) as u16).min(term.cols() - 1);
        let row = ((py / f64::from(ch)).floor().max(0.0) as u16).min(term.rows() - 1);
        Some(DragPoint { cell: Point { line: term.view_top() + u64::from(row), col }, x: px })
    }

    /// Reports a mouse event to the program if it asked for them; returns
    /// whether it does.
    fn report(&self, button: Option<mouse::Button>, action: mouse::Action, mods: mouse::Mods) -> bool {
        let Some(shared) = self.draw.borrow().shared.clone() else { return false };
        let term = shared.term.lock();
        let modes = term.modes();
        if modes.mouse_mode == term::MouseMode::None {
            return false;
        }
        let (cell, pixels) = self.pointer_cell(&term);
        drop(term);
        let mut bytes = Vec::new();
        mouse::encode(&mut bytes, modes.mouse_mode, modes.mouse_encoding, mouse::Event { button, action, mods, cell, pixels });
        shared.write(&bytes);
        true
    }

    fn reporting(&self) -> bool {
        self.draw.borrow().shared.as_ref().is_some_and(|s| s.term.lock().modes().mouse_mode != term::MouseMode::None)
    }

    /// ghostty's mouseButtonCallback for a press.
    fn button_pressed(self: &Rc<Self>, number: u32, x: f64, y: f64, state: gdk::ModifierType) {
        self.set_bell_ringing(false);
        self.pointer.set((x, y));
        let focused = self.focused.get();
        self.area.grab_focus();
        // A left click on an unfocused surface only focuses it.
        if !focused && number == 1 {
            self.suppress_left_release.set(true);
            return;
        }
        let Some(button) = button_of(number) else { return };
        self.buttons.set(self.buttons.get() | 1 << number);
        let mods = mods_of(state);
        let Some(shared) = self.draw.borrow().shared.clone() else { return };
        // shift+click extends the last left gesture after the click interval.
        if button == mouse::Button::Left && mods.shift {
            let g = self.gesture.borrow();
            let extend = g.count > 0
                && self.has_selection()
                && g.last_press.is_some_and(|t| t.elapsed() > theme::CLICK_INTERVAL);
            drop(g);
            if extend {
                self.gesture.borrow_mut().left_down = true;
                self.drag_to(mods);
                return;
            }
        }
        if !mods.shift && self.reporting() {
            self.set_selection(None);
            *self.gesture.borrow_mut() = Gesture::default();
            self.report(Some(button), mouse::Action::Press, mods);
            return;
        }
        match button {
            mouse::Button::Left => {
                let term = shared.term.lock();
                let Some(point) = self.drag_point(&term, x, y) else { return };
                let scale = self.font_scale.get();
                let px = (x * scale, y * scale);
                let cw = f64::from(self.cell_size().map_or(1, |c| c.0));
                let mut g = self.gesture.borrow_mut();
                let repeat = g.last_press.is_some_and(|t| t.elapsed() <= theme::CLICK_INTERVAL)
                    && (px.0 - g.first_px.0).hypot(px.1 - g.first_px.1) <= cw;
                g.count = if repeat { (g.count + 1).min(3) } else { 1 };
                if !repeat {
                    g.first_px = px;
                }
                g.last_press = Some(Instant::now());
                g.click = Some(point);
                g.left_down = true;
                g.dragged = false;
                g.output = g.count == 3 && mods.ctrl;
                let selection = match g.count {
                    1 => None,
                    // A double click on a URL selects all of it.
                    2 => links::url_at(&term, point.cell).map(|(sel, _)| sel).or_else(|| selection::word(&term, point.cell)),
                    _ if g.output => selection::output(&term, point.cell),
                    _ => selection::line(&term, point.cell, true),
                };
                drop(g);
                drop(term);
                self.set_selection(selection);
            }
            mouse::Button::Middle => {
                (self.handler)(Event::PastePrimary);
            }
            mouse::Button::Right => {
                // ghostty selects the word under a right click outside the
                // selection and copies it to the primary selection.
                let term = shared.term.lock();
                let Some(point) = self.drag_point(&term, x, y) else { return };
                let inside = term.selection().is_some_and(|s| s.contains(point.cell, term.cols()));
                let hovered = self.hover.borrow().as_ref().map(|link| link.cells);
                if !inside && let Some(word) = hovered.or_else(|| selection::word(&term, point.cell)) {
                    drop(term);
                    self.set_selection(Some(word));
                    self.copy_primary();
                }
                (self.handler)(Event::ContextMenu { pane: self.pane, x, y });
            }
            mouse::Button::Four | mouse::Button::Five | mouse::Button::Six | mouse::Button::Seven => {}
        }
    }

    /// ghostty's mouseButtonCallback for a release.
    fn button_released(self: &Rc<Self>, number: u32, x: f64, y: f64, state: gdk::ModifierType) {
        self.pointer.set((x, y));
        self.buttons.set(self.buttons.get() & !(1 << number));
        if number == 1 && self.suppress_left_release.replace(false) {
            return;
        }
        let Some(button) = button_of(number) else { return };
        let mods = mods_of(state);
        if button == mouse::Button::Left {
            self.gesture.borrow_mut().left_down = false;
            self.stop_autoscroll();
        }
        if !mods.shift && self.report(Some(button), mouse::Action::Release, mods) {
            return;
        }
        if button == mouse::Button::Left && self.has_selection() {
            self.copy_primary();
        }
        // ghostty opens the hovered link on a left release that did not drag,
        // or else moves the cursor to a click at the prompt.
        if button == mouse::Button::Left && !self.gesture.borrow().dragged {
            let target = self.hover.borrow().as_ref().map(|link| link.target.clone());
            match target {
                Some(target) => self.open_link(&target),
                None => self.prompt_click(x, y),
            }
        }
    }

    /// ghostty's maybePromptClick (cursor-click-to-move): with the shell's
    /// click option, the cursor at the prompt, a counted click that did not
    /// drag or select, at or below the current prompt.
    fn prompt_click(&self, x: f64, y: f64) {
        if self.gesture.borrow().count == 0 || self.has_selection() {
            return;
        }
        let Some(shared) = self.draw.borrow().shared.clone() else { return };
        let term = shared.term.lock();
        let Some(mode) = term.prompt_click() else { return };
        if !term.cursor_at_prompt() {
            return;
        }
        let Some(click) = self.drag_point(&term, x, y).map(|p| p.cell) else { return };
        let screen_top = term.end_line() - u64::from(term.rows());
        let cursor = Point { line: screen_top + u64::from(term.cursor().y), col: term.cursor().x };
        let Some(prompt) = (term.first_line()..=cursor.line).rev().find(|&l| term.line_prompt(l) == term::PromptMark::Prompt) else {
            return;
        };
        if click < (Point { line: prompt, col: 0 }) {
            return;
        }
        let mut bytes = Vec::new();
        match mode {
            term::PromptClick::Absolute | term::PromptClick::Relative => {
                let row = match mode {
                    term::PromptClick::Absolute => click.line - term.view_top() + 1,
                    _ => click.line - prompt + 1,
                };
                bytes.extend_from_slice(format!("\x1b[<0;{};{row}M", click.col + 1).as_bytes());
            }
            term::PromptClick::Cl => {
                let app = term.modes().app_cursor;
                let (left, right) = selection::prompt_click_move(&term, cursor, click);
                let (l, r): (&[u8], &[u8]) = if app { (b"\x1bOD", b"\x1bOC") } else { (b"\x1b[D", b"\x1b[C") };
                for _ in 0..left {
                    bytes.extend_from_slice(l);
                }
                for _ in 0..right {
                    bytes.extend_from_slice(r);
                }
            }
        }
        drop(term);
        if !bytes.is_empty() {
            shared.write(&bytes);
        }
    }

    /// ghostty's cursorPosCallback: motion reports, or dragging the
    /// selection with the left button.
    fn pointer_moved(self: &Rc<Self>, state: gdk::ModifierType) {
        let mods = mods_of(state);
        self.mods.set(mods);
        self.update_hover();
        let held = self.buttons.get();
        if self.reporting() && !(mods.shift && held != 0) {
            let button = [1, 2, 3].into_iter().find(|n| held & 1 << n != 0).and_then(button_of);
            self.report(button, mouse::Action::Motion, mods);
            return;
        }
        if self.gesture.borrow().left_down && !self.suppress_left_release.get() {
            self.drag_to(mods);
        }
    }

    /// Selects from the last left click to the pointer by the gesture's unit,
    /// scrolling while the pointer is above or below the surface.
    fn drag_to(self: &Rc<Self>, mods: mouse::Mods) {
        let (x, y) = self.pointer.get();
        let height = f64::from(self.area.height());
        if y <= 1.0 {
            self.start_autoscroll(true, mods);
        } else if y > height - 1.0 {
            self.start_autoscroll(false, mods);
        } else {
            self.stop_autoscroll();
        }
        let Some(shared) = self.draw.borrow().shared.clone() else { return };
        let term = shared.term.lock();
        let Some(drag) = self.drag_point(&term, x, y) else { return };
        let g = self.gesture.borrow();
        let Some(click) = g.click else { return };
        let selection = match g.count {
            1 => {
                let cw = self.cell_size().map_or(1, |c| c.0);
                selection::drag_cells(&term, click, drag, mods.ctrl && mods.alt, cw)
            }
            2 => selection::drag_words(&term, click.cell, drag.cell),
            _ if g.output => selection::drag_output(&term, click.cell, drag.cell),
            _ => selection::drag_lines(&term, click.cell, drag.cell),
        };
        drop(g);
        drop(term);
        if selection.is_some() {
            self.gesture.borrow_mut().dragged = true;
        }
        self.set_selection(selection);
    }

    /// ghostty's selection scroll: one row every 15ms while dragging past
    /// the top or the bottom.
    fn start_autoscroll(self: &Rc<Self>, up: bool, mods: mouse::Mods) {
        if self.autoscroll.borrow().as_ref().is_some_and(|&(_, u)| u == up) {
            return;
        }
        self.stop_autoscroll();
        let weak = Rc::downgrade(self);
        let id = glib::timeout_add_local(theme::SELECTION_SCROLL, move || {
            let Some(s) = weak.upgrade() else { return glib::ControlFlow::Break };
            if let Some(shared) = s.draw.borrow().shared.clone() {
                shared.term.lock().scroll_view(if up { 1 } else { -1 });
            }
            s.draw.borrow_mut().full = true;
            s.sync_scrollbar();
            s.drag_to(mods);
            glib::ControlFlow::Continue
        });
        *self.autoscroll.borrow_mut() = Some((id, up));
    }

    fn stop_autoscroll(&self) {
        if let Some((id, _)) = self.autoscroll.take() {
            id.remove();
        }
    }

    /// Opens ghostty's context menu at (`x`, `y`) on the GLArea; closing it
    /// gives the surface its focus back.
    pub fn popup_menu(&self, x: f64, y: f64) {
        let menu = self.menu.borrow_mut().get_or_insert_with(|| {
            let menu = gtk::PopoverMenu::from_model_full(&crate::gui::context_menu(), gtk::PopoverMenuFlags::NESTED);
            menu.set_halign(gtk::Align::Start);
            menu.set_has_arrow(false);
            menu.set_parent(&self.area);
            let area = self.area.clone();
            menu.connect_closed(move |_| {
                area.grab_focus();
            });
            menu
        }).clone();
        menu.set_pointing_to(Some(&gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
        menu.popup();
    }

    /// ghostty's link hover: with exactly ctrl held (shift too while a
    /// program reports the mouse), the link under the pointer is underlined
    /// and its target (a hyperlink's URI) shown at the bottom left.
    fn update_hover(&self) {
        let m = self.mods.get();
        let reporting = self.reporting();
        let wanted = m.ctrl && !m.alt && (!m.shift || reporting);
        let link = wanted
            .then(|| {
                let shared = self.draw.borrow().shared.clone()?;
                let term = shared.term.lock();
                let (x, y) = self.pointer.get();
                let point = self.drag_point(&term, x, y)?;
                links::link_at(&term, point.cell)
            })
            .flatten();
        if *self.hover.borrow() == link {
            return;
        }
        match &link {
            Some(link) => {
                self.url_left.set_label(&link.target);
                self.url_right.set_label(&link.target);
                self.url_left.set_visible(true);
            }
            None => {
                self.url_left.set_visible(false);
                self.url_right.set_visible(false);
            }
        }
        *self.hover.borrow_mut() = link;
        self.draw.borrow_mut().full = true;
        self.area.queue_render();
        self.update_cursor();
    }

    /// ghostty's processLinks and the GTK openUrl: a relative path that
    /// exists under the terminal's directory opens resolved; paths and
    /// file:// URLs go to xdg-open, other URIs to the OpenURI portal.
    fn open_link(&self, text: &str) {
        let pwd = self.draw.borrow().shared.as_ref().and_then(|s| s.term.lock().reported_cwd().map(str::to_owned));
        let resolved = (!text.starts_with('/'))
            .then(|| pwd.map(|dir| resolve_path(&dir, text)))
            .flatten()
            .filter(|p| std::path::Path::new(p).exists());
        let url = resolved.unwrap_or_else(|| text.to_owned());
        if url.starts_with('/') || url.starts_with("file://") {
            xdg_open(&url);
            return;
        }
        let fallback = url.clone();
        gtk::UriLauncher::new(&url).launch(
            self.area.root().and_downcast_ref::<gtk::Window>(),
            gio::Cancellable::NONE,
            move |result| {
                if result.is_err() {
                    xdg_open(&fallback);
                }
            },
        );
    }

    /// ghostty's mouse shape: the terminal's (OSC 22, or an arrow while it
    /// reports the mouse), overridden by modifiers (keyToMouseShape):
    /// shift shows text, and the rectangle-select chord a crosshair.
    pub fn update_cursor(&self) {
        let Some(shared) = self.draw.borrow().shared.clone() else { return };
        let (shape, reporting) = {
            let term = shared.term.lock();
            (term.mouse_shape(), term.modes().mouse_mode != term::MouseMode::None)
        };
        let m = self.mods.get();
        let rectangle = m.ctrl && m.alt;
        let name = match (reporting, m.shift, rectangle) {
            _ if self.hover.borrow().is_some() => "pointer",
            (true, true, true) | (false, _, true) => "crosshair",
            (_, true, _) => "text",
            _ => shape,
        };
        if self.cursor.replace(name) != name {
            self.area.set_cursor_from_name(Some(name));
        }
    }

    /// Moves the view `delta` lines up into the scrollback (negative: down)
    /// as far as it goes; returns whether it moved.
    fn scroll_view(&self, delta: impl FnOnce(&term::Terminal) -> isize) -> bool {
        {
            let mut d = self.draw.borrow_mut();
            let Some(shared) = d.shared.clone() else { return false };
            let mut term = shared.term.lock();
            let before = term.view_offset();
            let delta = delta(&term);
            term.scroll_view(delta);
            if term.view_offset() == before {
                return false;
            }
            d.full = true;
        }
        self.area.queue_render();
        true
    }

    /// ghostty's scroll_page_up and _down, scroll_to_top and _bottom (also
    /// scroll-to-bottom on a keystroke that writes to the PTY) and
    /// jump_to_prompt.
    pub fn scroll(&self, scroll: Scroll) {
        if self.scroll_view(|term| bindings::scroll_delta(term, scroll)) {
            self.sync_scrollbar();
        }
    }

    /// ghostty's setScrollbar from the terminal's scrollback.
    pub fn sync_scrollbar(&self) {
        let (offset, total, len) = {
            let d = self.draw.borrow();
            let Some(shared) = d.shared.as_ref() else { return };
            let term = shared.term.lock();
            let history = term.history_len();
            (history - term.view_offset(), history + usize::from(term.rows()), usize::from(term.rows()))
        };
        self.scroll_bin.set_scrollbar(offset, total, len);
    }

    /// ghostty's scroll_to_row from the scrollbar: `value` is the top row.
    fn scroll_to_row(&self, value: f64) {
        self.scroll_view(|term| {
            let history = term.history_len();
            let row = (value.round().max(0.0) as usize).min(history);
            (history - row) as isize - term.view_offset() as isize
        });
    }

    /// ghostty's start_search: shows the search overlay and focuses its entry
    /// with the query selected. Opening it searches anew for the query the
    /// entry kept from the last search, as for a changed query.
    pub fn start_search(&self) {
        if !self.search.is_open() {
            self.search.bin.set_visible(true);
            self.sync_veil();
            self.run_search(&self.search.entry.text());
        }
        self.search.entry.grab_focus();
        self.search.entry.select_region(0, -1);
    }

    /// ghostty's end_search: hides the overlay, forgets the matches and their
    /// highlights (the entry keeps the query), and gives the terminal the focus.
    fn end_search(&self) {
        self.search.bin.set_visible(false);
        if let Some(timer) = std::mem::take(&mut *self.found.borrow_mut()).timer {
            timer.remove();
        }
        self.search.count.set_label("");
        self.sync_veil();
        self.draw.borrow_mut().full = true;
        self.area.queue_render();
        self.area.grab_focus();
    }

    /// Searches the terminal for `query`: anew when it changed (ghostty's
    /// search action), showing the current match, or again for new output.
    fn run_search(&self, query: &str) {
        let Some(shared) = self.draw.borrow().shared.clone() else { return };
        let mut term = shared.term.lock();
        let matches = term.search(query);
        let bottom = term.view_top() + u64::from(term.rows()) - 1;
        let mut found = self.found.borrow_mut();
        found.ran = Some(Instant::now());
        let changed = found.update(query, matches, bottom);
        let scrolled = changed && found.current_match().is_some_and(|m| search::reveal(&mut term, m));
        drop(term);
        let label = found.label();
        drop(found);
        self.show_found(&label, scrolled);
    }

    /// ghostty's navigate_search: the next older match (up) or newer one,
    /// wrapping around, scrolled into view.
    fn navigate_search(&self, older: bool) {
        let Some(shared) = self.draw.borrow().shared.clone() else { return };
        let mut found = self.found.borrow_mut();
        found.step(older);
        let scrolled = found.current_match().is_some_and(|m| search::reveal(&mut shared.term.lock(), m));
        let label = found.label();
        drop(found);
        self.show_found(&label, scrolled);
    }

    fn show_found(&self, label: &str, scrolled: bool) {
        self.search.count.set_label(label);
        self.draw.borrow_mut().full = true;
        self.area.queue_render();
        if scrolled {
            self.sync_scrollbar();
        }
    }

    /// New output or a new size runs an open search again, at most every
    /// SEARCH_REFRESH; nothing happens while nothing is searched for.
    pub fn refresh_search(self: &Rc<Self>) {
        let mut found = self.found.borrow_mut();
        if found.query.is_empty() || found.timer.is_some() {
            return;
        }
        let wait = found.ran.map_or(Duration::ZERO, |ran| theme::SEARCH_REFRESH.saturating_sub(ran.elapsed()));
        let weak = Rc::downgrade(self);
        found.timer = Some(glib::timeout_add_local_once(wait, move || {
            let Some(s) = weak.upgrade() else { return };
            let query = {
                let mut found = s.found.borrow_mut();
                found.timer = None;
                found.query.clone()
            };
            s.run_search(&query);
        }));
    }

    /// Gives the surface the focus where the user left it: the search entry,
    /// keeping its selection, if it had the focus last, else the terminal.
    pub fn grab_focus(&self) {
        let entry = self.search.is_open() && self.found.borrow().focused;
        match entry.then(|| self.search.entry.delegate()).flatten().and_downcast::<gtk::Text>() {
            Some(text) => text.grab_focus_without_selecting(),
            None => self.area.grab_focus(),
        };
    }

    /// ghostty's scrollCallback for the vertical wheel: rows from the wheel
    /// (three per detent, scaled; touchpad deltas times ten), sent as cursor
    /// keys on the alternate screen of a program that wants no mouse, as
    /// wheel reports to one that does, or else scrolling the viewport.
    fn wheel(&self, dy: f64, state: gdk::ModifierType) {
        let (Some(renderer_cell), Some(shared)) = ({
            let d = self.draw.borrow();
            (d.renderer.as_ref().map(|r| r.cell_size()), d.shared.clone())
        }) else {
            return;
        };
        let scale = self.font_scale.get();
        let precision = self.precision_scroll.get();
        let multiplier = if precision { 10.0 } else { 1.0 };
        let yoff = dy * scale * -multiplier;
        let cell = f64::from(renderer_cell.1);
        let adjusted = if precision { yoff * theme::SCROLL_PRECISION } else { yoff * cell * theme::SCROLL_DISCRETE };
        let pending = self.pending_scroll.get() + adjusted;
        if pending.abs() < cell {
            self.pending_scroll.set(pending);
            return;
        }
        let amount = pending / cell;
        self.pending_scroll.set(pending - amount * cell);
        let delta = amount.trunc() as isize;
        let mut term = shared.term.lock();
        let modes = term.modes();
        let mut bytes = Vec::new();
        let reports = modes.mouse_mode != term::MouseMode::None;
        if reports || (modes.alt_screen && modes.alternate_scroll) {
            drop(term);
            self.set_selection(None);
            term = shared.term.lock();
        }
        if modes.alt_screen && modes.mouse_mode == term::MouseMode::None && modes.alternate_scroll {
            let seq: &[u8] = match (delta > 0, modes.app_cursor) {
                (true, true) => b"\x1bOA",
                (true, false) => b"\x1b[A",
                (false, true) => b"\x1bOB",
                (false, false) => b"\x1b[B",
            };
            for _ in 0..delta.unsigned_abs() {
                bytes.extend_from_slice(seq);
            }
        } else if modes.mouse_mode != term::MouseMode::None {
            let mods = mods_of(state);
            let (cell, pixels) = self.pointer_cell(&term);
            let button = if delta > 0 { mouse::Button::Four } else { mouse::Button::Five };
            let event = mouse::Event { button: Some(button), action: mouse::Action::Press, mods, cell, pixels };
            for _ in 0..delta.unsigned_abs() {
                mouse::encode(&mut bytes, modes.mouse_mode, modes.mouse_encoding, event);
            }
        } else {
            term.scroll_view(delta);
            drop(term);
            self.draw.borrow_mut().full = true;
            self.area.queue_render();
            self.sync_scrollbar();
            return;
        }
        drop(term);
        shared.write(&bytes);
    }

    /// ghostty's ecMouseScrollHorizontal: a touchpad swipe past 120 units
    /// switches tabs (negative to the next), forgotten after 500ms without
    /// input; a tilted wheel is reported as buttons 6 and 7 to a program
    /// that asks for the mouse.
    fn horizontal_scroll(self: &Rc<Self>, dx: f64, unit: gdk::ScrollUnit, state: gdk::ModifierType) {
        if unit == gdk::ScrollUnit::Surface {
            if let Some(id) = self.swipe_reset.take() {
                id.remove();
            }
            let pending = self.pending_swipe.get() + dx;
            if pending.abs() < theme::SWIPE_TAB {
                self.pending_swipe.set(pending);
                let weak = Rc::downgrade(self);
                let id = glib::timeout_add_local_once(theme::SWIPE_RESET, move || {
                    if let Some(s) = weak.upgrade() {
                        s.swipe_reset.take();
                        s.pending_swipe.set(0.0);
                    }
                });
                *self.swipe_reset.borrow_mut() = Some(id);
                return;
            }
            self.pending_swipe.set(0.0);
            (self.handler)(Event::SwipeTab { next: pending < 0.0 });
            return;
        }
        let delta = (-dx * self.font_scale.get()).round() as i64;
        if delta == 0 {
            return;
        }
        let button = if delta > 0 { mouse::Button::Six } else { mouse::Button::Seven };
        let mods = mods_of(state);
        for _ in 0..delta.unsigned_abs() {
            if !self.report(Some(button), mouse::Action::Press, mods) {
                return;
            }
        }
    }

    /// The grid cell under the pointer, clamped to the grid, and the
    /// pointer's position from the grid's origin in device pixels.
    fn pointer_cell(&self, term: &term::Terminal) -> ((u16, u16), (i32, i32)) {
        let Some((cw, ch)) = self.cell_size() else { return ((0, 0), (0, 0)) };
        let scale = self.font_scale.get();
        let pad = f64::from(padding(scale));
        let (x, y) = self.pointer.get();
        let (px, py) = (x * scale - pad, y * scale - pad);
        let col = (px / f64::from(cw)).floor().max(0.0) as u16;
        let row = (py / f64::from(ch)).floor().max(0.0) as u16;
        let cell = (col.min(term.cols().saturating_sub(1)), row.min(term.rows().saturating_sub(1)));
        (cell, (px.round() as i32, py.round() as i32))
    }

    fn set_preedit(&self, text: Option<String>) {
        if text.is_some() {
            self.set_selection(None);
        }
        if *self.preedit.borrow() != text {
            *self.preedit.borrow_mut() = text;
            self.draw.borrow_mut().full = true;
            self.area.queue_render();
        }
    }

    /// ghostty's updateImePosition: the candidate window goes below the
    /// middle of the cursor cell.
    fn update_ime_position(&self) {
        let d = self.draw.borrow();
        let (Some(renderer), Some(shared)) = (d.renderer.as_ref(), d.shared.as_ref()) else { return };
        let (cw, ch) = renderer.cell_size();
        let (cw, ch) = (f64::from(cw), f64::from(ch));
        let scale = self.font_scale.get();
        let pad = f64::from(padding(scale));
        let (x, y) = {
            let term = shared.term.lock();
            let cursor = term.cursor();
            (f64::from(cursor.x), f64::from(cursor.y))
        };
        let px = (x * cw + pad + cw / 2.0) / scale;
        let bottom = (y * ch + pad + ch) / scale;
        let top = (bottom - ch / scale).floor();
        let height = (bottom.ceil() - top).max(1.0);
        self.im.set_cursor_location(&gdk::Rectangle::new(px.floor() as i32, top as i32, 1, height as i32));
    }

    /// ghostty's setBellRinging: a focused surface never starts ringing.
    pub fn set_bell_ringing(&self, ringing: bool) {
        if ringing && self.focused.get() {
            return;
        }
        if self.bell.replace(ringing) != ringing {
            (self.handler)(Event::BellChanged);
        }
    }

    pub fn bell_ringing(&self) -> bool {
        self.bell.get()
    }

    /// ghostty's sendDesktopNotification.
    pub fn send_desktop_notification(&self, title: &str, body: &str) {
        self.notifications.send(self.focused.get(), title, body);
    }

    /// ghostty's setProgressReport with progress-style = true: any report
    /// but a removal shows the bar until the next one or for 15 seconds.
    pub fn set_progress_report(self: &Rc<Self>, report: term::ProgressReport) {
        use term::ProgressState;
        if let Some(timer) = self.progress_timer.take() {
            timer.remove();
        }
        let bar = &self.progress;
        let fraction = |progress: u8| f64::from(progress.min(100)) / 100.0;
        match report.state {
            ProgressState::Remove => {
                // A value stops the pulse animation, if there is one.
                bar.set_fraction(0.0);
                bar.set_visible(false);
                return;
            }
            ProgressState::Set | ProgressState::Error => {
                if report.state == ProgressState::Error {
                    bar.add_css_class("error");
                } else {
                    bar.remove_css_class("error");
                }
                match report.progress {
                    Some(progress) => bar.set_fraction(fraction(progress)),
                    None => bar.pulse(),
                }
            }
            ProgressState::Indeterminate => bar.pulse(),
            ProgressState::Pause => {
                if let Some(progress) = report.progress {
                    bar.set_fraction(fraction(progress));
                }
            }
        }
        bar.set_visible(true);
        let weak = Rc::downgrade(self);
        let timer = glib::timeout_add_local_once(theme::PROGRESS_TIMEOUT, move || {
            if let Some(s) = weak.upgrade() {
                s.progress_timer.take();
                s.set_progress_report(term::ProgressReport { state: ProgressState::Remove, progress: None });
            }
        });
        *self.progress_timer.borrow_mut() = Some(timer);
    }

    /// ghostty's childExited on the GTK surface: the banner tells how the
    /// child ended.
    pub fn show_child_exited(&self, code: u32) {
        let (title, class, other) = if code == 0 {
            ("Command Succeeded", "normal", "abnormal")
        } else {
            ("Command Failed", "abnormal", "normal")
        };
        self.banner.set_title(title);
        self.child_exited.add_css_class(class);
        self.child_exited.remove_css_class(other);
        self.child_exited.set_visible(true);
    }

    /// ghostty's start_command message (OSC 133;C).
    pub fn start_command(&self) {
        self.command_timer.set(Some(Instant::now()));
    }

    /// ghostty's stop_command message (OSC 133;D) and commandFinished with
    /// notify-on-command-finish = never and its action = bell: whether the
    /// bell rings for the command that just finished.
    pub fn stop_command(&self) -> bool {
        let Some(started) = self.command_timer.take() else { return false };
        // The next command finished, however long it took.
        let notify = self.notify_next_command.state().and_then(|s| s.get::<bool>()) == Some(true);
        self.notify_next_command.set_state(&false.to_variant());
        notify && started.elapsed() > theme::NOTIFY_COMMAND_AFTER
    }

    pub fn set_password_input(&self, on: bool) {
        if self.password.replace(on) != on {
            self.area.queue_render();
        }
    }

    pub fn cell_size(&self) -> Option<(u32, u32)> {
        self.draw.borrow().renderer.as_ref().map(|r| r.cell_size())
    }

    fn render(&self) {
        let mut d = self.draw.borrow_mut();
        let d = &mut *d;
        let Some(renderer) = d.renderer.as_mut() else { return };
        let Some(grid) = d.frame.grids.first_mut() else { return };
        d.frame.cursors.clear();
        if let Some(shared) = &d.shared {
            use std::sync::atomic::Ordering;
            shared.dirty.store(false, Ordering::Release);
            shared.want_lock.store(true, Ordering::Release);
            let mut term = shared.term.lock();
            shared.want_lock.store(false, Ordering::Release);
            let selection = term.selection();
            let found = self.found.borrow();
            compose::copy_pane(grid, &mut term, d.full, &found.matches, found.current, selection.as_ref());
            if let Some(link) = self.hover.borrow().as_ref() {
                compose::underline(grid, &term, link);
            }
            let focused = self.focused.get();
            let preedit = self.preedit.borrow();
            let cursor = if let Some(text) = preedit.as_deref().filter(|t| !t.is_empty()) {
                // ghostty draws the preedit over the cursor's row and no cursor.
                compose::preedit(grid, &term, text);
                None
            } else if self.password.get() {
                compose::lock_cursor(&term, 0)
            } else if !focused || !term.cursor().blinking || d.blink_on {
                compose::pane_cursor(&term, 0, focused)
            } else {
                None
            };
            d.frame.cursors.extend(cursor);
        }
        d.full = false;
        renderer.draw(&mut d.frame);
    }

    /// The surface's rectangle in `ancestor`'s coordinates, in logical pixels.
    pub fn bounds(&self, ancestor: &impl IsA<gtk::Widget>) -> Option<graphene::Rect> {
        self.outer.compute_bounds(ancestor)
    }

}

#[cfg(test)]
mod tests {
    use super::resolve_path;

    #[test]
    fn paths_resolve_like_zig() {
        assert_eq!(resolve_path("/a/b", "./c"), "/a/b/c");
        assert_eq!(resolve_path("/a/b/", "../c//d"), "/a/c/d");
        assert_eq!(resolve_path("/", "../x"), "/x");
    }
}
