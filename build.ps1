<#
  Builds FnCheatsheet.exe into .\dist with its SHA-256 checksum, after running the unit tests and the
  profile self-test (data schema + SMBIOS match cases).

  .\build.ps1                    # version from Cargo.toml
  .\build.ps1 -Version 0.1.0     # stamp a version (tools/release.ps1 passes it)
  .\build.ps1 -SkipTests
#>
param(
    [string]$Version = '',
    [switch]$SkipTests
)
$ErrorActionPreference = 'Stop'
Set-Location $PSScriptRoot
if ($Version) {
    if ($Version -notmatch '^\d+\.\d+\.\d+$') { throw "Version must look like 1.2.3 (got '$Version')" }
    $env:APP_VERSION = $Version
} else {
    Remove-Item Env:APP_VERSION -ErrorAction SilentlyContinue
}

if (-not $SkipTests) {
    cargo test --release --quiet
    if ($LASTEXITCODE -ne 0) { throw "cargo test failed ($LASTEXITCODE)" }
}
cargo build --release --quiet
if ($LASTEXITCODE -ne 0) { throw "cargo build failed ($LASTEXITCODE)" }

$dist = Join-Path $PSScriptRoot 'dist'
New-Item -ItemType Directory -Force $dist | Out-Null
$exe = Join-Path $dist 'FnCheatsheet.exe'
Copy-Item (Join-Path $PSScriptRoot 'target\release\FnCheatsheet.exe') $exe -Force

if (-not $SkipTests) {
    $report = Join-Path $dist 'selftest.txt'
    $p = Start-Process -FilePath $exe -ArgumentList @('--selftest', (Join-Path $PSScriptRoot 'data\match-tests.json'), '--out', $report) -Wait -PassThru -NoNewWindow
    $text = Get-Content $report -Raw
    Remove-Item $report
    if ($p.ExitCode -ne 0) { Write-Host $text; throw "Self-test failed ($($p.ExitCode))" }
    Write-Host ($text.Trim() -split "`n" | Select-Object -Last 3)
}

# FnProbe: the standalone Fn-signal diagnostic (C#, .NET Framework — built with the csc that ships with Windows).
$csc = Join-Path $env:WINDIR 'Microsoft.NET\Framework64\v4.0.30319\csc.exe'
if (Test-Path $csc) {
    $probe = Join-Path $dist 'FnProbe.exe'
    & $csc -nologo -platform:x64 -optimize -r:System.Management.dll -r:System.Windows.Forms.dll "-out:$probe" (Join-Path $PSScriptRoot 'tools\fn-probe\FnProbe.cs')
    if ($LASTEXITCODE -ne 0) { throw "FnProbe build failed ($LASTEXITCODE)" }
    $ph = (Get-FileHash $probe -Algorithm SHA256).Hash.ToLowerInvariant()
    [IO.File]::WriteAllText("$probe.sha256", "$ph  FnProbe.exe`n")
} else {
    Write-Warning 'csc.exe not found: FnProbe.exe not built'
}

$hash = (Get-FileHash $exe -Algorithm SHA256).Hash.ToLowerInvariant()
[IO.File]::WriteAllText("$exe.sha256", "$hash  FnCheatsheet.exe`n")
$kb = [math]::Round((Get-Item $exe).Length / 1KB)
Write-Host "Built $exe ($kb KB)"
Write-Host "SHA-256 $hash"
