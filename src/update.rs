//! Auto-update from this repository's GitHub releases.
//!
//! Once a day (and on demand) a background thread asks api.github.com for the latest release. If it is newer,
//! it downloads FnCheatsheet.exe, FnCheatsheet.exe.sha256 and FnCheatsheet.exe.sig — only from
//! `https://github.com/<REPO>/releases/download/<tag>/` — checks size, SHA-256, the minisign signature (made
//! offline with the maintainer's key, see [`UPDATE_KEYS`]) and the PE header, writes it next to the running exe
//! as `.new`, then swaps the files and restarts. No other host, no other path.
//!
//! The SHA-256 file only catches broken downloads: whoever can change a release can change both files. The
//! signature can't be forged without the private key, which never touches GitHub.
use std::fs;
use std::path::PathBuf;

use serde::Deserialize;
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::Networking::WinHttp::*;
use windows::Win32::Security::Cryptography::{BCryptHash, BCRYPT_SHA256_ALG_HANDLE};
use windows::Win32::Storage::FileSystem::{GetFileVersionInfoSizeW, GetFileVersionInfoW, MoveFileExW, VerQueryValueW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, VS_FIXEDFILEINFO};
use windows::Win32::UI::WindowsAndMessaging::{DestroyWindow, KillTimer, PostMessageW, SetTimer};

use crate::app::{self, WM_APP_UPDATE_DONE};
use crate::i18n::t;
use crate::util::{exe_path, log, unix_now, w, APP_VERSION, REPO};

pub const TIMER_FIRST: usize = 2;
const DAY: u64 = 24 * 60 * 60;
const RETRY_AFTER: u64 = 3 * 60 * 60;
const MAX_EXE: usize = 30 * 1024 * 1024;
const ASSET: &str = "FnCheatsheet.exe";

/// minisign public keys (Ed25519) that may sign updates: the key used for releases, then an offline backup
/// kept for the day the first is lost or must be retired. Private keys: `%USERPROFILE%\.tauri\fncheatsheet*.key`
/// on the maintainer's PC, used by `tools/release.ps1` (`cargo tauri signer sign`).
const UPDATE_KEYS: &[&str] = &[
    "RWRnhti+qIai5+rpmzMoxzDBLxgV5gij0kD33WgZN+4YitTDuUZJG5m+", // E7A286A8BED88667 release
    "RWSMUuiVs4yoW38lzKRYIgWbNsgATRtcRQMDvQ1IafX3cHRYqNtXy5j5", // 5BA88CB395E8528C offline backup
];

pub enum Outcome {
    UpToDate(String),
    /// Newer version exists but the app's folder isn't writable (e.g. Program Files): tell the user.
    Available(String),
    Ready { version: String, path: PathBuf },
    Failed(String),
}

struct Done {
    manual: bool,
    outcome: Outcome,
}

pub fn parse_version(s: &str) -> Option<(u32, u32, u32)> {
    let s = s.trim().trim_start_matches(['v', 'V']);
    let mut it = s.split('.').map(|p| p.parse::<u32>().ok());
    let v = (it.next()??, it.next()??, it.next()??);
    if it.next().is_some() { None } else { Some(v) }
}

// ───────────────────────── scheduling ─────────────────────────

pub fn schedule_first_check(hwnd: HWND) {
    unsafe {
        SetTimer(Some(hwnd), TIMER_FIRST, 90 * 1000, None);
    }
}

pub fn check_if_due(hwnd: HWND) {
    unsafe {
        let _ = KillTimer(Some(hwnd), TIMER_FIRST);
    }
    let due = app::with(|a| a.settings.auto_update && !a.update_busy && unix_now().saturating_sub(a.settings.last_update_check) >= DAY).unwrap_or(false);
    if due {
        start(hwnd, false);
    }
}

pub fn check_now(hwnd: HWND, manual: bool) {
    if app::with(|a| a.update_busy).unwrap_or(true) {
        return;
    }
    start(hwnd, manual);
}

