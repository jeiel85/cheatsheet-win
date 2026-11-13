//! Settings window — plain Win32 controls, three tabs, per-monitor DPI aware.
//! State lives in a thread-local; no borrow is held while calling into Win32 (controls send notifications back).
use std::cell::RefCell;

use windows::core::{w as wlit, PCWSTR};
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{CreateFontIndirectW, DeleteObject, GetSysColor, GetSysColorBrush, SetBkColor, SetTextColor, COLOR_WINDOW, HDC, HFONT, HGDIOBJ};
use windows::Win32::System::DataExchange::{CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
use windows::Win32::UI::Controls::*;
use windows::Win32::UI::HiDpi::{AdjustWindowRectExForDpi, GetDpiForWindow, SystemParametersInfoForDpi};
use windows::Win32::UI::Input::KeyboardAndMouse::EnableWindow;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::app;
use crate::i18n::t;
use crate::matcher::MatchKind;
use crate::settings::{VK_ANY_CTRL, VK_APPS, VK_LCONTROL, VK_RSHIFT};
use crate::util::{from_wide, url_encode, user_profiles_dir, w, APP_VERSION, REPO_URL, SITE_URL};
use crate::{hook, startup, update};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tab {
    Device,
    Trigger,
    About,
}

const CLASS: PCWSTR = wlit!("FnCheatsheetSettings");
const W_DIP: i32 = 680;
const H_DIP: i32 = 650;
const TIMER_LEARN: usize = 1;

// control ids
const ID_TAB: i32 = 10;
const ID_DEV_REPORT: i32 = 101;
const ID_DET: i32 = 102;
const ID_COPY: i32 = 103;
const ID_REQUEST: i32 = 104;
const ID_AUTO: i32 = 105;
const ID_MANUAL: i32 = 106;
const ID_SEARCH: i32 = 107;
const ID_LIST: i32 = 108;
const ID_ACTIVE: i32 = 109;
const ID_PREVIEW: i32 = 110;
const ID_FOLDER: i32 = 111;
const ID_RELOAD: i32 = 112;
const ID_TRIG_ENABLED: i32 = 201;
const ID_TRIG_KEY: i32 = 202;
const ID_HOLD: i32 = 203;
const ID_HOLD_LABEL: i32 = 204;
const ID_SKIP_FS: i32 = 205;
const ID_LEARN: i32 = 206;
const ID_USE_LEARNED: i32 = 207;
const ID_LEARN_STATUS: i32 = 208;
const ID_STARTUP: i32 = 209;
const ID_LANG: i32 = 210;
const ID_THEME: i32 = 211;
const ID_SCALE: i32 = 212;
const ID_SCALE_LABEL: i32 = 213;
const ID_PREVIEW2: i32 = 214;
const ID_AUTO_UPDATE: i32 = 301;
const ID_CHECK_NOW: i32 = 302;
const ID_UPDATE_STATUS: i32 = 303;
const ID_SITE: i32 = 304;
const ID_GITHUB: i32 = 305;
const ID_ISSUES: i32 = 306;
const ID_SOURCES: i32 = 307;
const ID_OPEN_SOURCE: i32 = 308;

const GOOD: u32 = 0x00417C10; // COLORREF is 0x00BBGGRR
const WARN: u32 = 0x00005B9A;

struct Ui {
    hwnd: HWND,
    dpi: u32,
    font: HFONT,
    bold: HFONT,
    tab: Tab,
    controls: Vec<(Option<Tab>, HWND)>,
    /// Profile index for each row of the profile list.
    rows: Vec<usize>,
    /// Source URL for each row of the sources list.
    source_urls: Vec<String>,
    learning: bool,
    learned: Option<(u32, u32, bool)>,
    learn_saw: bool,
    colors: Vec<(i32, u32)>,
}

thread_local! {
    static UI: RefCell<Option<Ui>> = const { RefCell::new(None) };
}

fn ui<R>(f: impl FnOnce(&mut Ui) -> R) -> Option<R> {
    UI.with(|u| u.try_borrow_mut().ok().and_then(|mut u| u.as_mut().map(f)))
}

fn item(id: i32) -> HWND {
    ui(|u| unsafe { GetDlgItem(Some(u.hwnd), id).unwrap_or_default() }).unwrap_or_default()
}

fn send(h: HWND, msg: u32, wp: usize, lp: isize) -> isize {
    unsafe { SendMessageW(h, msg, Some(WPARAM(wp)), Some(LPARAM(lp))).0 }
}

fn set_text(h: HWND, s: &str) {
    let ws = w(s);
    unsafe {
        let _ = SetWindowTextW(h, ws.pcwstr());
    }
}

fn get_text(h: HWND) -> String {
    let mut buf = vec![0u16; 512];
    unsafe {
        let n = GetWindowTextW(h, &mut buf);
        String::from_utf16_lossy(&buf[..n.max(0) as usize])
    }
}

fn checked(id: i32) -> bool {
    send(item(id), BM_GETCHECK, 0, 0) == BST_CHECKED.0 as isize
}

fn set_checked(id: i32, on: bool) {
    send(item(id), BM_SETCHECK, if on { BST_CHECKED.0 as usize } else { 0 }, 0);
}

fn set_color(id: i32, c: u32) {
    ui(|u| {
        u.colors.retain(|(i, _)| *i != id);
        u.colors.push((id, c));
    });
    unsafe {
        let _ = windows::Win32::Graphics::Gdi::InvalidateRect(Some(item(id)), None, true);
    }
}

// ───────────────────────── open / build ─────────────────────────

pub fn open(tab: Option<Tab>) {
    let existing = ui(|u| u.hwnd);
    if let Some(h) = existing {
        if let Some(t) = tab {
            select_tab(t);
        }
        unsafe {
            let _ = ShowWindow(h, SW_RESTORE);
            let _ = SetForegroundWindow(h);
        }
        return;
    }
    unsafe {
        let hinst = GetModuleHandleW(None).unwrap_or_default();
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(proc_),
            hInstance: hinst.into(),
            lpszClassName: CLASS,
            hbrBackground: GetSysColorBrush(COLOR_WINDOW),
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            hIcon: LoadImageW(Some(hinst.into()), PCWSTR(1 as _), IMAGE_ICON, 0, 0, LR_DEFAULTSIZE).map(|h| HICON(h.0)).unwrap_or_default(),
            ..Default::default()
        };
        RegisterClassExW(&wc);
        let title = w(&format!("Fn Cheatsheet {APP_VERSION} — {}", t("Settings", "설정")));
        let Ok(hwnd) = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            CLASS,
            title.pcwstr(),
            WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX,
            CW_USEDEFAULT, CW_USEDEFAULT, 100, 100,
            None, None, Some(hinst.into()), None,
        ) else {
            return;
        };
        let dpi = GetDpiForWindow(hwnd).max(96);
        UI.with(|u| {
            *u.borrow_mut() = Some(Ui {
                hwnd, dpi, font: HFONT::default(), bold: HFONT::default(), tab: tab.unwrap_or(Tab::Device), controls: vec![], rows: vec![],
                source_urls: vec![], learning: false, learned: None, learn_saw: false, colors: vec![],
            })
        });
        resize_window(hwnd, dpi, None);
        build();
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
    }
}

