<#
.SYNOPSIS
  Runs every benchmark scenario against the Zapive DEMO build and writes
  results to scripts/bench/results/<timestamp>.jsonl plus a Markdown table.

.EXAMPLE
  cargo build --release --features demo
  .\scripts\bench\Run-Benchmarks.ps1 -StartupRuns 5 -Seconds 60
#>
param(
    [string]$Exe = '',
    [int]$Seconds = 60,
    [int]$StartupRuns = 5,
    [int]$SettleSeconds = 10
)
$ErrorActionPreference = 'Stop'
$here = Split-Path -Parent $MyInvocation.MyCommand.Path
if (-not $Exe) { $Exe = Join-Path $here '../../target/release/zapive.exe' }
$measure = Join-Path $here 'Measure-Zapive.ps1'
$outDir = Join-Path $here 'results'
New-Item -ItemType Directory -Force $outDir | Out-Null
$stamp = Get-Date -Format 'yyyyMMdd-HHmmss'
$jsonl = Join-Path $outDir "$stamp.jsonl"

$runs = @()
1..$StartupRuns | ForEach-Object {
    $runs += @{ Scenario = 'startup'; Label = "startup-glass-dark #$_"; Args = @('--demo-style', 'glass', '--demo-theme', 'dark') }
}
$runs += @{ Scenario = 'idle-visible';   Label = 'idle visible, glass dark';   Args = @('--demo-style', 'glass', '--demo-theme', 'dark') }
$runs += @{ Scenario = 'idle-minimized'; Label = 'idle minimized, glass dark'; Args = @('--demo-style', 'glass', '--demo-theme', 'dark') }
$runs += @{ Scenario = 'idle-hidden';    Label = 'idle hidden, glass dark';    Args = @('--demo-style', 'glass', '--demo-theme', 'dark') }
$runs += @{ Scenario = 'scroll';         Label = 'wheel scroll, glass dark';   Args = @('--demo-style', 'glass', '--demo-theme', 'dark') }
$runs += @{ Scenario = 'idle-visible';   Label = 'idle visible, opaque dark';  Args = @('--demo-style', 'opaque', '--demo-theme', 'dark') }
$runs += @{ Scenario = 'idle-visible';   Label = 'idle visible, glass light';  Args = @('--demo-style', 'glass', '--demo-theme', 'light') }
$runs += @{ Scenario = 'idle-visible';   Label = 'idle visible, opaque light'; Args = @('--demo-style', 'opaque', '--demo-theme', 'light') }

foreach ($r in $runs) {
    Write-Host "== $($r.Label)"
    $line = & $measure -Exe $Exe -Scenario $r.Scenario -DemoArgs $r.Args -Seconds $Seconds `
        -SettleSeconds $SettleSeconds -Label $r.Label
    Add-Content -Path $jsonl -Value $line -Encoding utf8
    Start-Sleep -Seconds 3
}
Write-Host "results: $jsonl"
$jsonl
