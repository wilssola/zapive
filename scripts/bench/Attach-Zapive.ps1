<#
.SYNOPSIS
  READ-ONLY sampler for an already-running Zapive process. It never starts,
  signals, injects into or otherwise touches the process: it only reads
  process counters (Get-Process) and queries thread names with
  THREAD_QUERY_LIMITED_INFORMATION.

.DESCRIPTION
  Every second: CPU % (delta of TotalProcessorTime over wall time), working
  set, private bytes, thread count. At the end: the top threads by CPU time
  spent during the window, with thread id, name (GetThreadDescription),
  start address and the module that contains it.

.EXAMPLE
  .\Attach-Zapive.ps1 -AttachPid 1234 -Seconds 30
  .\Attach-Zapive.ps1 -Seconds 30          # picks the only running zapive
#>
param(
    [int]$AttachPid = 0,
    [int]$Seconds = 30,
    [int]$Top = 10
)
$ErrorActionPreference = 'Stop'
Add-Type -TypeDefinition @'
using System; using System.Runtime.InteropServices; using System.Text;
public static class Thr {
  [DllImport("kernel32.dll", SetLastError=true)] static extern IntPtr OpenThread(uint access, bool inherit, uint tid);
  [DllImport("kernel32.dll")] static extern bool CloseHandle(IntPtr h);
  [DllImport("kernel32.dll")] static extern int GetThreadDescription(IntPtr h, out IntPtr name);
  [DllImport("kernel32.dll")] static extern IntPtr LocalFree(IntPtr p);
  [DllImport("ntdll.dll")] static extern int NtQueryInformationThread(IntPtr h, int cls, out IntPtr info, int len, IntPtr ret);
  // ThreadQuerySetWin32StartAddress = 9; needs THREAD_QUERY_INFORMATION (0x40).
  public static ulong StartAddress(uint tid) {
    IntPtr h = OpenThread(0x0040, false, tid);
    if (h == IntPtr.Zero) return 0;
    try { IntPtr a; return NtQueryInformationThread(h, 9, out a, IntPtr.Size, IntPtr.Zero) == 0 ? (ulong)a.ToInt64() : 0; }
    finally { CloseHandle(h); }
  }
  public static string Name(uint tid) {
    IntPtr h = OpenThread(0x0800, false, tid); // THREAD_QUERY_LIMITED_INFORMATION
    if (h == IntPtr.Zero) return "";
    try { IntPtr p; if (GetThreadDescription(h, out p) < 0 || p == IntPtr.Zero) return "";
      string s = Marshal.PtrToStringUni(p); LocalFree(p); return s; }
    finally { CloseHandle(h); }
  }
}
'@
if ($AttachPid -eq 0) {
    $found = @(Get-Process -Name zapive -ErrorAction SilentlyContinue)
    if ($found.Count -ne 1) { throw "pass -AttachPid: found $($found.Count) zapive processes" }
    $AttachPid = $found[0].Id
}
$p = Get-Process -Id $AttachPid
if ($p.ProcessName -ne 'zapive') { throw "PID $AttachPid is not zapive" }
$logical = [Environment]::ProcessorCount

function Snap($p) {
    $p.Refresh()
    $threads = @{}
    foreach ($t in $p.Threads) {
        try { $cpu = $t.TotalProcessorTime.TotalSeconds } catch { $cpu = 0 }
        $addr = [Thr]::StartAddress([uint32]$t.Id)
        $threads[$t.Id] = @{ cpu = $cpu; addr = $addr; state = "$($t.ThreadState)"; wait = if ($t.ThreadState -eq 'Wait') { "$($t.WaitReason)" } else { '' } }
    }
    [pscustomobject]@{
        cpu = $p.TotalProcessorTime.TotalSeconds; ws = $p.WorkingSet64; priv = $p.PrivateMemorySize64
        threads = $p.Threads.Count; handles = $p.HandleCount; map = $threads
    }
}

$sw = [Diagnostics.Stopwatch]::StartNew()
$first = Snap $p; $prev = $first; $prevT = 0.0
$rows = @()
while ($sw.Elapsed.TotalSeconds -lt $Seconds) {
    Start-Sleep -Seconds 1
    $cur = Snap $p; $t = $sw.Elapsed.TotalSeconds
    $rows += [pscustomobject]@{
        t = [math]::Round($t, 1)
        cpuPctOfOneCore = [math]::Round(100 * ($cur.cpu - $prev.cpu) / ($t - $prevT), 1)
        wsMB = [math]::Round($cur.ws / 1MB, 1); privMB = [math]::Round($cur.priv / 1MB, 1)
        threads = $cur.threads
    }
    $prev = $cur; $prevT = $t
}
$wall = $sw.Elapsed.TotalSeconds
$modules = @($p.Modules | ForEach-Object { [pscustomobject]@{ name = $_.ModuleName; base = [uint64]$_.BaseAddress.ToInt64(); size = $_.ModuleMemorySize } })
function Where-Addr($a) {
    if (-not $a) { return '' }
    $m = $modules | Where-Object { $a -ge $_.base -and $a -lt ($_.base + $_.size) } | Select-Object -First 1
    if ($m) { '{0}+0x{1:x}' -f $m.name, ($a - $m.base) } else { '0x{0:x}' -f $a }
}
$threadRows = foreach ($id in $prev.map.Keys) {
    $before = if ($first.map.ContainsKey($id)) { $first.map[$id].cpu } else { 0 }
    $d = $prev.map[$id].cpu - $before
    [pscustomobject]@{
        tid = $id; cpuSec = [math]::Round($d, 2); pctOfOneCore = [math]::Round(100 * $d / $wall, 1)
        totalCpuSec = [math]::Round($prev.map[$id].cpu, 1)
        name = [Thr]::Name([uint32]$id); start = Where-Addr $prev.map[$id].addr
        state = $prev.map[$id].state; wait = $prev.map[$id].wait
    }
}
$totalCpu = $prev.cpu - $first.cpu
$summary = [ordered]@{
    pid = $AttachPid; seconds = [math]::Round($wall, 1); logicalProcessors = $logical
    cpuPctOfOneCore = [math]::Round(100 * $totalCpu / $wall, 1)
    cpuPctOfMachine = [math]::Round(100 * $totalCpu / $wall / $logical, 2)
    cpuPctMaxSecond = ($rows | Measure-Object cpuPctOfOneCore -Maximum).Maximum
    privMB = [math]::Round($prev.priv / 1MB, 1); privMB_start = [math]::Round($first.priv / 1MB, 1)
    wsMB = [math]::Round($prev.ws / 1MB, 1)
    threads = $prev.threads; handles = $prev.handles
    byName = @($threadRows | Group-Object { if ($_.name) { $_.name } else { '(unnamed)' } } | ForEach-Object {
        [pscustomobject]@{ name = $_.Name; threads = $_.Count; cpuSec = [math]::Round(($_.Group | Measure-Object cpuSec -Sum).Sum, 2); pctOfOneCore = [math]::Round(($_.Group | Measure-Object pctOfOneCore -Sum).Sum, 1) } } | Sort-Object cpuSec -Descending)
    topThreads = @($threadRows | Sort-Object cpuSec -Descending | Select-Object -First $Top)
    samples = $rows
}
$summary | ConvertTo-Json -Depth 5
