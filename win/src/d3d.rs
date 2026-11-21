//! D3D11 device shared by capture, colour conversion and the encoder, and the
//! monitor list.

use windows::Graphics::DirectX::Direct3D11::IDirect3DDevice;
use windows::Win32::Foundation::{HMODULE, LPARAM, RECT};
use windows::Win32::Graphics::Direct3D::{
    D3D_DRIVER_TYPE_HARDWARE, D3D_FEATURE_LEVEL_11_0, D3D_FEATURE_LEVEL_11_1,
};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_CREATE_DEVICE_VIDEO_SUPPORT, D3D11_SDK_VERSION,
    D3D11CreateDevice, ID3D11Device, ID3D11Multithread,
};
use windows::Win32::Graphics::Dxgi::IDXGIDevice;
use windows::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO, MONITORINFOEXW,
};
use windows::Win32::System::WinRT::Direct3D11::CreateDirect3D11DeviceFromDXGIDevice;
use windows::core::{BOOL, Interface, Result};

/// COM pointers that are safe to move across threads here: the device is put
/// into multithread-protected mode, and everything else is touched by one
/// thread at a time behind a mutex or a channel.
pub struct Shared<T>(pub T);
unsafe impl<T> Send for Shared<T> {}
unsafe impl<T> Sync for Shared<T> {}

pub fn create_device() -> Result<ID3D11Device> {
    let mut device = None;
    unsafe {
        D3D11CreateDevice(
            None,
            D3D_DRIVER_TYPE_HARDWARE,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT | D3D11_CREATE_DEVICE_VIDEO_SUPPORT,
            Some(&[D3D_FEATURE_LEVEL_11_1, D3D_FEATURE_LEVEL_11_0]),
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            None,
        )?;
    }
    let device = device.unwrap();
    // Capture callbacks, the video processor and the encoder's own threads all
    // use the immediate context.
    let multithread: ID3D11Multithread = device.cast()?;
    unsafe {
        let _ = multithread.SetMultithreadProtected(true);
    }
    Ok(device)
}

pub fn winrt_device(device: &ID3D11Device) -> Result<IDirect3DDevice> {
    let dxgi: IDXGIDevice = device.cast()?;
    let inspectable = unsafe { CreateDirect3D11DeviceFromDXGIDevice(&dxgi)? };
    inspectable.cast()
}

pub struct Monitor {
    pub handle: HMONITOR,
    pub name: String,
    pub rect: RECT,
    pub primary: bool,
}

pub fn monitors() -> Vec<Monitor> {
    unsafe extern "system" fn collect(handle: HMONITOR, _: HDC, _: *mut RECT, data: LPARAM) -> BOOL {
        let list = unsafe { &mut *(data.0 as *mut Vec<HMONITOR>) };
        list.push(handle);
        true.into()
    }

    let mut handles: Vec<HMONITOR> = Vec::new();
    unsafe {
        let _ = EnumDisplayMonitors(None, None, Some(collect), LPARAM(&mut handles as *mut _ as isize));
    }

    let mut monitors: Vec<Monitor> = handles
        .into_iter()
        .filter_map(|handle| {
            let mut info = MONITORINFOEXW::default();
            info.monitorInfo.cbSize = size_of::<MONITORINFOEXW>() as u32;
            let ok = unsafe { GetMonitorInfoW(handle, &mut info as *mut _ as *mut MONITORINFO) };
            if !ok.as_bool() {
                return None;
            }
            let len = info.szDevice.iter().position(|&c| c == 0).unwrap_or(info.szDevice.len());
            Some(Monitor {
                handle,
                name: String::from_utf16_lossy(&info.szDevice[..len]),
                rect: info.monitorInfo.rcMonitor,
                primary: info.monitorInfo.dwFlags & 1 != 0,
            })
        })
        .collect();
    // Primary first, so index 0 means the same thing as on macOS.
    monitors.sort_by_key(|m| !m.primary);
    monitors
}
