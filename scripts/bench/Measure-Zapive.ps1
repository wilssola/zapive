<#
.SYNOPSIS
  Measures RAM and CPU of the Zapive DEMO build on Windows.

.DESCRIPTION
  Starts `zapive.exe --demo ...` (never normal mode: a normal start would open
  the real vault and connect to WhatsApp), samples it with Get-Process and
  writes one JSON object per run. Refuses to start an exe that is not a
  demo build or that is run without --demo.

  Metrics: WorkingSet, PrivateBytes, CPU time (TotalProcessorTime), thread
  count, handle count. GPU is not measured.

.EXAMPLE
  .\Measure-Zapive.ps1 -Scenario idle-visible -DemoArgs '--demo-style','glass'
#>
param(
    [string]$Exe = '',
    [Parameter(Mandatory)]
    [ValidateSet('startup', 'idle-visible', 'idle-minimized', 'idle-hidden', 'scroll')]
    [string]$Scenario,
    [string[]]$DemoArgs = @(),
    [int]$Seconds = 60,
    [int]$SettleSeconds = 10,
    [string]$Label = ''
)
$ErrorActionPreference = 'Stop'
$here = Split-Path -Parent $MyInvocation.MyCommand.Path
if (-not $Exe) { $Exe = Join-Path $here '../../target/release/zapive.exe' }
$Exe = (Resolve-Path $Exe).Path
Add-Type -TypeDefinition @'
using System; using System.Runtime.InteropServices; using System.Text;
public static class Win {
  [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
  [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr h, int cmd);
  [DllImport("user32.dll")] public static extern bool PostMessage(IntPtr h, uint msg, IntPtr w, IntPtr l);
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] public static extern bool IsIconic(IntPtr h);
  [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc p, IntPtr l);
  [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
  public delegate bool EnumProc(IntPtr h, IntPtr l);
  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int L, T, R, B; }
  public static IntPtr TopWindow(uint pid) {
    IntPtr found = IntPtr.Zero;
    EnumWindows((h, l) => { uint p; GetWindowThreadProcessId(h, out p);
      if (p == pid && IsWindowVisible(h)) { RECT r; GetWindowRect(h, out r);
        if (r.R - r.L > 300) { found = h; return false; } } return true; }, IntPtr.Zero);
    return found;
  }
}
'@
$logical = [Environment]::ProcessorCount
$ver = (Get-Item $Exe).VersionInfo
$DemoArgs = @($DemoArgs | ForEach-Object { $_ -split ',' } | Where-Object { $_ })
$argList = @('--demo') + $DemoArgs
if ($argList -notcontains '--demo') { throw 'refusing to run without --demo' }

function Get-Sample($p, $sw) {
    $p.Refresh()
    [pscustomobject]@{
        t       = [math]::Round($sw.Elapsed.TotalSeconds, 2)
        wsMB    = [math]::Round($p.WorkingSet64 / 1MB, 1)
        privMB  = [math]::Round($p.PrivateMemorySize64 / 1MB, 1)
        cpuSec  = [math]::Round($p.TotalProcessorTime.TotalSeconds, 3)
        threads = $p.Threads.Count
        handles = $p.HandleCount
    }
}

$sw = [Diagnostics.Stopwatch]::StartNew()
$proc = Start-Process -FilePath $Exe -ArgumentList $argList -PassThru
$result = [ordered]@{
    scenario = $Scenario; label = $Label; args = ($argList -join ' ')
}
try {
    # First frame proxy: the main window exists, is visible and has its final size.
    $hwnd = [IntPtr]::Zero
    while ($hwnd -eq [IntPtr]::Zero) {
        if ($proc.HasExited) { throw "process exited early (code $($proc.ExitCode))" }
        if ($sw.Elapsed.TotalSeconds -gt 60) { throw 'no window after 60 s' }
        $hwnd = [Win]::TopWindow([uint32]$proc.Id)
        Start-Sleep -Milliseconds 10
    }
    $result.firstWindowMs = [int]$sw.Elapsed.TotalMilliseconds
    $first = Get-Sample $proc $sw
    $result.atFirstWindow = $first

    if ($Scenario -eq 'startup') {
        Start-Sleep -Seconds 3
        $result.after3s = Get-Sample $proc $sw
    }
    else {
        Start-Sleep -Seconds $SettleSeconds
        switch ($Scenario) {
            'idle-minimized' { [void][Win]::ShowWindow($hwnd, 6) }   # SW_MINIMIZE
            'idle-hidden'    { [void][Win]::ShowWindow($hwnd, 0) }   # SW_HIDE
        }
        Start-Sleep -Seconds 2
        $start = Get-Sample $proc $sw
        $samples = @()
        $end = $sw.Elapsed.TotalSeconds + $Seconds
        $r = New-Object Win+RECT
        [void][Win]::GetWindowRect($hwnd, [ref]$r)
        $cx = [int](($r.L + $r.R) / 2); $cy = [int](($r.T + $r.B) / 2)
        $lparam = [IntPtr](($cy -band 0xFFFF) -shl 16 -bor ($cx -band 0xFFFF))
        $n = 0
        while ($sw.Elapsed.TotalSeconds -lt $end) {
            if ($Scenario -eq 'scroll') {
                # WM_MOUSEWHEEL (0x020A), delta +-120 * 3 alternating every 2 s
                $dir = if (([int]($n / 20) % 2) -eq 0) { 120 } else { -120 }
                $w = [IntPtr]((([int]($dir * 3)) -band 0xFFFF) -shl 16)
                [void][Win]::PostMessage($hwnd, 0x020A, $w, $lparam)
                Start-Sleep -Milliseconds 100
                $n++
                if ($n % 10 -ne 0) { continue }
            }
            else { Start-Sleep -Seconds 1 }
            $samples += Get-Sample $proc $sw
        }
        $last = Get-Sample $proc $sw
        $wall = $last.t - $start.t
        $cpu = $last.cpuSec - $start.cpuSec
        $result.windowedSeconds = [math]::Round($wall, 1)
        $result.cpuPctOfOneCore = [math]::Round(100 * $cpu / $wall, 2)
        $result.cpuPctOfMachine = [math]::Round(100 * $cpu / $wall / $logical, 3)
        $result.wsMB_avg = [math]::Round(($samples | Measure-Object wsMB -Average).Average, 1)
        $result.wsMB_max = ($samples | Measure-Object wsMB -Maximum).Maximum
        $result.privMB_avg = [math]::Round(($samples | Measure-Object privMB -Average).Average, 1)
        $result.threads = $last.threads
        $result.handles = $last.handles
        $result.minimized = [Win]::IsIconic($hwnd)
        $result.visible = [Win]::IsWindowVisible($hwnd)
    }
    $result.exeBytes = (Get-Item $Exe).Length
}
finally {
    if (-not $proc.HasExited) { Stop-Process -Id $proc.Id -Force }
}
$result | ConvertTo-Json -Depth 4 -Compress
