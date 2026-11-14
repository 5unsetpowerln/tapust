//! OpenGL function loading for GTK's GL contexts, which are EGL contexts on
//! Wayland.

use std::ffi::{CString, c_char, c_void};
use std::sync::OnceLock;

use gtk::prelude::*;

type GetProcAddress = unsafe extern "C" fn(*const c_char) -> *const c_void;

fn egl_get_proc_address() -> GetProcAddress {
    static PROC: OnceLock<usize> = OnceLock::new();
    let address = *PROC.get_or_init(|| {
        // SAFETY: dlopen and dlsym with valid C strings; GTK has already
        // loaded libEGL, so this only finds it.
        unsafe {
            let lib = libc::dlopen(c"libEGL.so.1".as_ptr(), libc::RTLD_LAZY | libc::RTLD_GLOBAL);
            assert!(!lib.is_null(), "libEGL.so.1 is not loaded");
            let sym = libc::dlsym(lib, c"eglGetProcAddress".as_ptr());
            assert!(!sym.is_null(), "eglGetProcAddress not found");
            sym as usize
        }
    });
    // SAFETY: the symbol is eglGetProcAddress, which has this signature.
    unsafe { std::mem::transmute::<usize, GetProcAddress>(address) }
}

/// A glow context for the GL context that is current (GTK makes a GLArea's
/// context current before its realize and render handlers).
pub fn context() -> glow::Context {
    let get = egl_get_proc_address();
    // SAFETY: the loader returns function pointers of the current context.
    unsafe {
        glow::Context::from_loader_function(|name| {
            let name = CString::new(name).expect("GL function name");
            get(name.as_ptr())
        })
    }
}

/// Loads the EGL drivers and initializes the EGL display GDK will use for
/// `display`, on another thread. libglvnd loads the drivers (Mesa's with
/// LLVM, about 20 ms) once per process, at the first query of the client
/// extensions, and EGL gives the display GDK creates when the first window
/// is shown (gdk_display_create_egl_display: the Wayland platform, the
/// wl_display, no attributes), already initialized: Mesa's screen and its
/// driconf files, about 6 ms, are then done as well.
pub fn prepare(display: &gtk::gdk::Display) {
    let native = display.downcast_ref::<gdk_wayland::WaylandDisplay>().map(|wayland| {
        // SAFETY: a Wayland display's wl_display, which lives as long as it.
        unsafe { gdk_wayland::ffi::gdk_wayland_display_get_wl_display(wayland.as_ptr()) as usize }
    });
    let prepare = move || {
        // SAFETY: dlopen and dlsym with valid C strings, the EGL functions
        // with their signatures and valid arguments; EGL is thread-safe.
        unsafe {
            let lib = libc::dlopen(c"libEGL.so.1".as_ptr(), libc::RTLD_LAZY | libc::RTLD_GLOBAL);
            if lib.is_null() {
                return;
            }
            let symbol = |name: &std::ffi::CStr| libc::dlsym(lib, name.as_ptr());
            let (query, platform_display, initialize) =
                (symbol(c"eglQueryString"), symbol(c"eglGetPlatformDisplay"), symbol(c"eglInitialize"));
            if query.is_null() || platform_display.is_null() || initialize.is_null() {
                return;
            }
            let query: unsafe extern "C" fn(*const c_void, i32) -> *const c_char = std::mem::transmute(query);
            query(std::ptr::null(), 0x3055); // EGL_EXTENSIONS
            let Some(native) = native else { return };
            let platform_display: unsafe extern "C" fn(u32, *mut c_void, *const isize) -> *mut c_void =
                std::mem::transmute(platform_display);
            let initialize: unsafe extern "C" fn(*mut c_void, *mut i32, *mut i32) -> u32 =
                std::mem::transmute(initialize);
            let egl_display = platform_display(0x31D8, native as *mut c_void, std::ptr::null()); // EGL_PLATFORM_WAYLAND_EXT
            if !egl_display.is_null() {
                initialize(egl_display, std::ptr::null_mut(), std::ptr::null_mut());
            }
        }
    };
    let _ = std::thread::Builder::new().name("gl-prepare".into()).spawn(prepare);
}
