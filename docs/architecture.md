# Architecture

Zapive is one Rust binary (edition 2024) with about 40k lines of Rust and
Slint. The UI is compiled from `.slint` files at build time (`build.rs`),
with the gettext catalogs bundled.

## Threads and data flow

```
┌───────────────────────── UI thread ─────────────────────────┐
│  Slint event loop ── AppWindow (ui/components/*.slint)      │
│        ▲  properties / callbacks                            │
│        │                                                    │
│     bridge::Bridge  ── store (in memory) ── vault (SQLite)  │
│        │  Cmd (mpsc)                  ▲ bridge::ui_apply    │
└────────┼──────────────────────────────┼─────────────────────┘
         ▼                              │
┌──── tokio runtime (4 workers) ─────────┴────────────────────┐
│  wa::WaService ── whatsapp-rust client (WebSocket, Signal)  │
│  media downloads/decoding · link previews · updates · GIFs  │
│  mcp server (axum) · VoIP relay                             │
└─────────────────────────────────────────────────────────────┘
   dedicated threads: translation engine (CTranslate2), pdfium,
   audio (cpal streams), video player, camera encoder
```

- **`bridge`** is the only code that touches the Slint window. User
  actions become `Cmd` messages to the WhatsApp side. Events come back
  through `bridge::ui_apply`, which runs a closure on the UI thread.
- **`wa`** owns the whatsapp-rust client on tokio. `wa_map` turns protocol
  messages into `StoredMessage`s, for both live traffic and history sync.
- **`store`** keeps chats and messages in memory and persists them to the
  **`vault`**. Chats are loaded on demand, and at most 8 stay "warm".

## Module map (`src/`)

| Module | Responsibility |
| --- | --- |
| `main.rs` | Startup: logging, self-tests, single instance, vault/PIN, theme and backdrop, tray (Windows), tokio runtime, event loop |
| `bridge/` | UI-thread logic, one file per area (below) |
| `wa.rs`, `wa_map.rs` | WhatsApp client and commands; message normalisation |
| `store.rs` | In-memory chats/messages, canonical JIDs (LID aliases), hydration |
| `vault.rs` | Envelope encryption: data key, PIN (scrypt), AES-GCM records and files, accounts |
| `search.rs` | FTS5 index over keyed, prefix-preserving terms with encrypted snippets |
| `outbox.rs` | Persistent queue of messages and attachments being prepared or sent |
| `media.rs` | Encrypted media cache, downloads, image decoding, thumbnails |
| `audio.rs` | Ogg/Opus (opus-pure), Symphonia decoding, WSOLA time-stretch, waveforms, playback |
| `video.rs` | MP4 demux/mux, H.264 decode/encode (openh264), GIF |
| `call.rs` | Call audio: 16 kHz mono frames, cpal mic/speaker, ring tones |
| `camera.rs` | Camera capture (nokhwa) and H.264 Annex-B encoding for video calls |
| `sound.rs` | Synthesised incoming-message chime |
| `pdf.rs` | pdfium binding and one-time engine download |
| `translation.rs`, `translation/` | Engine worker; `engine` (CTranslate2 + SentencePiece), `models` (catalog, routes, install), `downloads` (allow-listed fetch), `detect` (language detection) |
| `mcp.rs` | MCP server: tools, bearer token, origin and host checks, streamable HTTP |
| `link_preview.rs` | Fetching previews with public-address-only resolution |
| `update.rs` | GitHub release check, SHA-256 verification, binary swap, release notes |
| `markup.rs` | WhatsApp markup to Slint StyledText |
| `i18n.rs` | Shared PO catalogs for Rust strings (semantic ids, plurals) |
| `emoji.rs`, `fonts.rs` | Emoji palette; bundled colour-emoji font fallback |
| `paths.rs` | Data and cache directories per OS, per-account file names |
| `single.rs` | Single-instance lock and "raise the running window" |
| `qr.rs` | QR code rendering for pairing |
| `logging.rs` | File logger (`zapive.log`, `ZAPIVE_LOG` level), panic hook |
| `platform/` | Everything OS-specific: `windows.rs` (DPAPI, toasts, title bar, autostart), `macos.rs` (Keychain, notifications, LaunchAgent), `linux.rs` (Secret Service, D-Bus notifications, autostart) |
| `demo/` | Demo mode (feature `demo`): invented data and art, screenshots |

### `bridge/`

`accounts` · `audio` (player, mini player, recording) · `calls` ·
`chat_info` (info panel, group admin, mute/favorite/lists/disappearing) ·
`chats` (list, filters, search, labels) · `composer` (drafts, typing) ·
`conversation` (rows, scrolling, warm set) · `documents` (PDF) · `helpers`
· `history` (backfill, sync dialog) · `incoming` (events from WhatsApp) ·
`mcp` (tool calls) · `media` · `message_actions` (react, star, pin,
receipts…) · `notifications` · `outbox` · `overlays` (lightbox, video) ·
`selection` (multi-select, confirm, export) · `session` (boot, pairing,
logout) · `settings` · `status` (stories viewer) · `stickers` (sticker and
GIF pickers) · `translation` · `updates` · `wiring` (connects every Slint
callback).

## UI (`ui/`)

`app.slint` exports `AppWindow` from `components/app-window.slint`. The
window is made of components such as `home-screen`, `settings-pane`,
`info-panel`, `message-parts`, `special-message`, `media-viewers`,
`status-viewer`, `call-window`, `whats-new`, `update-banner`, `splash`, the
dialogs, and `glass.slint` (the Liquid Glass surface). `theme.slint` holds
the colour tokens for dark/light and glass/opaque. Icons are Tabler
Icons.

## Crates

| Area | Crates |
| --- | --- |
| Protocol, calls | `whatsapp-rust` (git, pinned rev; `signal`, `sqlite-storage`, tokio transport, VoIP with pure-Rust MLOW codec and native relay) |
| UI | `slint` (git, pinned rev; winit backend, Skia software renderer, accessibility), `tray-icon` (Windows), `rfd` (file dialogs), `arboard` (clipboard) |
| Storage, crypto | `rusqlite` (bundled SQLite + FTS5), `aes-gcm`, `scrypt`, `hmac`, `sha2`, `rand`, `zeroize`, `base64` |
| Audio | `opus-pure`, `ogg`, `symphonia`, `cpal` |
| Video, images | `mp4`, `openh264` (built from source), `nokhwa`, `image`, `webp`, `kamadak-exif` |
| Translation | `ct2rs` (Windows: official DLL via `system`; elsewhere built from source with `ruy`), `sentencepiece`, `lingua`, `unicode-script`, `zip`, `flate2` |
| PDF | `pdfium-render` (pdfium loaded at run time) |
| Network | `tokio`, `ureq`, `url`, `axum`, `rust-mcp-sdk` |
| OS | `windows` (Win32/WinRT), `objc2` (macOS), `notify-rust` (Linux/macOS), `libc` |
| i18n | `rspolib` (PO parsing), `sys-locale` |

Pinned versions are in `Cargo.toml` and `Cargo.lock`. Build with
`--locked`.

## Design rules

- **Nothing written in plaintext** that can be avoided (see
  [privacy-and-security.md](privacy-and-security.md)).
- **No FFmpeg, no browser engine:** every codec Zapive needs is a Rust
  crate or a small C library that cargo compiles itself.
- **All OS-specific code lives in `platform/`.**
- **All user-visible text goes through i18n** with semantic ids.
