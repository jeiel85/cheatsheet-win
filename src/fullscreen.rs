//! "Is a full-screen app in front?" — so holding Ctrl (crouch!) in a game or video doesn't cover it.
//!
//! Windows only reports exclusive full-screen D3D apps and presentation mode through
//! `SHQueryUserNotificationState`. Most games now run *borderless windowed* and browsers/video players go
//! full screen by resizing a caption-less window over the monitor, which that API doesn't flag. So we also
//! look at the foreground window itself: if it covers its whole monitor (not just the work area) it's full
//! screen — unless it's the desktop/taskbar, one of our own windows, or an ordinary maximized window that
//! only fills the monitor because the taskbar auto-hides.
use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS};
use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MonitorFromWindow, MONITORINFO, MONITOR_DEFAULTTONULL};
use windows::Win32::System::Threading::GetCurrentProcessId;
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::*;

/// What we know about the foreground window, gathered by [`foreground`]; kept plain so the rules are testable.
#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    pub class: String,
    pub own_process: bool,
    pub shell: bool,
    pub visible: bool,
    pub minimized: bool,
    pub cloaked: bool,
    pub maximized: bool,
    /// The window has a caption or a sizing border (WS_CAPTION / WS_THICKFRAME).
    pub framed: bool,
    /// Visible bounds (DWM extended frame bounds, else the window rect), physical pixels.
    pub rect: (i32, i32, i32, i32),
    pub monitor: Option<(i32, i32, i32, i32)>,
}

/// Desktop and taskbar windows that cover a monitor but aren't "an app in front".
const SHELL_CLASSES: &[&str] = &["Progman", "WorkerW", "Shell_TrayWnd", "Shell_SecondaryTrayWnd"];

pub fn covers_monitor(s: &Snapshot) -> bool {
    let Some((ml, mt, mr, mb)) = s.monitor else { return false };
    if s.own_process || s.shell || !s.visible || s.minimized || s.cloaked {
        return false;
    }
    if SHELL_CLASSES.iter().any(|c| c.eq_ignore_ascii_case(&s.class)) {
        return false;
    }
    // A normal maximized window fills the monitor when the taskbar auto-hides; that's not full screen.
    // Browsers and players restore before going full screen, and borderless games have no frame.
    if s.maximized && s.framed {
        return false;
    }
    let (l, t, r, b) = s.rect;
    mr > ml && mb > mt && l <= ml && t <= mt && r >= mr && b >= mb
}

/// True when something full screen is in front, by either the shell's own state or the window's geometry.
pub fn app_in_front() -> bool {
    shell_reports() || foreground().is_some_and(|s| covers_monitor(&s))
}

/// The shell's own notion: exclusive D3D full screen, presentation mode, or a "rude" window it noticed.
pub fn shell_reports() -> bool {
    match unsafe { SHQueryUserNotificationState() } {
        Ok(s) => s == QUNS_BUSY || s == QUNS_RUNNING_D3D_FULL_SCREEN || s == QUNS_PRESENTATION_MODE,
        Err(_) => false,
    }
}

pub fn foreground() -> Option<Snapshot> {
    unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd.0.is_null() {
            return None;
        }
        Some(snapshot(hwnd))
    }
}

