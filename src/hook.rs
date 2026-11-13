//! Low-level keyboard/mouse hooks feeding the trigger state machine (`trigger.rs`).
//!
//! The hooks live on their own thread with a plain GetMessage loop, so a busy UI can never make Windows drop
//! them. Rules that keep Windows from silently removing a hook (LowLevelHooksTimeout):
//! - the callbacks only update the state machine and post messages — they never install or remove hooks, set
//!   timers or do I/O; the message loop applies those afterwards (`WM_APP_SYNC`);
//! - every callback ends in CallNextHookEx: input is never blocked or changed, and key events are never stored.
use std::cell::{Cell, RefCell};
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU32, Ordering};

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::System::SystemInformation::GetTickCount64;
use windows::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::settings::{Settings, VK_ANY_CTRL, VK_LCONTROL, VK_RCONTROL};
use crate::trigger::{Config, Machine, Out};
use crate::util::log;

pub const WM_APP_SHOW: u32 = WM_APP + 1;
pub const WM_APP_HIDE: u32 = WM_APP + 2;
/// wParam = 1 on key up; lParam = vk | scan << 16 | flags << 32.
pub const WM_APP_LEARN_KEY: u32 = WM_APP + 3;
const WM_APP_RECONFIG: u32 = WM_APP + 10;
const WM_APP_REINSTALL: u32 = WM_APP + 12;
/// Posted by the callbacks: apply the wanted mouse hook and timer from the message loop.
const WM_APP_SYNC: u32 = WM_APP + 13;

static MAIN_HWND: AtomicIsize = AtomicIsize::new(0);
static THREAD_ID: AtomicU32 = AtomicU32::new(0);
static CFG_VK: AtomicU32 = AtomicU32::new(VK_LCONTROL);
static CFG_SCAN: AtomicU32 = AtomicU32::new(0);
static CFG_EXT: AtomicBool = AtomicBool::new(false);
static CFG_HOLD: AtomicU32 = AtomicU32::new(900);
static CFG_ENABLED: AtomicBool = AtomicBool::new(true);
/// 0 = hold, 1 = double tap, 2 = both.
static CFG_STYLE: AtomicU32 = AtomicU32::new(0);
static LEARNING: AtomicBool = AtomicBool::new(false);

struct Hooks {
    kb: Option<HHOOK>,
    mouse: Option<HHOOK>,
    m: Machine,
}

thread_local! {
    static ST: RefCell<Hooks> = RefCell::new(Hooks { kb: None, mouse: None, m: Machine::default() });
    /// (deadline the timer is armed for, timer id)
    static TIMER: Cell<(u64, usize)> = const { Cell::new((0, 0)) };
}

fn config() -> Config {
    let style = CFG_STYLE.load(Ordering::Relaxed);
    Config { hold: style != 1, tap: style != 0, hold_ms: CFG_HOLD.load(Ordering::Relaxed) as u64 }
}

fn post_main(msg: u32) {
    let h = MAIN_HWND.load(Ordering::Relaxed);
    if h != 0 {
        unsafe {
            let _ = PostMessageW(Some(HWND(h as _)), msg, WPARAM(0), LPARAM(0));
        }
    }
}

fn post_main_lp(msg: u32, wp: usize, lp: isize) {
    let h = MAIN_HWND.load(Ordering::Relaxed);
    if h != 0 {
        unsafe {
            let _ = PostMessageW(Some(HWND(h as _)), msg, WPARAM(wp), LPARAM(lp));
        }
    }
}

fn emit(o: Option<Out>) {
    match o {
        Some(Out::Show) => post_main(WM_APP_SHOW),
        Some(Out::Hide) => post_main(WM_APP_HIDE),
        None => {}
    }
}

fn post_self(msg: u32) {
    let t = THREAD_ID.load(Ordering::Relaxed);
    if t != 0 {
        unsafe {
            let _ = PostThreadMessageW(t, msg, WPARAM(0), LPARAM(0));
        }
    }
}

pub fn start(main: HWND, s: &Settings) {
    MAIN_HWND.store(main.0 as isize, Ordering::Relaxed);
    configure(s);
    std::thread::Builder::new().name("hook".into()).spawn(thread_main).expect("hook thread");
}

pub fn configure(s: &Settings) {
    CFG_VK.store(s.trigger_vk, Ordering::Relaxed);
    CFG_SCAN.store(s.trigger_scan, Ordering::Relaxed);
    CFG_EXT.store(s.trigger_extended, Ordering::Relaxed);
    CFG_HOLD.store(s.hold_ms.max(200), Ordering::Relaxed);
    CFG_ENABLED.store(s.trigger_enabled, Ordering::Relaxed);
    CFG_STYLE.store(
        match s.trigger_style.as_str() {
            "double" => 1,
            "both" => 2,
            _ => 0,
        },
        Ordering::Relaxed,
    );
    post_self(WM_APP_RECONFIG);
}

/// Re-installs the keyboard hook (after unlock/resume, and hourly), in case Windows removed it.
pub fn reinstall() {
    post_self(WM_APP_REINSTALL);
}

pub fn set_learning(on: bool) {
    LEARNING.store(on, Ordering::Relaxed);
    post_self(WM_APP_RECONFIG);
}

fn module() -> windows::Win32::Foundation::HINSTANCE {
    unsafe { windows::Win32::System::LibraryLoader::GetModuleHandleW(None).unwrap_or_default().into() }
}

