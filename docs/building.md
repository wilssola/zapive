# Building from source

## Requirements

- **Rust stable.** `rust-toolchain.toml` selects the stable channel with
  `rustfmt` and `clippy`; install [rustup](https://rustup.rs) and it does
  the rest.
- Git access to GitHub: `slint` and `whatsapp-rust` are git dependencies
  pinned to exact revisions.

| Platform | Extra |
| --- | --- |
| **Windows 10/11 x64** | Visual Studio **MSVC Build Tools** (C++ workload). Run `./scripts/fetch-ctranslate2.ps1` once: it downloads the pinned official CTranslate2 4.8.4 wheel (SHA-256 verified) and puts `ctranslate2.dll`, `libiomp5md.dll` and the import libraries into `vendor/ctranslate2-4.8.4/`. |
| **Linux** (Debian/Ubuntu names) | `sudo apt-get install libasound2-dev libgtk-3-dev libxdo-dev cmake build-essential`. At run time, `secret-tool` (libsecret) is needed for the vault key. |
| **macOS** (Apple silicon) | `cmake` (`brew install cmake`); CTranslate2 is compiled from source. |

Why Windows is different: when `ct2rs` builds CTranslate2 itself, it
forces the static C runtime (`/MT`), but Skia needs the dynamic one
(`/MD`). So Windows links the official DLL instead, delay-loaded so that a
missing DLL only breaks translation. `.cargo/config.toml` points
`LIBRARY_PATH` at the vendor folder and disables `crt-static`, and
`build.rs` copies the DLLs next to the executable for `cargo run` and
`cargo test`.

OpenH264, SQLite and Opus are compiled by cargo. You don't need vcpkg,
LLVM or a system FFmpeg.

## Build and run

```sh
cargo build --locked              # debug
cargo run --locked                # run the app
cargo build --release --locked    # optimised: fat LTO, opt-level "s", stripped
```

The output is `target/release/zapive` (`zapive.exe` on Windows, which
needs the two CTranslate2 DLLs next to it).

## Cargo features

| Feature | Default | What it does |
| --- | --- | --- |
| `demo` | off | Compiles [demo mode](demo-mode.md) (`--demo`, `--demo-shots-all` …). Release builds never include it. Without the feature, `--demo` prints a hint and exits. |

## Checks before a pull request

```sh
cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
python3 -m unittest discover -s .github/scripts -p 'test_*.py'   # release-notes generator
```

CI (`.github/workflows/build.yml`) runs on pull requests, on manual
dispatch and weekly. It builds and tests on Linux, macOS and Windows, runs
fmt and clippy on Linux (blocking), runs
`cargo audit`, and tests the release-notes generator.

## Self-tests

Headless probes that exercise subsystems without a WhatsApp connection:

| Flag | Exercises |
| --- | --- |
| `--audio-selftest` | Opus encode/decode, time-stretch, waveforms |
| `--player-selftest [clip.mp4]` | Video player: play, pause, seek, end, replay |
| `--video-selftest` | Video decoding path |
| `--overlay-selftest` | Mouse selection and the link cursor over a styled bubble |
| `--update-selftest` | Updater logic |
| `--accounts-selftest` | Account switcher walkthrough |

Example: `cargo run --locked -- --audio-selftest`.

## Environment variables

| Variable | Effect |
| --- | --- |
| `ZAPIVE_LOG` | Log level for `zapive.log`: `off`, `error`, `warn`, `info` (default), `debug`, `trace` |
| `ZAPIVE_GH_TOKEN` | GitHub token the updater uses to reach release assets while the repository is private |
| `ZAPIVE_TRACE` | Extra frame timing output in the player self-test |

## Packaging

Done by `.github/workflows/release.yml` on every push to `master`; see
[releases.md](releases.md) and [packaging/README.md](../packaging/README.md).

| Platform | Artifact | Tool |
| --- | --- | --- |
| Windows | `Zapive-Setup-<version>.exe` | Inno Setup (`packaging/windows/zapive.iss`), self-signed (`sign.ps1`) |
| Linux | `Zapive-<version>-x86_64.AppImage` | `packaging/linux/make-appimage.sh` (appimagetool) |
| macOS | `Zapive-<version>.dmg` with `Zapive.app` | `packaging/macos/make-bundle.sh`, self-signed codesign |
| all | `zapive-<target>.zip` (`windows-x86_64`, `linux-x86_64`, `macos-aarch64`) | Payload for the in-app updater; the names are load-bearing |
| Linux | Flatpak (`io.github.wilssola.Zapive`) | `packaging/flatpak/` and the manual `flatpak` workflow; prepared, not published |

## Disk space tip

Debug builds use `debug = "line-tables-only"` because full debuginfo grew
`target/` past 7 GB. If space is tight, `cargo clean` from time to time.
