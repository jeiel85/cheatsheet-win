//! Per-user "start with Windows" through HKCU\...\Run — no admin, no scheduled task.
use windows::Win32::System::Registry::{
    RegCloseKey, RegCreateKeyExW, RegDeleteKeyValueW, RegGetValueW, RegSetValueExW, HKEY, HKEY_CURRENT_USER, KEY_SET_VALUE, REG_OPTION_NON_VOLATILE, REG_SZ, RRF_RT_REG_SZ,
};

use crate::util::{exe_path, from_wide, log, w};

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const VALUE: &str = "FnCheatsheet";

fn command() -> String {
    format!("\"{}\" --background", exe_path().display())
}

fn current() -> Option<String> {
    let key = w(RUN_KEY);
    let val = w(VALUE);
    let mut buf = vec![0u16; 1024];
    let mut size = (buf.len() * 2) as u32;
    let r = unsafe { RegGetValueW(HKEY_CURRENT_USER, key.pcwstr(), val.pcwstr(), RRF_RT_REG_SZ, None, Some(buf.as_mut_ptr() as _), Some(&mut size)) };
    if r.is_ok() {
        let s = from_wide(&buf);
        if !s.is_empty() {
            return Some(s);
        }
    }
    None
}

pub fn is_enabled() -> bool {
    current().is_some()
}

/// The Run entry exists but points at another exe (the app was moved).
pub fn points_elsewhere() -> bool {
    current().map(|c| !c.eq_ignore_ascii_case(&command())).unwrap_or(false)
}

pub fn set(enabled: bool) {
    let key = w(RUN_KEY);
    let val = w(VALUE);
    unsafe {
        if enabled {
            let mut hkey = HKEY::default();
            if RegCreateKeyExW(HKEY_CURRENT_USER, key.pcwstr(), None, None, REG_OPTION_NON_VOLATILE, KEY_SET_VALUE, None, &mut hkey, None).is_ok() {
                let data = w(&command());
                let bytes = std::slice::from_raw_parts(data.units().as_ptr() as *const u8, (data.units().len() + 1) * 2);
                if RegSetValueExW(hkey, val.pcwstr(), None, REG_SZ, Some(bytes)).is_err() {
                    log("startup: RegSetValueExW failed");
                }
                let _ = RegCloseKey(hkey);
            }
        } else {
            let _ = RegDeleteKeyValueW(HKEY_CURRENT_USER, key.pcwstr(), val.pcwstr());
        }
    }
}
