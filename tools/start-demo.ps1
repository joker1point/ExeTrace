# Start a DEMO-ONLY ExeTrace instance (isolated data dir + mutex).
#
# Use this for screen recording / demos: it keeps the real history database
# untouched and the app can run side-by-side with your normal instance.
# Close the window to exit the demo instance.
#
# NOTE: keep this file pure ASCII (PowerShell 5.1 + BOM-less UTF-8 lesson).
$env:EXETRACE_MUTEX_NAME = "Local\ExeTrace_Demo"
$env:EXETRACE_DATA_DIR = Join-Path $env:TEMP "exetrace_demo_data"
$exe = Join-Path $PSScriptRoot "..\dist\ExeTrace.exe"
if (-not (Test-Path $exe)) { Write-Host "dist\ExeTrace.exe not found - run build.ps1 first"; exit 1 }
Write-Host "Demo instance starting (data: $env:EXETRACE_DATA_DIR)"
Start-Process -FilePath $exe
