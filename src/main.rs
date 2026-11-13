//! Fn Cheatsheet — hold a key to see what your laptop's Fn keys do.
//!
//!   FnCheatsheet.exe                    tray app (opens Settings if already running)
//!   FnCheatsheet.exe --background       tray app, quiet start (used by "Start with Windows")
//!   FnCheatsheet.exe --selftest <match-tests.json> [--out <file>]
//!   FnCheatsheet.exe --render <profile-id|unknown> <out.png> [ko|en] [dark|light]
//!   FnCheatsheet.exe --device           print this PC's model strings and the match result
#![windows_subsystem = "windows"]

mod app;
mod data;
mod device;
mod glyphs;
mod hook;
mod i18n;
mod matcher;
mod overlay;
mod selftest;
mod settings;
mod startup;
mod ui;
mod update;
mod util;

use windows::core::w as wlit;
use windows::Win32::Foundation::{CloseHandle, GetLastError, ERROR_ALREADY_EXISTS, LPARAM, WAIT_ABANDONED, WAIT_OBJECT_0, WPARAM};
use windows::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED};
use windows::Win32::System::Console::{AttachConsole, ATTACH_PARENT_PROCESS};
use windows::Win32::System::Threading::{CreateMutexW, WaitForSingleObject};
use windows::Win32::UI::Controls::{InitCommonControlsEx, ICC_BAR_CLASSES, ICC_LINK_CLASS, ICC_STANDARD_CLASSES, ICC_TAB_CLASSES, INITCOMMONCONTROLSEX};
use windows::Win32::UI::WindowsAndMessaging::{FindWindowW, PostMessageW};

fn console_command(args: &[String]) -> Option<i32> {
    let cmd = args.first()?.as_str();
    if !matches!(cmd, "--selftest" | "--render" | "--device") {
        return None;
    }
    unsafe {
        let _ = AttachConsole(ATTACH_PARENT_PROCESS);
    }
    let mut lines: Vec<String> = Vec::new();
    let code = match cmd {
        "--selftest" => {
            let db = data::ProfileDb::load(None);
            let cases = args.get(1).filter(|a| !a.starts_with("--")).and_then(|p| std::fs::read_to_string(p).ok());
            if args.get(1).is_some_and(|a| !a.starts_with("--")) && cases.is_none() {
                lines.push(format!("FAIL cannot read {}", args[1]));
                1
            } else {
                let (report, fails) = selftest::run(&db, cases.as_deref());
                lines.extend(report);
                (fails > 0) as i32
            }
        }
        "--device" => {
            let dev = device::DeviceInfo::read();
            lines.push(dev.report().trim_end().replace("\r\n", "\n"));
            let db = data::ProfileDb::load(Some(&util::user_profiles_dir()));
            let r = matcher::match_device(&dev, &db.profiles);
            let c: Vec<String> = r.candidates.iter().map(|c| format!("{} ({}, {})", db.profiles[c.index].id, c.score, c.reason)).collect();
            lines.push(format!("Match: {:?} -> {}", r.kind, c.join(", ")));
            0
        }
        _ => {
            if args.len() < 3 {
                lines.push("usage: --render <profile-id|unknown> <out.png> [ko|en] [dark|light]".into());
                2
            } else {
                unsafe {
                    let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
                }
                i18n::apply(args.get(3).map(String::as_str).unwrap_or("ko"));
                let pal = if args.get(4).map(String::as_str) == Some("light") { overlay::LIGHT } else { overlay::DARK };
                let db = data::ProfileDb::load(None);
                let profile = db.find(&args[1]);
                if args[1] != "unknown" && profile.is_none() {
                    lines.push(format!("no profile {}", args[1]));
                    2
                } else {
                    let m = overlay::OverlayModel {
                        profile,
                        status: if profile.is_some() { format!("{} · {}", i18n::t("Detected automatically", "자동 감지됨"), i18n::t("model code", "모델 코드")) } else { String::new() },
                        device_label: "Example Corp · Example Laptop 14".into(),
                        ambiguous: false,
                    };
                    match overlay::render_png(&m, &pal, &args[2], 2.0) {
                        Ok(()) => {
                            lines.push(format!("wrote {}", args[2]));
                            0
                        }
                        Err(e) => {
                            lines.push(format!("render failed: {e}"));
                            1
                        }
                    }
                }
            }
        }
    };
    overlay::shutdown();
    let text = lines.join("\n");
    println!("{text}");
    if let Some(i) = args.iter().position(|a| a == "--out") {
        if let Some(p) = args.get(i + 1) {
            let _ = std::fs::write(p, format!("{text}\n"));
        }
    }
    Some(code)
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Some(code) = console_command(&args) {
        std::process::exit(code);
    }
    let background = args.iter().any(|a| a == "--background");
    let updated_from = args.iter().position(|a| a == "--updated").and_then(|i| args.get(i + 1).cloned());

    unsafe {
        // Single instance per user session.
        let mutex = match CreateMutexW(None, false, wlit!("Local\\FnCheatsheet-3C8E2B71-single-instance")) {
            Ok(m) => m,
            Err(_) => std::process::exit(1),
        };
        if GetLastError() == ERROR_ALREADY_EXISTS {
            if updated_from.is_some() {
                // Started by the updater: wait for the old version to exit.
                let r = WaitForSingleObject(mutex, 15_000);
                if r != WAIT_OBJECT_0 && r != WAIT_ABANDONED {
                    std::process::exit(1);
                }
            } else {
                if !background {
                    match FindWindowW(app::MAIN_CLASS, None) {
                        Ok(h) => {
                            let _ = PostMessageW(Some(h), app::WM_APP_OPEN_SETTINGS, WPARAM(0), LPARAM(0));
                        }
                        Err(e) => util::log(&format!("second instance: main window not found: {e}")),
                    }
                }
                let _ = CloseHandle(mutex);
                return;
            }
        } else {
            let _ = WaitForSingleObject(mutex, 0); // take ownership
        }

        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let icc = INITCOMMONCONTROLSEX {
            dwSize: std::mem::size_of::<INITCOMMONCONTROLSEX>() as u32,
            dwICC: ICC_STANDARD_CLASSES | ICC_TAB_CLASSES | ICC_BAR_CLASSES | ICC_LINK_CLASS,
        };
        let _ = InitCommonControlsEx(&icc);
        let code = app::run(background, updated_from);
        overlay::shutdown();
        let _ = CloseHandle(mutex);
        std::process::exit(code);
    }
}
