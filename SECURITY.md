# Security

## Design

Fn Cheatsheet installs a **low-level keyboard hook** (`WH_KEYBOARD_LL`) and updates itself, so it is built to be boring:

- **Never elevated.** The manifest asks for `asInvoker`; there is no admin mode, service, driver or scheduled task. "Start with Windows" is a per-user `HKCU\…\Run` entry.
- **No key logging.** The hook only compares each key event with the trigger key and keeps a "held alone?" state. Key events are not stored and never written to the log. The *capture key* screen shows the key you press, only while it is open.
- **Never blocks or changes input.** Every hook callback passes the event on (`CallNextHookEx`). The hooks run on their own thread with their own message loop, so a busy UI can't make Windows drop or delay them.
- **Reads model strings only.** Device detection reads `HKLM\HARDWARE\DESCRIPTION\System\BIOS` (manufacturer, product, family, version, SKU, baseboard). No serial numbers or UUIDs.
- **Profile files are data, not code.** User profile JSON in `%APPDATA%\FnCheatsheet\profiles\` is size-limited (2 MB). Patterns use the Rust `regex` crate, which runs in linear time (no backtracking), so a malicious pattern cannot hang the app.
- **Local data only.** `%APPDATA%\FnCheatsheet\settings.json`, optional profile files, and a small rotating log in `%LOCALAPPDATA%\FnCheatsheet\log.txt`.

## Network and auto-update

The only network access is the update check (it can be turned off in Settings → About):

1. Once a day (first check ~90 s after start) it calls `https://api.github.com/repos/jeiel85/cheatsheet-win/releases/latest` through WinHTTP (system TLS and proxy). Drafts and pre-releases are ignored.
2. If the release is newer, it downloads `FnCheatsheet.exe` and `FnCheatsheet.exe.sha256` — only when their `browser_download_url` is exactly `https://github.com/jeiel85/cheatsheet-win/releases/download/<tag>/<name>`. Redirects to GitHub's asset CDN are followed; HTTPS→HTTP redirects are refused. Sizes are capped.
3. It checks the size against the release metadata, the SHA-256 against the `.sha256` file, and the `MZ` executable header, then writes `FnCheatsheet.exe.new` next to the running exe.
4. It renames the running exe to `.old`, moves the new one in, starts it and exits. Any failure restores the old exe. The `.old` file is deleted on the next start.
5. If the exe's folder is not writable (e.g. `C:\Program Files`), nothing is downloaded; you only get a notice that a new version exists.

The checksum file comes from the same release, so it protects against corrupted or truncated downloads, not against a compromised GitHub account. Releases are built by GitHub Actions from tagged commits (`.github/workflows/release.yml`).

## Reporting a vulnerability

Please use GitHub's **private vulnerability reporting** (Security tab → *Report a vulnerability*) rather than a public issue.
