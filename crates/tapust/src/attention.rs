//! ghostty's bell-features = attention on Wayland (winproto/wayland.zig
//! setUrgent): a window asks for an xdg_activation_v1 token for its own
//! surface and activates itself with it. The token carries no input serial,
//! so the compositor marks the window urgent instead of focusing it.
//!
//! The objects live on GDK's wl_display connection, in the event queue of
//! wayland-client's backend, which is not GDK's. Whoever reads the socket
//! (GDK's poll source after every poll, or GDK and Mesa waiting on their own
//! queues) sorts the events into that queue. A GSource looks into the queue
//! before and after every poll, as GDK does for its surface queues, and
//! dispatches it. It never reads the socket, so it does not block and does
//! not take part in GDK's reads; GDK flushes the requests before it polls.

use std::cell::RefCell;
use std::ffi::c_int;
use std::ptr;
use std::rc::Rc;

use gdk_wayland::prelude::*;
use gtk::glib::translate::IntoGlib;
use gtk::prelude::*;
use gtk::{gdk, glib};
use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::wl_registry::{self, WlRegistry};
use wayland_client::protocol::wl_surface::WlSurface;
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle, delegate_noop};
use wayland_protocols::xdg::activation::v1::client::xdg_activation_token_v1::{self, XdgActivationTokenV1};
use wayland_protocols::xdg::activation::v1::client::xdg_activation_v1::XdgActivationV1;

pub struct Attention {
    connection: Connection,
    queue: RefCell<EventQueue<Tokens>>,
    tokens: RefCell<Tokens>,
}

struct Tokens {
    /// None when the compositor does not offer xdg_activation_v1.
    activation: Option<XdgActivationV1>,
    pending: Pending<WlSurface, XdgActivationTokenV1>,
}

/// The token each surface waits for, at most one a surface (ghostty's
/// activation_token).
struct Pending<S, T>(Vec<(S, T)>);

impl<S: PartialEq, T: PartialEq> Pending<S, T> {
    /// Adds the token of `surface`, returning the one it replaces.
    fn replace(&mut self, surface: S, token: T) -> Option<T> {
        let old = self.cancel(&surface);
        self.0.push((surface, token));
        old
    }

    fn cancel(&mut self, surface: &S) -> Option<T> {
        let i = self.0.iter().position(|(s, _)| s == surface)?;
        Some(self.0.swap_remove(i).1)
    }

    /// Takes the surface that waits for `token`.
    fn done(&mut self, token: &T) -> Option<S> {
        let i = self.0.iter().position(|(_, t)| t == token)?;
        Some(self.0.swap_remove(i).0)
    }
}

impl Attention {
    pub fn new(display: &gdk::Display) -> Rc<Attention> {
        let display = display.downcast_ref::<gdk_wayland::WaylandDisplay>().expect("tapust runs on Wayland only");
        let wl_display = display.wl_display().expect("the display's wl_display");
        let connection = Connection::from_backend(wl_display.backend().upgrade().expect("GDK's Wayland connection"));
        let (globals, queue) = registry_queue_init::<Tokens>(&connection).expect("the Wayland globals");
        let activation = globals
            .bind::<XdgActivationV1, _, _>(&queue.handle(), 1..=1, ())
            .inspect_err(|err| eprintln!("tapust: no xdg_activation_v1 ({err}), so a bell cannot ask for attention"))
            .ok();
        let attention = Rc::new(Attention {
            connection,
            queue: RefCell::new(queue),
            tokens: RefCell::new(Tokens { activation, pending: Pending(Vec::new()) }),
        });
        attach(attention.clone());
        attention
    }

    /// ghostty's setUrgent(true): a new request replaces the pending one.
    pub fn request(&self, window: &impl IsA<gtk::Native>) {
        let Some(surface) = wl_surface(window) else { return };
        let mut tokens = self.tokens.borrow_mut();
        let Some(activation) = &tokens.activation else { return };
        let token = activation.get_activation_token(&self.queue.borrow().handle(), ());
        token.set_surface(&surface);
        token.commit();
        if let Some(old) = tokens.pending.replace(surface, token) {
            old.destroy();
        }
    }

    /// ghostty's setUrgent(false). A window cancels before GDK destroys its
    /// wl_surface, which the pending token refers to.
    pub fn cancel(&self, window: &impl IsA<gtk::Native>) {
        let Some(surface) = wl_surface(window) else { return };
        if let Some(token) = self.tokens.borrow_mut().pending.cancel(&surface) {
            token.destroy();
        }
    }

    /// The queue holds events (wl_display_prepare_read_queue fails), which
    /// GDK or another reader put there.
    fn has_events(&self) -> bool {
        self.connection.prepare_read().is_none()
    }

