//! SMBIOS identity strings from HKLM\HARDWARE\DESCRIPTION\System\BIOS. No serial numbers or UUIDs are read.
use windows::Win32::System::Registry::{RegGetValueW, HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ};

use crate::util::{from_wide, w};

#[derive(Debug, Clone, Default)]
pub struct DeviceInfo {
    pub manufacturer: String,
    pub product: String,
    pub family: String,
    pub version: String,
    pub sku: String,
    pub baseboard_manufacturer: String,
    pub baseboard: String,
}

fn reg(name: &str) -> String {
    let key = w(r"HARDWARE\DESCRIPTION\System\BIOS");
    let val = w(name);
    let mut buf = [0u16; 256];
    let mut size = (buf.len() * 2) as u32;
    let r = unsafe { RegGetValueW(HKEY_LOCAL_MACHINE, key.pcwstr(), val.pcwstr(), RRF_RT_REG_SZ, None, Some(buf.as_mut_ptr() as _), Some(&mut size)) };
    if r.is_err() {
        return String::new();
    }
    clean(&from_wide(&buf))
}

/// OEM placeholder values carry no information.
pub fn clean(s: &str) -> String {
    let s = s.trim();
    match s.to_ascii_lowercase().as_str() {
        "to be filled by o.e.m." | "default string" | "system product name" | "system manufacturer" | "not applicable" | "none" | "o.e.m." => String::new(),
        _ => s.chars().take(200).collect(),
    }
}

impl DeviceInfo {
    pub fn read() -> DeviceInfo {
        DeviceInfo {
            manufacturer: reg("SystemManufacturer"),
            product: reg("SystemProductName"),
            family: reg("SystemFamily"),
            version: reg("SystemVersion"),
            sku: reg("SystemSKU"),
            baseboard_manufacturer: reg("BaseBoardManufacturer"),
            baseboard: reg("BaseBoardProduct"),
        }
    }

    pub fn fields(&self) -> [(&'static str, &str); 7] {
        [
            ("SystemManufacturer", &self.manufacturer),
            ("SystemProductName", &self.product),
            ("SystemFamily", &self.family),
            ("SystemVersion", &self.version),
            ("SystemSKU", &self.sku),
            ("BaseBoardManufacturer", &self.baseboard_manufacturer),
            ("BaseBoardProduct", &self.baseboard),
        ]
    }

    pub fn report(&self) -> String {
        self.fields().iter().map(|(k, v)| format!("{k}: {}\r\n", if v.is_empty() { "(empty)" } else { v })).collect()
    }

    /// Lenovo keeps the friendly name in SystemVersion; ASUS puts "1.0" there.
    pub fn friendly_version(&self) -> Option<&str> {
        let v = self.version.as_str();
        if v.chars().any(|c| c.is_alphabetic()) && !v.eq_ignore_ascii_case(&self.product) { Some(v) } else { None }
    }

    pub fn short_label(&self) -> String {
        let model = match self.friendly_version() {
            Some(v) if !self.product.is_empty() => format!("{v} ({})", self.product),
            Some(v) => v.to_string(),
            None => self.product.clone(),
        };
        format!("{} · {}", self.manufacturer, model).trim_matches(|c| c == ' ' || c == '·').to_string()
    }
}
