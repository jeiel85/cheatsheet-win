//! Korean / English strings. `t("English", "한국어")`.
use std::sync::atomic::{AtomicBool, Ordering};

static KOREAN: AtomicBool = AtomicBool::new(true);

pub fn apply(setting: &str) {
    let ko = match setting {
        "ko" => true,
        "en" => false,
        _ => {
            // Primary language id of the Windows UI language: LANG_KOREAN = 0x12.
            let lang = unsafe { windows::Win32::Globalization::GetUserDefaultUILanguage() };
            (lang & 0x3ff) == 0x12
        }
    };
    KOREAN.store(ko, Ordering::Relaxed);
}

pub fn korean() -> bool {
    KOREAN.load(Ordering::Relaxed)
}

pub fn t<'a>(en: &'a str, ko: &'a str) -> &'a str {
    if korean() { ko } else { en }
}

/// Picks the label for the current language, falling back to the other one.
pub fn pick<'a>(en: &'a str, ko: &'a str) -> &'a str {
    if korean() {
        if ko.is_empty() { en } else { ko }
    } else if en.is_empty() {
        ko
    } else {
        en
    }
}
