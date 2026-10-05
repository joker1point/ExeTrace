# ExeTrace (Rust/egui) build script
#
#   .\build.ps1
#
# Toolchain notes (lessons learned on this machine):
#   - No MSVC / Windows SDK: we use the GNU toolchain. The project pins
#     stable-x86_64-pc-windows-gnu via rust-toolchain.toml, because build
#     scripts / proc-macros compile with the HOST toolchain and would
#     otherwise demand link.exe.
#   - mingw-w64 lives in the user dir (C:\Users\<user>\mingw64), installed
#     from the gh-proxy mirror. Its bin MUST precede the old 32-bit
#     C:\MinGW\bin in PATH, otherwise rustc picks the wrong dlltool.
$ErrorActionPreference = "Stop"
$root = $PSScriptRoot
$mingwBin = Join-Path $env:USERPROFILE "mingw64\bin"

if (-not (Test-Path (Join-Path $mingwBin "gcc.exe"))) {
    throw "mingw-w64 not found at $mingwBin"
}
$env:PATH = "$mingwBin;$env:PATH"

Write-Host "[1/4] cargo build --release ..."
Push-Location $root
try {
    & cargo build --release
    if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }
} finally {
    Pop-Location
}

$artifact = Join-Path $root "target\x86_64-pc-windows-gnu\release\exetrace.exe"
if (-not (Test-Path $artifact)) { throw "artifact not found: $artifact" }

$outDir = Join-Path $root "dist"
New-Item -ItemType Directory -Force -Path $outDir | Out-Null
$out = Join-Path $outDir "ExeTrace.exe"
Copy-Item $artifact $out -Force

Write-Host "[2/4] artifact selftest ..."
& $out --selftest | Out-Null
$report = Join-Path $env:LOCALAPPDATA "ExeTrace\selftest.txt"
if (Test-Path $report) {
    Get-Content $report -Encoding UTF8 | ForEach-Object { Write-Host "    $_" }
    if ((Get-Content $report -Encoding UTF8 -Tail 1) -notmatch "SELFTEST OK") {
        throw "artifact selftest FAILED (see $report)"
    }
} else {
    throw "artifact produced no selftest report: $report"
}

Write-Host "[3/4] smoke GUI (start + window title + kill) ..."
$proc = Start-Process -FilePath $out -PassThru
Start-Sleep -Seconds 6
Add-Type -AssemblyName System.Windows.Forms -ErrorAction SilentlyContinue
$found = $false
for ($i = 0; $i -lt 20 -and -not $found; $i++) {
    Start-Sleep -Milliseconds 500
    $found = [bool](Get-Process -Id $proc.Id -ErrorAction SilentlyContinue)
}
if (-not $found) { throw "GUI process exited unexpectedly" }
Stop-Process -Id $proc.Id -Force -ErrorAction SilentlyContinue
Start-Sleep -Milliseconds 800
Get-Process exetrace -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue

$sizeMB = [Math]::Round((Get-Item $out).Length / 1MB, 1)
Write-Host "[4/4] Done: $out ($sizeMB MB)"
