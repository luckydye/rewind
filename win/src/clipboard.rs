//! Copy Frame puts a DIB on the clipboard; Copy Clip puts the MP4 there as a
//! file (CF_HDROP), which is what Explorer, Slack, Discord and Teams paste as
//! the video itself — the Windows twin of an NSURL on the pasteboard.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use windows::Win32::Foundation::{GlobalFree, HANDLE, HGLOBAL, HWND};
use windows::Win32::Graphics::Gdi::{BI_RGB, BITMAPINFOHEADER};
use windows::Win32::System::DataExchange::{CloseClipboard, EmptyClipboard, OpenClipboard, RegisterClipboardFormatW, SetClipboardData};
use windows::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock};
use windows::Win32::System::Ole::{CF_DIB, CF_HDROP, DROPEFFECT_COPY};
use windows::Win32::UI::Shell::DROPFILES;
use windows::core::w;

use crate::paths;
use crate::protocol::RawImage;

/// Owner window for the clipboard; set by the tray. Without an owner,
/// `EmptyClipboard` leaves the clipboard unowned and `SetClipboardData` fails.
static OWNER: std::sync::atomic::AtomicIsize = std::sync::atomic::AtomicIsize::new(0);

pub fn set_owner(hwnd: HWND) {
    OWNER.store(hwnd.0 as isize, std::sync::atomic::Ordering::Relaxed);
}

struct Open;

impl Open {
    /// Another app may hold the clipboard for a moment; retry briefly.
    fn new() -> Result<Self, String> {
        let owner = HWND(OWNER.load(std::sync::atomic::Ordering::Relaxed) as *mut _);
        for _ in 0..20 {
            if unsafe { OpenClipboard(Some(owner)) }.is_ok() {
                unsafe { EmptyClipboard().map_err(|e| e.message())? };
                return Ok(Open);
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        Err("the clipboard is busy".into())
    }

    fn put(&self, format: u32, bytes: &[u8]) -> Result<(), String> {
        unsafe {
            let global = GlobalAlloc(GMEM_MOVEABLE, bytes.len()).map_err(|e| e.message())?;
            let target = GlobalLock(global) as *mut u8;
            if target.is_null() {
                let _ = GlobalFree(Some(global));
                return Err("cannot lock clipboard memory".into());
            }
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), target, bytes.len());
            let _ = GlobalUnlock(global);
            // On success the clipboard owns the memory.
            if let Err(error) = SetClipboardData(format, Some(HANDLE(global.0))) {
                let _ = GlobalFree(Some(HGLOBAL(global.0)));
                return Err(error.message());
            }
        }
        Ok(())
    }
}

impl Drop for Open {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseClipboard();
        }
    }
}

/// Bottom-up 32-bit DIB: top-down DIBs are valid but a surprising number of
/// apps paste them upside down.
pub fn copy_image(image: &RawImage) -> Result<(), String> {
    let (width, height) = (image.width as usize, image.height as usize);
    let header = BITMAPINFOHEADER {
        biSize: size_of::<BITMAPINFOHEADER>() as u32,
        biWidth: width as i32,
        biHeight: height as i32,
        biPlanes: 1,
        biBitCount: 32,
        biCompression: BI_RGB.0,
        biSizeImage: (width * height * 4) as u32,
        ..Default::default()
    };
    let mut bytes = Vec::with_capacity(size_of::<BITMAPINFOHEADER>() + width * height * 4);
    bytes.extend_from_slice(unsafe {
        std::slice::from_raw_parts(&header as *const _ as *const u8, size_of::<BITMAPINFOHEADER>())
    });
    for row in image.pixels.chunks_exact(width * 4).rev() {
        bytes.extend_from_slice(row);
    }

    let clipboard = Open::new()?;
    clipboard.put(CF_DIB.0 as u32, &bytes)
}

pub fn copy_file(path: &Path) -> Result<(), String> {
    let mut bytes = Vec::new();
    let header = DROPFILES {
        pFiles: size_of::<DROPFILES>() as u32,
        fWide: true.into(),
        ..Default::default()
    };
    bytes.extend_from_slice(unsafe {
        std::slice::from_raw_parts(&header as *const _ as *const u8, size_of::<DROPFILES>())
    });
    // One UTF-16 path, then the double terminator that ends the list.
    for unit in path.as_os_str().to_string_lossy().encode_utf16().chain([0, 0]) {
        bytes.extend_from_slice(&unit.to_le_bytes());
    }

    let clipboard = Open::new()?;
    clipboard.put(CF_HDROP.0 as u32, &bytes)?;
    // Tells Explorer a paste is a copy, not a move out of our cache.
    let effect = unsafe { RegisterClipboardFormatW(w!("Preferred DropEffect")) };
    clipboard.put(effect, &DROPEFFECT_COPY.0.to_le_bytes())
}

/// Copied clips live outside `Videos\Rewind`: they exist only to be pasted.
/// The file has to outlive the copy for the paste to work, so copies older
/// than a day are removed here rather than right away.
pub fn new_cached_clip_path() -> PathBuf {
    let dir = paths::clipboard_dir();
    let _ = std::fs::create_dir_all(&dir);
    let cutoff = SystemTime::now() - Duration::from_secs(24 * 60 * 60);
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let old = entry.metadata().and_then(|m| m.modified()).is_ok_and(|t| t < cutoff);
            if old && entry.path().extension().is_some_and(|e| e == "mp4") {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
    dir.join(paths::timestamped_clip_name())
}
