//! What a key currently has switched on, read when the sheet is shown — nothing runs while it is hidden.
//!
//! - Standard Windows state, the same on every laptop and picked by the key's icon: speaker and mic mute, volume,
//!   brightness, display mode (only with a second display connected) and airplane mode.
//! - Maker-specific modes (e.g. the ASUS fan mode): the profile's `state` points at a registry DWORD that the
//!   maker's own software keeps in step with the key.
//!
//! Everything here is read-only and works without admin rights.
use windows::core::GUID;
use windows::Win32::Devices::Display::*;
use windows::Win32::Foundation::{LocalFree, ERROR_SUCCESS, HLOCAL};
use windows::Win32::Media::Audio::Endpoints::IAudioEndpointVolume;
use windows::Win32::Media::Audio::{eCapture, eConsole, eRender, EDataFlow, IMMDeviceEnumerator, MMDeviceEnumerator};
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_ALL};
use windows::Win32::System::Power::{GetSystemPowerStatus, PowerGetActiveScheme, PowerReadACValueIndex, PowerReadDCValueIndex, SYSTEM_POWER_STATUS};
use windows::Win32::System::Registry::{RegGetValueW, HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, RRF_RT_REG_DWORD, RRF_SUBKEY_WOW6464KEY};

use crate::data::{KeyHint, KeyState, StateValue};
use crate::i18n::t;
use crate::util::w;

/// Splits "HKLM\Software\…" into the root key and the subkey path.
pub fn split_root(path: &str) -> Option<(HKEY, &str)> {
    let (root, rest) = path.split_once('\\')?;
    let hkey = match root.to_ascii_uppercase().as_str() {
        "HKLM" | "HKEY_LOCAL_MACHINE" => HKEY_LOCAL_MACHINE,
        "HKCU" | "HKEY_CURRENT_USER" => HKEY_CURRENT_USER,
        _ => return None,
    };
    let rest = rest.trim_matches('\\');
    if rest.is_empty() {
        return None;
    }
    Some((hkey, rest))
}

fn read_dword(path: &str, value: &str) -> Option<u32> {
    let (root, sub) = split_root(path)?;
    let sub = w(sub);
    let val = w(value);
    let mut data = 0u32;
    let mut size = 4u32;
    // The 64-bit view, so a 32-bit build would read the same key the vendor service writes.
    let flags = RRF_RT_REG_DWORD | RRF_SUBKEY_WOW6464KEY;
    let r = unsafe { RegGetValueW(root, sub.pcwstr(), val.pcwstr(), flags, None, Some(&mut data as *mut u32 as _), Some(&mut size)) };
    r.is_ok().then_some(data)
}

/// The current state's entry, or None when the value is missing (vendor software not installed) or unknown.
pub fn current(s: &KeyState) -> Option<&StateValue> {
    let v = read_dword(&s.registry, &s.value)?;
    s.values.iter().find(|x| x.is == v)
}

// ───────────────────────── standard Windows state ─────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DisplayMode {
    PcOnly,
    Duplicate,
    Extend,
    SecondOnly,
}

/// One reading of the standard state, taken when the sheet opens. None = could not read (or not applicable).
#[derive(Clone, Debug, Default)]
pub struct System {
    pub speaker_muted: Option<bool>,
    pub volume: Option<u32>,
    pub mic_muted: Option<bool>,
    pub brightness: Option<u32>,
    pub display: Option<DisplayMode>,
    pub airplane: Option<bool>,
}

impl System {
    pub fn read() -> System {
        let (speaker_muted, volume) = endpoint(eRender);
        let (mic_muted, _) = endpoint(eCapture);
        System {
            speaker_muted,
            volume,
            mic_muted,
            brightness: brightness(),
            display: display_mode(),
            // Default value of this key: 1 while airplane mode is on.
            airplane: read_dword(r"HKLM\SYSTEM\CurrentControlSet\Control\RadioManagement\SystemRadioState", "").map(|v| v == 1),
        }
    }
}

/// Mute and volume (0–100) of the default console device for `flow`. COM is already initialized on this thread.
fn endpoint(flow: EDataFlow) -> (Option<bool>, Option<u32>) {
    let vol: windows::core::Result<IAudioEndpointVolume> = unsafe {
        CoCreateInstance::<_, IMMDeviceEnumerator>(&MMDeviceEnumerator, None, CLSCTX_ALL)
            .and_then(|e| e.GetDefaultAudioEndpoint(flow, eConsole))
            .and_then(|d| d.Activate(CLSCTX_ALL, None))
    };
    let Ok(vol) = vol else { return (None, None) };
    unsafe {
        let muted = vol.GetMute().ok().map(|b| b.as_bool());
        let level = vol.GetMasterVolumeLevelScalar().ok().map(|f| (f * 100.0).round().clamp(0.0, 100.0) as u32);
        (muted, level)
    }
}

