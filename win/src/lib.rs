//! Rewind for Windows: the daemon half. Same pipeline and the same wire
//! protocol as the macOS `rewindd`, so `rewind-ui` talks to either.

#![cfg(windows)]

pub mod annotation;
pub mod capture;
pub mod cli;
pub mod clipboard;
pub mod config_store;
pub mod convert;
pub mod d3d;
pub mod daemon;
pub mod encoder;
pub mod h264;
pub mod launcher;
pub mod login;
pub mod mp4;
pub mod paths;
pub mod pipeline;
pub mod preview;
pub mod protocol;
pub mod recorder;
pub mod ring;
pub mod router;
pub mod server;
pub mod transcode;
pub mod tray;

use windows::Win32::Media::MediaFoundation::{MF_VERSION, MFSTARTUP_FULL, MFStartup};
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx};
use windows::Win32::UI::HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext};

/// Once per process, before anything else. Per-monitor DPI awareness is the
/// Windows version of the Retina lesson in the README: without it, monitor
/// rects come back scaled and the overlay is placed in the wrong pixels.
pub fn initialize() {
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        MFStartup(MF_VERSION, MFSTARTUP_FULL).expect("MFStartup");
    }
}