fn resize_window(hwnd: HWND, dpi: u32, suggested: Option<&RECT>) {
    unsafe {
        let mut r = RECT { left: 0, top: 0, right: W_DIP * dpi as i32 / 96, bottom: H_DIP * dpi as i32 / 96 };
        let _ = AdjustWindowRectExForDpi(&mut r, WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX, false, WINDOW_EX_STYLE(0), dpi);
        let (w, h) = (r.right - r.left, r.bottom - r.top);
        match suggested {
            Some(s) => {
                let _ = SetWindowPos(hwnd, None, s.left, s.top, w, h, SWP_NOZORDER | SWP_NOACTIVATE);
            }
            None => {
                // center on the monitor with the cursor
                let mut pt = windows::Win32::Foundation::POINT::default();
                let _ = GetCursorPos(&mut pt);
                let mon = windows::Win32::Graphics::Gdi::MonitorFromPoint(pt, windows::Win32::Graphics::Gdi::MONITOR_DEFAULTTONEAREST);
                let mut mi = windows::Win32::Graphics::Gdi::MONITORINFO { cbSize: std::mem::size_of::<windows::Win32::Graphics::Gdi::MONITORINFO>() as u32, ..Default::default() };
                let _ = windows::Win32::Graphics::Gdi::GetMonitorInfoW(mon, &mut mi);
                let wa = mi.rcWork;
                let x = wa.left + ((wa.right - wa.left - w) / 2).max(0);
                let y = wa.top + ((wa.bottom - wa.top - h) / 2).max(0);
                let _ = SetWindowPos(hwnd, None, x, y, w, h, SWP_NOZORDER | SWP_NOACTIVATE);
            }
        }
    }
}

fn make_fonts(dpi: u32) -> (HFONT, HFONT) {
    unsafe {
        let mut ncm = NONCLIENTMETRICSW { cbSize: std::mem::size_of::<NONCLIENTMETRICSW>() as u32, ..Default::default() };
        let _ = SystemParametersInfoForDpi(SPI_GETNONCLIENTMETRICS.0, ncm.cbSize, Some(&mut ncm as *mut _ as _), 0, dpi);
        let normal = CreateFontIndirectW(&ncm.lfMessageFont);
        let mut b = ncm.lfMessageFont;
        b.lfWeight = 600;
        let bold = CreateFontIndirectW(&b);
        (normal, bold)
    }
}

