//! Tray-resident main loop: hidden main window, tray icon/menu, overlay wiring, timers, update hand-off.
use std::cell::RefCell;

use windows::core::{w as wlit, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_DWORD};
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::data::{Profile, ProfileDb};
use crate::device::DeviceInfo;
use crate::hook::{self, WM_APP_HIDE, WM_APP_LEARN_KEY, WM_APP_SHOW};
use crate::i18n::{self, t};
use crate::matcher::{match_device, MatchKind, MatchResult};
use crate::overlay::{palette_for, Overlay, OverlayModel};
use crate::settings::Settings;
use crate::util::{log, user_profiles_dir, w, APP_VERSION, SITE_URL};
use crate::{startup, ui, update};

pub const MAIN_CLASS: PCWSTR = wlit!("FnCheatsheetMain");
pub const WM_APP_TRAY: u32 = WM_APP + 20;
pub const WM_APP_OPEN_SETTINGS: u32 = WM_APP + 21;
pub const WM_APP_UPDATE_DONE: u32 = WM_APP + 22;
const TIMER_UPDATE: usize = 1;
const TRAY_ID: u32 = 1;
// NOTIFYICON_VERSION_4 callback events (shellapi.h)
const NIN_SELECT: u32 = WM_USER;
const NIN_KEYSELECT: u32 = WM_USER + 1;
const NIN_BALLOONUSERCLICK: u32 = WM_USER + 5;

const CMD_SHOW: usize = 100;
const CMD_MODEL: usize = 101;
const CMD_PAUSE: usize = 102;
const CMD_SETTINGS: usize = 103;
const CMD_SITE: usize = 104;
const CMD_UPDATE: usize = 105;
const CMD_EXIT: usize = 199;

pub struct App {
    pub settings: Settings,
    pub db: ProfileDb,
    pub device: DeviceInfo,
    pub detection: MatchResult,
    pub hwnd: HWND,
    pub update_status: String,
    pub update_busy: bool,
    taskbar_created: u32,
}

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
    static OVERLAY: RefCell<Option<Overlay>> = const { RefCell::new(None) };
}

/// Runs `f` with the app state. Never call anything that pumps messages (dialogs, menus) inside `f`.
pub fn with<R>(f: impl FnOnce(&mut App) -> R) -> Option<R> {
    APP.with(|a| a.try_borrow_mut().ok().and_then(|mut a| a.as_mut().map(f)))
}

impl App {
    pub fn active_index(&self) -> Option<usize> {
        if self.settings.profile_mode == "manual" {
            return self.db.profiles.iter().position(|p| p.id.eq_ignore_ascii_case(&self.settings.manual_profile_id));
        }
        self.detection.best()
    }

    pub fn active(&self) -> Option<&Profile> {
        self.active_index().map(|i| &self.db.profiles[i])
    }

    pub fn active_status(&self) -> String {
        if self.settings.profile_mode == "manual" {
            return if self.active().is_some() { t("Chosen in Settings", "설정에서 직접 고름").into() } else { String::new() };
        }
        match self.detection.kind {
            MatchKind::Matched => format!("{} · {}", t("Detected automatically", "자동 감지됨"), self.detection.candidates[0].reason),
            _ => String::new(),
        }
    }

    pub fn reload_profiles(&mut self) {
        self.db = ProfileDb::load(Some(&user_profiles_dir()));
        for e in &self.db.errors {
            log(&format!("profile load: {e}"));
        }
        self.device = DeviceInfo::read();
        self.detection = match_device(&self.device, &self.db.profiles);
    }
}

/// Saves settings and pushes them to the hook, language and tray.
pub fn apply_settings() {
    let s = with(|a| {
        i18n::apply(&a.settings.language);
        a.settings.save();
        a.settings.clone()
    });
    if let Some(s) = s {
        hook::configure(&s);
        update_tray_tip();
    }
}

