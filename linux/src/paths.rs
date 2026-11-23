//! Where things live in a Wayland session, and the names the daemon and the
//! overlay meet on.
//!
//! `$XDG_RUNTIME_DIR` is per-user and already mode 0700, which is where a
//! socket belongs; the config follows `$XDG_CONFIG_HOME` and clips land in
//! `~/Videos/Rewind`, the same idea as `~/Movies/Rewind` on macOS.

use std::path::PathBuf;

fn home() -> PathBuf {
    PathBuf::from(std::env::var_os("HOME").expect("HOME is unset"))
}

fn config_home() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).unwrap_or_else(|| home().join(".config"))
}

/// `$XDG_RUNTIME_DIR/rewind`: the sockets. A Wayland session always sets it.
pub fn runtime_dir() -> PathBuf {
    let runtime = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .expect("XDG_RUNTIME_DIR is unset: rewindd needs a Wayland session");
    runtime.join("rewind")
}

/// `$XDG_CONFIG_HOME/rewind`: the persisted configuration and portal token.
pub fn config_dir() -> PathBuf {
    config_home().join("rewind")
}

/// `~/Videos/Rewind`, the Wayland twin of `~/Movies/Rewind`.
pub fn clips_dir() -> PathBuf {
    home().join("Videos").join("Rewind")
}

/// `$XDG_CACHE_HOME/rewind/clipboard`: clips that exist only to be pasted.
pub fn clipboard_dir() -> PathBuf {
    std::env::var_os("XDG_CACHE_HOME").map(PathBuf::from).unwrap_or_else(|| home().join(".cache")).join("rewind").join("clipboard")
}

pub fn socket_path() -> PathBuf {
    runtime_dir().join("rewindd.sock")
}

/// One byte on it asks the resident overlay to show itself, like macOS.
pub fn ui_socket_path() -> PathBuf {
    runtime_dir().join("ui.sock")
}

/// `~/.config/autostart/rewind.desktop`: the Start at Login entry.
pub fn autostart_file() -> PathBuf {
    config_home().join("autostart").join("rewind.desktop")
}

/// `Rewind 2026-11-23 14.03.22.mp4`, same as the other platforms.
pub fn timestamped_clip_name(extension: &str) -> String {
    let now = unsafe { libc::time(std::ptr::null_mut()) };
    let mut time = unsafe { std::mem::zeroed::<libc::tm>() };
    // localtime_r cannot fail for a value from time(2).
    unsafe { libc::localtime_r(&now, &mut time) };
    format!(
        "Rewind {:04}-{:02}-{:02} {:02}.{:02}.{:02}.{extension}",
        time.tm_year + 1900,
        time.tm_mon + 1,
        time.tm_mday,
        time.tm_hour,
        time.tm_min,
        time.tm_sec
    )
}