fn start(hwnd: HWND, manual: bool) {
    app::with(|a| {
        a.update_busy = true;
        a.update_status = t("Checking for updates…", "업데이트 확인 중…").into();
    });
    crate::ui::refresh_update_status();
    let h = hwnd.0 as isize;
    let _ = std::thread::Builder::new().name("update".into()).spawn(move || {
        let outcome = check_and_download();
        let done = Box::new(Done { manual, outcome });
        unsafe {
            if PostMessageW(Some(HWND(h as _)), WM_APP_UPDATE_DONE, WPARAM(0), LPARAM(Box::into_raw(done) as isize)).is_err() {
                // window is gone; the box leaks once at shutdown, which is fine
            }
        }
    });
}

/// Called on the UI thread with the result of a check.
pub fn on_done(hwnd: HWND, lp: LPARAM) {
    if lp.0 == 0 {
        return;
    }
    let done = unsafe { Box::from_raw(lp.0 as *mut Done) };
    let status = match &done.outcome {
        Outcome::UpToDate(v) => format!("{} (v{APP_VERSION}, {} v{v})", t("Up to date", "최신 버전이에요"), t("latest", "최신")),
        Outcome::Available(v) => format!(
            "{} v{v} — {}",
            t("New version", "새 버전"),
            t("the app's folder is read-only, download it from the GitHub releases page", "앱 폴더에 쓸 수 없어서 자동 설치를 못 해요. GitHub 릴리스 페이지에서 받아 주세요")
        ),
        Outcome::Ready { version, .. } => format!("{} v{version}…", t("Installing", "설치 중")),
        Outcome::Failed(e) => format!("{}: {e}", t("Update check failed", "업데이트 확인 실패")),
    };
    log(&format!("update: {status}"));
    app::with(|a| {
        a.update_busy = false;
        a.update_status = status.clone();
        // A failed check (offline, GitHub hiccup) is retried in about 3 hours instead of tomorrow.
        a.settings.last_update_check = match done.outcome {
            Outcome::Failed(_) => unix_now().saturating_sub(DAY - RETRY_AFTER),
            _ => unix_now(),
        };
        a.settings.save();
    });
    crate::ui::refresh_update_status();
    match done.outcome {
        Outcome::Ready { version, path } => apply(hwnd, &version, &path),
        Outcome::Available(_) => app::balloon("Fn Cheatsheet", &status),
        Outcome::UpToDate(_) if done.manual => app::balloon("Fn Cheatsheet", &status),
        Outcome::Failed(_) if done.manual => app::balloon("Fn Cheatsheet", &status),
        _ => {}
    }
}

/// Swaps FnCheatsheet.exe for the verified download and restarts. Rolls back if anything fails.
fn apply(hwnd: HWND, version: &str, new: &PathBuf) {
    let exe = exe_path();
    let old = exe.with_extension("exe.old");
    let (e, o, n) = (w(&exe.display().to_string()), w(&old.display().to_string()), w(&new.display().to_string()));
    unsafe {
        if let Err(err) = MoveFileExW(e.pcwstr(), o.pcwstr(), MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH) {
            fail_apply(&format!("rename current exe: {err}"), new);
            return;
        }
        if let Err(err) = MoveFileExW(n.pcwstr(), e.pcwstr(), MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH) {
            let _ = MoveFileExW(o.pcwstr(), e.pcwstr(), MOVEFILE_REPLACE_EXISTING);
            fail_apply(&format!("move new exe: {err}"), new);
            return;
        }
    }
    match std::process::Command::new(&exe).args(["--background", "--updated", APP_VERSION]).spawn() {
        Ok(_) => {
            log(&format!("update: v{APP_VERSION} -> v{version}, restarting"));
            unsafe {
                let _ = DestroyWindow(hwnd);
            }
        }
        Err(err) => {
            // Put the old exe back so the next start isn't broken.
            unsafe {
                let _ = MoveFileExW(e.pcwstr(), n.pcwstr(), MOVEFILE_REPLACE_EXISTING);
                let _ = MoveFileExW(o.pcwstr(), e.pcwstr(), MOVEFILE_REPLACE_EXISTING);
            }
            fail_apply(&format!("start new version: {err}"), new);
        }
    }
}

fn fail_apply(msg: &str, new: &PathBuf) {
    log(&format!("update apply failed: {msg}"));
    let _ = fs::remove_file(new);
    let status = format!("{}: {msg}", t("Update failed", "업데이트 실패"));
    app::with(|a| a.update_status = status.clone());
    crate::ui::refresh_update_status();
}