/// (Re)creates every control for the current DPI and language.
fn build() {
    let Some((hwnd, dpi, old_ctrls, old_fonts, tab)) = ui(|u| (u.hwnd, u.dpi, std::mem::take(&mut u.controls), (u.font, u.bold), u.tab)) else { return };
    unsafe {
        for (_, c) in old_ctrls {
            let _ = DestroyWindow(c);
        }
        if !old_fonts.0 .0.is_null() {
            let _ = DeleteObject(HGDIOBJ(old_fonts.0 .0));
            let _ = DeleteObject(HGDIOBJ(old_fonts.1 .0));
        }
        let title = w(&format!("Fn Cheatsheet {APP_VERSION} — {}", t("Settings", "설정")));
        let _ = SetWindowTextW(hwnd, title.pcwstr());
    }
    let (font, bold) = make_fonts(dpi);
    ui(|u| {
        u.font = font;
        u.bold = bold;
    });

    let s = |v: i32| v * dpi as i32 / 96;
    let mk = |tab: Option<Tab>, class: PCWSTR, text: &str, style: u32, x: i32, y: i32, wd: i32, ht: i32, id: i32, is_bold: bool| -> HWND {
        let txt = w(text);
        let h = unsafe {
            CreateWindowExW(
                if class == wlit!("EDIT") || class == wlit!("LISTBOX") { WS_EX_CLIENTEDGE } else { WINDOW_EX_STYLE(0) },
                class,
                txt.pcwstr(),
                WINDOW_STYLE(style) | WS_CHILD,
                s(x), s(y), s(wd), s(ht),
                Some(hwnd),
                Some(HMENU(id as isize as _)),
                None,
                None,
            )
            .unwrap_or_default()
        };
        send(h, WM_SETFONT, if is_bold { bold.0 as usize } else { font.0 as usize }, 1);
        ui(|u| u.controls.push((tab, h)));
        h
    };
    let stat = |tab: Tab, text: &str, x: i32, y: i32, wd: i32, ht: i32, id: i32, b: bool| mk(Some(tab), wlit!("STATIC"), text, 0, x, y, wd, ht, id, b);
    let btn = |tab: Tab, text: &str, x: i32, y: i32, wd: i32, id: i32| mk(Some(tab), wlit!("BUTTON"), text, (BS_PUSHBUTTON as u32) | WS_TABSTOP.0, x, y, wd, 28, id, false);
    let check = |tab: Tab, text: &str, x: i32, y: i32, wd: i32, id: i32| mk(Some(tab), wlit!("BUTTON"), text, (BS_AUTOCHECKBOX as u32) | WS_TABSTOP.0, x, y, wd, 22, id, false);
    let combo = |tab: Tab, x: i32, y: i32, wd: i32, id: i32, items: &[&str], sel: usize| {
        let h = mk(Some(tab), wlit!("COMBOBOX"), "", (CBS_DROPDOWNLIST as u32) | WS_TABSTOP.0 | WS_VSCROLL.0, x, y, wd, 200, id, false);
        for it in items {
            let ws = w(it);
            send(h, CB_ADDSTRING, 0, ws.pcwstr().0 as isize);
        }
        send(h, CB_SETCURSEL, sel, 0);
        h
    };
    let track = |tab: Tab, x: i32, y: i32, wd: i32, id: i32, min: i32, max: i32, pos: i32| {
        let h = mk(Some(tab), TRACKBAR_CLASSW, "", TBS_HORZ | TBS_NOTICKS | WS_TABSTOP.0, x, y, wd, 30, id, false);
        send(h, TBM_SETRANGE, 1, ((max as isize) << 16) | (min as isize & 0xFFFF));
        send(h, TBM_SETPOS, 1, pos as isize);
        h
    };

    // Tab strip
    let tabs = mk(None, WC_TABCONTROLW, "", WS_TABSTOP.0 | WS_CLIPSIBLINGS.0, 10, 8, W_DIP - 20, 30, ID_TAB, false);
    for (i, name) in [t("Device", "기기"), t("Trigger & display", "누르기·표시"), t("About & updates", "정보·업데이트")].iter().enumerate() {
        let mut ws = w(name);
        let tci = TCITEMW { mask: TCIF_TEXT, pszText: ws.pwstr(), ..Default::default() };
        send(tabs, TCM_INSERTITEMW, i, &tci as *const _ as isize);
    }

    let Some((settings, device, det_kind, det_text, list_info, db_errors, data_count, data_version, update_status, sources, notes)) = app::with(|a| {
        let det_text = match a.detection.kind {
            MatchKind::Matched => format!("{}{}  ({})", t("Detected: ", "자동 감지: "), a.db.profiles[a.detection.candidates[0].index].short_name(), a.detection.candidates[0].reason),
            MatchKind::Ambiguous => {
                let top = a.detection.candidates[0].score;
                let names: Vec<&str> = a.detection.candidates.iter().filter(|c| c.score == top).map(|c| a.db.profiles[c.index].short_name()).collect();
                format!("{}{}", t("Several profiles match equally — not guessing. Pick yours below: ", "똑같이 맞는 프로필이 여러 개라 추측하지 않았어요. 아래에서 고르세요: "), names.join(", "))
            }
            MatchKind::Unknown => t(
                "This model is not in the list yet. Fn Cheatsheet does not guess — choose a model with the same keyboard below, or request it.",
                "아직 목록에 없는 모델이에요. 추측하지 않으니 키보드가 같은 모델을 아래에서 고르거나 추가를 요청해 주세요.",
            )
            .to_string(),
        };
        let active = a.active();
        (
            a.settings.clone(),
            a.device.clone(),
            a.detection.kind,
            det_text,
            (),
            a.db.errors.clone(),
            a.db.profiles.len(),
            a.db.data_version.clone(),
            a.update_status.clone(),
            active.map(|p| p.sources.iter().map(|s| (s.title.clone(), s.url.clone(), s.kind.clone())).collect::<Vec<_>>()).unwrap_or_default(),
            active.map(|p| p.notes().to_string()).unwrap_or_default(),
        )
    }) else {
        return;
    };
    let _ = list_info;

    // ── Device ──
    let d = Tab::Device;
    stat(d, &device.short_label(), 20, 48, 640, 20, 0, true);
    mk(Some(d), wlit!("EDIT"), &device.report(), (ES_MULTILINE | ES_READONLY) as u32 | WS_TABSTOP.0, 20, 72, 640, 112, ID_DEV_REPORT, false);
    stat(d, &det_text, 20, 190, 640, 36, ID_DET, true);
    btn(d, t("Copy device info", "기기 정보 복사"), 20, 230, 160, ID_COPY);
    btn(d, t("Request this model (GitHub)", "이 모델 추가 요청 (GitHub)"), 188, 230, 230, ID_REQUEST);
    let mut auto_label = t("Use automatic detection (recommended)", "자동 감지 사용 (권장)").to_string();
    if det_kind != MatchKind::Matched {
        auto_label += t("  — not available for this PC", "  — 이 PC는 자동 감지 불가");
    }
    let auto = mk(Some(d), wlit!("BUTTON"), &auto_label, (BS_AUTORADIOBUTTON as u32) | WS_TABSTOP.0 | WS_GROUP.0, 20, 270, 640, 22, ID_AUTO, false);
    mk(Some(d), wlit!("BUTTON"), t("Always use the model I pick:", "내가 고른 모델을 항상 사용:"), BS_AUTORADIOBUTTON as u32, 20, 294, 640, 22, ID_MANUAL, false);
    unsafe {
        let _ = EnableWindow(auto, det_kind == MatchKind::Matched);
    }
    stat(d, t("Search:", "검색:"), 20, 326, 60, 20, 0, false);
    mk(Some(d), wlit!("EDIT"), "", (ES_AUTOHSCROLL as u32) | WS_TABSTOP.0, 84, 322, 576, 24, ID_SEARCH, false);
    mk(Some(d), wlit!("LISTBOX"), "", (LBS_NOTIFY | LBS_NOINTEGRALHEIGHT) as u32 | WS_VSCROLL.0 | WS_TABSTOP.0, 20, 352, 640, 178, ID_LIST, false);
    stat(d, "", 20, 536, 640, 20, ID_ACTIVE, true);
    btn(d, t("Preview", "미리 보기"), 20, 562, 120, ID_PREVIEW);
    btn(d, t("Open my profiles folder", "내 프로필 폴더 열기"), 148, 562, 200, ID_FOLDER);
    btn(d, t("Reload", "다시 불러오기"), 356, 562, 140, ID_RELOAD);
    if !db_errors.is_empty() {
        stat(d, &format!("{}{}", t("Some profile files could not be loaded: ", "불러오지 못한 프로필: "), db_errors.join("; ")), 20, 598, 640, 36, 0, false);
    }

    // ── Trigger & display ──
    let g = Tab::Trigger;
    stat(g, t(
        "On almost every ASUS and Lenovo laptop the Fn key is handled inside the keyboard controller and never reaches Windows, so no app can see it held. Fn Cheatsheet shows the sheet when you hold another key alone — by default Left Ctrl, right next to Fn.",
        "ASUS·레노버 노트북 대부분은 Fn 키를 키보드 컨트롤러 안에서 처리해서 Windows에 신호가 오지 않아요. 그래서 Fn 바로 옆의 왼쪽 Ctrl처럼 다른 키를 혼자 길게 누르면 보여 줘요.",
    ), 20, 48, 640, 52, 0, false);
    check(g, t("Show the cheat sheet while I hold the key", "키를 길게 누르고 있으면 치트시트 보기"), 20, 104, 640, ID_TRIG_ENABLED);
    stat(g, t("Key:", "키:"), 20, 138, 120, 20, 0, false);
    let presets: [(u32, &str); 4] = [
        (VK_LCONTROL, t("Left Ctrl (next to Fn)", "왼쪽 Ctrl (Fn 옆)")),
        (VK_ANY_CTRL, t("Either Ctrl", "양쪽 Ctrl 아무거나")),
        (VK_RSHIFT, t("Right Shift", "오른쪽 Shift")),
        (VK_APPS, t("Menu key", "메뉴 키")),
    ];
    let mut key_items: Vec<String> = presets.iter().map(|p| p.1.to_string()).collect();
    let preset_sel = presets.iter().position(|p| p.0 == settings.trigger_vk && settings.trigger_scan == 0);
    if preset_sel.is_none() {
        key_items.push(format!("{}{}", t("Captured: ", "직접 등록: "), describe_key(settings.trigger_vk, settings.trigger_scan, settings.trigger_extended)));
    }
    let key_refs: Vec<&str> = key_items.iter().map(String::as_str).collect();
    combo(g, 150, 134, 300, ID_TRIG_KEY, &key_refs, preset_sel.unwrap_or(key_items.len() - 1));
    stat(g, "", 20, 174, 200, 20, ID_HOLD_LABEL, false);
    track(g, 220, 168, 300, ID_HOLD, 3, 20, (settings.hold_ms / 100) as i32);
    check(g, t("Don't show over full-screen apps and games", "전체 화면 앱·게임 위에는 띄우지 않기"), 20, 204, 640, ID_SKIP_FS);
    stat(g, t(
        "Pressing any other key, clicking or scrolling while holding cancels it, so Ctrl+C and Ctrl+click work as usual.",
        "누르는 동안 다른 키를 누르거나 클릭·스크롤하면 취소돼요. Ctrl+C, Ctrl+클릭은 평소처럼 동작해요.",
    ), 20, 230, 640, 36, 0, false);
    stat(g, t("Does my Fn key send anything?", "내 Fn 키가 신호를 보내나요?"), 20, 276, 640, 20, 0, true);
    stat(g, t("Press Start, then hold Fn alone for two seconds.", "시작을 누른 뒤 Fn 키만 2초 동안 누르고 있어 보세요."), 20, 298, 640, 20, 0, false);
    btn(g, t("Start", "시작"), 20, 324, 120, ID_LEARN);
    let use_btn = btn(g, t("Use this key", "이 키로 쓰기"), 148, 324, 160, ID_USE_LEARNED);
    unsafe {
        let _ = EnableWindow(use_btn, false);
    }
    stat(g, "", 20, 358, 640, 40, ID_LEARN_STATUS, true);
    stat(g, t("General", "일반"), 20, 408, 640, 20, 0, true);
    check(g, t("Start with Windows", "Windows 시작 시 실행"), 20, 432, 640, ID_STARTUP);
    stat(g, t("Language:", "언어:"), 20, 466, 120, 20, 0, false);
    combo(g, 150, 462, 220, ID_LANG, &[t("Windows language", "Windows 언어 따라"), "한국어", "English"], ["auto", "ko", "en"].iter().position(|v| *v == settings.language).unwrap_or(0));
    stat(g, t("Theme:", "화면 테마:"), 20, 500, 120, 20, 0, false);
    combo(g, 150, 496, 220, ID_THEME, &[t("Windows theme", "Windows 테마 따라"), t("Dark", "어둡게"), t("Light", "밝게")], ["auto", "dark", "light"].iter().position(|v| *v == settings.theme).unwrap_or(0));
    stat(g, "", 20, 540, 130, 20, ID_SCALE_LABEL, false);
    track(g, 150, 534, 260, ID_SCALE, 7, 16, (settings.overlay_scale * 10.0).round() as i32);
    btn(g, t("Preview", "미리 보기"), 420, 534, 120, ID_PREVIEW2);

    // ── About & updates ──
    let a = Tab::About;
    stat(a, &format!("Fn Cheatsheet {APP_VERSION}"), 20, 48, 640, 22, 0, true);
    stat(a, t("Hold a key to see what your laptop's Fn keys do.", "키를 길게 누르면 내 노트북의 Fn 키 기능을 보여 줘요."), 20, 72, 640, 20, 0, false);
    stat(a, &format!("{}{}{} · {}", t("Model data: ", "모델 데이터: "), data_count, t(" profiles", "개 프로필"), data_version), 20, 94, 640, 20, 0, false);
    check(a, t("Update automatically (checks GitHub once a day)", "자동 업데이트 (하루 한 번 GitHub 확인)"), 20, 124, 640, ID_AUTO_UPDATE);
    btn(a, t("Check now", "지금 확인"), 20, 152, 120, ID_CHECK_NOW);
    stat(a, &update_status, 150, 157, 510, 36, ID_UPDATE_STATUS, false);
    stat(a, t(
        "Privacy: the only network access is the update check to GitHub (api.github.com, github.com). The app reads only your PC's model strings (no serial numbers) and never records what you type — the keyboard hook only checks whether the trigger key is held alone. It never runs as administrator.",
        "개인정보: 네트워크는 업데이트 확인(api.github.com, github.com)에만 써요. PC 모델 문자열만 읽고(일련번호는 읽지 않음), 입력한 내용은 기록하지 않아요. 키보드 후킹은 트리거 키를 혼자 누르고 있는지만 확인하고, 관리자 권한으로 실행하지 않아요.",
    ), 20, 198, 640, 70, 0, false);
    btn(a, t("Website & model list", "소개 페이지·모델 목록"), 20, 274, 200, ID_SITE);
    btn(a, "GitHub", 228, 274, 110, ID_GITHUB);
    btn(a, t("Report a problem", "문제 신고"), 346, 274, 140, ID_ISSUES);
    stat(a, t("Sources for the model in use (double-click to open)", "사용 중인 모델의 출처 (두 번 클릭하면 열림)"), 20, 316, 640, 20, 0, true);
    let src_list = mk(Some(a), wlit!("LISTBOX"), "", (LBS_NOTIFY | LBS_NOINTEGRALHEIGHT) as u32 | WS_VSCROLL.0 | WS_HSCROLL.0 | WS_TABSTOP.0, 20, 340, 640, 104, ID_SOURCES, false);
    let mut urls = Vec::new();
    for (title, url, kind) in &sources {
        let label = format!("{}{}", if title.is_empty() { url } else { title }, if kind == "community" { t(" (community)", " (커뮤니티)") } else { "" });
        let ws = w(&label);
        send(src_list, LB_ADDSTRING, 0, ws.pcwstr().0 as isize);
        urls.push(url.clone());
    }
    send(src_list, LB_SETHORIZONTALEXTENT, s(1400) as usize, 0);
    btn(a, t("Open source", "출처 열기"), 20, 450, 120, ID_OPEN_SOURCE);
    stat(a, &notes, 20, 486, 640, 110, 0, false);
    stat(a, t("MIT License · © 2026 jeiel85 · Not affiliated with ASUS or Lenovo.", "MIT License · © 2026 jeiel85 · ASUS·Lenovo와 관계없는 비공식 프로젝트예요."), 20, 604, 640, 20, 0, false);
    ui(|u| u.source_urls = urls);

    // Initial values
    set_checked(ID_AUTO, settings.profile_mode == "auto");
    set_checked(ID_MANUAL, settings.profile_mode == "manual");
    set_checked(ID_TRIG_ENABLED, settings.trigger_enabled);
    set_checked(ID_SKIP_FS, settings.skip_fullscreen);
    set_checked(ID_STARTUP, startup::is_enabled());
    set_checked(ID_AUTO_UPDATE, settings.auto_update);
    set_color(ID_DET, if det_kind == MatchKind::Matched { GOOD } else { WARN });
    update_hold_label();
    update_scale_label();
    fill_list();
    refresh_active();
    select_tab(tab);
}

