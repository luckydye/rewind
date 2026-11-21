//! Window setup the GPUI API cannot express — Windows edition of
//! `activate.rs`, same function names so `app.rs` stays platform-neutral.
//!
//! * GPUI's `PopUp` kind already gets `WS_EX_TOOLWINDOW`: no taskbar button
//!   and no Alt+Tab entry, the equivalent of an accessory app. It still has a
//!   caption style and rounded corners, which are stripped here.
//! * Windows only lets a process take the foreground if it was handed the
//!   right. The daemon calls `AllowSetForegroundWindow` right after the
//!   hotkey, so `SetForegroundWindow` normally succeeds; if it races, the
//!   input queues are attached for the one call, which is the documented
//!   fallback.
//! * `WDA_EXCLUDEFROMCAPTURE` is `NSWindowSharingNone`: the recorder never
//!   films its own overlay.

use std::sync::atomic::{AtomicIsize, AtomicUsize, Ordering};

use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MONITOR_DEFAULTTOPRIMARY, MONITORINFO, MonitorFromWindow, RDW_INTERNALPAINT, RedrawWindow,
};
use windows::Win32::Graphics::Dwm::{
    DWMWA_CLOAK, DWMWA_TRANSITIONS_FORCEDISABLED, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_DONOTROUND, DwmSetWindowAttribute,
};
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::WindowsAndMessaging::*;

static WINDOW: AtomicIsize = AtomicIsize::new(0);

fn hwnd() -> Option<HWND> {
    let raw = WINDOW.load(Ordering::Relaxed);
    (raw != 0).then_some(HWND(raw as *mut _))
}

/// The overlay has exactly one window; its handle is captured once at launch.
pub fn remember(window: &gpui::Window) {
    // GPUI has an inherent `window_handle()` too; the trait's is the HWND.
    if let Ok(handle) = HasWindowHandle::window_handle(window) {
        if let RawWindowHandle::Win32(win32) = handle.as_raw() {
            WINDOW.store(win32.hwnd.get(), Ordering::Relaxed);
        }
    }
}

/// `WindowKind::PopUp` already makes it a tool window.
pub fn make_accessory() {}

pub fn make_borderless() {
    let Some(hwnd) = hwnd() else { return };
    unsafe {
        let style = GetWindowLongPtrW(hwnd, GWL_STYLE) as u32;
        let style = (style & !(WS_CAPTION.0 | WS_THICKFRAME.0 | WS_SYSMENU.0 | WS_MINIMIZEBOX.0 | WS_MAXIMIZEBOX.0))
            | WS_POPUP.0;
        SetWindowLongPtrW(hwnd, GWL_STYLE, style as isize);
        let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32 | WS_EX_TOOLWINDOW.0 | WS_EX_TOPMOST.0;
        SetWindowLongPtrW(hwnd, GWL_EXSTYLE, ex as isize);

        // Square corners at the screen edge, and no zoom-in animation: the
        // overlay should simply be there when the hotkey is pressed.
        let corners = DWMWCP_DONOTROUND;
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_WINDOW_CORNER_PREFERENCE,
            &corners as *const _ as *const _,
            size_of_val(&corners) as u32,
        );
        let disabled: i32 = 1;
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_TRANSITIONS_FORCEDISABLED,
            &disabled as *const _ as *const _,
            size_of_val(&disabled) as u32,
        );

        // REWIND_OVERLAY_CAPTURABLE=1 lifts the exclusion for screenshots
        // while debugging the overlay itself.
        if std::env::var_os("REWIND_OVERLAY_CAPTURABLE").is_none() {
            let _ = SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE);
        }

        // GPUI sized the window from the primary display in logical pixels,
        // which is smaller than the screen at any scale above 100%. The first
        // show places it before the daemon has said which display is being
        // captured, so cover the primary monitor in physical pixels now.
        let mut info = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
        let monitor = MonitorFromWindow(hwnd, MONITOR_DEFAULTTOPRIMARY);
        let (x, y, w, h) = if GetMonitorInfoW(monitor, &mut info).as_bool() {
            let r = info.rcMonitor;
            (r.left, r.top, r.right - r.left, r.bottom - r.top)
        } else {
            (0, 0, 0, 0)
        };
        let flags = if w > 0 { SWP_NOACTIVATE | SWP_FRAMECHANGED } else { SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_FRAMECHANGED };
        let _ = SetWindowPos(hwnd, Some(HWND_TOPMOST), x, y, w, h, flags);
    }
}

/// Covers one display. The daemon reports monitor rects in physical
/// virtual-screen pixels, which is what `SetWindowPos` takes in a
/// per-monitor-DPI-aware process.
pub fn place(x: f64, y: f64, width: f64, height: f64) {
    let Some(hwnd) = hwnd() else { return };
    unsafe {
        let _ = SetWindowPos(
            hwnd,
            Some(HWND_TOPMOST),
            x as i32,
            y as i32,
            width as i32,
            height as i32,
            SWP_NOACTIVATE,
        );
    }
}

