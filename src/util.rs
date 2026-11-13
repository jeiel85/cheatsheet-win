//! Small helpers: UTF-16 strings, app paths, a size-capped log file.
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

use windows::core::{PCWSTR, PWSTR};
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::UI::Shell::{FOLDERID_LocalAppData, FOLDERID_RoamingAppData, SHGetKnownFolderPath, KF_FLAG_DEFAULT};

pub const APP_VERSION: &str = env!("APP_VERSION");
pub const REPO: &str = "jeiel85/cheatsheet-win";
pub const REPO_URL: &str = "https://github.com/jeiel85/cheatsheet-win";
pub const SITE_URL: &str = "https://jeiel85.github.io/cheatsheet-win/";

/// Null-terminated UTF-16 buffer that lives as long as the value.
pub struct WStr(Vec<u16>);

impl WStr {
    pub fn new(s: &str) -> Self {
        WStr(s.encode_utf16().chain(std::iter::once(0)).collect())
    }
    pub fn pcwstr(&self) -> PCWSTR {
        PCWSTR(self.0.as_ptr())
    }
    pub fn pwstr(&mut self) -> PWSTR {
        PWSTR(self.0.as_mut_ptr())
    }
    /// Without the terminating NUL.
    pub fn units(&self) -> &[u16] {
        &self.0[..self.0.len() - 1]
    }
}

pub fn w(s: &str) -> WStr {
    WStr::new(s)
}

pub fn from_wide(buf: &[u16]) -> String {
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..end])
}

fn known_folder(id: &windows::core::GUID) -> PathBuf {
    unsafe {
        match SHGetKnownFolderPath(id, KF_FLAG_DEFAULT, None) {
            Ok(p) => {
                let s = p.to_string().unwrap_or_default();
                CoTaskMemFree(Some(p.0 as _));
                PathBuf::from(s)
            }
            Err(_) => std::env::temp_dir(),
        }
    }
}

/// %APPDATA%\FnCheatsheet — settings and the user's own profile files.
pub fn app_data_dir() -> PathBuf {
    known_folder(&FOLDERID_RoamingAppData).join("FnCheatsheet")
}

pub fn user_profiles_dir() -> PathBuf {
    app_data_dir().join("profiles")
}

/// %LOCALAPPDATA%\FnCheatsheet — log file.
pub fn local_dir() -> PathBuf {
    known_folder(&FOLDERID_LocalAppData).join("FnCheatsheet")
}

pub fn exe_path() -> PathBuf {
    std::env::current_exe().unwrap_or_else(|_| PathBuf::from("FnCheatsheet.exe"))
}

static LOG_GATE: Mutex<()> = Mutex::new(());

/// Appends one line to %LOCALAPPDATA%\FnCheatsheet\log.txt (rotated at 256 KB). Never panics.
pub fn log(msg: &str) {
    let _g = LOG_GATE.lock();
    let dir = local_dir();
    let _ = fs::create_dir_all(&dir);
    let file = dir.join("log.txt");
    if fs::metadata(&file).map(|m| m.len() > 256 * 1024).unwrap_or(false) {
        let old = dir.join("log.1.txt");
        let _ = fs::remove_file(&old);
        let _ = fs::rename(&file, &old);
    }
    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(&file) {
        let _ = writeln!(f, "{} {}", now_string(), msg);
    }
}

pub fn now_string() -> String {
    let st = unsafe { windows::Win32::System::SystemInformation::GetLocalTime() };
    format!("{:04}-{:02}-{:02} {:02}:{:02}:{:02}", st.wYear, st.wMonth, st.wDay, st.wHour, st.wMinute, st.wSecond)
}

/// Seconds since the Unix epoch (for "last update check").
pub fn unix_now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// Opens an https URL in the default browser. Anything else is ignored.
pub fn open_url(url: &str) {
    if !url.starts_with("https://") {
        return;
    }
    let u = w(url);
    let op = w("open");
    unsafe {
        windows::Win32::UI::Shell::ShellExecuteW(None, op.pcwstr(), u.pcwstr(), PCWSTR::null(), PCWSTR::null(), windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL);
    }
}

/// Percent-encodes a string for use in a URL query value.
pub fn url_encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}
