//! Auto-update from this repository's GitHub releases.
//!
//! Once a day (and on demand) a background thread asks api.github.com for the latest release. If it is newer,
//! it downloads FnCheatsheet.exe and FnCheatsheet.exe.sha256 — only from
//! `https://github.com/<REPO>/releases/download/<tag>/` — checks size, SHA-256 and the PE header, writes it next
//! to the running exe as `.new`, then swaps the files and restarts. No other host, no other path.
use std::fs;
use std::path::PathBuf;

use serde::Deserialize;
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::Networking::WinHttp::*;
use windows::Win32::Security::Cryptography::{BCryptHash, BCRYPT_SHA256_ALG_HANDLE};
use windows::Win32::Storage::FileSystem::{MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH};
use windows::Win32::UI::WindowsAndMessaging::{DestroyWindow, KillTimer, PostMessageW, SetTimer};

use crate::app::{self, WM_APP_UPDATE_DONE};
use crate::i18n::t;
use crate::util::{exe_path, log, unix_now, w, APP_VERSION, REPO};

pub const TIMER_FIRST: usize = 2;
const DAY: u64 = 24 * 60 * 60;
const MAX_EXE: usize = 30 * 1024 * 1024;
const ASSET: &str = "FnCheatsheet.exe";

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
        a.settings.last_update_check = unix_now();
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

    let new = exe_path().with_extension("exe.new");
    fs::write(&new, &exe).map_err(|e| format!("{} ({e})", t("cannot write next to the app", "앱 폴더에 쓸 수 없어요")))?;
    Ok(Outcome::Ready { version: latest_s, path: new })
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
    fn sha256() {
        assert_eq!(sha256_hex(b"abc").unwrap(), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    }
}
