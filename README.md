# Zapive

A native WhatsApp desktop client written entirely in Rust: **one ~37 MB
executable**, no browser engine, no Electron.
It connects to WhatsApp as a linked device (like WhatsApp Web)
and aims to look and feel like the official desktop app while using a
fraction of the resources — about **140–180 MB of RAM** connected with
~900 chats loaded.

> Zapive is an independent project, not affiliated with or endorsed by
> WhatsApp/Meta. Use a secondary line if account safety is a concern.

## Features

**Messaging**
- Pairing by QR code or phone-number code (Linked Devices).
- Chat list with pins, archive, unread counters, mention badges and
  communities; previews render instantly from metadata.
- Text with styled markup, replies with quote preview, reactions,
  forwarding, starring, deleting; edits and revokes are reflected.
- Groups with sender names/colors, channels (newsletters) with metadata,
  status/stories viewer, and a call log with reject support.

**Media, fully in-process**
- Photos, videos, GIFs, stickers, documents and voice notes — sent and
  received. Outgoing images get thumbnails; stickers are converted to
  512px webp; picked GIFs become gif-playback MP4s.
- Voice notes play at 1x/1.5x/2x/3x with pitch preserved (WSOLA
  time-stretch) and show real waveforms; recording encodes straight to
  ogg/opus like the phone apps do.
- Click-to-play video overlay with soundtrack, GIF zoom, sticker/GIF
  picker with Openverse search, in-panel media gallery.

**Desktop integration**
- Native toasts with sender avatar, click-to-open and quick actions.
- System tray with minimize-to-tray, single-instance lock, light/dark
  theme following the system, English/Portuguese UI.

**Self-updating**
- Every push to `master` is versioned automatically from the
  conventional-commit history, built for Windows, Linux and macOS by CI
  and published to GitHub Releases.
- The app checks for releases on launch and every 6 hours; a banner
  offers the download and swaps the executable in place — restart and
  you are on the new version. (While the repository is private, set
  `ZAPIVE_GH_TOKEN` so the updater can reach the release assets.)

**Privacy & security**
- Chats, settings, media and avatars are envelope-encrypted on disk: a
  random data key sealed by Windows DPAPI (machine + user bound) and
  optionally by a PIN (scrypt-derived key). Only whatsapp-rust's own
  protocol store (`wa.db`) stays plain SQLite.
- Optional PIN lock screen on launch.

## Performance

| Metric | Value |
|---|---|
| Executable | ~37 MB, single file, static CRT |
| RAM at boot | ~140 MB |
| RAM connected (~900 chats) | ~180 MB |
| Cold boot | instant list render; zero message blobs parsed |

Messages load lazily: opening a chat (or letting the pointer rest on it
for ~120 ms — the click is probably coming) hydrates its tail from the
encrypted vault in under a millisecond. At most 8 chats stay warm in
RAM; the rest live on disk.

## Architecture

| Layer | Crate(s) |
|---|---|
| WhatsApp protocol | [whatsapp-rust](https://github.com/jlucaso1/whatsapp-rust) (Signal encryption, WebSocket, media CDN) |
| UI | [Slint](https://github.com/slint-ui/slint) — compiled declarative UI, FemtoVG/OpenGL renderer with a software fallback for RDP |
| Voice notes | [opus-pure](https://github.com/stephenberry/opus-pure) (Rust port of libopus) + [ogg](https://github.com/RustAudio/ogg), hand-rolled WSOLA for speeds |
| Other audio | [symphonia](https://github.com/pdeljanov/Symphonia) (AAC/MP3/WAV/FLAC), [cpal](https://github.com/RustAudio/cpal) for playback/recording |
| Video | [mp4](https://github.com/alfg/mp4-rust) (demux/mux), [openh264](https://github.com/ralfbiedert/openh264-rs) (H.264), [image](https://github.com/image-rs/image) (GIF) |
| Storage | SQLite via [rusqlite](https://github.com/rusqlite/rusqlite), AES-256-GCM envelope encryption, DPAPI/scrypt key wrapping |
| Async | [tokio](https://github.com/tokio-rs/tokio) (2 worker threads) beside the Slint event loop, bridged by command/apply channels |

Format coverage is deliberately WhatsApp-shaped: H.264/MP4 video,
ogg/opus voice notes, AAC soundtracks, GIF. Exotic containers (HEVC,
MKV) appear as documents without preview.

## Building

Prerequisites: [Rust](https://rustup.rs) and, on Windows, the MSVC Build
Tools. No vcpkg, no LLVM, no CMake, no system FFmpeg — the two bundled C
sources (openh264, SQLite) are compiled by cargo itself.

```powershell
cargo build --release
# -> target\release\zapive.exe
```

Linux needs `libasound2-dev libgtk-3-dev libxdo-dev`; macOS needs
nothing extra. Both are smoke-built by CI (`.github/workflows/build.yml`)
but not yet regularly tested. `cargo run -- --audio-selftest` exercises
the opus encode/decode, time-stretch and waveform paths headlessly.

## Data locations

| What | Windows | macOS | Linux |
|---|---|---|---|
| Encrypted vault (chats, settings) | `%APPDATA%\Zapive\vault.db` | `~/Library/Application Support/Zapive/vault.db` | `$XDG_DATA_HOME/zapive/vault.db` |
| WhatsApp session (protocol state) | `%APPDATA%\Zapive\wa.db` | `~/Library/Application Support/Zapive/wa.db` | `$XDG_DATA_HOME/zapive/wa.db` |
| Encrypted media + avatar cache | `%LOCALAPPDATA%\Zapive\Cache\media` | `~/Library/Caches/Zapive/media` | `$XDG_CACHE_HOME/zapive/media` |

`$XDG_DATA_HOME` defaults to `~/.local/share` and `$XDG_CACHE_HOME` to
`~/.cache`. The vault and the media cache are envelope-encrypted; `wa.db`
is whatsapp-rust's own store, plain SQLite guarded by OS file
permissions. Deleting the cache never loses the account; deleting
`wa.db` requires pairing again.

## Credits

- Protocol: [whatsapp-rust](https://github.com/jlucaso1/whatsapp-rust)
- UI toolkit: [Slint](https://slint.dev)
- Icons: [Tabler Icons](https://github.com/tabler/tabler-icons) (MIT)
- Opus port: [opus-pure](https://docs.rs/opus-pure)