fn select_tab(tab: Tab) {
    let Some((controls, tabs)) = ui(|u| {
        u.tab = tab;
        (u.controls.clone(), unsafe { GetDlgItem(Some(u.hwnd), ID_TAB).unwrap_or_default() })
    }) else {
        return;
    };
    let idx = match tab {
        Tab::Device => 0,
        Tab::Trigger => 1,
        Tab::About => 2,
    };
    send(tabs, TCM_SETCURSEL, idx, 0);
    unsafe {
        for (t, h) in controls {
            let show = t.is_none() || t == Some(tab);
            let _ = ShowWindow(h, if show { SW_SHOW } else { SW_HIDE });
        }
    }
}

// ───────────────────────── device tab ─────────────────────────

fn fill_list() {
    let list = item(ID_LIST);
    let q = get_text(item(ID_SEARCH)).trim().to_lowercase();
    let Some((rows, labels, selected)) = app::with(|a| {
        let cands: Vec<usize> = a.detection.candidates.iter().map(|c| c.index).collect();
        let mut idx: Vec<usize> = (0..a.db.profiles.len())
            .filter(|&i| {
                let p = &a.db.profiles[i];
                q.is_empty() || format!("{} {} {} {} {}", p.vendor, p.series, p.name, p.match_rule.product.join(" "), p.match_rule.mtm.join(" ")).to_lowercase().contains(&q)
            })
            .collect();
        idx.sort_by(|&x, &y| {
            let (px, py) = (&a.db.profiles[x], &a.db.profiles[y]);
            cands.contains(&y).cmp(&cands.contains(&x)).then_with(|| px.vendor.cmp(&py.vendor)).then_with(|| px.name.cmp(&py.name))
        });
        let labels: Vec<String> = idx
            .iter()
            .map(|&i| {
                let p = &a.db.profiles[i];
                format!(
                    "{}{} · {}{}{}",
                    if cands.contains(&i) { "★ " } else { "" },
                    p.vendor,
                    p.name,
                    if p.years.is_empty() { String::new() } else { format!("  ({})", p.years) },
                    if p.is_builtin() { "" } else { t("  [my file]", "  [내 파일]") }
                )
            })
            .collect();
        let selected = if a.settings.profile_mode == "manual" { idx.iter().position(|&i| a.db.profiles[i].id.eq_ignore_ascii_case(&a.settings.manual_profile_id)) } else { None };
        (idx, labels, selected)
    }) else {
        return;
    };
    ui(|u| u.rows = rows);
    send(list, LB_RESETCONTENT, 0, 0);
    for l in labels {
        let ws = w(&l);
        send(list, LB_ADDSTRING, 0, ws.pcwstr().0 as isize);
    }
    send(list, LB_SETCURSEL, selected.map(|s| s as usize).unwrap_or(usize::MAX), 0);
}

