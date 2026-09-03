# Zapive

A native WhatsApp desktop client: one Rust binary, no browser engine.
Built on [whatsapp-rust](https://github.com/jlucaso1/whatsapp-rust) for the
protocol and [Slint](https://slint.dev) for the UI, with FFmpeg statically
linked for all media work (voice notes with 1x–3x speeds, video, GIF
conversion, recording).

## Building (Windows)

Prerequisites, one time:

```powershell
# Rust (MSVC toolchain) — https://rustup.rs
# LLVM (libclang, needed by the FFmpeg bindings)
scoop install llvm
# Static FFmpeg with the codecs Zapive uses
vcpkg install "ffmpeg[avcodec,avformat,avfilter,avdevice,swresample,swscale,opus,openh264]:x64-windows-static" --recurse
```

`VCPKG_ROOT` must point at the vcpkg installation. Then:

```powershell
cargo build --release
# -> target\release\zapive.exe (single file, ~70 MB)
```

## Data locations

| What | Where (Windows) |
|---|---|
| Encrypted vault (chats, settings) | `%APPDATA%\Zapive\vault.db` |
| WhatsApp session (protocol state) | `%APPDATA%\Zapive\wa.db` |
| Encrypted media cache | `%LOCALAPPDATA%\Zapive\Cache\media-v2` |

Every media file and chat row is sealed with a random data key wrapped by
Windows DPAPI (and optionally a PIN via scrypt). Deleting the cache never
loses the account; deleting `wa.db` requires pairing again.

## Notes

- Pairing: QR code or phone-number code, Linked Devices on the phone.
- The `--audio-selftest` flag exercises the in-process opus encode/decode
  and waveform path without opening the window.
- Linux/macOS: the code paths exist (XDG/Library dirs, keychain/secret
  service, notify-send/osascript) but are untested; FFmpeg comes from the
  system package manager there.
