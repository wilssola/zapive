# Benchmarks (RAM and CPU)

Reproducible RAM/CPU measurements of the Zapive **demo build** on Windows.
The demo build (`--features demo`, run with `--demo`) shows the real UI on
invented data. It never opens the real vault, never connects to WhatsApp and
never claims the single-instance lock, so it is safe to run next to a logged-in
installation. It also means these numbers cover the UI, renderer and startup
only: the WhatsApp client, sqlite stores, search index, media cache and tokio
traffic of a real session are **not** included.

## Method

- Harness: `scripts/bench/Measure-Zapive.ps1` (one run), `Run-Benchmarks.ps1`
  (all scenarios), `Format-Results.ps1` (Markdown tables).
- The harness refuses to start the exe without `--demo` and only ever stops
  the process it started.
- Metrics, from `Get-Process`: `WorkingSet64`, `PrivateMemorySize64`,
  `TotalProcessorTime` (CPU time), thread count, handle count. GPU is not
  measured.
- CPU % = CPU-time delta / wall time over the window (1 s samples, 60 s). "Of 1
  core" can exceed 100 %; "of machine" divides by the logical processor count.
- Cold start = wall time from `Start-Process` until a visible top-level window
  wider than 300 px exists (a proxy for first frame; it is polled every 10 ms,
  so it is not the time of the first painted frame). Memory is read at that
  moment and 3 s later. "Cold" means a fresh process; the exe was in the OS
  file cache after the first run.
- Idle scenarios: 10 s settle after the window appears (splash fade and layout),
  then 2 s, then a 60 s window. Minimized uses `SW_MINIMIZE`, hidden uses
  `SW_HIDE`.
- Scroll: `WM_MOUSEWHEEL` posted to the window every 100 ms, direction flipping
  every 2 s. **Not verified** to actually scroll the list (see Limitations).

## Environment

| Item | Value |
| --- | --- |
| CPU | AMD Ryzen 7 9800X3D, 8 cores / 16 threads |
| RAM | 61.7 GB (DDR5-5600) |
| Windows | 10.0.26300 (26H2, UBR 9711) |
| Display | primary 3440x1440, DPI 96 (scale factor 100 %) |
| GPUs present | NVIDIA RTX 5070 Ti, AMD Radeon iGPU, virtual display adapters (renderer is Skia software, so GPU is unused) |
| Window | demo default 1440x900 logical |
| Build | `cargo build --release --features demo`; profile: `lto = "fat"`, `codegen-units = 1`, `opt-level = "s"`, `strip = true`, panic = unwind (default) |
| Rust | 1.99.1 |
| Release exe (demo build) | 115,525,120 bytes (110.2 MiB) |
| Installed release exe | 113,171,152 bytes (107.9 MiB), file size read only; it was **not** started |
| `ctranslate2.dll` (separate) | 61,254,144 bytes |

## Results

Run `20261103-183316` (BEFORE; 5 cold starts, then one 60 s run per scenario).

| Cold start (n=5) | min | median | max |
| --- | ---: | ---: | ---: |
| Time to first window (ms) | 197 | 218 | 263 |
| Working set at +3 s (MB) | 87.6 | 87.6 | 88.4 |
| Private bytes at +3 s (MB) | 49.7 | 50.5 | 50.6 |

| Scenario | CPU % of 1 core | CPU % of machine | WS avg (MB) | WS max (MB) | Private avg (MB) | Threads | Handles |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| idle visible, glass dark | 1.8 | 0.113 | 88.2 | 88.7 | 50.7 | 6 | 314 |
| idle minimized, glass dark | 0 | 0 | 87.7 | 87.7 | 49.9 | 6 | 314 |
| idle hidden, glass dark | 0 | 0 | 87.6 | 87.6 | 49.7 | 6 | 314 |
| wheel scroll, glass dark (unverified) | 2.24 | 0.14 | 88.2 | 88.2 | 50.8 | 6 | 314 |
| idle visible, opaque dark | 0 | 0 | 77.6 | 77.6 | 39.8 | 6 | 314 |
| idle visible, glass light | 0 | 0 | 87.8 | 87.8 | 49.9 | 6 | 314 |
| idle visible, opaque light | 0 | 0 | 77.6 | 77.6 | 39.8 | 6 | 314 |

### Before vs after the performance changes

BEFORE is the run above (`20261103-183316`, commit tree before the other
perf work). AFTER is `20261103-200928`: same harness, same machine, rebuilt
from the working tree after the other perf changes landed. Release profile is
unchanged between the two (`opt-level = "s"`, see below), so the difference
comes from the code.

