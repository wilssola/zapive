# Zapive

A native WhatsApp desktop client: one Rust binary, no browser engine, no
FFmpeg. Built on [whatsapp-rust](https://github.com/jlucaso1/whatsapp-rust)
for the protocol and [Slint](https://slint.dev) for the UI, with
`opus-pure` (a Rust port of libopus) for voice notes and `symphonia`,
`mp4`, `ogg` and `image` for the rest. Voice notes with 1x–3x speeds,
video playback, GIF→MP4 conversion and recording all stay in-process.

## Building

Rust (from [rustup](https://rustup.rs)) and, on Windows, the MSVC build
tools. No vcpkg, no LLVM, no CMake, no system FFmpeg:

```powershell
cargo build --release
# -> target\release\zapive.exe (single file)
```

## Data locations

| What | Where (Windows) |
|---|---|
| Encrypted vault (chats, settings) | `%APPDATA%\Zapive\vault.db` |
| WhatsApp session (protocol state) | `%APPDATA%\Zapive\wa.db` |
| Encrypted media cache | `%LOCALAPPDATA%\Zapive\Cache\media` |

Every media file and chat row is sealed with a random data key wrapped by
Windows DPAPI (and optionally a PIN via scrypt). Deleting the cache never
loses the account; deleting `wa.db` requires pairing again.

## Notes

- Pairing: QR code or phone-number code, Linked Devices on the phone.
- The `--audio-selftest` flag exercises the in-process opus encode/decode
  and waveform path without opening the window.
- Linux/macOS: the code paths exist (XDG/Library dirs, keychain/secret
  service, notify-send/osascript) but are untested; CI runs smoke builds
  for both.
- Format coverage is WhatsApp's wire reality: H.264/MP4 video, ogg/opus
  voice notes, AAC soundtracks, GIF. Exotic containers (HEVC, MKV) show
  as plain documents without preview.
