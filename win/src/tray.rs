//! Notification-area icon, global hotkey and the daemon's message loop: the
//! Windows twin of `MenuController.swift` and `HotKey.swift`.
//!
//! Left click opens the overlay, right click opens the menu. The hotkey is
//! Alt+Shift+S: Ctrl+Shift+S is Save As in most apps, Win+Shift+S is the
//! Snipping Tool, and Ctrl+Alt is AltGr on many European layouts.

use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU32, Ordering};
use std::sync::{Arc, OnceLock};

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateBitmap, CreateDIBSection, DIB_RGB_COLORS, DeleteObject, HGDIOBJ,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueW};
use windows::Win32::UI::Input::KeyboardAndMouse::{MOD_ALT, MOD_NOREPEAT, MOD_SHIFT, RegisterHotKey, UnregisterHotKey};
use windows::Win32::UI::Shell::{
    NIF_ICON, NIF_MESSAGE, NIF_SHOWTIP, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY, NIM_SETVERSION, NIN_SELECT,
    NOTIFYICON_VERSION_4, NOTIFYICONDATAW, Shell_NotifyIconW,
};

/// `NIN_SELECT | NINF_KEY`: the icon activated from the keyboard.
const NIN_KEYSELECT: u32 = NIN_SELECT | 0x1;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{HSTRING, PCWSTR, w};

use crate::clipboard;
use crate::launcher::Launcher;
use crate::login;
use crate::paths;
use crate::recorder::{self, Recorder};

pub const HOTKEY_LABEL: &str = "Alt+Shift+S";

const WM_TRAY: u32 = WM_APP + 1;
const HOTKEY_ID: i32 = 1;
const TIMER_ID: usize = 1;

const ID_TOGGLE: usize = 10;
const ID_OPEN: usize = 11;
const ID_CLIPS: usize = 12;
const ID_LOGIN: usize = 13;
const ID_QUIT: usize = 14;
const ID_DISPLAY: usize = 100;

struct Tray {
    recorder: Arc<Recorder>,
    launcher: Launcher,
}

static TRAY: OnceLock<Tray> = OnceLock::new();
static WINDOW: AtomicIsize = AtomicIsize::new(0);
static TASKBAR_CREATED: AtomicU32 = AtomicU32::new(0);
static SHOWS_RECORDING: AtomicBool = AtomicBool::new(false);

fn window() -> HWND {
    HWND(WINDOW.load(Ordering::Relaxed) as *mut _)
}

/// Ends the daemon from any thread: the `quit` command, Ctrl+C.
pub fn post_quit() {
    unsafe {
        let _ = PostMessageW(Some(window()), WM_CLOSE, WPARAM(0), LPARAM(0));
    }
}

pub fn run(recorder: Arc<Recorder>, launcher: Launcher) -> Result<(), String> {
    let _ = TRAY.set(Tray { recorder, launcher });
    unsafe {
        let instance = GetModuleHandleW(None).map_err(|e| e.message())?;
        let class = WNDCLASSW {
            lpfnWndProc: Some(window_proc),
            hInstance: instance.into(),
            lpszClassName: w!("RewindDaemon"),
            ..Default::default()
        };
        RegisterClassW(&class);
        // A hidden top-level window rather than a message-only one: only
        // top-level windows hear "TaskbarCreated" when Explorer restarts.
        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            w!("RewindDaemon"),
            w!("Rewind"),
            WS_OVERLAPPED,
            0,
            0,
            0,
            0,
            None,
            None,
            Some(instance.into()),
            None,
        )
        .map_err(|e| e.message())?;
        WINDOW.store(hwnd.0 as isize, Ordering::Relaxed);
        clipboard::set_owner(hwnd);
        TASKBAR_CREATED.store(RegisterWindowMessageW(w!("TaskbarCreated")), Ordering::Relaxed);

        add_icon(hwnd);
        if RegisterHotKey(Some(hwnd), HOTKEY_ID, MOD_ALT | MOD_SHIFT | MOD_NOREPEAT, 'S' as u32).is_err() {
            eprintln!("rewindd: {HOTKEY_LABEL} is taken by another app; running without a hotkey");
        }
        SetTimer(Some(hwnd), TIMER_ID, 1000, None);

        let mut message = MSG::default();
        while GetMessageW(&mut message, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
    Ok(())
}

fn tray() -> &'static Tray {
    TRAY.get().expect("tray context")
}

