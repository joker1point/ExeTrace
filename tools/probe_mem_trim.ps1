# ExeTrace background-memory probe: measure EmptyWorkingSet effect and rebound.
#
# Usage: powershell -File tools\probe_mem_trim.ps1 [-ProcName exetrace] [-IntervalSec 15] [-Samples 5]
#
# Metrics:
#   PrivateWS = Private Working Set (Task Manager "Memory" column)
#   WS        = total working set (incl. shared pages)
#   Commit    = private commit size
#
# NOTE: keep this file PURE ASCII -- Windows PowerShell 5.1 parses BOM-less
# .ps1 as ANSI and non-ASCII bytes break the parser (lesson from build.ps1).
param(
    [string]$ProcName = "exetrace",
    [int]$IntervalSec = 15,
    [int]$Samples = 5
)
$ErrorActionPreference = "Continue"

Add-Type -Namespace Etx -Name Mem -MemberDefinition @'
[DllImport("kernel32.dll", SetLastError=true)] public static extern IntPtr OpenProcess(int access, bool inherit, int pid);
[DllImport("psapi.dll", SetLastError=true)] public static extern bool EmptyWorkingSet(IntPtr h);
[DllImport("kernel32.dll")] public static extern bool CloseHandle(IntPtr h);
'@
Add-Type -Namespace Etx -Name Win -MemberDefinition @'
[DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
'@

function Get-Big { param([string]$n)
    Get-Process -Name $n -ErrorAction SilentlyContinue | Sort-Object WorkingSet64 -Descending | Select-Object -First 1
}

function Show-Mem { param([string]$tag)
    $p = Get-Big $ProcName
    if (-not $p) { Write-Host ("{0,-12} process not found" -f $tag); return }
    # PrivateWS must be read per-PID: the onefile bootloader shares the process
    # name, and "\Process(name)\..." counter picks the WRONG instance.
    $perf = Get-CimInstance Win32_PerfFormattedData_PerfProc_Process -Filter "IDProcess=$($p.Id)" -ErrorAction SilentlyContinue
    $pws = if ($perf) { [math]::Round($perf.WorkingSetPrivate / 1MB, 1) } else { "n/a" }
    Write-Host ("{0,-12} PID={1}  PrivateWS={2,7} MB  WS={3,7} MB  Commit={4,7} MB" -f `
        $tag, $p.Id, $pws, [math]::Round($p.WorkingSet64 / 1MB, 1), [math]::Round($p.PrivateMemorySize64 / 1MB, 1))
}

$p0 = Get-Big $ProcName
if (-not $p0) { Write-Host "process not found: $ProcName"; exit 1 }
Write-Host ("target: {0} (PID {1})  main-window visible: {2}" -f $ProcName, $p0.Id, [Etx.Win]::IsWindowVisible($p0.MainWindowHandle))
Write-Host ""
Write-Host "=== A. baseline (every $IntervalSec s) ==="
Show-Mem "t0"
for ($i = 1; $i -le $Samples; $i++) { Start-Sleep -Seconds $IntervalSec; Show-Mem "t+$($i * $IntervalSec)s" }

Write-Host ""
Write-Host "=== B. EmptyWorkingSet ==="
$p = Get-Big $ProcName
$h = [Etx.Mem]::OpenProcess(0x0500, $false, $p.Id)   # QUERY_INFORMATION | SET_QUOTA
if ($h -eq [IntPtr]::Zero) {
    Write-Host "OpenProcess failed err=$([Runtime.InteropServices.Marshal]::GetLastWin32Error())"
    exit 1
}
$ok = [Etx.Mem]::EmptyWorkingSet($h)
[Etx.Mem]::CloseHandle($h) | Out-Null
Write-Host "EmptyWorkingSet returned: $ok"
Show-Mem "trim+0s"
for ($i = 1; $i -le $Samples; $i++) { Start-Sleep -Seconds $IntervalSec; Show-Mem "trim+$($i * $IntervalSec)s" }
