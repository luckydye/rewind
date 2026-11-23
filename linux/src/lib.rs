//! Rewind for Wayland: the daemon half. Same pipeline and the same wire
//! protocol as the macOS `rewindd` and the Windows build, so `rewind-ui` talks
//! to any of them.
//!
//! Wayland has no screen-capture API. Capture starts at
//! `org.freedesktop.portal.ScreenCast`, which hands back one PipeWire node and
//! an fd; `pipewiresrc` consumes it and the pipeline encodes straight to
//! H.264. There is no OS media framework to lean on, so GStreamer is the
//! encoder, the video processor and the decoder.

#![cfg(target_os = "linux")]

pub mod annotation;
pub mod capture;
pub mod cli;
pub mod client;
pub mod clipboard;
pub mod codec;
pub mod config_store;
pub mod daemon;
pub mod h264;
pub mod image_file;
pub mod mp4;
pub mod paths;
pub mod portal;
pub mod preview;
pub mod protocol;
pub mod recorder;
pub mod ring;
pub mod router;
pub mod server;
pub mod transcode;

/// Once per process, before anything else.
pub fn initialize() {
    gstreamer::init().expect("cannot initialize GStreamer");
}
