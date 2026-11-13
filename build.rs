// Embeds the icon, version info and application manifest into FnCheatsheet.exe.
// The version comes from APP_VERSION (set by build.ps1 / the release workflow) or Cargo.toml.
use std::{env, fs, path::PathBuf};

fn main() {
    println!("cargo:rerun-if-env-changed=APP_VERSION");
    println!("cargo:rerun-if-changed=assets/app.ico");
    println!("cargo:rerun-if-changed=assets/app.manifest");
    println!("cargo:rerun-if-changed=data/profiles.json");

    let version = env::var("APP_VERSION").unwrap_or_else(|_| env::var("CARGO_PKG_VERSION").unwrap());
    let parts: Vec<u32> = version.split('.').map(|p| p.parse().expect("APP_VERSION must be like 1.2.3")).collect();
    assert_eq!(parts.len(), 3, "APP_VERSION must be like 1.2.3");
    println!("cargo:rustc-env=APP_VERSION={version}");

    let root = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let esc = |p: PathBuf| p.display().to_string().replace('\\', "\\\\");
    let rc = format!(
        r#"#pragma code_page(65001)
1 ICON "{icon}"
1 24 "{manifest}"
1 VERSIONINFO
FILEVERSION {a},{b},{c},0
PRODUCTVERSION {a},{b},{c},0
FILEOS 0x40004
FILETYPE 0x1
BEGIN
  BLOCK "StringFileInfo"
  BEGIN
    BLOCK "040904B0"
    BEGIN
      VALUE "CompanyName", "jeiel85"
      VALUE "FileDescription", "Fn Cheatsheet"
      VALUE "FileVersion", "{v}"
      VALUE "InternalName", "FnCheatsheet"
      VALUE "LegalCopyright", "Copyright (c) 2026 jeiel85. MIT License."
      VALUE "OriginalFilename", "FnCheatsheet.exe"
      VALUE "ProductName", "Fn Cheatsheet"
      VALUE "ProductVersion", "{v}"
    END
  END
  BLOCK "VarFileInfo"
  BEGIN
    VALUE "Translation", 0x409, 1200
  END
END
"#,
        icon = esc(root.join("assets").join("app.ico")),
        manifest = esc(root.join("assets").join("app.manifest")),
        a = parts[0], b = parts[1], c = parts[2], v = version,
    );
    let out = PathBuf::from(env::var("OUT_DIR").unwrap()).join("app.rc");
    fs::write(&out, rc).unwrap();
    embed_resource::compile(&out, embed_resource::NONE).manifest_required().unwrap();
}
