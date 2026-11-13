<#
  Fn Cheatsheet release: build -> sign -> tag -> GitHub release. Runs on the maintainer's PC, because the
  update signing key must never reach GitHub (a stolen GitHub account must not be able to ship an update).

    .\tools\release.ps1 -Version 0.1.6            # build, sign, tag, upload, publish
    .\tools\release.ps1 -Version 0.1.6 -DryRun    # build and sign into dist\ only

  Requirements
    - Update signing key: %USERPROFILE%\.tauri\fncheatsheet.key  (cargo tauri signer generate -w ...)
      Its public key is in src/update.rs (UPDATE_KEYS), next to an offline backup key.
      If both are lost, installed copies can never verify another update. Back them up.
    - cargo-tauri (for `cargo tauri signer sign`), gh CLI logged in.
    - Clean main, in sync with origin; Cargo.toml version == -Version; docs/release-notes/v<Version>.md.
#>
param(
    [Parameter(Mandatory)][string]$Version,
    [switch]$DryRun,
    [string]$KeyPath = (Join-Path $HOME '.tauri\fncheatsheet.key')
)
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root
$env:PATH = "$HOME\.cargo\bin;$env:PATH"
$repo = 'jeiel85/cheatsheet-win'
$tag = "v$Version"

function Check($ok, $msg) { if (-not $ok) { throw $msg } }

# ── preflight ──
Check ($Version -match '^\d+\.\d+\.\d+$') "Version must look like 1.2.3 (got '$Version')"
Check (Test-Path $KeyPath) "Signing key not found: $KeyPath"
$cargoVersion = (Select-String -Path Cargo.toml -Pattern '^version\s*=\s*"([^"]+)"' | Select-Object -First 1).Matches[0].Groups[1].Value
Check ($cargoVersion -eq $Version) "Cargo.toml says $cargoVersion, not $Version"
$notes = "docs/release-notes/$tag.md"
Check (Test-Path $notes) "Release notes missing: $notes"
if (-not $DryRun) {
    Check ((git branch --show-current) -eq 'main') 'Not on main'
    Check (-not (git status --porcelain)) 'Working tree is not clean'
    git fetch -q origin main --tags
    Check ((git rev-parse HEAD) -eq (git rev-parse origin/main)) 'main is not in sync with origin/main'
    Check (-not (git tag -l $tag)) "Tag $tag already exists"
}

# ── build (tests, self-test, FnProbe, checksums) ──
& "$root\build.ps1" -Version $Version
Check ($LASTEXITCODE -eq 0 -or $null -eq $LASTEXITCODE) 'build failed'
$exe = Join-Path $root 'dist\FnCheatsheet.exe'
$sig = "$exe.sig"

# ── sign (the key is read from the file by the signer; build.ps1 never sees it) ──
Remove-Item $sig -ErrorAction SilentlyContinue
try {
    $env:TAURI_SIGNING_PRIVATE_KEY_PASSWORD = ''
    cargo tauri signer sign --private-key-path $KeyPath --app-version $Version $exe | Out-Null
    Check ($LASTEXITCODE -eq 0) 'signing failed'
} finally {
    Remove-Item Env:TAURI_SIGNING_PRIVATE_KEY_PASSWORD -ErrorAction SilentlyContinue
}
Check (Test-Path $sig) "signature not found: $sig"

# ── verify exactly like installed copies will ──
$out = Join-Path $env:TEMP "fncheatsheet-verify-$Version.txt"
$p = Start-Process $exe -ArgumentList '--verify-update', "`"$exe`"", "`"$sig`"", $Version, '--out', "`"$out`"" -Wait -PassThru -WindowStyle Hidden
$result = Get-Content $out -Raw -ErrorAction SilentlyContinue
Remove-Item $out -ErrorAction SilentlyContinue
Check ($p.ExitCode -eq 0) "signature check failed: $result"
Write-Host "Signed and verified: $($result.Trim())" -ForegroundColor Green

if ($DryRun) { Write-Host 'Dry run: nothing tagged or uploaded.'; return }

# ── tag + draft release + assets ──
git tag -a $tag -m "Fn Cheatsheet $tag"
git push -q origin $tag
Check ($LASTEXITCODE -eq 0) 'tag push failed'
$assets = 'FnCheatsheet.exe', 'FnCheatsheet.exe.sha256', 'FnCheatsheet.exe.sig', 'FnProbe.exe', 'FnProbe.exe.sha256' | ForEach-Object { Join-Path $root "dist\$_" }
# Draft first: a published release without its assets would become "latest" and break the updater.
gh release create $tag --repo $repo --draft --title "Fn Cheatsheet $tag" --notes-file $notes --verify-tag @assets
Check ($LASTEXITCODE -eq 0) 'release create failed'
$names = gh release view $tag --repo $repo --json assets -q '.assets[].name'
foreach ($a in 'FnCheatsheet.exe', 'FnCheatsheet.exe.sha256', 'FnCheatsheet.exe.sig') { Check ($names -contains $a) "missing asset $a" }

# ── publish; "latest" only if newer than the current latest ──
$current = gh release view --repo $repo --json tagName -q '.tagName' 2>$null
$makeLatest = $LASTEXITCODE -ne 0 -or -not $current -or ([version]$Version -gt [version]$current.TrimStart('v'))
gh release edit $tag --repo $repo --draft=false "--latest=$($makeLatest.ToString().ToLower())"
Check ($LASTEXITCODE -eq 0) 'publish failed'
Write-Host "Published $tag" -ForegroundColor Green
