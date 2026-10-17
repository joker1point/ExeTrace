# ExeTrace (Rust + Win32 native controls) - one-click build
#
# ASCII only: PowerShell 5.1 reads BOM-less files as ANSI and would break on CJK.
# GNU toolchain notes (see README):
#   - mingw bin must be FIRST in PATH (old 32-bit dlltool breaks x86-64 import libs)
#   - .cargo/config.toml pins linker/ar paths for this machine (see config.toml.example)
#
# Usage: .\build.ps1 [-MingwBin <path>]
#   -MingwBin defaults to %USERPROFILE%\mingw64\bin

param(
    [string]$MingwBin = "$env:USERPROFILE\mingw64\bin"
)

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $MyInvocation.MyCommand.Path
Set-Location $root

if (Test-Path $MingwBin) {
    $env:PATH = "$MingwBin;$env:PATH"
} else {
    Write-Host "WARN: mingw bin not found: $MingwBin  (pass -MingwBin <path>)"
}

Write-Host "[1/3] cargo build --release"
cargo build --release
if ($LASTEXITCODE -ne 0) { Write-Host "BUILD FAILED (cargo)"; exit 1 }

$exe = Join-Path $root "target\x86_64-pc-windows-gnu\release\exetrace.exe"
if (-not (Test-Path $exe)) { Write-Host "NOT FOUND: $exe"; exit 1 }

New-Item -ItemType Directory -Force -Path (Join-Path $root "dist") | Out-Null
$out = Join-Path $root "dist\ExeTrace.exe"
Copy-Item $exe $out -Force
$mb = [Math]::Round((Get-Item $out).Length / 1MB, 2)
Write-Host "[2/3] dist\ExeTrace.exe  ($mb MB)"

Write-Host "[3/3] selftest (results also saved to %LOCALAPPDATA%\ExeTrace\selftest.txt)"
& $out --selftest
if ($LASTEXITCODE -ne 0) { Write-Host "SELFTEST FAILED"; exit 1 }

Write-Host "Done: $out"
