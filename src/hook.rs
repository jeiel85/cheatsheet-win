//! "Hold a key alone" detector on WH_KEYBOARD_LL.
//!
//! The hooks live on their own thread with its own message loop, so a busy UI can never make Windows
//! drop them (LowLevelHooksTimeout). Every callback ends in CallNextHookEx — input is never blocked or changed,
//! and key events are never stored: the hook only tracks whether the trigger key is held alone.
use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU32, Ordering};

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::settings::{Settings, VK_ANY_CTRL, VK_LCONTROL, VK_RCONTROL};
use crate::util::log;

pub const WM_APP_SHOW: u32 = WM_APP + 1;
pub const WM_APP_HIDE: u32 = WM_APP + 2;
/// wParam = 1 on key up; lParam = vk | scan << 16 | flags << 32.
pub const WM_APP_LEARN_KEY: u32 = WM_APP + 3;
const WM_APP_RECONFIG: u32 = WM_APP + 10;
const WM_APP_DROP_MOUSE: u32 = WM_APP + 11;
const WM_APP_REINSTALL: u32 = WM_APP + 12;

static MAIN_HWND: AtomicIsize = AtomicIsize::new(0);
static THREAD_ID: AtomicU32 = AtomicU32::new(0);
static CFG_VK: AtomicU32 = AtomicU32::new(VK_LCONTROL);
static CFG_SCAN: AtomicU32 = AtomicU32::new(0);
static CFG_EXT: AtomicBool = AtomicBool::new(false);
static CFG_HOLD: AtomicU32 = AtomicU32::new(900);
static CFG_ENABLED: AtomicBool = AtomicBool::new(true);
static LEARNING: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    Idle,
    Pending,
    Showing,
    /// Cancelled while the trigger is still down; re-arms on release.
    Blocked,
}

struct HookState {
    kb: Option<HHOOK>,
    mouse: Option<HHOOK>,
    state: State,
    trigger_down: bool,
    start: POINT,
    timer: usize,
}

thread_local! {
    static ST: RefCell<HookState> = const { RefCell::new(HookState { kb: None, mouse: None, state: State::Idle, trigger_down: false, start: POINT { x: 0, y: 0 }, timer: 0 }) };
}

