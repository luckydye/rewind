//! `Rewind.exe`: the same daemon in the GUI subsystem, so Start at Login and
//! a double-click open no console window. The bundle executable on macOS is
//! named `Rewind` for the same reason.

#![windows_subsystem = "windows"]

use windows::Win32::UI::WindowsAndMessaging::{MB_ICONWARNING, MB_OK, MessageBoxW};
use windows::core::{HSTRING, w};

fn main() {
    rewindd::initialize();
    if let Err(message) = rewindd::daemon::run() {
        // No console to print to; "already running" is the common case and
        // is not worth a dialog.
        if !message.contains("already running") {
            unsafe {
                MessageBoxW(None, &HSTRING::from(format!("Rewind could not start: {message}")), w!("Rewind"), MB_OK | MB_ICONWARNING);
            }
        }
        std::process::exit(1);
    }
}