fn refresh_active() {
    let Some((name, mode_auto)) = app::with(|a| (a.active().map(|p| p.short_name().to_string()), a.settings.profile_mode == "auto")) else { return };
    let text = format!(
        "{}{}",
        t("In use: ", "사용 중: "),
        name.clone().unwrap_or_else(|| t("none — the sheet will ask you to pick a model", "없음 — 화면에 모델을 고르라는 안내가 나와요").to_string())
    );
    set_text(item(ID_ACTIVE), &text);
    set_color(ID_ACTIVE, if name.is_some() { GOOD } else { WARN });
    set_checked(ID_AUTO, mode_auto);
    set_checked(ID_MANUAL, !mode_auto);
}

fn copy_to_clipboard(hwnd: HWND, text: &str) {
    unsafe {
        if OpenClipboard(Some(hwnd)).is_err() {
            return;
        }
        let _ = EmptyClipboard();
        let units: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
        if let Ok(mem) = GlobalAlloc(GMEM_MOVEABLE, units.len() * 2) {
            let p = GlobalLock(mem) as *mut u16;
            if !p.is_null() {
                std::ptr::copy_nonoverlapping(units.as_ptr(), p, units.len());
                let _ = GlobalUnlock(mem);
                let _ = SetClipboardData(13 /* CF_UNICODETEXT */, Some(windows::Win32::Foundation::HANDLE(mem.0)));
            }
        }
        let _ = CloseClipboard();
    }
}