fn post_main(msg: u32, wp: usize, lp: isize) {
    let h = MAIN_HWND.load(Ordering::Relaxed);
    if h != 0 {
        unsafe {
            let _ = PostMessageW(Some(HWND(h as _)), msg, WPARAM(wp), LPARAM(lp));
        }
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
    std::thread::Builder::new()
        .name("hook".into())
        .spawn(thread_main)
        .expect("hook thread");
}

pub fn configure(s: &Settings) {
    CFG_VK.store(s.trigger_vk, Ordering::Relaxed);
    CFG_SCAN.store(s.trigger_scan, Ordering::Relaxed);
    CFG_EXT.store(s.trigger_extended, Ordering::Relaxed);
    CFG_HOLD.store(s.hold_ms.max(200), Ordering::Relaxed);
    CFG_ENABLED.store(s.trigger_enabled, Ordering::Relaxed);
    post_self(WM_APP_RECONFIG);
}

/// Windows silently removes a low-level hook whose callback ever times out, and input stops on the secure
/// desktop. Re-installing after unlock/resume (and periodically) keeps the trigger alive.
pub fn reinstall() {
    post_self(WM_APP_REINSTALL);
}

pub fn set_learning(on: bool) {
    LEARNING.store(on, Ordering::Relaxed);
    post_self(WM_APP_RECONFIG);
}

fn thread_main() {
    unsafe {
        let mut msg = MSG::default();
        // Create the thread's message queue before anyone posts to it.
        let _ = PeekMessageW(&mut msg, None, WM_USER, WM_USER, PM_NOREMOVE);
        THREAD_ID.store(windows::Win32::System::Threading::GetCurrentThreadId(), Ordering::Relaxed);
        ensure_keyboard_hook();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            match msg.message {
                WM_APP_RECONFIG => {
                    reset();
                    ensure_keyboard_hook();
                }
                WM_APP_DROP_MOUSE => remove_mouse_hook(),
                WM_APP_REINSTALL => {
                    reset();
                    ST.with(|st| {
                        if let Some(h) = st.borrow_mut().kb.take() {
                            let _ = UnhookWindowsHookEx(h);
                        }
                    });
                    ensure_keyboard_hook();
                }
                WM_TIMER if msg.hwnd.0.is_null() => on_hold_elapsed(msg.wParam.0),
                _ => {
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            }
        }
    }
}

fn module() -> windows::Win32::Foundation::HINSTANCE {
    unsafe { windows::Win32::System::LibraryLoader::GetModuleHandleW(None).unwrap_or_default().into() }
}

fn ensure_keyboard_hook() {
    let want = CFG_ENABLED.load(Ordering::Relaxed) || LEARNING.load(Ordering::Relaxed);
    ST.with(|st| {
        let mut st = st.borrow_mut();
        unsafe {
            if want && st.kb.is_none() {
                match SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_proc), Some(module()), 0) {
                    Ok(h) => st.kb = Some(h),
                    Err(e) => log(&format!("SetWindowsHookEx(keyboard) failed: {e}")),
                }
            } else if !want {
                if let Some(h) = st.kb.take() {
                    let _ = UnhookWindowsHookEx(h);
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

unsafe extern "system" fn keyboard_proc(code: i32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if code >= 0 {
        let k = &*(lp.0 as *const KBDLLHOOKSTRUCT);
        let msg = wp.0 as u32;
        let down = msg == WM_KEYDOWN || msg == WM_SYSKEYDOWN;
        let up = msg == WM_KEYUP || msg == WM_SYSKEYUP;
        if LEARNING.load(Ordering::Relaxed) {
            let lp = (k.vkCode as isize) | ((k.scanCode as isize & 0xFFFF) << 16) | ((k.flags.0 as isize) << 32);
            post_main(WM_APP_LEARN_KEY, up as usize, lp);
        } else if CFG_ENABLED.load(Ordering::Relaxed) && (down || up) {
            on_key(is_trigger(k), down);
        }
    }
    CallNextHookEx(None, code, wp, lp)
}

fn on_key(trigger: bool, down: bool) {
    ST.with(|st| {
        let mut st = st.borrow_mut();
        if trigger {
            if down {
                if st.trigger_down {
                    return; // auto-repeat
                }
                st.trigger_down = true;
                if st.state == State::Idle {
                    begin_pending(&mut st);
                }
            } else {
                st.trigger_down = false;
                let was_showing = st.state == State::Showing;
                stop_pending(&mut st);
                st.state = State::Idle;
                if was_showing {
                    post_main(WM_APP_HIDE, 0, 0);
                }
            }
            return;
        }
        // Another key while the trigger is held: it's being used as a modifier (Ctrl+C …).
        if down && (st.state == State::Pending || st.state == State::Showing) {
            let was_showing = st.state == State::Showing;
            stop_pending(&mut st);
            st.state = if st.trigger_down { State::Blocked } else { State::Idle };
            if was_showing {
                post_main(WM_APP_HIDE, 0, 0);
            }
        }
    });
}

fn begin_pending(st: &mut HookState) {
    st.state = State::Pending;
    unsafe {
        let _ = windows::Win32::UI::WindowsAndMessaging::GetCursorPos(&mut st.start);
        if st.mouse.is_none() {
            st.mouse = SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_proc), Some(module()), 0).ok();
        }
        st.timer = SetTimer(None, 0, CFG_HOLD.load(Ordering::Relaxed), None);
    }
}

fn stop_pending(st: &mut HookState) {
    unsafe {
        if st.timer != 0 {
            let _ = KillTimer(None, st.timer);
            st.timer = 0;
        }
        if let Some(h) = st.mouse.take() {
            let _ = UnhookWindowsHookEx(h);
        }
    }
}

fn reset() {
    ST.with(|st| {
        let mut st = st.borrow_mut();
        let was_showing = st.state == State::Showing;
        stop_pending(&mut st);
        st.state = State::Idle;
        st.trigger_down = false;
        if was_showing {
            post_main(WM_APP_HIDE, 0, 0);
        }
    });
}

fn remove_mouse_hook() {
    ST.with(|st| {
        if let Some(h) = st.borrow_mut().mouse.take() {
            unsafe {
                let _ = UnhookWindowsHookEx(h);
            }
        }
    });
}

fn mouse_button_down() -> bool {
    // VK_LBUTTON, VK_RBUTTON, VK_MBUTTON, VK_XBUTTON1, VK_XBUTTON2
    [0x01, 0x02, 0x04, 0x05, 0x06].iter().any(|&vk| unsafe { GetAsyncKeyState(vk) } as u16 & 0x8000 != 0)
}

fn on_hold_elapsed(timer: usize) {
    ST.with(|st| {
        let mut st = st.borrow_mut();
        if timer != st.timer {
            unsafe {
                let _ = KillTimer(None, timer);
            }
            return;
        }
        unsafe {
            let _ = KillTimer(None, st.timer);
        }
        st.timer = 0;
        if st.state != State::Pending || !st.trigger_down {
            return;
        }
        // A drag with Ctrl held (copy-drag) starts with the button already down: not a request for the sheet.
        if mouse_button_down() {
            st.state = State::Blocked;
            if let Some(h) = st.mouse.take() {
                unsafe {
                    let _ = UnhookWindowsHookEx(h);
                }
            }
            return;
        }
        st.state = State::Showing;
        post_main(WM_APP_SHOW, 0, 0);
    });
}

unsafe extern "system" fn mouse_proc(code: i32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if code >= 0 {
        ST.with(|st| {
            let Ok(mut st) = st.try_borrow_mut() else { return };
            if st.state != State::Pending && st.state != State::Showing {
                return;
            }
            let msg = wp.0 as u32;
            let mut cancel = msg != WM_MOUSEMOVE; // any button or wheel: Ctrl+click, Ctrl+wheel …
            if !cancel && st.state == State::Pending {
                let m = &*(lp.0 as *const MSLLHOOKSTRUCT);
                cancel = (m.pt.x - st.start.x).abs() > 16 || (m.pt.y - st.start.y).abs() > 16;
            }
            if cancel {
                let was_showing = st.state == State::Showing;
                if st.timer != 0 {
                    let _ = KillTimer(None, st.timer);
                    st.timer = 0;
                }
                st.state = State::Blocked;
                post_self(WM_APP_DROP_MOUSE);
                if was_showing {
                    post_main(WM_APP_HIDE, 0, 0);
                }
            }
        });
    }
    CallNextHookEx(None, code, wp, lp)
}
