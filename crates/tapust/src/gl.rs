//! OpenGL function loading for GTK's GL contexts, which are EGL contexts on
//! Wayland.

use std::ffi::{CString, c_char, c_void};
use std::sync::OnceLock;

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