fn request_model() {
    let Some((title, body)) = app::with(|a| {
        let d = &a.device;
        let title = format!("Add model: {}{}", d.friendly_version().map(|v| format!("{v} ")).unwrap_or_default(), d.product);
        let body = format!("### Device info (from Fn Cheatsheet {APP_VERSION})\n```\n{}```\n\n### Official manual link (if you have it)\n\n\n### Notes\n", d.report().replace("\r\n", "\n"));
        (title, body)
    }) else {
        return;
    };
    crate::util::open_url(&format!("{REPO_URL}/issues/new?labels=model-request&title={}&body={}", url_encode(&title), url_encode(&body)));
}

fn open_profiles_folder() {
    let dir = user_profiles_dir();
    let _ = std::fs::create_dir_all(&dir);
    let readme = dir.join("README.txt");
    if !readme.exists() {
        let _ = std::fs::write(
            &readme,
            format!("Put your own profile files (*.json) here. Same format as data/profiles.json in the repository:\r\n{REPO_URL}/blob/main/data/profiles.json\r\nA profile with the same id as a built-in one replaces it. Then press Reload in Settings.\r\n"),
        );
    }
    let d = w(&dir.display().to_string());
    unsafe {
        windows::Win32::UI::Shell::ShellExecuteW(None, wlit!("open"), d.pcwstr(), PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL);
    }
}

// ───────────────────────── trigger tab ─────────────────────────

fn update_hold_label() {
    let ms = app::with(|a| a.settings.hold_ms).unwrap_or(900);
    set_text(item(ID_HOLD_LABEL), &format!("{}{:.1}{}", t("Hold for ", "누르고 있을 시간: "), ms as f32 / 1000.0, t(" s", "초")));
}

fn update_scale_label() {
    let sc = app::with(|a| a.settings.overlay_scale).unwrap_or(1.0);
    set_text(item(ID_SCALE_LABEL), &format!("{}{}%", t("Size: ", "크기: "), (sc * 100.0).round()));
}

pub fn describe_key(vk: u32, scan: u32, ext: bool) -> String {
    let mut name = String::new();
    if scan != 0 {
        let mut buf = [0u16; 64];
        let lp = ((scan & 0xFF) << 16) | if ext { 1 << 24 } else { 0 };
        let n = unsafe { windows::Win32::UI::Input::KeyboardAndMouse::GetKeyNameTextW(lp as i32, &mut buf) };
        if n > 0 {
            name = from_wide(&buf);
        }
    }
    let hex = format!("VK 0x{vk:02X}{}", if scan != 0 { format!(", scan 0x{scan:02X}{}", if ext { " E0" } else { "" }) } else { String::new() });
    if name.is_empty() { hex } else { format!("{name} ({hex})") }
}

fn start_learning() {
    let Some(hwnd) = ui(|u| {
        u.learning = true;
        u.learned = None;
        u.learn_saw = false;
        u.hwnd
    }) else {
        return;
    };
    unsafe {
        let _ = EnableWindow(item(ID_USE_LEARNED), false);
        let _ = EnableWindow(item(ID_LEARN), false);
    }
    set_color(ID_LEARN_STATUS, 0);
    set_text(item(ID_LEARN_STATUS), t("Listening… hold Fn now.", "듣는 중… 지금 Fn 키를 누르고 있어 보세요."));
    hook::set_learning(true);
    unsafe {
        SetTimer(Some(hwnd), TIMER_LEARN, 5000, None);
    }
}

