<#
.SYNOPSIS
  A/B-compares several demo builds (different profiles/allocators) with the
  same scenarios. Each exe needs ctranslate2.dll and libiomp5md.dll beside it.

.EXAMPLE
  .\Compare-Builds.ps1 -Exe C:\bin\size.exe,C:\bin\o3.exe -Runs 3 -Seconds 30
#>
param(
    [Parameter(Mandatory)][string[]]$Exe,
    [int]$Runs = 3,
    [int]$Seconds = 30
)
$ErrorActionPreference = 'Stop'
$here = Split-Path -Parent $MyInvocation.MyCommand.Path
$measure = Join-Path $here 'Measure-Zapive.ps1'
$median = { param($a) $s = @($a | Sort-Object); $s[[int][math]::Floor(($s.Count - 1) / 2)] }
foreach ($e in $Exe) {
    $size = (Get-Item $e).Length
    $start = 1..$Runs | ForEach-Object {
        & $measure -Exe $e -Scenario startup -DemoArgs '--demo-style', 'glass' | ConvertFrom-Json
    }
    $idle = 1..$Runs | ForEach-Object {
        & $measure -Exe $e -Scenario idle-visible -Seconds $Seconds -SettleSeconds 5 -DemoArgs '--demo-style', 'glass' | ConvertFrom-Json
    }
    $scroll = 1..$Runs | ForEach-Object {
        & $measure -Exe $e -Scenario scroll -Seconds $Seconds -SettleSeconds 5 -DemoArgs '--demo-style', 'glass' | ConvertFrom-Json
    }
    [pscustomobject]@{
        exe          = Split-Path -Leaf $e
        sizeMiB      = [math]::Round($size / 1MB, 1)
        firstWinMs   = & $median ($start.firstWindowMs)
        ws3sMB       = & $median ($start.after3s.wsMB)
        priv3sMB     = & $median ($start.after3s.privMB)
        idleWsMB     = & $median ($idle.wsMB_avg)
        idleCpuPct   = & $median ($idle.cpuPctOfOneCore)
        scrollWsMB   = & $median ($scroll.wsMB_avg)
        scrollCpuPct = & $median ($scroll.cpuPctOfOneCore)
    }
}
