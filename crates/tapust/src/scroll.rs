//! ghostty's GhosttySurface as GtkScrolledWindow sees it: an Adw.Bin that
//! implements GtkScrollable, so the scrolled window shows an overlay
//! scrollbar for the scrollback instead of scrolling the child as a whole.

use std::cell::{Cell, RefCell};

use adw::subclass::prelude::*;
use gtk::glib;
use adw::prelude::*;

/// What runs when the user moves the scrollbar: the new top row.
type OnScroll = Box<dyn Fn(f64)>;

mod imp {
    use super::*;

    #[derive(glib::Properties)]
    #[properties(wrapper_type = super::SurfaceBin)]
    pub struct SurfaceBin {
        #[property(override_interface = gtk::Scrollable, get, set, nullable)]
        hadjustment: RefCell<Option<gtk::Adjustment>>,
        #[property(override_interface = gtk::Scrollable, get, set = Self::set_vadjustment, nullable)]
        vadjustment: RefCell<Option<gtk::Adjustment>>,
        #[property(override_interface = gtk::Scrollable, get, set, builder(gtk::ScrollablePolicy::Minimum))]
        hscroll_policy: Cell<gtk::ScrollablePolicy>,
        #[property(override_interface = gtk::Scrollable, get, set, builder(gtk::ScrollablePolicy::Minimum))]
        vscroll_policy: Cell<gtk::ScrollablePolicy>,
        pub(super) on_scroll: RefCell<Option<OnScroll>>,
        vadj_handler: RefCell<Option<glib::SignalHandlerId>>,
        /// Set while the terminal configures the adjustment, so its own
        /// change does not come back as a scroll (ghostty blocks the signal).
        pub(super) updating: Cell<bool>,
    }

    impl Default for SurfaceBin {
        fn default() -> Self {
            SurfaceBin {
                hadjustment: RefCell::new(None),
                vadjustment: RefCell::new(None),
                hscroll_policy: Cell::new(gtk::ScrollablePolicy::Minimum),
                vscroll_policy: Cell::new(gtk::ScrollablePolicy::Minimum),
                on_scroll: RefCell::new(None),
                vadj_handler: RefCell::new(None),
                updating: Cell::new(false),
            }
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for SurfaceBin {
        const NAME: &'static str = "TapustSurface";
        type Type = super::SurfaceBin;
        type ParentType = adw::Bin;
        type Interfaces = (gtk::Scrollable,);
    }

    #[glib::derived_properties]
    impl ObjectImpl for SurfaceBin {}
    impl WidgetImpl for SurfaceBin {}
    impl BinImpl for SurfaceBin {}
    impl ScrollableImpl for SurfaceBin {}

    impl SurfaceBin {
        fn set_vadjustment(&self, adj: Option<gtk::Adjustment>) {
            if let (Some(old), Some(id)) = (self.vadjustment.take(), self.vadj_handler.take()) {
                old.disconnect(id);
            }
            if let Some(adj) = &adj {
                let weak = self.obj().downgrade();
                let id = adj.connect_value_changed(move |adj| {
                    let Some(bin) = weak.upgrade() else { return };
                    let imp = bin.imp();
                    if imp.updating.get() {
                        return;
                    }
                    if let Some(f) = imp.on_scroll.borrow().as_ref() {
                        f(adj.value());
                    }
                });
                *self.vadj_handler.borrow_mut() = Some(id);
            }
            *self.vadjustment.borrow_mut() = adj;
        }
    }
}

glib::wrapper! {
    pub struct SurfaceBin(ObjectSubclass<imp::SurfaceBin>)
        @extends adw::Bin, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::Scrollable;
}

impl SurfaceBin {
    pub fn new(child: &impl IsA<gtk::Widget>) -> SurfaceBin {
        let bin: SurfaceBin = glib::Object::builder().build();
        bin.set_child(Some(child));
        bin.add_css_class("surface");
        bin
    }

    /// Runs `f` with the top row the user scrolled the scrollbar to.
    pub fn connect_scroll(&self, f: impl Fn(f64) + 'static) {
        *self.imp().on_scroll.borrow_mut() = Some(Box::new(f));
    }

    /// ghostty's setScrollbar: the adjustment counts rows; `offset` is the
    /// top visible row of `total`, `len` rows are visible.
    pub fn set_scrollbar(&self, offset: usize, total: usize, len: usize) {
        let Some(vadj) = self.vadjustment() else { return };
        let (value, upper, page) = (offset as f64, total as f64, len as f64);
        let close = |a: f64, b: f64| (a - b).abs() < 0.001;
        if close(vadj.value(), value) && close(vadj.upper(), upper) && close(vadj.page_size(), page) {
            return;
        }
        self.imp().updating.set(true);
        vadj.configure(value, 0.0, upper, 1.0, page, page);
        self.imp().updating.set(false);
    }
}
