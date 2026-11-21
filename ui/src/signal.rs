//! How the daemon wakes the resident overlay.
//!
//! macOS: one byte on `~/Library/Application Support/Rewind/ui.sock`.
//! Windows: an auto-reset named event. The kernel object lives exactly as long
//! as this process holds it, so the daemon's `OpenEvent` failing is the same
//! signal as a refused socket connection: nothing is resident, spawn one.

#[cfg(unix)]
pub struct Listener(std::os::unix::net::UnixListener);

#[cfg(unix)]
impl Listener {
    /// `None` when another overlay already answers. A stale socket file from
    /// a killed overlay would block the bind forever, so it is removed unless
    /// something actually answers on it.
    pub fn bind() -> Option<Self> {
        use std::os::unix::net::{UnixListener, UnixStream};
        let home = std::env::var("HOME").expect("HOME is unset");
        let path = std::path::PathBuf::from(home).join("Library/Application Support/Rewind/ui.sock");
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if path.exists() {
            match UnixStream::connect(&path) {
                Ok(_) => return None,
                Err(_) => {
                    let _ = std::fs::remove_file(&path);
                }
            }
        }
        UnixListener::bind(&path).ok().map(Listener)
    }

    /// Blocks until the daemon asks for the overlay. `false` ends the wait.
    pub fn accept(&self) -> bool {
        self.0.accept().is_ok()
    }
}

#[cfg(windows)]
pub struct Listener(windows::Win32::Foundation::HANDLE);

// A kernel event handle may be waited on from any thread.
#[cfg(windows)]
unsafe impl Send for Listener {}
#[cfg(windows)]
unsafe impl Sync for Listener {}

#[cfg(windows)]
pub fn event_name() -> String {
    // Must match `paths::overlay_event_name` in the Windows daemon.
    let user: String = std::env::var("USERNAME")
        .unwrap_or_else(|_| "user".into())
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    format!(r"Local\Rewind.Overlay.{user}")
}

#[cfg(windows)]
impl Listener {
    pub fn bind() -> Option<Self> {
        use windows::Win32::Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, GetLastError};
        use windows::Win32::System::Threading::CreateEventW;
        use windows::core::HSTRING;

        let name = HSTRING::from(event_name());
        let event = unsafe { CreateEventW(None, false, false, &name).ok()? };
        if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
            unsafe {
                let _ = CloseHandle(event);
            }
            return None;
        }
        Some(Listener(event))
    }

    pub fn accept(&self) -> bool {
        use windows::Win32::Foundation::WAIT_OBJECT_0;
        use windows::Win32::System::Threading::{INFINITE, WaitForSingleObject};
        unsafe { WaitForSingleObject(self.0, INFINITE) == WAIT_OBJECT_0 }
    }
}
