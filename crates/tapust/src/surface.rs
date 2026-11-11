//! A pane's widgets, as ghostty builds them (surface-scrolled-window.blp and
//! surface.blp): an Adw.Bin with a GtkScrolledWindow around an Overlay whose
//! child is the GLArea that draws the terminal, with ghostty's resize
//! overlay, unfocused-split veil, drag handle and drop overlay on top.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Instant;

use adw::prelude::*;
use gtk::{gdk, gio, glib, graphene};
use mux::{Direction, PaneId};

use crate::compose;
use crate::font::Fonts;
use crate::frame::{Frame, Grid};
use crate::pty::Shared;
use crate::renderer::Renderer;
use crate::scroll::SurfaceBin;
use crate::links;
use crate::mouse;
use crate::selection::{self, DragPoint, Point, Selection};
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
    /// The cell size in device pixels is known or changed (ghostty's
    /// cell_size action).
    CellSize { cell: (u32, u32) },
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
    /// Clicks in the current series (1 cell, 2 word, 3 line).
    count: u8,
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
    selection: RefCell<Option<Selection>>,
    gesture: RefCell<Gesture>,
    /// Mouse buttons held (bit per button number), for motion reports.
    buttons: Cell<u8>,
    /// A left click that only focused the surface; its release is ignored.
    suppress_left_release: Cell<bool>,
    /// Scrolls while a selection is dragged above or below the surface.
    autoscroll: RefCell<Option<(glib::SourceId, bool)>>,
    /// The terminal was on the alternate screen at the last frame.
    was_alt: Cell<bool>,
    /// Modifiers held (from the key controller), for the pointer's shape.
    mods: Cell<mouse::Mods>,
    /// The CSS cursor name last given to the GLArea.
    cursor: Cell<&'static str>,
    /// The link under the pointer while ctrl is held, and its text.
    hover: RefCell<Option<(Selection, String)>>,
    last_blink_reset: Cell<Option<Instant>>,
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