/// Cleans up after an update (the previous exe) or an interrupted download.
pub fn after_start(updated_from: Option<&str>) {
    let exe = exe_path();
    for leftover in [exe.with_extension("exe.old"), exe.with_extension("exe.new")] {
        if leftover.exists() {
            let _ = fs::remove_file(&leftover);
        }
    }
    if let Some(v) = updated_from {
        log(&format!("updated from v{v} to v{APP_VERSION}"));
    }
}

// ───────────────────────── check & download (background thread) ─────────────────────────

#[derive(Deserialize)]
struct Release {
    tag_name: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    assets: Vec<Asset>,
}

#[derive(Deserialize)]
struct Asset {
    name: String,
    browser_download_url: String,
    #[serde(default)]
    size: u64,
}

fn check_and_download() -> Outcome {
    match check_and_download_inner() {
        Ok(o) => o,
        Err(e) => Outcome::Failed(e),
    }
}

fn check_and_download_inner() -> Result<Outcome, String> {
    let api = format!("https://api.github.com/repos/{REPO}/releases/latest");
    let body = http_get(&api, 1024 * 1024, "application/vnd.github+json")?;
    let rel: Release = serde_json::from_slice(&body).map_err(|e| format!("release JSON: {e}"))?;
    if rel.draft || rel.prerelease {
        return Ok(Outcome::UpToDate(APP_VERSION.into()));
    }
    let latest = parse_version(&rel.tag_name).ok_or_else(|| format!("bad tag {}", rel.tag_name))?;
    let current = parse_version(APP_VERSION).unwrap_or((0, 0, 0));
    let latest_s = format!("{}.{}.{}", latest.0, latest.1, latest.2);
    if latest <= current {
        return Ok(Outcome::UpToDate(latest_s));
    }

    if !folder_writable() {
        return Ok(Outcome::Available(latest_s));
    }

    let prefix = format!("https://github.com/{REPO}/releases/download/{}/", rel.tag_name);
    let find = |name: &str| -> Result<&Asset, String> {
        let a = rel.assets.iter().find(|a| a.name == name).ok_or_else(|| format!("release has no {name}"))?;
        if a.browser_download_url != format!("{prefix}{name}") {
            return Err(format!("unexpected download URL for {name}"));
        }
        Ok(a)
    };
    let exe_asset = find(ASSET)?;
    let sum_asset = find(&format!("{ASSET}.sha256"))?;
    let sig_asset = find(&format!("{ASSET}.sig"))?;
    if exe_asset.size == 0 || exe_asset.size as usize > MAX_EXE {
        return Err("unexpected exe size".into());
    }

    let sum_text = String::from_utf8(http_get(&sum_asset.browser_download_url, 4096, "application/octet-stream")?).map_err(|_| "checksum file is not text")?;
    let expected = sum_text.split_whitespace().next().unwrap_or("").to_ascii_lowercase();
    if expected.len() != 64 || !expected.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err("checksum file is malformed".into());
    }

    let exe = http_get(&exe_asset.browser_download_url, MAX_EXE, "application/octet-stream")?;
    if exe.len() as u64 != exe_asset.size {
        return Err("download size mismatch".into());
    }
    if exe.len() < 1024 || &exe[..2] != b"MZ" {
        return Err("download is not a Windows executable".into());
    }
    let actual = sha256_hex(&exe)?;
    if actual != expected {
        return Err("SHA-256 mismatch".into());
    }
    let sig = http_get(&sig_asset.browser_download_url, 8192, "application/octet-stream")?;
    verify_signature(&exe, &sig, ASSET, &latest_s, UPDATE_KEYS)?;

    let new = exe_path().with_extension("exe.new");
    fs::write(&new, &exe).map_err(|e| format!("{} ({e})", t("cannot write next to the app", "앱 폴더에 쓸 수 없어요")))?;
    // The exe must say it is the version the release claims (no mislabelled or downgraded binaries).
    match file_version(&new) {
        Some(v) if v == latest => Ok(Outcome::Ready { version: latest_s, path: new }),
        other => {
            let _ = fs::remove_file(&new);
            Err(format!("version mismatch: release {latest_s}, file {}", other.map(|v| format!("{}.{}.{}", v.0, v.1, v.2)).unwrap_or_else(|| "unknown".into())))
        }
    }
}

