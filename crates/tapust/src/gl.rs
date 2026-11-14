//! OpenGL function loading for GTK's GL contexts, which are EGL contexts on
//! Wayland.

use std::ffi::{CStr, CString, c_char, c_void};
use std::sync::OnceLock;

use parking_lot::Mutex;

use gtk::prelude::*;

type GetProcAddress = unsafe extern "C" fn(*const c_char) -> *const c_void;

fn egl_get_proc_address() -> GetProcAddress {
    let address = egl(c"eglGetProcAddress");
    assert!(!address.is_null(), "eglGetProcAddress not found");
    // SAFETY: the symbol is eglGetProcAddress, which has this signature.
    unsafe { std::mem::transmute::<*mut c_void, GetProcAddress>(address) }
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

/// libEGL's function `name`, or null.
fn egl(name: &CStr) -> *mut c_void {
    static LIB: OnceLock<usize> = OnceLock::new();
    // SAFETY: dlopen and dlsym with valid C strings.
    unsafe {
        let lib = *LIB.get_or_init(|| libc::dlopen(c"libEGL.so.1".as_ptr(), libc::RTLD_LAZY | libc::RTLD_GLOBAL) as usize);
        if lib == 0 {
            std::ptr::null_mut()
        } else {
            libc::dlsym(lib as *mut c_void, name.as_ptr())
        }
    }
}

/// The context the GL thread compiled tapust's programs in, with its
/// display. Mesa readies its shader compiler as a process compiles its
/// first program and drops it with the last context, so this one is kept
/// until a pane's renderer has its own.
static WARM: Mutex<Option<(usize, usize)>> = Mutex::new(None);

/// Readies on another thread, as the first window is made, what GTK and
/// the first pane's renderer would otherwise do on the main thread as it is
/// shown:
/// - libglvnd loads the EGL drivers (Mesa's with LLVM, about 20 ms) once per
///   process, at the first query of the client extensions;
/// - EGL gives the display GDK creates for `display` (the Wayland platform,
///   the wl_display, no attributes) already initialized, with Mesa's screen
///   and its driconf files (about 6 ms);
/// - Mesa's shader compiler, readied in a context of that display, makes the
///   renderer's first program 3 ms faster.
pub fn prepare(display: &gtk::gdk::Display) {
    let native = display.downcast_ref::<gdk_wayland::WaylandDisplay>().map(|wayland| {
        // SAFETY: a Wayland display's wl_display, which lives as long as it.
        unsafe { gdk_wayland::ffi::gdk_wayland_display_get_wl_display(wayland.as_ptr()) as usize }
    });
    let prepare = move || {
        let functions = [
            c"eglQueryString",
            c"eglGetPlatformDisplay",
            c"eglInitialize",
            c"eglBindAPI",
            c"eglCreateContext",
            c"eglMakeCurrent",
        ]
        .map(egl);
        if functions.iter().any(|f| f.is_null()) {
            return;
        }
        let [query, platform_display, initialize, bind_api, create_context, make_current] = functions;
        // SAFETY: the EGL functions with their signatures and valid
        // arguments; EGL is thread-safe.
        unsafe {
            let query: unsafe extern "C" fn(*const c_void, i32) -> *const c_char = std::mem::transmute(query);
            let platform_display: unsafe extern "C" fn(u32, *mut c_void, *const isize) -> *mut c_void =
                std::mem::transmute(platform_display);
            let initialize: unsafe extern "C" fn(*mut c_void, *mut i32, *mut i32) -> u32 =
                std::mem::transmute(initialize);
            let bind_api: unsafe extern "C" fn(u32) -> u32 = std::mem::transmute(bind_api);
            let create_context: unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void, *const i32) -> *mut c_void =
                std::mem::transmute(create_context);
            let make_current: unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void, *mut c_void) -> u32 =
                std::mem::transmute(make_current);
            query(std::ptr::null(), 0x3055); // EGL_EXTENSIONS
            let Some(native) = native else { return };
            let display = platform_display(0x31D8, native as *mut c_void, std::ptr::null()); // EGL_PLATFORM_WAYLAND_EXT
            if display.is_null() || initialize(display, std::ptr::null_mut(), std::ptr::null_mut()) == 0 {
                return;
            }
            bind_api(0x30A2); // EGL_OPENGL_API
            // OpenGL 3.3 core, as the panes' GLArea asks for, without a
            // config or surface.
            let attributes = [0x3098, 3, 0x30FB, 3, 0x30FD, 1, 0x3038];
            let none = std::ptr::null_mut();
            let context = create_context(display, none, none, attributes.as_ptr());
            if context.is_null() {
                return;
            }
            if make_current(display, none, none, context) != 0 {
                crate::renderer::warm_up(&self::context());
                make_current(display, none, none, none);
            }
            *WARM.lock() = Some((display as usize, context as usize));
        }
    };
    let _ = std::thread::Builder::new().name("gl-prepare".into()).spawn(prepare);
}

/// Destroys the context `prepare` readied the shader compiler in, once a
/// renderer has its own.
pub fn drop_warm_context() {
    let Some((display, context)) = WARM.lock().take() else { return };
    let destroy = egl(c"eglDestroyContext");
    if !destroy.is_null() {
        // SAFETY: eglDestroyContext with the context and its display, which
        // no thread has current.
        unsafe {
            let destroy: unsafe extern "C" fn(*mut c_void, *mut c_void) -> u32 = std::mem::transmute(destroy);
            destroy(display as *mut c_void, context as *mut c_void);
        }
    }
}
