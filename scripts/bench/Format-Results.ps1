<#
.SYNOPSIS  Turns a results .jsonl file into Markdown tables.
#>
param([Parameter(Mandatory)][string]$Path)
$rows = Get-Content $Path -Encoding utf8 | Where-Object { $_ } | ForEach-Object { $_ | ConvertFrom-Json }
$start = $rows | Where-Object scenario -eq 'startup'
if ($start) {
    $ms = $start.firstWindowMs | Sort-Object
    $ws = $start.after3s.wsMB | Sort-Object
    $pv = $start.after3s.privMB | Sort-Object
    $median = { param($a) $a[[int][math]::Floor(($a.Count - 1) / 2)] }
    "| Cold start (n=$($start.Count)) | min | median | max |"
    "| --- | ---: | ---: | ---: |"
    "| Time to first window (ms) | $($ms[0]) | $(& $median $ms) | $($ms[-1]) |"
    "| Working set at +3 s (MB) | $($ws[0]) | $(& $median $ws) | $($ws[-1]) |"
    "| Private bytes at +3 s (MB) | $($pv[0]) | $(& $median $pv) | $($pv[-1]) |"
    ""
}
"| Scenario | CPU % of 1 core | CPU % of machine | WS avg (MB) | WS max (MB) | Private avg (MB) | Threads | Handles |"
"| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |"
foreach ($r in ($rows | Where-Object scenario -ne 'startup')) {
    "| $($r.label) | $($r.cpuPctOfOneCore) | $($r.cpuPctOfMachine) | $($r.wsMB_avg) | $($r.wsMB_max) | $($r.privMB_avg) | $($r.threads) | $($r.handles) |"
}
