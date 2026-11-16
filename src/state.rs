//! What a key currently has switched on (e.g. the fan mode), read when the sheet is shown. Profiles point at a
//! registry DWORD that the maker's own software keeps in step with the key; reading it needs no admin rights and
//! costs one registry call, so nothing runs while the sheet is hidden.
use windows::Win32::System::Registry::{RegGetValueW, HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, RRF_RT_REG_DWORD, RRF_SUBKEY_WOW6464KEY};

use crate::data::{KeyState, StateValue};
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
}
