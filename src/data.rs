//! Profile database: the built-in data/profiles.json plus optional user files (same id replaces).
use serde::Deserialize;
use std::fs;
use std::path::Path;

use crate::i18n::pick;

pub const MAX_USER_FILE_BYTES: u64 = 2 * 1024 * 1024;
static BUILTIN: &str = include_str!("../data/profiles.json");

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct MatchRule {
    pub manufacturer: Vec<String>,
    pub product: Vec<String>,
    pub family: Vec<String>,
    pub mtm: Vec<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct FnLock {
    pub combo: String,
    pub default_en: String,
    pub default_ko: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct KeyHint {
    /// "top" (Esc/F-row) or "combo" (always Fn + key).
    pub row: String,
    pub key: String,
    pub icon: String,
    pub en: String,
    pub ko: String,
    pub note_en: String,
    pub note_ko: String,
}

impl KeyHint {
    pub fn label(&self) -> &str {
        pick(&self.en, &self.ko)
    }
    pub fn note(&self) -> &str {
        pick(&self.note_en, &self.note_ko)
    }
    pub fn is_top(&self) -> bool {
        self.row.eq_ignore_ascii_case("top")
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct SourceRef {
    pub title: String,
    pub url: String,
    pub kind: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Profile {
    pub id: String,
    pub vendor: String,
    pub series: String,
    pub name: String,
    pub years: String,
    #[serde(rename = "match")]
    pub match_rule: MatchRule,
    pub fn_lock: FnLock,
    pub keys: Vec<KeyHint>,
    pub sources: Vec<SourceRef>,
    pub verified: String,
    #[serde(rename = "notes_en")]
    pub notes_en: String,
    #[serde(rename = "notes_ko")]
    pub notes_ko: String,
    /// "builtin" or the user file it came from.
    #[serde(skip)]
    pub origin: String,
}

impl Profile {
    pub fn notes(&self) -> &str {
        pick(&self.notes_en, &self.notes_ko)
    }
    pub fn fn_lock_default(&self) -> &str {
        pick(&self.fn_lock.default_en, &self.fn_lock.default_ko)
    }
    /// "ThinkPad T14 Gen 4 / … (21HD, 21HE…)" without the code list.
    pub fn short_name(&self) -> &str {
        match self.name.find(" (") {
            Some(i) if i > 12 => &self.name[..i],
            _ => &self.name,
        }
    }
    pub fn is_builtin(&self) -> bool {
        self.origin == "builtin"
    }
}

#[derive(Deserialize)]
struct Doc {
    #[serde(default, rename = "dataVersion")]
    data_version: String,
    #[serde(default)]
    profiles: Vec<serde_json::Value>,
}

#[derive(Default)]
pub struct ProfileDb {
    pub profiles: Vec<Profile>,
    pub errors: Vec<String>,
    pub data_version: String,
}

impl ProfileDb {
    pub fn load(user_dir: Option<&Path>) -> ProfileDb {
        let mut db = ProfileDb::default();
        db.add_from(BUILTIN, "builtin", true);
        if let Some(dir) = user_dir {
            if let Ok(rd) = fs::read_dir(dir) {
                let mut files: Vec<_> = rd
                    .filter_map(|e| e.ok())
                    .map(|e| e.path())
                    .filter(|p| p.extension().map(|x| x.eq_ignore_ascii_case("json")).unwrap_or(false))
                    .collect();
                files.sort();
                for f in files {
                    let name = f.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                    match fs::metadata(&f) {
                        Ok(m) if m.len() > MAX_USER_FILE_BYTES => db.errors.push(format!("{name}: file too large")),
                        Ok(_) => match fs::read_to_string(&f) {
                            Ok(text) => db.add_from(&text, &f.display().to_string(), false),
                            Err(e) => db.errors.push(format!("{name}: {e}")),
                        },
                        Err(e) => db.errors.push(format!("{name}: {e}")),
                    }
                }
            }
        }
        db
    }

    pub fn add_from(&mut self, json: &str, origin: &str, builtin: bool) {
        let label = if builtin { "builtin".to_string() } else { Path::new(origin).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default() };
        let doc: Doc = match serde_json::from_str(json.trim_start_matches('\u{feff}')) {
            Ok(d) => d,
            Err(e) => {
                self.errors.push(format!("{label}: {e}"));
                return;
            }
        };
        if builtin {
            self.data_version = doc.data_version;
        }
        for v in doc.profiles {
            match serde_json::from_value::<Profile>(v) {
                Ok(mut p) => {
                    if p.id.trim().is_empty() || p.name.trim().is_empty() {
                        self.errors.push(format!("{label}: profile without id or name"));
                        continue;
                    }
                    p.keys.retain(|k| !k.key.trim().is_empty());
                    p.origin = origin.to_string();
                    if let Some(i) = self.profiles.iter().position(|x| x.id.eq_ignore_ascii_case(&p.id)) {
                        self.profiles[i] = p;
                    } else {
                        self.profiles.push(p);
                    }
                }
                Err(e) => self.errors.push(format!("{label}: {e}")),
            }
        }
    }

    pub fn find(&self, id: &str) -> Option<&Profile> {
        if id.is_empty() {
            return None;
        }
        self.profiles.iter().find(|p| p.id.eq_ignore_ascii_case(id))
    }
}