fn load_fonts(scale: f64) -> Option<Fonts> {
    Fonts::new(theme::FONT_FAMILY, theme::FONT_POINTS, scale)
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
    pub fn new(pane: PaneId, term_bg: [u8; 4], handler: Handler) -> Rc<Surface> {
        let area = gtk::GLArea::builder().hexpand(true).vexpand(true).focusable(true).focus_on_click(true).build();
        area.set_allowed_apis(gdk::GLAPI::GL);
        area.set_required_version(3, 3);
        area.set_auto_render(false);
        area.set_cursor_from_name(Some("text"));
        let page = gtk::Box::builder().hexpand(true).vexpand(true).build();
        page.append(&area);
        let overlay = gtk::Overlay::builder().focusable(false).focus_on_click(false).child(&page).build();

        // ghostty's ResizeOverlay: a centered `background` label shown 750 ms.
        let resize_label = gtk::Label::builder()
            .css_classes(["background", "resize-overlay"])
            .can_focus(false)
            .can_target(false)
            .focusable(false)
            .focus_on_click(false)
            .selectable(false)
            .justify(gtk::Justification::Center)
            .halign(gtk::Align::Center)
            .valign(gtk::Align::Center)
            .build();
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
        // ghostty's url_left and url_right: the hovered link's text.
        let url_label = |align| {
            gtk::Label::builder().css_classes(["background", "url-overlay"]).visible(false).halign(align).valign(gtk::Align::End).build()
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

        let veil = gtk::Revealer::builder().transition_duration(0).can_focus(false).can_target(false).focusable(false).build();
        veil.set_child(Some(&gtk::DrawingArea::builder().css_classes(["unfocused-split"]).build()));
        overlay.add_overlay(&veil);

        let drag_handle = gtk::CenterBox::builder()
            .halign(gtk::Align::Fill)
            .valign(gtk::Align::Start)
            .visible(false)
            .css_classes(["drag-handle"])
            .build();
        drag_handle.set_center_widget(Some(&gtk::Image::from_icon_name("view-more-horizontal-symbolic")));
        drag_handle.set_cursor_from_name(Some("grab"));
        overlay.add_overlay(&drag_handle);

        let drop_overlay = gtk::DrawingArea::builder()
            .halign(gtk::Align::Fill)
            .valign(gtk::Align::Fill)
            .can_focus(false)
            .can_target(false)
            .focusable(false)
            .css_classes(["drop-overlay"])
            .build();
        overlay.add_overlay(&drop_overlay);

        let scroll_bin = SurfaceBin::new(&overlay);
        let scrolled = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .child(&scroll_bin)
            .build();
        let outer = adw::Bin::builder().child(&adw::Bin::builder().child(&scrolled).build()).build();

        let surface = Rc::new(Surface {
            pane,
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
            selection: RefCell::new(None),
            gesture: RefCell::new(Gesture::default()),
            buttons: Cell::new(0),
            suppress_left_release: Cell::new(false),
            autoscroll: RefCell::new(None),
            was_alt: Cell::new(false),
            mods: Cell::new(mouse::Mods::default()),
            cursor: Cell::new("text"),
            hover: RefCell::new(None),
            last_blink_reset: Cell::new(None),
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
        let weak = Rc::downgrade(self);
        self.area.connect_realize(move |area| {
            let Some(s) = weak.upgrade() else { return };
            area.make_current();
            if area.error().is_some() {
                eprintln!("tapust: no OpenGL context: {:?}", area.error());
                return;
            }
            let scale = area.native().and_then(|n| n.surface()).map_or(1.0, |s| s.scale());
            let Some(fonts) = load_fonts(scale) else { return };
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
        let (weak, h) = (Rc::downgrade(self), handler.clone());
        self.area.connect_resize(move |area, width, height| {
            let Some(s) = weak.upgrade() else { return };
            let scale = area.native().and_then(|n| n.surface()).map_or(1.0, |s| s.scale());
            let (cols, rows, cell) = {
                let mut d = s.draw.borrow_mut();
                let Some(renderer) = d.renderer.as_mut() else { return };
                // ghostty's content scale callback: a new scale needs fonts
                // rasterized for it.
                if scale != s.font_scale.get()
                    && let Some(fonts) = load_fonts(scale)
                {
                    renderer.set_fonts(fonts);
                    s.font_scale.set(scale);
                    s.schedule_cell_size();
                }
                renderer.resize(width as u32, height as u32);
                let (cw, ch) = renderer.cell_size();
                let pad = padding(scale);
                let fit = |size: i32, cell: u32| ((size.max(0) as u32).saturating_sub(2 * pad) / cell.max(1)).max(1) as u16;
                let (cols, rows) = (fit(width, cw), fit(height, ch));
                let bg = d.term_bg;
                d.frame.grids = vec![Grid::new(s.pane, (0, 0, width as u32, height as u32), (pad as i32, pad as i32), (cols, rows), bg)];
                d.full = true;
                (cols, rows, (cw, ch))
            };
            s.size.set((cols, rows));
            // ghostty shows the overlay whenever the pixel size changes,
            // except for the first size.
            let old = s.pixels.replace((width, height));
            if old != (width, height) && old != (0, 0) && s.created.elapsed() >= theme::RESIZE_OVERLAY_DELAY {
                s.schedule_resize_overlay();
            }
            h(Event::Resized { pane: s.pane, cols, rows, cell });
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
        self.veil.set_reveal_child(self.split.get() && !focused);
        if focused {
            self.set_bell_ringing(false);
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
        self.veil.set_reveal_child(split && !self.focused.get());
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

    /// ghostty's setScrollbar from the terminal's scrollback.
    fn set_selection(&self, selection: Option<Selection>) {
        if *self.selection.borrow() == selection {
            return;
        }
        let changed = self.selection.borrow().is_some() != selection.is_some();
        *self.selection.borrow_mut() = selection;
        self.draw.borrow_mut().full = true;
        self.area.queue_render();
        if changed {
            (self.handler)(Event::SelectionChanged);
        }
    }

    pub fn clear_selection(&self) {
        self.set_selection(None);
    }

    pub fn has_selection(&self) -> bool {
        self.selection.borrow().is_some()
    }

    /// The selected text as ghostty copies it.
    pub fn selection_text(&self) -> Option<String> {
        let sel = (*self.selection.borrow())?;
        let shared = self.draw.borrow().shared.clone()?;
        let text = selection::text(&shared.term.lock(), &sel);
        (!text.is_empty()).then_some(text)
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
                let selection = match g.count {
                    1 => None,
                    // A double click on a link selects all of it.
                    2 => links::link_at(&term, point.cell).map(|(sel, _)| sel).or_else(|| selection::word(&term, point.cell)),
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
                let inside = self.selection.borrow().is_some_and(|s| s.contains(point.cell, term.cols()));
                let hovered = self.hover.borrow().as_ref().map(|(sel, _)| *sel);
                if !inside && let Some(word) = hovered.or_else(|| selection::word(&term, point.cell)) {
                    drop(term);
                    self.set_selection(Some(word));
                    self.copy_primary();
                }
            }
            mouse::Button::Four | mouse::Button::Five => {}
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
        // ghostty opens the hovered link on a left release that did not drag.
        if button == mouse::Button::Left && !self.gesture.borrow().dragged {
            let link = self.hover.borrow().as_ref().map(|(_, text)| text.clone());
            if let Some(text) = link {
                self.open_link(&text);
            }
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

    /// ghostty's link hover: with exactly ctrl held (shift too while a
    /// program reports the mouse), the link under the pointer is underlined
    /// and its text shown at the bottom left.
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
            Some((_, text)) => {
                self.url_left.set_label(text);
                self.url_right.set_label(text);
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

    /// ghostty's scroll-to-bottom on a keystroke that writes to the PTY.
    pub fn scroll_to_bottom(&self) {
        {
            let mut d = self.draw.borrow_mut();
            let Some(shared) = d.shared.clone() else { return };
            let mut term = shared.term.lock();
            let offset = term.view_offset() as isize;
            if offset == 0 {
                return;
            }
            term.scroll_view(-offset);
            d.full = true;
        }
        self.area.queue_render();
        self.sync_scrollbar();
    }

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
        {
            let mut d = self.draw.borrow_mut();
            let Some(shared) = d.shared.clone() else { return };
            let mut term = shared.term.lock();
            let history = term.history_len();
            let row = (value.round().max(0.0) as usize).min(history);
            let delta = (history - row) as isize - term.view_offset() as isize;
            if delta == 0 {
                return;
            }
            term.scroll_view(delta);
            d.full = true;
        }
        self.area.queue_render();
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
            // ghostty drops the selection when the screen switches.
            let alt = term.modes().alt_screen;
            if alt != self.was_alt.replace(alt) && self.selection.borrow_mut().take().is_some() {
                d.full = true;
                (self.handler)(Event::SelectionChanged);
            }
            compose::copy_pane(grid, &mut term, d.full, self.selection.borrow().as_ref());
            if let Some((link, _)) = self.hover.borrow().as_ref() {
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
