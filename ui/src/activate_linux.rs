//! Window setup the GPUI API cannot express — Wayland edition of
//! `activate.rs`, same function names so `app.rs` stays platform-neutral.
//!
//! Wayland gives clients much less than AppKit or Win32:
//!
//! * A client cannot place a window. The overlay covers the output by asking
//!   for fullscreen, which is also what keeps it above ordinary windows.
//! * There is no "toast window". The toast is drawn inside the same
//!   fullscreen surface; the rest of it is transparent.
//! * GPUI exposes no unmap, so the overlay is not resident: `app.rs` exits the
//!   process when it closes and the daemon spawns a fresh one with `--show`.
//! * There is no capture exclusion. The daemon stops appending frames while
//!   `signal::hold_capture_pause` keeps its pause lease alive, so the overlay
//!   never lands in the ring.

/// Enter fullscreen once the window exists. A Wayland client cannot ask for a
/// size or position, and a borderless window the size of the display would
/// still be placed wherever the compositor likes.
pub fn remember(window: &gpui::Window) {
    window.toggle_fullscreen();
    window.activate_window();
}

/// No application-level accessory policy on Wayland; the window is the app.
pub fn make_accessory() {}

/// `WindowOptions` already asks for no decorations and a transparent
/// background, and fullscreen hides whatever the compositor adds.
pub fn make_borderless() {}

/// The one stream the portal captures; the overlay is always fullscreen, so
/// there is nothing to place. Wayland would ignore the request anyway.
pub fn place(_x: f64, _y: f64, _width: f64, _height: f64) {}

/// The toast is a view inside the fullscreen surface, not a window of its own.
pub fn show_toast(_screen: Option<(f64, f64, f64, f64)>, _width: f64, _height: f64, _top: f64, _scale: f64) {}

pub fn end_toast() {}

/// GPUI's Wayland surface has no window-level alpha animation; the content
/// fades through `exit_progress` instead.
pub fn start_fade() {}

pub fn prepare_fade() {}

/// The window is activated when it is created, and Wayland has no
/// `activateIgnoringOtherApps`.
pub fn activate() {}

/// A window that cannot check this would make `app.rs` retry focus forever.
pub fn is_active() -> bool {
    true
}

pub fn is_key_window() -> bool {
    true
}

/// `app.rs` exits the process at the end of a hide on Linux; hiding a Wayland
/// window is not something GPUI offers. The pause is released just before that
/// exit so the toast, which is still on screen, is not recorded.
pub fn hide() {}

/// Nothing is cloaked while hidden on Wayland.
pub fn take_reveal() -> bool {
    false
}

pub fn reveal() {}