const VIDEO_SUBGROUP: GUID = GUID::from_u128(0x7516b95f_f776_4464_8c53_06167f40cc99);
const VIDEO_BRIGHTNESS: GUID = GUID::from_u128(0xaded5e82_b909_4619_9949_f5d71dac0bcb);

/// The built-in panel's brightness as the active power plan stores it (Windows keeps it in step with the
/// brightness keys and the slider), for the current power source.
fn brightness() -> Option<u32> {
    unsafe {
        let mut scheme: *mut GUID = std::ptr::null_mut();
        if PowerGetActiveScheme(None, &mut scheme) != ERROR_SUCCESS || scheme.is_null() {
            return None;
        }
        let mut ps = SYSTEM_POWER_STATUS::default();
        let on_battery = GetSystemPowerStatus(&mut ps).is_ok() && ps.ACLineStatus == 0;
        let mut v = 0u32;
        // (The two functions declare different return types; both are Win32 error codes.)
        let ok = if on_battery {
            PowerReadDCValueIndex(None, Some(scheme), Some(&VIDEO_SUBGROUP), Some(&VIDEO_BRIGHTNESS), &mut v) == ERROR_SUCCESS.0
        } else {
            PowerReadACValueIndex(None, Some(scheme), Some(&VIDEO_SUBGROUP), Some(&VIDEO_BRIGHTNESS), &mut v) == ERROR_SUCCESS
        };
        let _ = LocalFree(Some(HLOCAL(scheme as _)));
        ok.then_some(v.min(100))
    }
}

fn query_paths(flags: QUERY_DISPLAY_CONFIG_FLAGS, topology: Option<&mut DISPLAYCONFIG_TOPOLOGY_ID>) -> Option<Vec<DISPLAYCONFIG_PATH_INFO>> {
    unsafe {
        let (mut np, mut nm) = (0u32, 0u32);
        if GetDisplayConfigBufferSizes(flags, &mut np, &mut nm) != ERROR_SUCCESS {
            return None;
        }
        let mut paths = vec![DISPLAYCONFIG_PATH_INFO::default(); np as usize];
        let mut modes = vec![DISPLAYCONFIG_MODE_INFO::default(); nm as usize];
        let topo = topology.map(|t| t as *mut _);
        if QueryDisplayConfig(flags, &mut np, paths.as_mut_ptr(), &mut nm, modes.as_mut_ptr(), topo) != ERROR_SUCCESS {
            return None;
        }
        paths.truncate(np as usize);
        Some(paths)
    }
}

/// The Win+P mode, but only when a second display is connected: with one screen there is nothing to show.
fn display_mode() -> Option<DisplayMode> {
    let all = query_paths(QDC_ALL_PATHS, None)?;
    let mut targets: Vec<(u32, i32, u32)> = all
        .iter()
        .filter(|p| p.targetInfo.targetAvailable.as_bool())
        .map(|p| (p.targetInfo.adapterId.LowPart, p.targetInfo.adapterId.HighPart, p.targetInfo.id))
        .collect();
    targets.sort_unstable();
    targets.dedup();
    if targets.len() < 2 {
        return None;
    }
    let mut topo = DISPLAYCONFIG_TOPOLOGY_ID::default();
    query_paths(QDC_DATABASE_CURRENT, Some(&mut topo))?;
    match topo {
        DISPLAYCONFIG_TOPOLOGY_INTERNAL => Some(DisplayMode::PcOnly),
        DISPLAYCONFIG_TOPOLOGY_CLONE => Some(DisplayMode::Duplicate),
        DISPLAYCONFIG_TOPOLOGY_EXTEND => Some(DisplayMode::Extend),
        DISPLAYCONFIG_TOPOLOGY_EXTERNAL => Some(DisplayMode::SecondOnly),
        _ => None,
    }
}

// ───────────────────────── badges ─────────────────────────

/// What the overlay draws next to a key.
#[derive(Clone, Debug, PartialEq)]
pub struct Badge {
    pub text: String,
    /// Highlighted (something is switched on, or a mode) rather than a plain value.
    pub on: bool,
    /// A named mode ("Whisper"): the Fn+key list prefixes it with "Now:".
    pub mode: bool,
}

fn on_off(on: bool) -> Badge {
    Badge { text: if on { t("On", "켜짐") } else { t("Off", "꺼짐") }.to_string(), on, mode: false }
}