/// For `tools/release.ps1`: the signature it just made must pass exactly the check installed apps run.
pub fn verify_release_files(exe: &std::path::Path, sig: &std::path::Path, version: &str) -> Result<(), String> {
    let data = fs::read(exe).map_err(|e| format!("{}: {e}", exe.display()))?;
    let sig = fs::read(sig).map_err(|e| format!("{}: {e}", sig.display()))?;
    let name = exe.file_name().and_then(|n| n.to_str()).unwrap_or_default();
    if name != ASSET {
        return Err(format!("file must be named {ASSET}"));
    }
    verify_signature(&data, &sig, ASSET, version, UPDATE_KEYS)
}

/// Checks a `cargo tauri signer sign --app-version` signature: base64 of a minisign signature (prehashed Ed25519)
/// whose signed trusted comment names this file and this version, so a signature can't be moved to another
/// file or an older build.
fn verify_signature(data: &[u8], sig_file: &[u8], file: &str, version: &str, keys: &[&str]) -> Result<(), String> {
    let text = std::str::from_utf8(sig_file).map_err(|_| "signature file is not text")?;
    let decoded = base64_decode(text.trim()).ok_or("signature file is not base64")?;
    let lines = String::from_utf8(decoded).map_err(|_| "signature is not text")?;
    let sig = minisign_verify::Signature::decode(&lines).map_err(|e| format!("bad signature: {e}"))?;
    let key_ok = keys.iter().any(|k| minisign_verify::PublicKey::from_base64(k).is_ok_and(|pk| pk.verify(data, &sig, false).is_ok()));
    if !key_ok {
        return Err("update signature is not valid".into());
    }
    // verify() above also checked the global signature over the trusted comment.
    let fields: Vec<&str> = sig.trusted_comment().split('\t').collect();
    if !fields.contains(&format!("file:{file}").as_str()) || !fields.contains(&format!("version:{version}").as_str()) {
        return Err(format!("signature is for another file or version ({})", sig.trusted_comment()));
    }
    Ok(())
}

fn base64_decode(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let (mut acc, mut bits) = (0u32, 0u32);
    let mut pad = 0;
    for c in s.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' => {
                pad += 1;
                continue;
            }
            b'\r' | b'\n' => continue,
            _ => return None,
        };
        if pad > 0 {
            return None; // data after padding
        }
        acc = (acc << 6) | v as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    (pad <= 2).then_some(out)
}

/// FILEVERSION major.minor.patch from the exe's version resource.
fn file_version(path: &std::path::Path) -> Option<(u32, u32, u32)> {
    unsafe {
        let p = w(&path.display().to_string());
        let size = GetFileVersionInfoSizeW(p.pcwstr(), None);
        if size == 0 {
            return None;
        }
        let mut buf = vec![0u8; size as usize];
        GetFileVersionInfoW(p.pcwstr(), None, size, buf.as_mut_ptr() as _).ok()?;
        let mut ptr = std::ptr::null_mut();
        let mut len = 0u32;
        let root = w("\\");
        if !VerQueryValueW(buf.as_ptr() as _, root.pcwstr(), &mut ptr, &mut len).as_bool() || (len as usize) < std::mem::size_of::<VS_FIXEDFILEINFO>() {
            return None;
        }
        let fi = &*(ptr as *const VS_FIXEDFILEINFO);
        Some((fi.dwFileVersionMS >> 16, fi.dwFileVersionMS & 0xFFFF, fi.dwFileVersionLS >> 16))
    }
}

/// Can we put FnCheatsheet.exe.new next to the running exe?
fn folder_writable() -> bool {
    let probe = exe_path().with_extension("exe.write-test");
    let ok = fs::write(&probe, b"").is_ok();
    let _ = fs::remove_file(&probe);
    ok
}