unsafe extern "system" fn window_proc(hwnd: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe {
        match message {
            WM_TRAY => {
                match (lparam.0 & 0xffff) as u32 {
                    WM_CONTEXTMENU => {
                        let x = (wparam.0 & 0xffff) as i16 as i32;
                        let y = ((wparam.0 >> 16) & 0xffff) as i16 as i32;
                        show_menu(hwnd, x, y);
                    }
                    NIN_SELECT | NIN_KEYSELECT => tray().launcher.open(),
                    _ => {}
                }
                LRESULT(0)
            }
            WM_HOTKEY => {
                tray().launcher.open();
                LRESULT(0)
            }
            WM_TIMER => {
                refresh(hwnd);
                LRESULT(0)
            }
            WM_CLOSE => {
                let _ = DestroyWindow(hwnd);
                LRESULT(0)
            }
            WM_DESTROY => {
                let _ = UnregisterHotKey(Some(hwnd), HOTKEY_ID);
                let data = icon_data(hwnd);
                let _ = Shell_NotifyIconW(NIM_DELETE, &data);
                PostQuitMessage(0);
                LRESULT(0)
            }
            _ if message == TASKBAR_CREATED.load(Ordering::Relaxed) && message != 0 => {
                add_icon(hwnd);
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, message, wparam, lparam),
        }
    }
}

fn icon_data(hwnd: HWND) -> NOTIFYICONDATAW {
    NOTIFYICONDATAW {
        cbSize: size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: 1,
        ..Default::default()
    }
}

fn set_tip(data: &mut NOTIFYICONDATAW, text: &str) {
    let units: Vec<u16> = text.encode_utf16().take(data.szTip.len() - 1).collect();
    data.szTip[..units.len()].copy_from_slice(&units);
    data.szTip[units.len()] = 0;
}

fn add_icon(hwnd: HWND) {
    let recording = tray().recorder.state().recording;
    let mut data = icon_data(hwnd);
    data.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP | NIF_SHOWTIP;
    data.uCallbackMessage = WM_TRAY;
    data.hIcon = draw_icon(recording);
    set_tip(&mut data, "Rewind");
    SHOWS_RECORDING.store(recording, Ordering::Relaxed);
    unsafe {
        if !Shell_NotifyIconW(NIM_ADD, &data).as_bool() {
            eprintln!("rewindd: the shell refused the notification icon");
        }
        data.Anonymous.uVersion = NOTIFYICON_VERSION_4;
        let _ = Shell_NotifyIconW(NIM_SETVERSION, &data);
        // The shell keeps its own copy.
        let _ = DestroyIcon(data.hIcon);
    }
}

fn status_line() -> String {
    let state = tray().recorder.state();
    if state.recording {
        format!(
            "Buffering {:.0}s of {}s · {}×{}@{}",
            state.available_seconds, state.buffer_seconds as i64, state.width, state.height, state.config.fps
        )
    } else if let Some(error) = state.last_error {
        format!("Capture stopped — {error}")
    } else {
        "Not recording".into()
    }
}

/// Once a second: tooltip, and the icon when recording starts or stops.
fn refresh(hwnd: HWND) {
    let state = tray().recorder.state();
    let mut data = icon_data(hwnd);
    data.uFlags = NIF_TIP | NIF_SHOWTIP;
    let tip = if state.recording {
        format!("Rewind — {:.0}s buffered", state.available_seconds)
    } else {
        "Rewind — not recording".into()
    };
    set_tip(&mut data, &tip);
    if SHOWS_RECORDING.swap(state.recording, Ordering::Relaxed) != state.recording {
        data.uFlags |= NIF_ICON;
        data.hIcon = draw_icon(state.recording);
    }
    unsafe {
        let _ = Shell_NotifyIconW(NIM_MODIFY, &data);
        if !data.hIcon.is_invalid() {
            let _ = DestroyIcon(data.hIcon);
        }
    }
}

fn append(menu: HMENU, flags: MENU_ITEM_FLAGS, id: usize, text: &str) {
    let text = HSTRING::from(text);
    unsafe {
        let _ = AppendMenuW(menu, flags, id, PCWSTR(text.as_ptr()));
    }
}