    fn dispatch(&self) {
        let mut tokens = self.tokens.borrow_mut();
        self.queue.borrow_mut().dispatch_pending(&mut *tokens).expect("the Wayland connection");
    }
}

fn wl_surface(window: &impl IsA<gtk::Native>) -> Option<WlSurface> {
    let surface = window.surface()?.downcast::<gdk_wayland::WaylandSurface>().expect("a Wayland surface");
    Some(surface.wl_surface().expect("the surface's wl_surface"))
}

impl Dispatch<XdgActivationTokenV1, ()> for Tokens {
    fn event(
        tokens: &mut Self,
        token: &XdgActivationTokenV1,
        event: xdg_activation_token_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let xdg_activation_token_v1::Event::Done { token: id } = event else { return };
        // A replaced or cancelled token is destroyed and gets no more events.
        let Some(surface) = tokens.pending.done(token) else { return };
        tokens.activation.as_ref().expect("xdg_activation_v1 made the token").activate(id, &surface);
        token.destroy();
    }
}

delegate_noop!(Tokens: XdgActivationV1);

/// Globals that come or go later change nothing.
impl Dispatch<WlRegistry, GlobalListContents> for Tokens {
    fn event(
        _: &mut Self,
        _: &WlRegistry,
        _: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

/// The GSource that dispatches the queue; it has no file descriptor, since
/// GDK's poll source wakes the main loop for the socket.
#[repr(C)]
struct QueueSource {
    base: glib::ffi::GSource,
    attention: Rc<Attention>,
}

unsafe extern "C" fn prepare(source: *mut glib::ffi::GSource, timeout: *mut c_int) -> glib::ffi::gboolean {
    // SAFETY: GLib passes a source made by `attach` and a valid timeout.
    unsafe {
        *timeout = -1;
        (*source.cast::<QueueSource>()).attention.has_events().into_glib()
    }
}

unsafe extern "C" fn check(source: *mut glib::ffi::GSource) -> glib::ffi::gboolean {
    // SAFETY: GLib passes a source made by `attach`.
    unsafe { (*source.cast::<QueueSource>()).attention.has_events().into_glib() }
}

unsafe extern "C" fn dispatch(
    source: *mut glib::ffi::GSource,
    _: glib::ffi::GSourceFunc,
    _: glib::ffi::gpointer,
) -> glib::ffi::gboolean {
    // SAFETY: GLib passes a source made by `attach`.
    unsafe { (*source.cast::<QueueSource>()).attention.dispatch() };
    glib::ffi::G_SOURCE_CONTINUE
}

unsafe extern "C" fn finalize(source: *mut glib::ffi::GSource) {
    // SAFETY: GLib finalizes a source made by `attach` once.
    unsafe { ptr::drop_in_place(&raw mut (*source.cast::<QueueSource>()).attention) }
}

static FUNCS: glib::ffi::GSourceFuncs = glib::ffi::GSourceFuncs {
    prepare: Some(prepare),
    check: Some(check),
    dispatch: Some(dispatch),
    finalize: Some(finalize),
    closure_callback: None,
    closure_marshal: None,
};

/// Attaches a QueueSource to the main context, which keeps it.
fn attach(attention: Rc<Attention>) {
    // SAFETY: g_source_new allocates a QueueSource whose GSource part it
    // initializes; the rest is written before the source is attached. GLib
    // only reads FUNCS.
    unsafe {
        let source = glib::ffi::g_source_new(ptr::addr_of!(FUNCS).cast_mut(), size_of::<QueueSource>() as u32);
        ptr::write(&raw mut (*source.cast::<QueueSource>()).attention, attention);
        glib::ffi::g_source_attach(source, ptr::null_mut());
        glib::ffi::g_source_unref(source);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_new_request_replaces_the_pending_token() {
        let mut pending = Pending(Vec::new());
        assert_eq!(pending.replace('a', 1), None);
        assert_eq!(pending.replace('b', 2), None);
        assert_eq!(pending.replace('a', 3), Some(1));
        // The replaced token's done finds no surface; the new one's does, once.
        assert_eq!(pending.done(&1), None);
        assert_eq!(pending.done(&3), Some('a'));
        assert_eq!(pending.done(&3), None);
        assert_eq!(pending.0, [('b', 2)]);
    }

    #[test]
    fn a_cancelled_token_is_gone() {
        let mut pending = Pending(Vec::new());
        pending.replace('a', 1);
        assert_eq!(pending.cancel(&'b'), None);
        assert_eq!(pending.cancel(&'a'), Some(1));
        assert_eq!(pending.cancel(&'a'), None);
        assert_eq!(pending.done(&1), None);
        assert!(pending.0.is_empty());
    }
}