pub fn sha256_hex(data: &[u8]) -> Result<String, String> {
    let mut out = [0u8; 32];
    let st = unsafe { BCryptHash(BCRYPT_SHA256_ALG_HANDLE, None, data, &mut out) };
    if st.is_err() {
        return Err(format!("BCryptHash failed: {:#x}", st.0));
    }
    Ok(out.iter().map(|b| format!("{b:02x}")).collect())
}

struct Handle(*mut core::ffi::c_void);
impl Drop for Handle {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                let _ = WinHttpCloseHandle(self.0);
            }
        }
    }
}

/// HTTPS GET through WinHTTP (system proxy, system TLS, redirects followed, never HTTPS→HTTP).
fn http_get(url: &str, limit: usize, accept: &str) -> Result<Vec<u8>, String> {
    let rest = url.strip_prefix("https://").ok_or("only https URLs are allowed")?;
    let (host, path) = rest.split_once('/').map(|(h, p)| (h, format!("/{p}"))).unwrap_or((rest, "/".into()));
    unsafe {
        let ua = w(&format!("FnCheatsheet/{APP_VERSION} (+https://github.com/{REPO})"));
        let session = Handle(WinHttpOpen(ua.pcwstr(), WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, windows::core::PCWSTR::null(), windows::core::PCWSTR::null(), 0));
        if session.0.is_null() {
            return Err("WinHttpOpen failed".into());
        }
        let _ = WinHttpSetTimeouts(session.0, 10_000, 10_000, 15_000, 30_000);
        let h = w(host);
        let connect = Handle(WinHttpConnect(session.0, h.pcwstr(), 443, 0));
        if connect.0.is_null() {
            return Err(format!("cannot connect to {host}"));
        }
        let verb = w("GET");
        let p = w(&path);
        let req = Handle(WinHttpOpenRequest(connect.0, verb.pcwstr(), p.pcwstr(), windows::core::PCWSTR::null(), windows::core::PCWSTR::null(), std::ptr::null(), WINHTTP_FLAG_SECURE));
        if req.0.is_null() {
            return Err("WinHttpOpenRequest failed".into());
        }
        let headers = w(&format!("Accept: {accept}\r\nX-GitHub-Api-Version: 2022-11-28\r\n"));
        WinHttpSendRequest(req.0, Some(headers.units()), None, 0, 0, 0).map_err(|e| format!("send: {e}"))?;
        WinHttpReceiveResponse(req.0, std::ptr::null_mut()).map_err(|e| format!("receive: {e}"))?;
        let mut status = 0u32;
        let mut size = 4u32;
        let mut index = 0u32;
        WinHttpQueryHeaders(req.0, WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER, windows::core::PCWSTR::null(), Some(&mut status as *mut u32 as _), &mut size, &mut index)
            .map_err(|e| format!("status: {e}"))?;
        if status != 200 {
            return Err(format!("HTTP {status} from {host}"));
        }
        let mut body = Vec::new();
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            let mut read = 0u32;
            WinHttpReadData(req.0, buf.as_mut_ptr() as _, buf.len() as u32, &mut read).map_err(|e| format!("read: {e}"))?;
            if read == 0 {
                break;
            }
            body.extend_from_slice(&buf[..read as usize]);
            if body.len() > limit {
                return Err("response too large".into());
            }
        }
        Ok(body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Made with a throwaway key: `cargo tauri signer sign --app-version 9.9.9 FnCheatsheet.exe` over TEST_DATA.
    const TEST_KEY: &str = "RWRC5LcaqEEoHyifyH4pWapR8EvBAo9GQIZZkHWugw1XFrqxHYRgbSJC";
    const TEST_DATA: &[u8] = b"MZ test payload";
    const TEST_SIG: &str = "dW50cnVzdGVkIGNvbW1lbnQ6IHNpZ25hdHVyZSBmcm9tIHRhdXJpIHNlY3JldCBrZXkKUlVSQzVMY2FxRUVvSDZURWRDZlVPNEJabTlmRFlBQm01VkFLTFg0b2xMYVdKUDFqcGsyY0tJdVVWeUhzbVdnajBUTGdYMm1lUEh5TzRaR0FzNHlPWFlTd1VndWNBejFwSncwPQp0cnVzdGVkIGNvbW1lbnQ6IHRpbWVzdGFtcDoxNzk0NTU1MzIyCWZpbGU6Rm5DaGVhdHNoZWV0LmV4ZQl2ZXJzaW9uOjkuOS45CkU1VGhuWlFlUzljbHcxRWpRTWozM1lOQ2xKVlZjNXlydHYydjI2TW5lOXMrajVYZkMwWXdUeXhXRldEVU9zY241VTJ1NnBLcFUyS3pQOU54YUV2d0RBPT0K";

    #[test]
    fn signature_accepts_the_signed_file() {
        assert_eq!(verify_signature(TEST_DATA, TEST_SIG.as_bytes(), "FnCheatsheet.exe", "9.9.9", &[TEST_KEY]), Ok(()));
        // Any listed key may have signed it; trailing newline from the .sig file is fine.
        assert_eq!(verify_signature(TEST_DATA, format!("{TEST_SIG}\r\n").as_bytes(), "FnCheatsheet.exe", "9.9.9", &[UPDATE_KEYS[0], TEST_KEY]), Ok(()));
    }

    #[test]
    fn signature_rejects_tampering_and_misuse() {
        let ok = |data: &[u8], sig: &str, file: &str, ver: &str, keys: &[&str]| verify_signature(data, sig.as_bytes(), file, ver, keys).is_ok();
        assert!(!ok(b"MZ test payloaD", TEST_SIG, "FnCheatsheet.exe", "9.9.9", &[TEST_KEY]), "changed file");
        assert!(!ok(TEST_DATA, TEST_SIG, "FnCheatsheet.exe", "9.9.9", UPDATE_KEYS), "signed by someone else");
        assert!(!ok(TEST_DATA, TEST_SIG, "FnCheatsheet.exe", "9.9.10", &[TEST_KEY]), "other version");
        assert!(!ok(TEST_DATA, TEST_SIG, "FnProbe.exe", "9.9.9", &[TEST_KEY]), "other file");
        assert!(!ok(TEST_DATA, "", "FnCheatsheet.exe", "9.9.9", &[TEST_KEY]), "empty");
        assert!(!ok(TEST_DATA, "not base64!", "FnCheatsheet.exe", "9.9.9", &[TEST_KEY]), "garbage");
        // Trusted comment edited (version 9.9.9 -> 9.9.8) without re-signing: the global signature fails.
        let lines = String::from_utf8(base64_decode(TEST_SIG).unwrap()).unwrap().replace("version:9.9.9", "version:9.9.8");
        let forged = b64(lines.as_bytes());
        assert!(!ok(TEST_DATA, &forged, "FnCheatsheet.exe", "9.9.8", &[TEST_KEY]), "edited trusted comment");
    }

    #[test]
    fn release_keys_parse() {
        for k in UPDATE_KEYS {
            assert!(minisign_verify::PublicKey::from_base64(k).is_ok(), "{k}");
        }
    }

    #[test]
    fn base64_round_trip() {
        for s in ["", "f", "fo", "foo", "foob", "fooba", "foobar"] {
            assert_eq!(base64_decode(&b64(s.as_bytes())).unwrap(), s.as_bytes());
        }
        assert!(base64_decode("Zm9v=x").is_none());
    }

    fn b64(data: &[u8]) -> String {
        const A: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut s = String::new();
        for c in data.chunks(3) {
            let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
            for i in 0..4 {
                s.push(if i <= c.len() { A[(n >> (18 - 6 * i) & 63) as usize] as char } else { '=' });
            }
        }
        s
    }

    #[test]
    fn versions() {
        assert_eq!(parse_version("v1.2.3"), Some((1, 2, 3)));
        assert_eq!(parse_version("0.10.0"), Some((0, 10, 0)));
        assert!(parse_version("v1.2").is_none());
        assert!(parse_version("v1.2.3-beta").is_none());
        assert!(parse_version("1.2.3.4").is_none());
        assert!(parse_version("v0.10.0") > parse_version("v0.9.9"));
    }

    #[test]
    fn own_version_resource() {
        // The test binary has no version resource; the real exe is checked in CI via --selftest builds.
        assert!(file_version(std::path::Path::new(r"C:\definitely-missing.exe")).is_none());
    }

    #[test]
    fn sha256() {
        assert_eq!(sha256_hex(b"abc").unwrap(), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    }
}