| Metric | Before | After | Change |
| --- | ---: | ---: | ---: |
| Release exe, demo build (MiB) | 110.2 | 104.7 | -5.5 |
| Time to first window, median (ms) | 218 | 210 | -8 (within noise) |
| Working set at +3 s (MB) | 87.6 | 76.6 | -11.0 (-12.6 %) |
| Private bytes at +3 s (MB) | 50.5 | 44.5 | -6.0 (-11.9 %) |
| Idle visible, glass: CPU % of 1 core | 1.8 | 0 | single-run noise before |
| Idle visible, glass: WS avg (MB) | 88.2 | 76.5 | -11.7 |
| Idle minimized: WS avg (MB) | 87.7 | 76.6 | -11.1 |
| Idle hidden: WS avg (MB) | 87.6 | 76.7 | -10.9 |
| Wheel scroll (unverified): CPU % of 1 core | 2.24 | 0.05 | see Limitations |
| Idle visible, opaque: WS avg (MB) | 77.6 | 66.7 | -10.9 |
| Glass minus opaque (MB) | +10.6 | +9.8 | about the same |

### Release profile experiment

Three demo builds of the same source tree (copied to separate directories so
the working tree was not touched), 3 runs each, median, 25 s windows:

| Build | Exe (MiB) | First window (ms) | WS +3 s (MB) | Private +3 s (MB) | Idle WS (MB) | Idle CPU % | Scroll CPU % |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| `opt-level = "s"` (kept) | 104.7 | 230 | 77.2 | 45.3 | 76.6 | 0 | 0 |
| `opt-level = 3` | 129.9 | 180 | 77.9 | 44.6 | 77.8 | 0 | 0 |
| `opt-level = "s"` + mimalloc | 104.4 | 255 | 98.2 | 87.7 | 78.7 | 0 | 0 |

Decision: keep `opt-level = "s"`, `lto = "fat"`, `codegen-units = 1`,
`strip = true`.

- `opt-level = 3` makes the exe 24 % larger (+25 MiB) for about 50 ms faster
  start and no RAM or CPU gain in this UI-only workload. The size also hits
  every release download and every self-update.
- mimalloc did not lower RAM: working set at start was 21 MB higher and
  private bytes nearly doubled (it reserves arenas up front); steady idle was
  2 MB higher. It was not adopted, so no dependency was added.
- `panic = "abort"` is not possible: `catch_unwind` is used in `src/single.rs`
  (single-instance guard) and `src/translation.rs` (callbacks and the
  translation worker), which rely on unwinding.
- `lto = "thin"` was not measured; fat LTO with one codegen unit is the
  existing, working configuration and the build already takes 11 to 15 minutes.

Caveats: CPU is 0 in every build because the demo is idle and the scroll
messages are not verified to move the list, so these runs cannot rank the
profiles on CPU. Only size, start time and startup RAM differ meaningfully.
Start-time differences of tens of ms are close to the harness's 10 ms polling
and run-to-run spread (197 to 297 ms in the AFTER run).

Reading the table:

- Idle is essentially zero CPU. The one non-zero idle value (1.8 % of a core,
  about 1.1 s of CPU in 60 s) is a single run and is not reproduced in the other
  idle runs; treat it as noise until repeated.
- Glass costs about +10.5 MB working set and +10.9 MB private bytes over opaque.
  Part of that is the demo's synthetic backdrop image (`demo/art.rs`), so the
  real-desktop cost can be lower.
- Dark vs light makes no measurable difference.
- Thread count is 11 right after start and 6 once settled.
- Minimized and hidden do not reduce memory (no trimming) but CPU stays zero.

## Limitations

- Single 60 s run per scenario except cold start. Differences under about 1 %
  CPU or 1 MB are within noise.
- No WhatsApp session: network, tokio tasks, sqlite, search indexing, media
  decode and translation models are not exercised.
- The scroll scenario posts synthetic wheel messages; whether the list moved
  was not verified visually, so its CPU number must not be read as a scroll
  cost. A real scroll test needs in-app scripting (a `--demo-script` mode) or UI
  automation.
- GPU use is not measured (the app renders with Skia software).
- Time to first window is a polling proxy, not time to first painted frame.

## How to reproduce

```powershell
cargo build --release --features demo
.\scripts\bench\Run-Benchmarks.ps1 -Seconds 60 -StartupRuns 5
.\scripts\bench\Format-Results.ps1 -Path .\scripts\bench\results\<timestamp>.jsonl
```

Single scenario:

```powershell
.\scripts\bench\Measure-Zapive.ps1 -Scenario idle-visible -Seconds 60 `
    -DemoArgs '--demo-style','opaque','--demo-theme','light'
```

Scenarios: `startup`, `idle-visible`, `idle-minimized`, `idle-hidden`, `scroll`.
Close other heavy apps, keep the machine plugged in and do not touch the demo
window while a run is in progress. Results land in `scripts/bench/results/`
(git-ignored).

A/B comparison of several builds (for example a different `opt-level`), each
exe needing `ctranslate2.dll` and `libiomp5md.dll` beside it:

```powershell
.\scripts\bench\Compare-Builds.ps1 -Exe C:\bin\size.exe,C:\bin\o3.exe -Runs 3 -Seconds 25
```
