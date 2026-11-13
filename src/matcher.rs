//! Maps SMBIOS strings to a profile. Only model-level evidence counts:
//!   machine type (Lenovo MTM) 100 · model code in product/baseboard 90 · family/version regex 80.
//! A manufacturer match alone never selects a profile; equal top scores are reported as ambiguous.
//! The `regex` crate runs in linear time, so patterns from user files cannot hang the app.
use std::cell::RefCell;
use std::collections::HashMap;

use regex::{Regex, RegexBuilder};

use crate::data::Profile;
use crate::device::DeviceInfo;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatchKind {
    Matched,
    Ambiguous,
    Unknown,
}

#[derive(Debug, Clone)]
pub struct Candidate {
    pub index: usize,
    pub score: u32,
    pub reason: String,
}

#[derive(Debug, Clone)]
pub struct MatchResult {
    pub kind: MatchKind,
    pub candidates: Vec<Candidate>,
}

impl MatchResult {
    pub fn best(&self) -> Option<usize> {
        if self.kind == MatchKind::Matched { Some(self.candidates[0].index) } else { None }
    }
}

thread_local! {
    static CACHE: RefCell<HashMap<String, Option<Regex>>> = RefCell::new(HashMap::new());
}

/// Compiled once per pattern; invalid patterns (e.g. in a user file) simply never match.
fn compiled<R>(pattern: &str, f: impl FnOnce(&Regex) -> R) -> Option<R> {
    CACHE.with(|c| {
        let mut c = c.borrow_mut();
        let re = c
            .entry(pattern.to_string())
            .or_insert_with(|| RegexBuilder::new(pattern).case_insensitive(true).size_limit(1 << 20).build().ok());
        re.as_ref().map(f)
    })
}

fn is_match(input: &str, pattern: &str) -> bool {
    if input.is_empty() || pattern.is_empty() {
        return false;
    }
    compiled(pattern, |re| re.is_match(input)).unwrap_or(false)
}

/// Model codes must start at a token boundary, so "B1403" never hits "AB1403".
fn code_match(input: &str, fragment: &str) -> bool {
    if input.is_empty() || fragment.is_empty() {
        return false;
    }
    compiled(&format!("(?:{fragment})"), |re| {
        let bytes = input.as_bytes();
        let mut start = 0;
        while start <= input.len() {
            let Some(m) = re.find_at(input, start) else { return false };
            if m.start() == 0 || !bytes[m.start() - 1].is_ascii_alphanumeric() {
                return true;
            }
            // advance one character past this match start
            start = m.start() + input[m.start()..].chars().next().map(|c| c.len_utf8()).unwrap_or(1);
        }
        false
    })
    .unwrap_or(false)
}

fn score(dev: &DeviceInfo, p: &Profile) -> Option<(u32, String)> {
    let m = &p.match_rule;
    if m.manufacturer.is_empty() || !m.manufacturer.iter().any(|rx| is_match(&dev.manufacturer, rx)) {
        return None;
    }
    if dev.product.len() >= 4 && dev.product.is_char_boundary(4) {
        let mtm = &dev.product[..4];
        if m.mtm.iter().any(|t| t.trim().eq_ignore_ascii_case(mtm)) {
            return Some((100, format!("{} {}", crate::i18n::t("machine type", "머신 타입"), mtm.to_ascii_uppercase())));
        }
    }
    for frag in &m.product {
        if code_match(&dev.baseboard, frag) {
            return Some((90, format!("{} {}", crate::i18n::t("model code", "모델 코드"), dev.baseboard)));
        }
        if code_match(&dev.product, frag) {
            return Some((90, format!("{} {}", crate::i18n::t("model code", "모델 코드"), dev.product)));
        }
    }
    for rx in &m.family {
        if is_match(&dev.version, rx) {
            return Some((80, format!("{} {}", crate::i18n::t("model name", "모델명"), dev.version)));
        }
        if is_match(&dev.family, rx) {
            return Some((80, format!("{} {}", crate::i18n::t("model name", "모델명"), dev.family)));
        }
    }
    None
}

pub fn match_device(dev: &DeviceInfo, profiles: &[Profile]) -> MatchResult {
    let mut candidates: Vec<Candidate> = profiles
        .iter()
        .enumerate()
        .filter_map(|(i, p)| score(dev, p).map(|(s, r)| Candidate { index: i, score: s, reason: r }))
        .collect();
    candidates.sort_by(|a, b| b.score.cmp(&a.score).then_with(|| profiles[a.index].id.cmp(&profiles[b.index].id)));
    let kind = if candidates.is_empty() {
        MatchKind::Unknown
    } else if candidates.len() > 1 && candidates[1].score == candidates[0].score {
        MatchKind::Ambiguous
    } else {
        MatchKind::Matched
    };
    MatchResult { kind, candidates }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn boundary() {
        assert!(code_match("ASUS EXPERTBOOK P1503CVA", "P1503CVA"));
        assert!(code_match("P1503CVA", "P1503CVA"));
        assert!(!code_match("XP1503CVA", "P1503CVA"));
        assert!(code_match("XP1503CVA P1503CVA", "P1503CVA"));
        assert!(code_match("Vivobook_ASUSLaptop X1504VA_X1504VA", "X1504VA"));
    }
}
