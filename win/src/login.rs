//! Start at login: a value under `HKCU\...\Run`, the per-user equivalent of
//! `SMAppService.mainApp`. It points at `Rewind.exe`, the GUI-subsystem build
//! of the daemon, so login does not flash a console window.

use std::path::PathBuf;

use windows::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_SET_VALUE, REG_SZ, RRF_RT_REG_SZ, RegCloseKey, RegDeleteValueW, RegGetValueW, RegOpenKeyExW,
    RegSetValueExW,
};
use windows::core::w;

const RUN_KEY: windows::core::PCWSTR = w!(r"Software\Microsoft\Windows\CurrentVersion\Run");
const VALUE: windows::core::PCWSTR = w!("Rewind");

/// `Rewind.exe` beside the running binary, or the running binary itself.
fn target() -> Result<PathBuf, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let gui = exe.with_file_name("Rewind.exe");
    Ok(if gui.exists() { gui } else { exe })
}

pub fn is_enabled() -> bool {
    let mut size = 0u32;
    unsafe { RegGetValueW(HKEY_CURRENT_USER, RUN_KEY, VALUE, RRF_RT_REG_SZ, None, None, Some(&mut size)).is_ok() }
}

pub fn set(enabled: bool) -> Result<(), String> {
    unsafe {
        let mut key = HKEY::default();
        RegOpenKeyExW(HKEY_CURRENT_USER, RUN_KEY, None, KEY_SET_VALUE, &mut key).ok().map_err(|e| e.message())?;
        let result = if enabled {
            let target = target()?;
            let mut command = format!("\"{}\"", target.display());
            if target.file_name().is_some_and(|n| n.eq_ignore_ascii_case("rewindd.exe")) {
                command.push_str(" daemon");
            }
            let wide: Vec<u16> = command.encode_utf16().chain([0]).collect();
            let bytes = std::slice::from_raw_parts(wide.as_ptr() as *const u8, wide.len() * 2);
            RegSetValueExW(key, VALUE, None, REG_SZ, Some(bytes)).ok()
        } else {
            match RegDeleteValueW(key, VALUE).ok() {
                Err(e) if e.code() == windows::Win32::Foundation::ERROR_FILE_NOT_FOUND.to_hresult() => Ok(()),
                other => other,
            }
        };
        let _ = RegCloseKey(key);
        result.map_err(|e| e.message())
    }
}