pub fn apps_use_light_theme() -> bool {
    let key = w(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize");
    let val = w("AppsUseLightTheme");
    let mut data = 0u32;
    let mut size = 4u32;
    let r = unsafe { RegGetValueW(HKEY_CURRENT_USER, key.pcwstr(), val.pcwstr(), RRF_RT_REG_DWORD, None, Some(&mut data as *mut u32 as _), Some(&mut size)) };
    r.is_ok() && data == 1
}

// ───────────────────────── overlay ─────────────────────────

pub fn show_overlay(pinned: bool) {
    // Copy what we need, then draw without holding the app state.
    let Some((profile, status, label, ambiguous, theme, scale, skip_fs)) = with(|a| {
        (
            a.active().cloned(),
            a.active_status(),
            a.device.short_label(),
            a.settings.profile_mode == "auto" && a.detection.kind == MatchKind::Ambiguous,
            a.settings.theme.clone(),
            a.settings.overlay_scale,
            a.settings.skip_fullscreen,
        )
    }) else {
        return;
    };
    if !pinned && skip_fs && fullscreen_app_in_front() {
        return;
    }
    let model = OverlayModel { profile: profile.as_ref(), status, device_label: label, ambiguous };
    let pal = palette_for(&theme);
    OVERLAY.with(|o| {
        if let Ok(mut o) = o.try_borrow_mut() {
            if let Some(o) = o.as_mut() {
                o.show(&model, &pal, scale, pinned);
            }
        }
    });
}

pub fn hide_overlay() {
    OVERLAY.with(|o| {
        if let Ok(mut o) = o.try_borrow_mut() {
            if let Some(o) = o.as_mut() {
                o.hide();
            }
        }
    });
}

/// A full-screen game, video or presentation is in front: holding Ctrl there (crouch!) must not cover it.
fn fullscreen_app_in_front() -> bool {
    match unsafe { SHQueryUserNotificationState() } {
        Ok(s) => s == QUNS_BUSY || s == QUNS_RUNNING_D3D_FULL_SCREEN || s == QUNS_PRESENTATION_MODE,
        Err(_) => false,
    }
}

// ───────────────────────── tray ─────────────────────────

fn tray_data(hwnd: HWND) -> NOTIFYICONDATAW {
    let mut nid = NOTIFYICONDATAW { cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32, hWnd: hwnd, uID: TRAY_ID, ..Default::default() };
    nid.Anonymous.uVersion = NOTIFYICON_VERSION_4;
    nid
}

fn copy_into(dst: &mut [u16], s: &str) {
    let units: Vec<u16> = s.encode_utf16().take(dst.len() - 1).collect();
    dst[..units.len()].copy_from_slice(&units);
    dst[units.len()] = 0;
}

fn tray_tip() -> String {
    let name = with(|a| a.active().map(|p| p.short_name().to_string())).flatten();
    let s = format!("Fn Cheatsheet — {}", name.unwrap_or_else(|| t("model not set", "모델 미설정").to_string()));
    s.chars().take(120).collect()
}

fn load_icon(size: i32) -> HICON {
    unsafe {
        let hinst = GetModuleHandleW(None).unwrap_or_default();
        LoadImageW(Some(hinst.into()), PCWSTR(1 as _), IMAGE_ICON, size, size, LR_DEFAULTCOLOR).map(|h| HICON(h.0)).unwrap_or_default()
    }
}

fn add_tray(hwnd: HWND) {
    unsafe {
        let mut nid = tray_data(hwnd);
        nid.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP | NIF_SHOWTIP;
        nid.uCallbackMessage = WM_APP_TRAY;
        let dpi = windows::Win32::UI::HiDpi::GetDpiForWindow(hwnd).max(96);
        nid.hIcon = load_icon(windows::Win32::UI::HiDpi::GetSystemMetricsForDpi(SM_CXSMICON, dpi));
        copy_into(&mut nid.szTip, &tray_tip());
        let _ = Shell_NotifyIconW(NIM_DELETE, &nid);
        if !Shell_NotifyIconW(NIM_ADD, &nid).as_bool() {
            log("tray: NIM_ADD failed");
        }
        let _ = Shell_NotifyIconW(NIM_SETVERSION, &nid);
    }
}

pub fn update_tray_tip() {
    let Some(hwnd) = with(|a| a.hwnd) else { return };
    unsafe {
        let mut nid = tray_data(hwnd);
        nid.uFlags = NIF_TIP | NIF_SHOWTIP;
        copy_into(&mut nid.szTip, &tray_tip());
        let _ = Shell_NotifyIconW(NIM_MODIFY, &nid);
    }
}

pub fn balloon(title: &str, text: &str) {
    let Some(hwnd) = with(|a| a.hwnd) else { return };
    unsafe {
        let mut nid = tray_data(hwnd);
        nid.uFlags = NIF_INFO;
        nid.dwInfoFlags = NIIF_INFO;
        copy_into(&mut nid.szInfoTitle, title);
        copy_into(&mut nid.szInfo, text);
        let _ = Shell_NotifyIconW(NIM_MODIFY, &nid);
    }
}

fn remove_tray(hwnd: HWND) {
    unsafe {
        let nid = tray_data(hwnd);
        let _ = Shell_NotifyIconW(NIM_DELETE, &nid);
    }
}

fn show_menu(hwnd: HWND) {
    let Some((model, paused, upd)) = with(|a| {
        (
            a.active().map(|p| p.short_name().to_string()),
            !a.settings.trigger_enabled,
            if a.update_busy { t("Checking for updates…", "업데이트 확인 중…").to_string() } else { t("Check for updates", "업데이트 확인").to_string() },
        )
    }) else {
        return;
    };
    unsafe {
        let Ok(menu) = CreatePopupMenu() else { return };
        let add = |flags: MENU_ITEM_FLAGS, id: usize, s: &str| {
            let ws = w(s);
            let _ = AppendMenuW(menu, flags, id, ws.pcwstr());
        };
        add(MF_STRING, CMD_SHOW, t("Show cheat sheet", "치트시트 보기"));
        add(MF_STRING, CMD_MODEL, &model.unwrap_or_else(|| t("Model not set — choose…", "모델 미설정 — 고르기…").to_string()));
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
        add(if paused { MF_STRING | MF_CHECKED } else { MF_STRING }, CMD_PAUSE, t("Pause hold trigger", "길게 누르기 일시 중지"));
        add(MF_STRING, CMD_SETTINGS, t("Settings…", "설정…"));
        add(MF_STRING, CMD_UPDATE, &upd);
        add(MF_STRING, CMD_SITE, t("Supported laptops (web)", "지원 노트북 목록 (웹)"));
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
        add(MF_STRING, CMD_EXIT, t("Exit", "종료"));
        let mut pt = POINT::default();
        let _ = GetCursorPos(&mut pt);
        let _ = SetForegroundWindow(hwnd);
        let cmd = TrackPopupMenu(menu, TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_BOTTOMALIGN, pt.x, pt.y, None, hwnd, None);
        let _ = DestroyMenu(menu);
        let _ = PostMessageW(Some(hwnd), WM_NULL, WPARAM(0), LPARAM(0));
        run_command(hwnd, cmd.0 as usize);
    }
}

fn run_command(hwnd: HWND, cmd: usize) {
    match cmd {
        CMD_SHOW => show_overlay(true),
        CMD_MODEL => ui::open(Some(ui::Tab::Device)),
        CMD_PAUSE => {
            with(|a| a.settings.trigger_enabled = !a.settings.trigger_enabled);
            apply_settings();
        }
        CMD_SETTINGS => ui::open(None),
        CMD_SITE => crate::util::open_url(&format!("{SITE_URL}#models")),
        CMD_UPDATE => update::check_now(hwnd, true),
        CMD_EXIT => unsafe {
            let _ = DestroyWindow(hwnd);
        },
        _ => {}
    }
}

// ───────────────────────── main window ─────────────────────────

unsafe extern "system" fn main_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let taskbar_created = with(|a| a.taskbar_created).unwrap_or(0);
    match msg {
        WM_APP_SHOW => {
            show_overlay(false);
            LRESULT(0)
        }
        WM_APP_HIDE => {
            hide_overlay();
            LRESULT(0)
        }
        WM_APP_LEARN_KEY => {
            ui::on_learn_key(wp.0 == 1, lp.0);
            LRESULT(0)
        }
        WM_APP_TRAY => {
            let event = (lp.0 & 0xFFFF) as u32;
            match event {
                NIN_SELECT | NIN_KEYSELECT => show_overlay(true),
                WM_CONTEXTMENU => show_menu(hwnd),
                NIN_BALLOONUSERCLICK => ui::open(Some(ui::Tab::Device)),
                _ => {}
            }
            LRESULT(0)
        }
        WM_APP_OPEN_SETTINGS => {
            ui::open(None);
            LRESULT(0)
        }
        WM_APP_UPDATE_DONE => {
            update::on_done(hwnd, lp);
            LRESULT(0)
        }
        WM_TIMER if wp.0 == TIMER_UPDATE || wp.0 == update::TIMER_FIRST => {
            update::check_if_due(hwnd);
            LRESULT(0)
        }
        WM_SETTINGCHANGE => {
            // Theme changes are picked up at the next show; nothing cached to refresh.
            DefWindowProcW(hwnd, msg, wp, lp)
        }
        WM_DESTROY => {
            remove_tray(hwnd);
            PostQuitMessage(0);
            LRESULT(0)
        }
        m if m != 0 && m == taskbar_created => {
            add_tray(hwnd); // Explorer restarted
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

pub fn run(background: bool, updated_from: Option<String>) -> i32 {
    unsafe {
        let mut settings = Settings::load();
        i18n::apply(&settings.language);
        let hinst = GetModuleHandleW(None).unwrap_or_default();
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(main_proc),
            hInstance: hinst.into(),
            lpszClassName: MAIN_CLASS,
            ..Default::default()
        };
        RegisterClassExW(&wc);
        // A real (hidden) top-level window, not message-only: it must receive the TaskbarCreated broadcast.
        let hwnd = match CreateWindowExW(WINDOW_EX_STYLE(0), MAIN_CLASS, wlit!("Fn Cheatsheet"), WS_OVERLAPPED, 0, 0, 0, 0, None, None, Some(hinst.into()), None) {
            Ok(h) => h,
            Err(e) => {
                log(&format!("main window failed: {e}"));
                return 1;
            }
        };

        let first_run = !settings.first_run_done;
        if first_run {
            settings.first_run_done = true;
            settings.save();
        }
        let mut app = App {
            settings: settings.clone(),
            db: ProfileDb::default(),
            device: DeviceInfo::default(),
            detection: MatchResult { kind: MatchKind::Unknown, candidates: vec![] },
            hwnd,
            update_status: String::new(),
            update_busy: false,
            taskbar_created: RegisterWindowMessageW(wlit!("TaskbarCreated")),
        };
        app.reload_profiles();
        let has_profile = app.active().is_some();
        log(&format!(
            "started v{APP_VERSION} · {} · {:?}{}",
            app.device.short_label(),
            app.detection.kind,
            app.detection.best().map(|i| format!(" {}", app.db.profiles[i].id)).unwrap_or_default()
        ));
        APP.with(|a| *a.borrow_mut() = Some(app));
        OVERLAY.with(|o| *o.borrow_mut() = Some(Overlay::create()));

        add_tray(hwnd);
        hook::start(hwnd, &settings);
        if startup::points_elsewhere() {
            startup::set(true); // the exe moved: keep "start with Windows" pointing here
        }
        update::after_start(updated_from.as_deref());
        SetTimer(Some(hwnd), TIMER_UPDATE, 60 * 60 * 1000, None);
        // First check shortly after start, not during logon.
        update::schedule_first_check(hwnd);

        if updated_from.is_some() {
            balloon("Fn Cheatsheet", &format!("{} v{APP_VERSION}", t("Updated to", "업데이트 완료:")));
        } else if first_run || (!has_profile && !background) {
            ui::open(Some(ui::Tab::Device));
        } else if !has_profile {
            balloon("Fn Cheatsheet", t("This laptop model is not set yet. Click here to choose it.", "이 노트북 모델이 아직 정해지지 않았어요. 눌러서 골라 주세요."));
        }

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            if ui::is_dialog_message(&msg) {
                continue;
            }
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        0
    }
}
