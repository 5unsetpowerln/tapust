//! A pane's widgets, as ghostty builds them (surface-scrolled-window.blp and
//! surface.blp): an Adw.Bin with a GtkScrolledWindow around an Overlay whose
//! child is the GLArea that draws the terminal, with ghostty's resize
//! overlay, unfocused-split veil, drag handle and drop overlay on top.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Instant;

use adw::prelude::*;
use gtk::{gdk, glib, graphene};
use mux::{Direction, PaneId};

use crate::compose;
use crate::font::Fonts;
use crate::frame::{Frame, Grid};
use crate::pty::Shared;
use crate::renderer::Renderer;
use crate::theme;

/// What a surface tells the application.
pub enum Event {
    /// GTK gave the GLArea a new size: the grid it holds.
    Resized { pane: PaneId, cols: u16, rows: u16, cell: (u32, u32) },
    Focused { pane: PaneId },
    /// A key press; returns whether it was used.
    Key { pane: PaneId, keyval: gdk::Key, state: gdk::ModifierType },
    /// A pane's drag handle was dropped on the `dir` side of this pane.
    Dropped { source: PaneId, target: PaneId, dir: Direction },
    /// The GLArea was mapped (ghostty restores focus after this).
    Mapped,
    /// The cell size in device pixels is known or changed (ghostty's
    /// cell_size action).
    CellSize { cell: (u32, u32) },
}

pub type Handler = Rc<dyn Fn(Event) -> bool>;

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
    resize_hide: RefCell<Option<glib::SourceId>>,
    resize_idle: Cell<bool>,
    blink_timer: RefCell<Option<glib::SourceId>>,
    /// The PTY looks like a password prompt (ghostty's password_input).
    password: Cell<bool>,
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

        let scrolled = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .child(&overlay)
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
            resize_hide: RefCell::new(None),
            resize_idle: Cell::new(false),
            blink_timer: RefCell::new(None),
            password: Cell::new(false),
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
            s.font_scale.set(scale);
            s.schedule_cell_size();
        });
        let weak = Rc::downgrade(self);
        self.area.connect_unrealize(move |area| {
            if let Some(s) = weak.upgrade() {
                area.make_current();
                s.draw.borrow_mut().renderer = None;
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

        let click = gtk::GestureClick::new();
        let area = self.area.clone();
        click.connect_pressed(move |_, _, _, _| {
            area.grab_focus();
        });
        self.area.add_controller(click);

        let focus = gtk::EventControllerFocus::new();
        let (weak, h) = (Rc::downgrade(self), handler.clone());
        focus.connect_enter(move |_| {
            if let Some(s) = weak.upgrade() {
                s.set_focused(true);
                h(Event::Focused { pane: s.pane });
            }
        });
        let weak = Rc::downgrade(self);
        focus.connect_leave(move |_| {
            if let Some(s) = weak.upgrade() {
                s.set_focused(false);
            }
        });
        self.area.add_controller(focus);

        let keys = gtk::EventControllerKey::new();
        let (weak, h) = (Rc::downgrade(self), handler.clone());
        keys.connect_key_pressed(move |_, keyval, _, state| {
            let Some(s) = weak.upgrade() else { return glib::Propagation::Proceed };
            if h(Event::Key { pane: s.pane, keyval, state }) { glib::Propagation::Stop } else { glib::Propagation::Proceed }
        });
        self.area.add_controller(keys);

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
        self.veil.set_reveal_child(self.split.get() && !focused);
        if focused {
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
            compose::copy_pane(grid, &mut term, d.full);
            let focused = self.focused.get();
            let cursor = if self.password.get() {
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