/// Key events from the hook while capturing. Shown only in this window; nothing is stored.
pub fn on_learn_key(up: bool, lp: isize) {
    if up {
        return;
    }
    let (vk, scan, flags) = ((lp & 0xFFFF) as u32, ((lp >> 16) & 0xFFFF) as u32, ((lp >> 32) & 0xFFFF_FFFF) as u32);
    let ext = flags & 0x01 != 0;
    let injected = flags & 0x10 != 0;
    let Some(active) = ui(|u| {
        if !u.learning {
            return false;
        }
        u.learn_saw = true;
        if u.learned.is_none() {
            u.learned = Some((vk, scan, ext));
        }
        true
    }) else {
        return;
    };
    if active {
        set_text(
            item(ID_LEARN_STATUS),
            &format!("{}{}{}", t("Received: ", "받은 신호: "), describe_key(vk, scan, ext), if injected { t(" (sent by software)", " (소프트웨어가 보낸 입력)") } else { "" }),
        );
    }
}

fn finish_learning() {
    hook::set_learning(false);
    let Some((hwnd, saw, learned)) = ui(|u| {
        u.learning = false;
        (u.hwnd, u.learn_saw, u.learned)
    }) else {
        return;
    };
    unsafe {
        let _ = KillTimer(Some(hwnd), TIMER_LEARN);
        let _ = EnableWindow(item(ID_LEARN), true);
    }
    let status = item(ID_LEARN_STATUS);
    match learned {
        None if !saw => {
            set_color(ID_LEARN_STATUS, WARN);
            set_text(status, t(
                "Nothing arrived — this laptop's Fn key doesn't reach Windows (normal for most models). Use Left Ctrl, right next to Fn.",
                "아무 신호도 오지 않았어요. 이 노트북의 Fn 키는 Windows에 신호를 보내지 않아요(대부분 그래요). Fn 바로 옆의 왼쪽 Ctrl을 쓰세요.",
            ));
        }
        Some((vk, scan, ext)) => {
            // Typing keys would auto-repeat while held.
            let risky = matches!(vk, 0x08 | 0x09 | 0x0D | 0x20) || (0x30..=0x5A).contains(&vk);
            set_color(ID_LEARN_STATUS, if risky { WARN } else { GOOD });
            set_text(
                status,
                &format!(
                    "{}{}{}",
                    t("First key: ", "처음 받은 키: "),
                    describe_key(vk, scan, ext),
                    if risky { t(" — a typing key would repeat while held. Pick another key.", " — 글자 입력 키라 길게 누르면 계속 입력돼요. 다른 키를 고르세요.") } else { "" }
                ),
            );
            unsafe {
                let _ = EnableWindow(item(ID_USE_LEARNED), !risky);
            }
        }
        None => {}
    }
}

fn use_learned() {
    let Some(Some((vk, scan, ext))) = ui(|u| u.learned) else { return };
    app::with(|a| {
        a.settings.trigger_vk = vk;
        a.settings.trigger_scan = scan;
        a.settings.trigger_extended = ext;
    });
    app::apply_settings();
    build();
    select_tab(Tab::Trigger);
}

// ───────────────────────── about tab ─────────────────────────

pub fn refresh_update_status() {
    let status = app::with(|a| a.update_status.clone()).unwrap_or_default();
    if ui(|_| ()).is_some() {
        set_text(item(ID_UPDATE_STATUS), &status);
    }
}

// ───────────────────────── messages ─────────────────────────

pub fn is_dialog_message(msg: &MSG) -> bool {
    let Some(h) = ui(|u| u.hwnd) else { return false };
    unsafe { IsDialogMessageW(h, msg).as_bool() }
}

