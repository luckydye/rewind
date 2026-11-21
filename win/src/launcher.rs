//! Owns the overlay process and the hotkey hand-off, like `UILauncher.swift`.
//!
//! The overlay is preloaded hidden and waits on a named event; signalling it
//! is the whole "show" request. Only if nobody holds the event (never started,
//! or it died) is a process spawned.

use std::os::windows::process::CommandExt;
use std::path::PathBuf;

use windows::Win32::Foundation::CloseHandle;
use windows::Win32::System::Threading::{CREATE_NO_WINDOW, EVENT_MODIFY_STATE, OpenEventW, SetEvent};
use windows::Win32::UI::WindowsAndMessaging::{ASFW_ANY, AllowSetForegroundWindow};
use windows::core::HSTRING;

use crate::paths;

#[derive(Default)]
pub struct Launcher;

impl Launcher {
    /// Runs at daemon start. Harmless when an overlay is already resident.
    pub fn preload(&self) {
        if !overlay_running() {
            spawn("--resident");
        }
    }

    pub fn open(&self) {
        // Windows only lets the foreground process hand focus on. The daemon
        // is that process right after the hotkey or a tray click, so it passes
        // the right along before waking the overlay.
        unsafe {
            let _ = AllowSetForegroundWindow(ASFW_ANY);
        }
        if !poke() {
            spawn("--show");
        }
    }
}

fn open_event() -> Option<windows::Win32::Foundation::HANDLE> {
    unsafe { OpenEventW(EVENT_MODIFY_STATE, false, &HSTRING::from(paths::overlay_event_name())).ok() }
}

fn overlay_running() -> bool {
    match open_event() {
        Some(event) => {
            unsafe {
                let _ = CloseHandle(event);
            }
            true
        }
        None => false,
    }
}

fn poke() -> bool {
    let Some(event) = open_event() else { return false };
    unsafe {
        let ok = SetEvent(event).is_ok();
        let _ = CloseHandle(event);
        ok
    }
}

/// Next to the daemon in an install; in a development tree, the UI crate's
/// own target directory with the same profile.
fn ui_executable() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("REWIND_UI") {
        return Some(PathBuf::from(path));
    }
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    let beside = dir.join("rewind-ui.exe");
    if beside.exists() {
        return Some(beside);
    }
    let profile = dir.file_name()?;
    let development = dir.parent()?.parent()?.parent()?.join("ui").join("target").join(profile).join("rewind-ui.exe");
    development.exists().then_some(development)
}

fn spawn(argument: &str) {
    let Some(path) = ui_executable() else {
        eprintln!("rewindd: cannot find rewind-ui.exe next to rewindd or in ui\\target");
        return;
    };
    // rewind-ui is a console program (its CLI prints); started from the tray
    // it must not open a console window.
    if let Err(error) = std::process::Command::new(&path).arg(argument).creation_flags(CREATE_NO_WINDOW.0).spawn() {
        eprintln!("rewindd: cannot launch {}: {error}", path.display());
    }
}
