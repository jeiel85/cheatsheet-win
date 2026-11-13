//! Data validation + SMBIOS match cases. Used by `FnCheatsheet.exe --selftest` (CI) and `cargo test`.
use regex::Regex;
use serde::Deserialize;
use std::collections::HashSet;

use crate::data::ProfileDb;
use crate::device::DeviceInfo;
use crate::glyphs::ICONS;
use crate::matcher::{match_device, MatchKind};

#[derive(Deserialize, Default)]
#[serde(default)]
struct Case {
    name: String,
    expect: String,
    manufacturer: String,
    product: String,
    family: String,
    version: String,
    baseboard: String,
}

#[derive(Deserialize)]
struct Cases {
    cases: Vec<Case>,
}

/// Returns (report lines, failure count).
pub fn run(db: &ProfileDb, cases_json: Option<&str>) -> (Vec<String>, usize) {
    let mut out = Vec::new();
    let mut fails = 0usize;
    let mut fail = |out: &mut Vec<String>, m: String| {
        fails += 1;
        out.push(format!("FAIL {m}"));
    };

    for e in &db.errors {
        fail(&mut out, format!("load: {e}"));
    }
    out.push(format!("profiles: {}, data version {}", db.profiles.len(), db.data_version));

    let id_re = Regex::new("^[a-z0-9]+(-[a-z0-9]+)*$").unwrap();
    let mtm_re = Regex::new("^[0-9A-Z]{4}$").unwrap();
    let mut ids = HashSet::new();
    for p in &db.profiles {
        let id = &p.id;
        if !id_re.is_match(id) { fail(&mut out, format!("{id}: id must be kebab-case")); }
        if !ids.insert(id.to_ascii_lowercase()) { fail(&mut out, format!("{id}: duplicate id")); }
        if p.vendor != "ASUS" && p.vendor != "Lenovo" { fail(&mut out, format!("{id}: unknown vendor {}", p.vendor)); }
        let m = &p.match_rule;
        if m.manufacturer.is_empty() { fail(&mut out, format!("{id}: match.manufacturer is required")); }
        if m.product.len() + m.family.len() + m.mtm.len() == 0 { fail(&mut out, format!("{id}: needs a model-level match rule")); }
        for rx in m.manufacturer.iter().chain(&m.product).chain(&m.family) {
            if Regex::new(rx).is_err() { fail(&mut out, format!("{id}: bad regex {rx}")); }
        }
        for t in &m.mtm {
            if !mtm_re.is_match(t) { fail(&mut out, format!("{id}: bad machine type {t}")); }
        }
        if p.keys.is_empty() { fail(&mut out, format!("{id}: no keys")); }
        if !p.sources.iter().any(|s| s.kind == "official" || s.kind == "official-copy") { fail(&mut out, format!("{id}: needs an official source")); }
        for s in &p.sources {
            if !s.url.starts_with("https://") { fail(&mut out, format!("{id}: source url must be https: {}", s.url)); }
        }
        let mut seen = HashSet::new();
        for k in &p.keys {
            if k.row != "top" && k.row != "combo" { fail(&mut out, format!("{id}/{}: row must be top|combo", k.key)); }
            if !ICONS.contains(&k.icon.as_str()) { fail(&mut out, format!("{id}/{}: unknown icon {}", k.key, k.icon)); }
            if k.en.trim().is_empty() || k.ko.trim().is_empty() { fail(&mut out, format!("{id}/{}: needs en and ko labels", k.key)); }
            if !seen.insert(format!("{}:{}", k.row, k.key)) { fail(&mut out, format!("{id}/{}: duplicate key in row {}", k.key, k.row)); }
        }
    }

    if let Some(json) = cases_json {
        match serde_json::from_str::<Cases>(json.trim_start_matches('\u{feff}')) {
            Ok(cases) => {
                for c in &cases.cases {
                    let dev = DeviceInfo {
                        manufacturer: c.manufacturer.clone(),
                        product: c.product.clone(),
                        family: c.family.clone(),
                        version: c.version.clone(),
                        baseboard: c.baseboard.clone(),
                        ..Default::default()
                    };
                    let r = match_device(&dev, &db.profiles);
                    let got = match r.kind {
                        MatchKind::Matched => db.profiles[r.candidates[0].index].id.clone(),
                        MatchKind::Ambiguous => "ambiguous".into(),
                        MatchKind::Unknown => "unknown".into(),
                    };
                    if !got.eq_ignore_ascii_case(&c.expect) {
                        let cands: Vec<String> = r.candidates.iter().map(|x| format!("{}:{}", db.profiles[x.index].id, x.score)).collect();
                        fail(&mut out, format!("match {}: expected {}, got {} [{}]", c.name, c.expect, got, cands.join(", ")));
                    }
                }
                out.push(format!("match cases: {}", cases.cases.len()));
            }
            Err(e) => fail(&mut out, format!("match-tests.json: {e}")),
        }
    }

    out.push(if fails == 0 { "OK".into() } else { format!("{fails} failure(s)") });
    (out, fails)
}

#[cfg(test)]
mod tests {
    #[test]
    fn builtin_data_and_match_cases() {
        let db = crate::data::ProfileDb::load(None);
        let cases = include_str!("../data/match-tests.json");
        let (report, fails) = super::run(&db, Some(cases));
        assert_eq!(fails, 0, "{}", report.join("\n"));
    }
}
