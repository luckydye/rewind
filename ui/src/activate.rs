//! Window setup the GPUI API cannot express.
//!
//! Two things are missing from gpui 0.2.2:
//!
//! * `-[NSApplication activateIgnoringOtherApps:]`, which GPUI calls, has been
//!   a no-op since macOS 14. Without the modern `-activate` the overlay draws
//!   above everything but every keystroke goes to the app that was frontmost.
//! * Every window GPUI creates carries `NSWindowStyleMaskTitled`, so the
//!   overlay has traffic lights, rounded corners, and content pushed down by a
//!   titlebar. Stripping the style mask makes it genuinely borderless.

use std::ffi::{c_char, c_void};

#[link(name = "AppKit", kind = "framework")]
unsafe extern "C" {
    fn objc_getClass(name: *const c_char) -> *mut c_void;
    fn sel_registerName(name: *const c_char) -> *mut c_void;
    fn objc_msgSend();
}

type SendObject = unsafe extern "C" fn(*mut c_void, *mut c_void) -> *mut c_void;
type SendUnsigned = unsafe extern "C" fn(*mut c_void, *mut c_void, usize) -> *mut c_void;
type SendPointer = unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void) -> *mut c_void;
type SendPair = unsafe extern "C" fn(*mut c_void, *mut c_void, f64, f64) -> *mut c_void;

fn selector(name: &std::ffi::CStr) -> *mut c_void {
    unsafe { sel_registerName(name.as_ptr()) }
}

unsafe fn send() -> SendObject {
    std::mem::transmute(objc_msgSend as *const ())
}

unsafe fn shared_application() -> *mut c_void {
    let class = objc_getClass(c"NSApplication".as_ptr());
    assert!(!class.is_null(), "NSApplication is not linked");
    let app = send()(class, selector(c"sharedApplication"));
    assert!(!app.is_null(), "NSApplication.sharedApplication was null");
    app
}

unsafe fn only_window() -> *mut c_void {
    let windows = send()(shared_application(), selector(c"windows"));
    let window = send()(windows, selector(c"firstObject"));
    assert!(!window.is_null(), "the overlay window does not exist yet");
    window
}

pub fn activate() {
    unsafe {
        let send: SendObject = std::mem::transmute(objc_msgSend as *const ());
        send(shared_application(), selector(c"activate"));

        // Ordering the window front after activating closes the race where
        // activation is accepted but the window has not been ordered yet.
        let order: SendPointer = std::mem::transmute(objc_msgSend as *const ());
        order(only_window(), selector(c"makeKeyAndOrderFront:"), std::ptr::null_mut());
    }
}

/// Turns the overlay window borderless and gives it the exact display size.
pub fn make_borderless(width: f32, height: f32) {
    unsafe {
        let window = only_window();

        // NSWindowStyleMaskBorderless is zero: no titlebar, no traffic lights,
        // no rounded corners, and the content view matches the window frame.
        let set_style: SendUnsigned = std::mem::transmute(objc_msgSend as *const ());
        set_style(window, selector(c"setStyleMask:"), 0);

        let set_pair: SendPair = std::mem::transmute(objc_msgSend as *const ());
        set_pair(window, selector(c"setContentSize:"), width as f64, height as f64);

        // AppKit's origin is bottom-left of the screen; the primary display's
        // bottom-left is (0, 0) and the window is as tall as the display.
        set_pair(window, selector(c"setFrameOrigin:"), 0.0, 0.0);
    }
}