fn on_command(hwnd: HWND, id: i32, code: u32) {
    match (id, code) {
        (ID_COPY, BN_CLICKED) => {
            let r = app::with(|a| a.device.report()).unwrap_or_default();
            copy_to_clipboard(hwnd, &r);
        }
        (ID_REQUEST, BN_CLICKED) => request_model(),
        (ID_AUTO, BN_CLICKED) => {
            app::with(|a| a.settings.profile_mode = "auto".into());
            app::apply_settings();
            fill_list();
            refresh_active();
        }
        (ID_MANUAL, BN_CLICKED) => {
            app::with(|a| {
                a.settings.profile_mode = "manual".into();
                if a.db.find(&a.settings.manual_profile_id).is_none() {
                    if let Some(c) = a.detection.candidates.first() {
                        a.settings.manual_profile_id = a.db.profiles[c.index].id.clone();
                    }
                }
            });
            app::apply_settings();
            fill_list();
            refresh_active();
        }
        (ID_SEARCH, EN_CHANGE) => fill_list(),
        (ID_LIST, LBN_SELCHANGE) => {
            let sel = send(item(ID_LIST), LB_GETCURSEL, 0, 0);
            let Some(Some(idx)) = ui(|u| u.rows.get(sel as usize).copied()) else { return };
            app::with(|a| {
                a.settings.profile_mode = "manual".into();
                a.settings.manual_profile_id = a.db.profiles[idx].id.clone();
            });
            app::apply_settings();
            refresh_active();
        }
        (ID_PREVIEW | ID_PREVIEW2, BN_CLICKED) => app::show_overlay(true),
        (ID_FOLDER, BN_CLICKED) => open_profiles_folder(),
        (ID_RELOAD, BN_CLICKED) => {
            app::with(|a| a.reload_profiles());
            app::update_tray_tip();
            build();
        }
        (ID_TRIG_ENABLED, BN_CLICKED) => {
            let on = checked(ID_TRIG_ENABLED);
            app::with(|a| a.settings.trigger_enabled = on);
            app::apply_settings();
        }
        (ID_SKIP_FS, BN_CLICKED) => {
            let on = checked(ID_SKIP_FS);
            app::with(|a| a.settings.skip_fullscreen = on);
            app::apply_settings();
        }
        (ID_TRIG_KEY, CBN_SELCHANGE) => {
            let sel = send(item(ID_TRIG_KEY), CB_GETCURSEL, 0, 0);
            let vks = [VK_LCONTROL, VK_ANY_CTRL, VK_RSHIFT, VK_APPS];
            if let Some(&vk) = vks.get(sel as usize) {
                app::with(|a| {
                    a.settings.trigger_vk = vk;
                    a.settings.trigger_scan = 0;
                    a.settings.trigger_extended = false;
                });
                app::apply_settings();
            }
        }
        (ID_LEARN, BN_CLICKED) => start_learning(),
        (ID_USE_LEARNED, BN_CLICKED) => use_learned(),
        (ID_STARTUP, BN_CLICKED) => startup::set(checked(ID_STARTUP)),
        (ID_LANG, CBN_SELCHANGE) => {
            let sel = send(item(ID_LANG), CB_GETCURSEL, 0, 0) as usize;
            let v = ["auto", "ko", "en"].get(sel).copied().unwrap_or("auto");
            app::with(|a| a.settings.language = v.into());
            app::apply_settings();
            build();
        }
        (ID_THEME, CBN_SELCHANGE) => {
            let sel = send(item(ID_THEME), CB_GETCURSEL, 0, 0) as usize;
            let v = ["auto", "dark", "light"].get(sel).copied().unwrap_or("auto");
            app::with(|a| a.settings.theme = v.into());
            app::apply_settings();
        }
        (ID_AUTO_UPDATE, BN_CLICKED) => {
            let on = checked(ID_AUTO_UPDATE);
            app::with(|a| a.settings.auto_update = on);
            app::apply_settings();
        }
        (ID_CHECK_NOW, BN_CLICKED) => {
            if let Some(main) = app::with(|a| a.hwnd) {
                update::check_now(main, true);
            }
        }
        (ID_SITE, BN_CLICKED) => crate::util::open_url(SITE_URL),
        (ID_GITHUB, BN_CLICKED) => crate::util::open_url(REPO_URL),
        (ID_ISSUES, BN_CLICKED) => crate::util::open_url(&format!("{REPO_URL}/issues")),
        (ID_SOURCES, LBN_DBLCLK) | (ID_OPEN_SOURCE, BN_CLICKED) => {
            let sel = send(item(ID_SOURCES), LB_GETCURSEL, 0, 0);
            let sel = if sel < 0 { 0 } else { sel as usize };
            if let Some(Some(url)) = ui(|u| u.source_urls.get(sel).cloned()) {
                crate::util::open_url(&url);
            }
        }
        _ => {}
    }
}

fn on_hscroll(ctrl: HWND) {
    let pos = send(ctrl, WM_USER /* TBM_GETPOS */, 0, 0) as i32;
    if ctrl == item(ID_HOLD) {
        app::with(|a| a.settings.hold_ms = (pos.clamp(3, 20) * 100) as u32);
        update_hold_label();
    } else if ctrl == item(ID_SCALE) {
        app::with(|a| a.settings.overlay_scale = pos.clamp(7, 16) as f32 / 10.0);
        update_scale_label();
    }
    app::apply_settings();
}

unsafe extern "system" fn proc_(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_COMMAND => {
            on_command(hwnd, (wp.0 & 0xFFFF) as i32, ((wp.0 >> 16) & 0xFFFF) as u32);
            LRESULT(0)
        }
        WM_NOTIFY => {
            let hdr = &*(lp.0 as *const NMHDR);
            if hdr.idFrom == ID_TAB as usize && hdr.code == TCN_SELCHANGE {
                let sel = send(hdr.hwndFrom, TCM_GETCURSEL, 0, 0);
                select_tab(match sel {
                    1 => Tab::Trigger,
                    2 => Tab::About,
                    _ => Tab::Device,
                });
            }
            LRESULT(0)
        }
        WM_HSCROLL => {
            if lp.0 != 0 {
                on_hscroll(HWND(lp.0 as _));
            }
            LRESULT(0)
        }
        WM_TIMER if wp.0 == TIMER_LEARN => {
            finish_learning();
            LRESULT(0)
        }
        WM_CTLCOLORSTATIC => {
            let hdc = HDC(wp.0 as _);
            let id = GetDlgCtrlID(HWND(lp.0 as _));
            let c = ui(|u| u.colors.iter().find(|(i, _)| *i == id).map(|x| x.1)).flatten();
            SetTextColor(hdc, COLORREF(c.unwrap_or(0)));
            SetBkColor(hdc, COLORREF(GetSysColor(COLOR_WINDOW)));
            LRESULT(GetSysColorBrush(COLOR_WINDOW).0 as isize)
        }
        WM_DPICHANGED => {
            let dpi = (wp.0 & 0xFFFF) as u32;
            ui(|u| u.dpi = dpi);
            resize_window(hwnd, dpi, Some(&*(lp.0 as *const RECT)));
            build();
            LRESULT(0)
        }
        WM_CLOSE => {
            let _ = DestroyWindow(hwnd);
            LRESULT(0)
        }
        WM_DESTROY => {
            hook::set_learning(false);
            let fonts = ui(|u| (u.font, u.bold));
            UI.with(|u| {
                if let Ok(mut u) = u.try_borrow_mut() {
                    *u = None;
                }
            });
            if let Some((a, b)) = fonts {
                let _ = DeleteObject(HGDIOBJ(a.0));
                let _ = DeleteObject(HGDIOBJ(b.0));
            }
            app::update_tray_tip();
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}
