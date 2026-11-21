//! Where things live on Windows, and the names of the kernel objects the
//! daemon and the overlay meet on.

use std::path::PathBuf;

use windows::Win32::System::SystemInformation::GetLocalTime;
use windows::Win32::UI::Shell::{FOLDERID_LocalAppData, FOLDERID_RoamingAppData, FOLDERID_Videos, KF_FLAG_DEFAULT, SHGetKnownFolderPath};
use windows::core::GUID;

fn known_folder(id: &GUID) -> PathBuf {
    unsafe {
        match SHGetKnownFolderPath(id, KF_FLAG_DEFAULT, None) {
            Ok(path) => {
                let result = PathBuf::from(path.to_string().unwrap_or_default());
                windows::Win32::System::Com::CoTaskMemFree(Some(path.0 as *const _));
                result
            }
            Err(_) => std::env::temp_dir(),
        }
    }
}

/// `%APPDATA%\Rewind`: the persisted configuration.
pub fn config_dir() -> PathBuf {
    known_folder(&FOLDERID_RoamingAppData).join("Rewind")
}

/// `%USERPROFILE%\Videos\Rewind`: exported clips, the Windows twin of
/// `~/Movies/Rewind`.
pub fn clips_dir() -> PathBuf {
    known_folder(&FOLDERID_Videos).join("Rewind")
}

/// `%LOCALAPPDATA%\Rewind\Clipboard`: clips that exist only to be pasted.
pub fn clipboard_dir() -> PathBuf {
    known_folder(&FOLDERID_LocalAppData).join("Rewind").join("Clipboard")
}

/// Pipes and named events are machine-wide, so both carry the user name: two
/// people signed in at once must not reach each other's recorder.
fn user_suffix() -> String {
    std::env::var("USERNAME")
        .unwrap_or_else(|_| "user".into())
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect()
}

pub fn pipe_name() -> String {
    format!(r"\\.\pipe\rewind-{}-rewindd", user_suffix())
}

/// Auto-reset event the resident overlay waits on; signalling it is the whole
/// "show" request, like the one byte on `ui.sock` on macOS.
pub fn overlay_event_name() -> String {
    format!(r"Local\Rewind.Overlay.{}", user_suffix())
}

/// `Rewind 2026-11-21 14.03.22.mp4`, same as the macOS daemon.
pub fn timestamped_clip_name(extension: &str) -> String {
    let t = unsafe { GetLocalTime() };
    format!(
        "Rewind {:04}-{:02}-{:02} {:02}.{:02}.{:02}.{extension}",
        t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond
    )
}