fn thread_main() {
    unsafe {
        let mut msg = MSG::default();
        // Create the thread's message queue before anyone posts to it.
        let _ = PeekMessageW(&mut msg, None, WM_USER, WM_USER, PM_NOREMOVE);
        THREAD_ID.store(windows::Win32::System::Threading::GetCurrentThreadId(), Ordering::Relaxed);
        set_keyboard_hook(false);
        // Hook callbacks are serviced while the thread waits in GetMessage.
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            match msg.message {
                WM_TIMER if msg.hwnd.0.is_null() => {
                    let _ = KillTimer(None, msg.wParam.0);
                    TIMER.with(|t| {
                        if t.get().1 == msg.wParam.0 {
                            t.set((0, 0));
                        }
                    });
                    let o = ST.with(|st| st.borrow_mut().m.tick(GetTickCount64() + 5, mouse_button_down()));
                    emit(o);
                }
                WM_APP_RECONFIG => {
                    let o = ST.with(|st| st.borrow_mut().m.reset());
                    emit(o);
                    set_keyboard_hook(false);
                }
                WM_APP_REINSTALL => {
                    let o = ST.with(|st| st.borrow_mut().m.reset());
                    emit(o);
                    set_keyboard_hook(true);
                }
                WM_APP_SYNC => {}
                _ => {
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            }
            sync();
        }
    }
}

/// Applies the state machine's wishes: mouse hook on/off and the deadline timer. Message loop only.
fn sync() {
    let (want_mouse, deadline) = ST.with(|st| {
        let st = st.borrow();
        (st.m.want_mouse, st.m.deadline)
    });
    ST.with(|st| {
        let mut st = st.borrow_mut();
        unsafe {
            if want_mouse && st.mouse.is_none() {
                st.mouse = SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_proc), Some(module()), 0).ok();
            } else if !want_mouse {
                if let Some(h) = st.mouse.take() {
                    let _ = UnhookWindowsHookEx(h);
                }
            }
        }
    });
    TIMER.with(|t| {
        let (armed, id) = t.get();
        if armed == deadline {
            return;
        }
        unsafe {
            if id != 0 {
                let _ = KillTimer(None, id);
            }
            if deadline == 0 {
                t.set((0, 0));
            } else {
                let ms = deadline.saturating_sub(GetTickCount64()).clamp(1, u32::MAX as u64 - 1) as u32;
                t.set((deadline, SetTimer(None, 0, ms, None)));
            }
        }
    });
}

fn set_keyboard_hook(force_reinstall: bool) {
    let want = CFG_ENABLED.load(Ordering::Relaxed) || LEARNING.load(Ordering::Relaxed);
    ST.with(|st| {
        let mut st = st.borrow_mut();
        unsafe {
            if force_reinstall || !want {
                if let Some(h) = st.kb.take() {
                    let _ = UnhookWindowsHookEx(h);
                }
            }
            if want && st.kb.is_none() {
                match SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_proc), Some(module()), 0) {
                    Ok(h) => st.kb = Some(h),
                    Err(e) => log(&format!("SetWindowsHookEx(keyboard) failed: {e}")),
                }
            }
        }
    });
}

fn is_trigger(k: &KBDLLHOOKSTRUCT) -> bool {
    let vk = CFG_VK.load(Ordering::Relaxed);
    if vk == VK_ANY_CTRL {
        return k.vkCode == VK_LCONTROL || k.vkCode == VK_RCONTROL;
    }
    if k.vkCode != vk {
        return false;
    }
    let scan = CFG_SCAN.load(Ordering::Relaxed);
    if scan != 0 {
        let ext = (k.flags.0 & LLKHF_EXTENDED.0) != 0;
        return k.scanCode == scan && ext == CFG_EXT.load(Ordering::Relaxed);
    }
    true
}

fn mouse_button_down() -> bool {
    // VK_LBUTTON, VK_RBUTTON, VK_MBUTTON, VK_XBUTTON1, VK_XBUTTON2
    [0x01, 0x02, 0x04, 0x05, 0x06].iter().any(|&vk| unsafe { GetAsyncKeyState(vk) } as u16 & 0x8000 != 0)
}

unsafe extern "system" fn keyboard_proc(code: i32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if code >= 0 {
        let k = &*(lp.0 as *const KBDLLHOOKSTRUCT);
        let msg = wp.0 as u32;
        let down = msg == WM_KEYDOWN || msg == WM_SYSKEYDOWN;
        let up = msg == WM_KEYUP || msg == WM_SYSKEYUP;
        if LEARNING.load(Ordering::Relaxed) {
            let lp = (k.vkCode as isize) | ((k.scanCode as isize & 0xFFFF) << 16) | ((k.flags.0 as isize) << 32);
            post_main_lp(WM_APP_LEARN_KEY, up as usize, lp);
        } else if CFG_ENABLED.load(Ordering::Relaxed) && (down || up) {
            let mut pt = POINT::default();
            let _ = GetCursorPos(&mut pt);
            let cfg = config();
            let o = ST.with(|st| st.try_borrow_mut().ok().and_then(|mut st| st.m.key(&cfg, is_trigger(k), down, GetTickCount64(), (pt.x, pt.y), mouse_button_down())));
            emit(o);
            post_self(WM_APP_SYNC);
        }
    }
    CallNextHookEx(None, code, wp, lp)
}

unsafe extern "system" fn mouse_proc(code: i32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if code >= 0 {
        let msg = wp.0 as u32;
        let is_move = msg == WM_MOUSEMOVE;
        let pos = {
            let m = &*(lp.0 as *const MSLLHOOKSTRUCT);
            (m.pt.x, m.pt.y)
        };
        let (o, changed) = ST.with(|st| {
            let Ok(mut st) = st.try_borrow_mut() else { return (None, false) };
            let before = (st.m.want_mouse, st.m.deadline);
            let o = st.m.mouse(is_move, pos);
            (o, before != (st.m.want_mouse, st.m.deadline))
        });
        emit(o);
        if changed {
            post_self(WM_APP_SYNC);
        }
    }
    CallNextHookEx(None, code, wp, lp)
}