/// Brings the hidden window back as a small card at the top centre of the
/// monitor it was covering, without taking the foreground: the app in front
/// keeps its keys while a render finishes. `WS_EX_NOACTIVATE` keeps a click on
/// the card from activating it either. Sizes are in points.
pub fn show_toast(_screen: Option<(f64, f64, f64, f64)>, width: f64, height: f64, top: f64, scale: f64) {
    let Some(hwnd) = hwnd() else { return };
    unsafe {
        let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32 | WS_EX_NOACTIVATE.0;
        SetWindowLongPtrW(hwnd, GWL_EXSTYLE, ex as isize);

        let mut info = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
        let monitor = MonitorFromWindow(hwnd, MONITOR_DEFAULTTOPRIMARY);
        if !GetMonitorInfoW(monitor, &mut info).as_bool() {
            return;
        }
        let area = info.rcMonitor;
        let (w, h) = ((width * scale).round() as i32, (height * scale).round() as i32);
        let x = area.left + (area.right - area.left - w) / 2;
        let y = area.top + (top * scale).round() as i32;

        // Cloaked until the card has been drawn at its new size.
        prepare_fade();
        let _ = SetWindowPos(hwnd, Some(HWND_TOPMOST), x, y, w, h, SWP_NOACTIVATE);
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
    }
}

/// The window is about to be the overlay again, which must take focus.
pub fn end_toast() {
    let Some(hwnd) = hwnd() else { return };
    unsafe {
        let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32 & !WS_EX_NOACTIVATE.0;
        SetWindowLongPtrW(hwnd, GWL_EXSTYLE, ex as isize);
    }
}

pub fn activate() {
    let Some(hwnd) = hwnd() else { return };
    unsafe {
        let _ = ShowWindow(hwnd, SW_SHOW);
        if GetForegroundWindow() != hwnd && !SetForegroundWindow(hwnd).as_bool() {
            let foreground = GetForegroundWindow();
            let theirs = GetWindowThreadProcessId(foreground, None);
            let ours = GetCurrentThreadId();
            if theirs != 0 && theirs != ours {
                let _ = windows::Win32::System::Threading::AttachThreadInput(ours, theirs, true);
                let _ = SetForegroundWindow(hwnd);
                let _ = BringWindowToTop(hwnd);
                let _ = windows::Win32::System::Threading::AttachThreadInput(ours, theirs, false);
            }
        }
        let _ = SetWindowPos(hwnd, Some(HWND_TOPMOST), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE);
    }
}

/// Whether the foreground window belongs to this process: the Windows
/// reading of "the overlay is the active application".
pub fn is_active() -> bool {
    unsafe {
        let mut pid = 0u32;
        GetWindowThreadProcessId(GetForegroundWindow(), Some(&mut pid));
        pid == std::process::id()
    }
}

/// Whether the overlay is the window keystrokes go to.
pub fn is_key_window() -> bool {
    hwnd().is_some_and(|hwnd| unsafe { GetForegroundWindow() } == hwnd)
}

/// Hiding the foreground window hands activation to the next window in
/// z-order, which is the app that was in front before the overlay opened.
pub fn hide() {
    let Some(hwnd) = hwnd() else { return };
    unsafe {
        let _ = ShowWindow(hwnd, SW_HIDE);
    }
}

/// A hidden window keeps its last drawn surface, so showing it would flash
/// the previous session's overlay before the new one is drawn. macOS starts
/// at alpha 0 for the same reason; a DirectComposition window cannot take a
/// layered alpha, so it is cloaked instead: shown, focusable, and able to take
/// the foreground, but nothing on screen until it is uncloaked.
///
/// GPUI draws only on `WM_PAINT`, which a cloaked window never receives (it
/// has no visible region), so while cloaked a pump posts paints itself with
/// `RDW_INTERNALPAINT`. That lets GPUI draw the new overlay off screen, and the
/// frame callbacks that uncloak it run. The pump uncloaks after 1.5 s
/// regardless, so a missing frame can never leave the overlay invisible.
pub fn prepare_fade() {
    REVEAL_PENDING.store(false, Ordering::Relaxed);
    let Some(hwnd) = hwnd() else { return };
    set_cloaked(true);
    let generation = CLOAK_GENERATION.fetch_add(1, Ordering::SeqCst) + 1;
    let raw = hwnd.0 as isize;
    std::thread::spawn(move || {
        let hwnd = HWND(raw as *mut _);
        let started = std::time::Instant::now();
        while CLOAK_GENERATION.load(Ordering::SeqCst) == generation {
            if started.elapsed() > std::time::Duration::from_millis(1500) {
                reveal();
                break;
            }
            unsafe {
                let _ = RedrawWindow(Some(hwnd), None, None, RDW_INTERNALPAINT);
            }
            std::thread::sleep(std::time::Duration::from_millis(8));
        }
    });
}

/// Bumped by every cloak and every reveal; the paint pump runs only while
/// its own cloak is the current one.
static CLOAK_GENERATION: AtomicUsize = AtomicUsize::new(0);

/// The first real frame has arrived. The uncloak waits for the render that
/// shows it; see `take_reveal`.
pub fn start_fade() {
    REVEAL_PENDING.store(true, Ordering::Relaxed);
}

/// True once, for the render after `start_fade`: that render should schedule
/// `reveal` for after it is on screen.
pub fn take_reveal() -> bool {
    REVEAL_PENDING.swap(false, Ordering::Relaxed)
}

pub fn reveal() {
    CLOAK_GENERATION.fetch_add(1, Ordering::SeqCst);
    set_cloaked(false);
}

static REVEAL_PENDING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn set_cloaked(cloaked: bool) {
    let Some(hwnd) = hwnd() else { return };
    let value: i32 = cloaked.into();
    unsafe {
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_CLOAK, &value as *const _ as *const _, size_of_val(&value) as u32);
    }
}