pub unsafe fn snapshot(hwnd: HWND) -> Snapshot {
    let mut s = Snapshot::default();
    let mut buf = [0u16; 128];
    let n = GetClassNameW(hwnd, &mut buf);
    s.class = String::from_utf16_lossy(&buf[..n.max(0) as usize]);
    let mut pid = 0u32;
    GetWindowThreadProcessId(hwnd, Some(&mut pid));
    s.own_process = pid == GetCurrentProcessId();
    s.shell = hwnd == GetShellWindow() || hwnd == GetDesktopWindow();
    s.visible = IsWindowVisible(hwnd).as_bool();
    s.minimized = IsIconic(hwnd).as_bool();
    s.maximized = IsZoomed(hwnd).as_bool();
    let style = GetWindowLongW(hwnd, GWL_STYLE) as u32;
    s.framed = style & (WS_CAPTION.0 | WS_THICKFRAME.0) != 0;
    let mut cloaked = 0u32;
    if DwmGetWindowAttribute(hwnd, DWMWA_CLOAKED, &mut cloaked as *mut u32 as _, 4).is_ok() {
        s.cloaked = cloaked != 0;
    }
    let mut r = RECT::default();
    if DwmGetWindowAttribute(hwnd, DWMWA_EXTENDED_FRAME_BOUNDS, &mut r as *mut RECT as _, std::mem::size_of::<RECT>() as u32).is_err() {
        let _ = GetWindowRect(hwnd, &mut r);
    }
    s.rect = (r.left, r.top, r.right, r.bottom);
    let mon = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONULL);
    if !mon.is_invalid() {
        let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        if GetMonitorInfoW(mon, &mut mi).as_bool() {
            let m = mi.rcMonitor;
            s.monitor = Some((m.left, m.top, m.right, m.bottom));
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    const MON: (i32, i32, i32, i32) = (0, 0, 1920, 1080);

    fn win(rect: (i32, i32, i32, i32)) -> Snapshot {
        Snapshot { class: "GameWindow".into(), visible: true, rect, monitor: Some(MON), ..Default::default() }
    }

    #[test]
    fn borderless_window_over_monitor_is_full_screen() {
        assert!(covers_monitor(&win(MON)));
    }

    #[test]
    fn slightly_oversized_window_still_counts() {
        assert!(covers_monitor(&win((-8, -8, 1928, 1088))));
    }

    #[test]
    fn work_area_sized_window_is_not_full_screen() {
        // Maximized with a visible taskbar: 48 px short at the bottom.
        let mut s = win((0, 0, 1920, 1032));
        s.maximized = true;
        s.framed = true;
        assert!(!covers_monitor(&s));
        assert!(!covers_monitor(&win((0, 0, 1920, 1032))));
    }

    #[test]
    fn maximized_framed_window_with_auto_hide_taskbar_is_not_full_screen() {
        let mut s = win(MON);
        s.maximized = true;
        s.framed = true;
        assert!(!covers_monitor(&s));
    }

    #[test]
    fn maximized_borderless_popup_is_full_screen() {
        let mut s = win(MON);
        s.maximized = true;
        assert!(covers_monitor(&s));
    }

    #[test]
    fn window_on_second_monitor_uses_that_monitor() {
        let mut s = win((-1920, 0, 0, 1080));
        s.monitor = Some((-1920, 0, 0, 1080));
        assert!(covers_monitor(&s));
        s.rect = (-1920, 0, -100, 1080);
        assert!(!covers_monitor(&s));
    }

    #[test]
    fn desktop_taskbar_and_own_windows_never_count() {
        for class in ["Progman", "WorkerW", "Shell_TrayWnd", "shell_secondarytraywnd"] {
            let mut s = win(MON);
            s.class = class.into();
            assert!(!covers_monitor(&s), "{class}");
        }
        let mut s = win(MON);
        s.shell = true;
        assert!(!covers_monitor(&s));
        let mut s = win(MON);
        s.own_process = true;
        assert!(!covers_monitor(&s));
    }

    /// Real windows: checks the Win32 side (style, DWM bounds, monitor) without needing the foreground.
    #[test]
    fn snapshot_of_real_windows() {
        use windows::core::w;
        unsafe {
            let (cx, cy) = (GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN));
            let Ok(popup) = CreateWindowExW(WINDOW_EX_STYLE(0), w!("STATIC"), w!("fs"), WS_POPUP | WS_VISIBLE, 0, 0, cx, cy, None, None, None, None) else { return };
            let s = snapshot(popup);
            let _ = DestroyWindow(popup);
            let Some(m) = s.monitor else { return }; // headless runner without a display
            assert!(s.own_process && !s.framed && s.visible, "{s:?}");
            assert_eq!(s.rect, m, "{s:?}");
            assert!(covers_monitor(&Snapshot { own_process: false, ..s.clone() }), "{s:?}");

            let normal = CreateWindowExW(WINDOW_EX_STYLE(0), w!("STATIC"), w!("fs"), WS_OVERLAPPEDWINDOW, 0, 0, 640, 480, None, None, None, None).unwrap();
            let _ = ShowWindow(normal, SW_MAXIMIZE);
            let s = snapshot(normal);
            let _ = DestroyWindow(normal);
            assert!(s.maximized && s.framed, "{s:?}");
            assert!(!covers_monitor(&Snapshot { own_process: false, ..s.clone() }), "{s:?}");
        }
    }

    #[test]
    fn hidden_minimized_cloaked_or_monitorless_windows_never_count() {
        let mut s = win(MON);
        s.visible = false;
        assert!(!covers_monitor(&s));
        let mut s = win(MON);
        s.minimized = true;
        assert!(!covers_monitor(&s));
        let mut s = win(MON);
        s.cloaked = true;
        assert!(!covers_monitor(&s));
        let mut s = win(MON);
        s.monitor = None;
        assert!(!covers_monitor(&s));
    }
}
