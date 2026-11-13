# Contributing a laptop model

Thanks for helping! Model data lives in one file: [`data/profiles.json`](data/profiles.json).
The app embeds it at build time and the website reads the same file.

## The one rule: official sources only

Every key in a profile must come from the **maker's own documentation for that exact model** —
the ASUS E-Manual, the Lenovo User Guide, Lenovo PSREF or a vendor KB article. Put the link in `sources`
(with the page number for PDFs), `"kind": "official"`. An unmodified copy of the vendor's PDF hosted elsewhere
(when the vendor link is dead) may be cited as `"kind": "official-copy"`. Reviews, forums and Reddit may only back up
*detection strings*, marked `"kind": "community"`.

A wrong key is worse than a missing model, so if something differs by year or SKU, split the profile or add a
`note_en` / `note_ko` to that key instead of guessing.

## Profile format

```jsonc
{
  "id": "asus-expertbook-p1-p1403-p1503",          // kebab-case, never changes once published
  "vendor": "ASUS",                                 // "ASUS" or "Lenovo"
  "series": "ExpertBook P1",
  "name": "ASUS ExpertBook P1 (P1403CVA / P1503CVA)",
  "years": "2023–2024",
  "match": {
    "manufacturer": ["^ASUSTeK"],                   // regex vs SystemManufacturer (required)
    "product": ["P1503CVA", "P1403CVA"],            // regex vs SystemProductName / BaseBoardProduct, starts at a word boundary
    "family": [],                                   // regex vs SystemVersion / SystemFamily (Lenovo: "ThinkPad T14 Gen 4")
    "mtm": []                                       // Lenovo machine types: first 4 chars of SystemProductName, e.g. "21HM"
  },
  "fnLock": { "combo": "Fn+Esc", "default_en": "…", "default_ko": "…" },
  "keys": [
    { "row": "top",   "key": "F1", "icon": "mute", "en": "Mute speakers", "ko": "스피커 음소거" },
    { "row": "combo", "key": "B",  "icon": "break", "en": "Break", "ko": "Break", "note_en": "…", "note_ko": "…",
      "evidence_en": "Where this came from (not shown on screen)", "evidence_ko": "…" }
  ],
  "sources": [ { "title": "…User Guide, p.23", "url": "https://…", "kind": "official" } ],
  "verified": "official-manual"                     // or "official-web" / "partial"
}
```

- `row: "top"` — Esc / F1–F12 row and its neighbours; whether they need Fn depends on Fn Lock.
- `row: "combo"` — always Fn + that key.
- `icon` — one of the names in [`src/glyphs.rs`](src/glyphs.rs).
- Labels are short (they go on a key cap): "Volume up", "볼륨 높이기". Put qualifiers ("selected models only",
  "customizable in MyASUS") in `note_en` / `note_ko` — always both languages. Research provenance ("legend printed on
  the key in the guide's illustration") goes in `evidence_en` / `evidence_ko`, which the overlay doesn't show.
- `verified`: `official-manual` / `official-web` when every key is backed by the source, `partial` otherwise (the overlay
  then shows a "partly verified" warning).
- Patterns are [Rust `regex`](https://docs.rs/regex) syntax, case-insensitive: no look-around or backreferences.

### Detection must never guess

The app picks a profile only when a **model-level** rule matches (machine type → 100, model code → 90,
family name → 80). A manufacturer match alone never selects anything; two equal matches make the app ask the user.
So keep `product` / `family` rules tight, and add a case to [`data/match-tests.json`](data/match-tests.json) for
every model you add — including a negative one if a neighbouring model must *not* match.

## Check your change

```powershell
.\build.ps1          # cargo test, builds dist\FnCheatsheet.exe, runs the self-test (schema + match cases)
.\dist\FnCheatsheet.exe --render <profile-id> preview.png ko dark   # see the overlay as a PNG
```

CI runs the same self-test on every pull request.

## Trying a profile without rebuilding

Drop a JSON file with `{ "profiles": [ … ] }` into `%APPDATA%\FnCheatsheet\profiles\` and press **Reload** in
Settings → Device. A profile with the same `id` as a built-in one replaces it.
