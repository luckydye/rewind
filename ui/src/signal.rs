//! How the daemon wakes the resident overlay.
//!
//! macOS: one byte on `~/Library/Application Support/Rewind/ui.sock`.
//! Windows: an auto-reset named event. The kernel object lives exactly as long
//! as this process holds it, so the daemon's `OpenEvent` failing is the same
//! signal as a refused socket connection: nothing is resident, spawn one.
//! Wayland: the same socket under `$XDG_RUNTIME_DIR/rewind`, though the
//! overlay there is spawned per use rather than kept resident.

#[cfg(unix)]
pub struct Listener(std::os::unix::net::UnixListener);

#[cfg(target_os = "macos")]
fn socket_path() -> std::path::PathBuf {
    let home = std::env::var("HOME").expect("HOME is unset");
    std::path::PathBuf::from(home).join("Library/Application Support/Rewind/ui.sock")
}

/// Must match `paths::ui_socket_path` in the Wayland daemon.
#[cfg(target_os = "linux")]
fn socket_path() -> std::path::PathBuf {
    let runtime = std::env::var("XDG_RUNTIME_DIR").expect("XDG_RUNTIME_DIR is unset");
    std::path::PathBuf::from(runtime).join("rewind").join("ui.sock")
}

#[cfg(unix)]
impl Listener {
    /// `None` when another overlay already answers. A stale socket file from
    /// a killed overlay would block the bind forever, so it is removed unless
    /// something actually answers on it.
    pub fn bind() -> Option<Self> {
        use std::os::unix::fs::PermissionsExt;
        use std::os::unix::net::{UnixListener, UnixStream};
        let path = socket_path();
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
            let _ = std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700));
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

/// Whether the overlay is still on screen. The heartbeat refreshes the lease
/// only while this is true, so the toast that follows a copy is recorded.
#[cfg(target_os = "linux")]
static PAUSING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Keeps the daemon's pause lease alive while the overlay is on screen.
///
/// Wayland offers no capture exclusion, so the daemon must stop appending
/// while the overlay is visible or it would record itself. There is no
/// window-unmap callback to hang that on, and a client that dies must not
/// pause capture forever, hence a lease: the daemon resumes 15 s after the
/// last request, and this thread refreshes it every 5 s.
#[cfg(target_os = "linux")]
pub fn hold_capture_pause() {
    use std::sync::atomic::Ordering;
    PAUSING.store(true, Ordering::SeqCst);
    std::thread::spawn(|| loop {
        if PAUSING.load(Ordering::SeqCst) {
            let _ = crate::ipc::call(
                &crate::ipc::Client::new(),
                crate::protocol::Request::new(crate::protocol::Command::PauseCapture),
            );
        }
        std::thread::sleep(std::time::Duration::from_secs(5));
    });
}

/// The overlay is going away: let the daemon record the toast and anything
/// after it without waiting for the lease to lapse.
#[cfg(target_os = "linux")]
pub fn stop_pausing() {
    PAUSING.store(false, std::sync::atomic::Ordering::SeqCst);
    let _ = crate::ipc::call(
        &crate::ipc::Client::new(),
        crate::protocol::Request::new(crate::protocol::Command::ResumeCapture),
    );
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
