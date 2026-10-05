# ExeTrace build script: clean venv + PyInstaller onefile
#
#   .\build.ps1
#
# Constraints (lessons learned):
#   - Never build in a conda base / daily-driver env: dependency pollution
#     inflates the exe size several times over.
#   - Delivery form is a single exe; after building, always run --selftest
#     (windowed product has no console: read the report file back).
#   - Keep this file pure ASCII. Windows PowerShell 5.1 reads BOM-less .ps1
#     as ANSI, and non-ASCII text breaks the parser.
$ErrorActionPreference = "Stop"
# Neutralize externally injected sitecustomize shims (e.g. an IDE-provided
# PYTHONPATH) that intercept shutil removals and make PyInstaller --clean fail
# silently, leaving a stale exe behind (seen 2026-10-05).
$env:PYTHONPATH = ""
$root = $PSScriptRoot
$venv = Join-Path $root ".venv-build"
$py = Join-Path $venv "Scripts\python.exe"

function New-BuildVenv {
    param([string]$Path)
    foreach ($launcher in @(@("py", "-3.14"), @("py", "-3.13"), @("python", ""))) {
        try {
            if ($launcher[1]) { & $launcher[0] $launcher[1] -m venv $Path } else { & $launcher[0] -m venv $Path }
            if (Test-Path (Join-Path $Path "Scripts\python.exe")) { return }
        } catch { }
    }
    throw "Cannot create build venv: no usable Python 3 interpreter found."
}

if (-not (Test-Path $py)) {
    Write-Host "[1/5] Creating clean build venv ..."
    New-BuildVenv -Path $venv
} else {
    Write-Host "[1/5] Reusing existing build venv"
}

Write-Host "[2/5] Installing dependencies ..."
& $py -m pip install -q --disable-pip-version-check -r (Join-Path $root "requirements.txt") pyinstaller

Write-Host "[3/5] Generating app icon ..."
& $py (Join-Path $root "tools\gen_icon.py")

Write-Host "[4/5] PyInstaller packaging (onefile / windowed) ..."
& $py -m PyInstaller --noconfirm --clean --onefile --windowed `
    --name "ExeTrace" `
    --icon (Join-Path $root "assets\app.ico") `
    --hidden-import "pystray._win32" `
    --version-file (Join-Path $root "packaging\version_info.txt") `
    --distpath (Join-Path $root "dist") `
    --workpath (Join-Path $root "build") `
    (Join-Path $root "src\main.py")

$exe = Join-Path $root "dist\ExeTrace.exe"
if (-not (Test-Path $exe)) { throw "Build failed: $exe not found." }

# Freshness gate: a pre-existing exe would otherwise pass every later check
# even if this build never refreshed it.
$newestSrc = Get-ChildItem (Join-Path $root "src") -Recurse -File -Filter "*.py" |
    Sort-Object LastWriteTime -Descending | Select-Object -First 1
if ((Get-Item $exe).LastWriteTime -lt $newestSrc.LastWriteTime) {
    throw "Stale artifact: $exe is older than src\$($newestSrc.Name). Packaging did not refresh the exe."
}

Write-Host "[5/5] Verifying the artifact (--selftest, report written to disk) ..."
& $exe --selftest | Out-Null
$report = Join-Path $env:LOCALAPPDATA "ExeTrace\selftest.txt"
if (Test-Path $report) {
    Get-Content $report -Encoding UTF8 | ForEach-Object { Write-Host "    $_" }
    if ((Get-Content $report -Encoding UTF8 -Tail 1) -notmatch "SELFTEST OK") {
        throw "Artifact selftest FAILED. See $report"
    }
} else {
    throw "Artifact produced no selftest report: $report"
}

$sizeMB = [Math]::Round((Get-Item $exe).Length / 1MB, 1)
Write-Host ""
Write-Host "Done: $exe  ($sizeMB MB)"
