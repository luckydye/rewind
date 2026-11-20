//! Bring the overlay to the front.
//!
//! GPUI 0.2.2 calls `-[NSApplication activateIgnoringOtherApps:]`, which has
//! been a no-op since macOS 14. The replacement is the argument-less
//! `-[NSApplication activate]`, which GPUI does not expose, so it is sent
//! directly. Without this the overlay draws above everything but every
//! keystroke still goes to the app that was frontmost before.

#[cfg(target_os = "macos")]
pub fn activate() {
    use std::ffi::c_void;

    #[link(name = "AppKit", kind = "framework")]
    unsafe extern "C" {
        fn objc_getClass(name: *const std::os::raw::c_char) -> *mut c_void;
        fn sel_registerName(name: *const std::os::raw::c_char) -> *mut c_void;
        fn objc_msgSend(receiver: *mut c_void, selector: *mut c_void) -> *mut c_void;
    }

    unsafe {
        let class = objc_getClass(c"NSApplication".as_ptr());
        assert!(!class.is_null(), "NSApplication is not linked");
        let shared = objc_msgSend(class, sel_registerName(c"sharedApplication".as_ptr()));
        assert!(!shared.is_null(), "NSApplication.sharedApplication was null");

        let activate = sel_registerName(c"activate".as_ptr());
        objc_msgSend(shared, activate);
    }
}
