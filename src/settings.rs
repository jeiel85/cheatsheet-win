//! %APPDATA%\FnCheatsheet\settings.json — values are clamped on load, so a hand-edited file can't break anything.
use serde::{Deserialize, Serialize};
use std::fs;

use crate::util::{app_data_dir, log};

pub const VK_LCONTROL: u32 = 0xA2;
pub const VK_RCONTROL: u32 = 0xA3;
pub const VK_RSHIFT: u32 = 0xA1;
pub const VK_APPS: u32 = 0x5D;
/// Pseudo code for "either Ctrl" (VK_LBUTTON's value, which a keyboard hook never receives).
pub const VK_ANY_CTRL: u32 = 0x01;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Settings {
    /// "auto" = detected profile, "manual" = manual_profile_id.
    pub profile_mode: String,
    pub manual_profile_id: String,
    pub trigger_vk: u32,
    /// Hardware scan code of a captured key (0 = any scan code for trigger_vk).
    pub trigger_scan: u32,
    pub trigger_extended: bool,
    pub hold_ms: u32,
    pub trigger_enabled: bool,
    /// "hold" | "double" | "both"
    pub trigger_style: String,
    /// Don't show while a full-screen app or game is in front.
    pub skip_fullscreen: bool,
    /// "auto" | "ko" | "en"
    pub language: String,
    /// "auto" | "dark" | "light"
    pub theme: String,
    pub overlay_scale: f32,
    pub auto_update: bool,
    pub last_update_check: u64,
    pub first_run_done: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            profile_mode: "auto".into(),
            manual_profile_id: String::new(),
            trigger_vk: VK_LCONTROL,
            trigger_scan: 0,
            trigger_extended: false,
            hold_ms: 900,
            trigger_enabled: true,
            trigger_style: "hold".into(),
            skip_fullscreen: true,
            language: "auto".into(),
            theme: "auto".into(),
            overlay_scale: 1.0,
            auto_update: true,
            last_update_check: 0,
            first_run_done: false,
        }
    }
}

fn one_of(v: &str, allowed: &[&str], def: &str) -> String {
    if allowed.contains(&v) { v.to_string() } else { def.to_string() }
}

impl Settings {
    pub fn load() -> Settings {
        let path = app_data_dir().join("settings.json");
        let text = match fs::metadata(&path) {
            Ok(m) if m.len() <= 64 * 1024 => fs::read_to_string(&path).unwrap_or_default(),
            Ok(_) => {
                log("settings.json too large, using defaults");
                String::new()
            }
            Err(_) => String::new(),
        };
        if text.is_empty() {
            return Settings::default();
        }
        match serde_json::from_str::<Settings>(text.trim_start_matches('\u{feff}')) {
            Ok(s) => s.sanitized(),
            Err(e) => {
                log(&format!("settings.json unreadable ({e}), using defaults"));
                Settings::default()
            }
        }
    }

    pub fn sanitized(mut self) -> Settings {
        self.profile_mode = one_of(&self.profile_mode, &["auto", "manual"], "auto");
        self.trigger_style = one_of(&self.trigger_style, &["hold", "double", "both"], "hold");
        self.language = one_of(&self.language, &["auto", "ko", "en"], "auto");
        self.theme = one_of(&self.theme, &["auto", "dark", "light"], "auto");
        self.trigger_vk = self.trigger_vk.clamp(1, 254);
        self.trigger_scan = self.trigger_scan.min(0xFFFF);
        self.hold_ms = self.hold_ms.clamp(200, 5000);
        self.overlay_scale = if self.overlay_scale.is_finite() { self.overlay_scale.clamp(0.6, 2.0) } else { 1.0 };
        self.manual_profile_id.truncate(200);
        self
    }

    /// Atomic save: write a temp file, then replace.
    pub fn save(&self) {
        let dir = app_data_dir();
        let _ = fs::create_dir_all(&dir);
        let path = dir.join("settings.json");
        let tmp = dir.join("settings.json.tmp");
        match serde_json::to_string_pretty(self) {
            Ok(text) => {
                if fs::write(&tmp, text).is_ok() {
                    if let Err(e) = fs::rename(&tmp, &path) {
                        log(&format!("settings save failed: {e}"));
                    }
                }
            }
            Err(e) => log(&format!("settings serialize failed: {e}")),
        }
    }
}