fn value(text: String) -> Badge {
    Badge { text, on: false, mode: false }
}

/// The badge for `k`: the profile's own registry state first, else the standard state its icon stands for.
pub fn badge(k: &KeyHint, sys: &System) -> Option<Badge> {
    if let Some(s) = &k.state {
        return current(s).map(|v| Badge { text: v.label().to_string(), on: true, mode: true });
    }
    standard_badge(&k.icon, sys)
}

pub fn standard_badge(icon: &str, sys: &System) -> Option<Badge> {
    match icon {
        "mute" => sys.speaker_muted.map(on_off),
        "mic-mute" => sys.mic_muted.map(on_off),
        "airplane" => sys.airplane.map(on_off),
        "vol-up" | "vol-down" => sys.volume.map(|v| value(format!("{v}%"))),
        "bright-up" | "bright-down" => sys.brightness.map(|v| value(format!("{v}%"))),
        "display" => sys.display.map(|d| {
            let s = match d {
                DisplayMode::PcOnly => t("PC only", "PC 화면만"),
                DisplayMode::Duplicate => t("Duplicate", "복제"),
                DisplayMode::Extend => t("Extend", "확장"),
                DisplayMode::SecondOnly => t("Second only", "두 번째만"),
            };
            value(s.to_string())
        }),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::ProfileDb;

    #[test]
    fn split_root_accepts_short_and_long_names() {
        assert_eq!(split_root(r"HKLM\SOFTWARE\ASUS").map(|(h, s)| (h == HKEY_LOCAL_MACHINE, s)), Some((true, r"SOFTWARE\ASUS")));
        assert_eq!(split_root(r"hkey_current_user\Software\X\").map(|(h, s)| (h == HKEY_CURRENT_USER, s)), Some((true, r"Software\X")));
        assert!(split_root(r"HKCR\foo").is_none());
        assert!(split_root("HKLM").is_none());
        assert!(split_root(r"HKLM\").is_none());
    }

    #[test]
    fn builtin_states_are_well_formed() {
        let db = ProfileDb::load(None);
        assert!(db.errors.is_empty(), "{:?}", db.errors);
        let mut n = 0;
        for p in &db.profiles {
            for k in &p.keys {
                let Some(s) = &k.state else { continue };
                n += 1;
                let at = format!("{} / {}", p.id, k.key);
                assert!(split_root(&s.registry).is_some(), "{at}: bad registry path {}", s.registry);
                assert!(!s.value.trim().is_empty(), "{at}: no value name");
                assert!(s.values.len() >= 2, "{at}: needs at least two values");
                for v in &s.values {
                    assert!(!v.en.trim().is_empty() && !v.ko.trim().is_empty(), "{at}: value {} needs en and ko", v.is);
                    assert_eq!(s.values.iter().filter(|x| x.is == v.is).count(), 1, "{at}: value {} listed twice", v.is);
                }
            }
        }
        assert!(n >= 1, "expected the ExpertBook fan-mode state");
    }

    #[test]
    fn missing_value_reads_as_none() {
        let s = KeyState {
            registry: r"HKCU\Software\FnCheatsheet-test-does-not-exist".into(),
            value: "Nope".into(),
            values: vec![StateValue { is: 0, en: "A".into(), ko: "가".into() }],
        };
        assert!(current(&s).is_none());
    }

    #[test]
    fn standard_badges_follow_the_icon() {
        let sys = System { speaker_muted: Some(true), volume: Some(40), mic_muted: Some(false), brightness: Some(70), display: Some(DisplayMode::Extend), airplane: None };
        assert_eq!(standard_badge("mute", &sys).map(|b| b.on), Some(true));
        assert_eq!(standard_badge("mic-mute", &sys).map(|b| b.on), Some(false));
        assert_eq!(standard_badge("vol-up", &sys).map(|b| b.text), Some("40%".to_string()));
        assert_eq!(standard_badge("bright-down", &sys).map(|b| b.text), Some("70%".to_string()));
        assert!(standard_badge("display", &sys).is_some());
        assert!(standard_badge("airplane", &sys).is_none(), "unreadable state shows nothing");
        assert!(standard_badge("calculator", &sys).is_none());
        assert!(standard_badge("display", &System::default()).is_none(), "one screen: no display badge");
    }

    #[test]
    fn reading_this_pc_returns_cleanly() {
        // Values depend on the machine; this only checks that every reader comes back without panicking.
        let s = System::read();
        assert!(s.volume.is_none_or(|v| v <= 100));
        assert!(s.brightness.is_none_or(|v| v <= 100));
    }
}