fn show_menu(hwnd: HWND, x: i32, y: i32) {
    let context = tray();
    let state = context.recorder.state();
    unsafe {
        let Ok(menu) = CreatePopupMenu() else { return };
        append(menu, MF_STRING | MF_GRAYED, 0, &status_line());
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
        append(menu, MF_STRING, ID_TOGGLE, if state.recording { "Stop Recording" } else { "Start Recording" });
        append(menu, MF_STRING, ID_OPEN, &format!("Open Rewind…\t{HOTKEY_LABEL}"));

        if let Ok(capture) = CreatePopupMenu() {
            append(capture, MF_STRING | MF_GRAYED, 0, "Entire Screen");
            for display in recorder::displays() {
                let checked = if state.config.display_index == display.index { MF_CHECKED } else { MF_UNCHECKED };
                append(capture, MF_STRING | checked, ID_DISPLAY + display.index as usize, &display.name());
            }
            append(menu, MF_POPUP, capture.0 as usize, "Capture");
        }
        append(menu, MF_STRING, ID_CLIPS, "Show Clips Folder");
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
        let login_flags = if login::is_enabled() { MF_CHECKED } else { MF_UNCHECKED };
        append(menu, MF_STRING | login_flags, ID_LOGIN, "Start at Login");
        append(menu, MF_STRING, ID_QUIT, "Quit Rewind");

        // Without this the menu does not close when clicking elsewhere.
        let _ = SetForegroundWindow(hwnd);
        let command = TrackPopupMenu(menu, TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_BOTTOMALIGN, x, y, None, hwnd, None).0 as usize;
        let _ = PostMessageW(Some(hwnd), WM_NULL, WPARAM(0), LPARAM(0));
        let _ = DestroyMenu(menu);

        match command {
            0 => {}
            ID_TOGGLE => {
                let recording = state.recording;
                // Starting builds the whole pipeline; keep the menu thread free.
                std::thread::spawn(move || {
                    let recorder = &tray().recorder;
                    if recording { recorder.stop() } else { recorder.start() };
                });
            }
            ID_OPEN => context.launcher.open(),
            ID_CLIPS => {
                let dir = paths::clips_dir();
                let _ = std::fs::create_dir_all(&dir);
                let _ = std::process::Command::new("explorer").arg(dir).spawn();
            }
            ID_LOGIN => {
                if let Err(error) = login::set(!login::is_enabled()) {
                    let text = HSTRING::from(format!("Could not change Start at Login: {error}"));
                    MessageBoxW(Some(hwnd), PCWSTR(text.as_ptr()), w!("Rewind"), MB_OK | MB_ICONWARNING);
                }
            }
            ID_QUIT => {
                let _ = DestroyWindow(hwnd);
            }
            id if id >= ID_DISPLAY => {
                let mut config = state.config.clone();
                config.display_index = (id - ID_DISPLAY) as i64;
                config.application_bundle_id = None;
                std::thread::spawn(move || {
                    tray().recorder.apply(config);
                });
            }
            _ => {}
        }
    }
}

fn light_taskbar() -> bool {
    let mut value = 0u32;
    let mut size = 4u32;
    unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            w!(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize"),
            w!("SystemUsesLightTheme"),
            RRF_RT_REG_DWORD,
            None,
            Some(&mut value as *mut _ as *mut _),
            Some(&mut size),
        )
        .is_ok()
            && value == 1
    }
}

/// A ring, with a red dot inside while recording. Drawn at the small-icon
/// size for the current DPI, antialiased by coverage, in the taskbar's
/// foreground colour.
fn draw_icon(recording: bool) -> HICON {
    unsafe {
        let size = GetSystemMetrics(SM_CXSMICON).max(16);
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: size,
                biHeight: -size,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits = std::ptr::null_mut();
        let Ok(color) = CreateDIBSection(None, &info, DIB_RGB_COLORS, &mut bits, None, 0) else {
            return HICON::default();
        };
        let pixels = std::slice::from_raw_parts_mut(bits as *mut u8, (size * size * 4) as usize);

        let s = size as f32;
        let centre = s / 2.0;
        let outer = s * 0.44;
        let thickness = (s * 0.11).max(1.5);
        let dot = s * 0.2;
        let ink: [f32; 3] = if light_taskbar() { [0.12, 0.12, 0.12] } else { [1.0, 1.0, 1.0] };
        let ring_alpha = if recording { 1.0 } else { 0.65 };
        let red = [0.898, 0.282, 0.302];

        for py in 0..size {
            for px in 0..size {
                let (dx, dy) = (px as f32 + 0.5 - centre, py as f32 + 0.5 - centre);
                let d = (dx * dx + dy * dy).sqrt();
                let ring = ((outer - d).clamp(-0.5, 0.5) + 0.5).min(((d - (outer - thickness)).clamp(-0.5, 0.5)) + 0.5);
                let fill = if recording { (dot - d).clamp(-0.5, 0.5) + 0.5 } else { 0.0 };
                let (mut r, mut g, mut b, mut a) = (0.0, 0.0, 0.0, 0.0);
                for (coverage, rgb, alpha) in [(ring, ink, ring_alpha), (fill, red, 1.0)] {
                    let c = coverage * alpha;
                    if c > 0.0 {
                        r = r * (1.0 - c) + rgb[0] * c;
                        g = g * (1.0 - c) + rgb[1] * c;
                        b = b * (1.0 - c) + rgb[2] * c;
                        a = a + c * (1.0 - a);
                    }
                }
                let i = ((py * size + px) * 4) as usize;
                // Straight alpha, BGRA; colour channels un-premultiplied.
                let un = |v: f32| if a > 0.0 { ((v / a.max(1e-3)).min(1.0) * 255.0) as u8 } else { 0 };
                pixels[i] = un(b * 1.0);
                pixels[i + 1] = un(g * 1.0);
                pixels[i + 2] = un(r * 1.0);
                pixels[i + 3] = (a * 255.0) as u8;
            }
        }

        let mask = CreateBitmap(size, size, 1, 1, None);
        let icon = CreateIconIndirect(&ICONINFO {
            fIcon: true.into(),
            xHotspot: 0,
            yHotspot: 0,
            hbmMask: mask,
            hbmColor: color,
        })
        .unwrap_or_default();
        let _ = DeleteObject(HGDIOBJ(mask.0));
        let _ = DeleteObject(HGDIOBJ(color.0));
        icon
    }
}
