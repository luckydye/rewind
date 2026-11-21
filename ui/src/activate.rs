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
type SendDouble = unsafe extern "C" fn(*mut c_void, *mut c_void, f64) -> *mut c_void;
type SendBool = unsafe extern "C" fn(*mut c_void, *mut c_void) -> bool;

fn selector(name: &std::ffi::CStr) -> *mut c_void {
    unsafe { sel_registerName(name.as_ptr()) }
}

fn send() -> SendObject {
    unsafe { std::mem::transmute(objc_msgSend as *const ()) }
}

fn shared_application() -> *mut c_void {
    unsafe {
        let class = objc_getClass(c"NSApplication".as_ptr());
        assert!(!class.is_null(), "NSApplication is not linked");
        let app = send()(class, selector(c"sharedApplication"));
        assert!(!app.is_null(), "NSApplication.sharedApplication was null");
        app
    }
}

fn only_window() -> *mut c_void {
    unsafe {
        let windows = send()(shared_application(), selector(c"windows"));
        let window = send()(windows, selector(c"firstObject"));
        assert!(!window.is_null(), "the overlay window does not exist yet");
        window
    }
}

/// AppKit finds the window through `NSApplication.windows`; only the Windows
/// version needs the handle.
pub fn remember(_window: &gpui::Window) {}

/// The overlay stays resident between uses, so it must not sit in the Dock
/// or the app switcher: NSApplicationActivationPolicyAccessory.
pub fn make_accessory() {
    unsafe {
        let set: SendUnsigned = std::mem::transmute(objc_msgSend as *const ());
        set(shared_application(), selector(c"setActivationPolicy:"), 1);
    }
}

/// Hides the overlay and hands focus back to whichever app was frontmost
/// before it opened. Ordering the window out alone would leave no app active.
pub fn hide() {
    unsafe {
        let hide: SendPointer = std::mem::transmute(objc_msgSend as *const ());
        hide(shared_application(), selector(c"hide:"), std::ptr::null_mut());
    }
}

pub fn activate() {
    unsafe {
        let send: SendObject = std::mem::transmute(objc_msgSend as *const ());
        let unhide: SendPointer = std::mem::transmute(objc_msgSend as *const ());
        unhide(
            shared_application(),
            selector(c"unhideWithoutActivation"),
            std::ptr::null_mut(),
        );
        send(shared_application(), selector(c"activate"));

        // Ordering the window front after activating closes the race where
        // activation is accepted but the window has not been ordered yet.
        // `makeKeyWindow` is the part that routes keys here; ordering alone
        // leaves the window frontmost but not key.
        let order: SendPointer = std::mem::transmute(objc_msgSend as *const ());
        order(only_window(), selector(c"makeKeyAndOrderFront:"), std::ptr::null_mut());
        order(only_window(), selector(c"makeKeyWindow"), std::ptr::null_mut());
    }
}

/// Whether macOS considers the overlay the active application. A window can be
/// key while its app is inactive, and keys still go elsewhere.
pub fn is_active() -> bool {
    unsafe {
        let is_active: SendBool = std::mem::transmute(objc_msgSend as *const ());
        is_active(shared_application(), selector(c"isActive"))
    }
}

/// Whether the overlay is the key window, the one macOS sends keys to.
pub fn is_key_window() -> bool {
    unsafe {
        let key = send()(shared_application(), selector(c"keyWindow"));
        !key.is_null() && key == only_window()
    }
}

/// Turns the overlay window borderless, transparent to capture, and top level.
pub fn make_borderless() {
    unsafe {
        let window = only_window();

        // NSWindowStyleMaskBorderless is zero: no titlebar, no traffic lights,
        // no rounded corners, and the content view matches the window frame.
        let set_style: SendUnsigned = std::mem::transmute(objc_msgSend as *const ());
        set_style(window, selector(c"setStyleMask:"), 0);

        let set_unsigned: SendUnsigned = std::mem::transmute(objc_msgSend as *const ());

        // NSStatusWindowLevel: above the menu bar, so the overlay is genuinely
        // in front of everything rather than floating under it.
        set_unsigned(window, selector(c"setLevel:"), 25);

        // NSWindowSharingNone: excluded from screen capture, so the recorder
        // never films its own overlay and the preview cannot recurse into
        // itself. REWIND_OVERLAY_CAPTURABLE=1 lifts that for screenshots while
        // debugging the overlay itself.
        if std::env::var_os("REWIND_OVERLAY_CAPTURABLE").is_none() {
            set_unsigned(window, selector(c"setSharingType:"), 0);
        }

    }
}

/// Places the overlay over one display. Coordinates are AppKit's: origin at
/// the bottom-left of the primary screen, which is what NSScreen reports.
pub fn place(x: f64, y: f64, width: f64, height: f64) {
    unsafe {
        let window = only_window();
        let set_pair: SendPair = std::mem::transmute(objc_msgSend as *const ());
        set_pair(window, selector(c"setContentSize:"), width, height);
        set_pair(window, selector(c"setFrameOrigin:"), x, y);
    }
}

/// Cross-fades the window in. AppKit animates `alphaValue` in the window
/// server, so this costs no frames in our render loop — driving it from a step
/// task inside GPUI meant the overlay was painted once and then left at
/// whatever opacity that first frame had.
pub fn start_fade() {
    unsafe {
        let window = only_window();
        let set_double: SendDouble = std::mem::transmute(objc_msgSend as *const ());

        let context_class = objc_getClass(c"NSAnimationContext".as_ptr());
        assert!(!context_class.is_null(), "NSAnimationContext is not linked");
        let send: SendObject = std::mem::transmute(objc_msgSend as *const ());

        send(context_class, selector(c"beginGrouping"));
        let context = send(context_class, selector(c"currentContext"));
        set_double(context, selector(c"setDuration:"), 0.11);

        let proxy = send(window, selector(c"animator"));
        set_double(proxy, selector(c"setAlphaValue:"), 1.0);

        send(context_class, selector(c"endGrouping"));
    }
}

/// Starts the window fully transparent so `start_fade` has somewhere to go.
pub fn prepare_fade() {
    unsafe {
        let set_double: SendDouble = std::mem::transmute(objc_msgSend as *const ());
        set_double(only_window(), selector(c"setAlphaValue:"), 0.0);
    }
}
