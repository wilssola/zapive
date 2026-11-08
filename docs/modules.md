# Module map

Use this map to locate an existing implementation before adding a module. A path denotes ownership of behavior, not an assertion that it already exposes a small interface. Runtime flow and seams are in [architecture.md](architecture.md).

| Module | Current responsibility |
| --- | --- |
| `src/main.rs`, `src/paths.rs` | Startup, runtime and event loop; OS data/cache paths and per-account names. |
| `src/bridge/` | UI-thread state and Slint window wiring. `wiring.rs` registers callbacks; `session.rs` handles account boot/pairing; `accounts.rs` owns the switcher, per-account state and headless background accounts; `incoming.rs` processes WhatsApp events; `conversation.rs` owns rows and warm conversations; `settings.rs` handles settings. Other files are organized by action area (media, calls, translation, notifications, etc.). |
| `src/wa/mod.rs` | WhatsApp client lifecycle, `Cmd` and `WaService` sender. |
| `src/wa/executor.rs`, `src/wa/pump.rs` | Command dispatch and protocol event handling. |
| `src/wa/session.rs`, `src/wa/messaging.rs`, `src/wa/media.rs`, `src/wa/attachments.rs`, `src/wa/chats.rs`, `src/wa/groups.rs`, `src/wa/calls.rs`, `src/wa/polls.rs`, `src/wa/profile.rs`, `src/wa/util.rs` | Protocol-side behavior split by area; these are currently private submodules of `wa`. |
| `src/wa_map.rs` | Protocol-message conversion for history and live traffic. |
| `src/store.rs`, `src/vault.rs`, `src/search.rs`, `src/outbox.rs` | In-memory chat state and hydration; encrypted SQLite vault; search index; persistent outgoing queue. |
| `src/media.rs`, `src/audio.rs`, `src/video.rs`, `src/call.rs`, `src/camera.rs`, `src/pdf.rs` | Media caching/processing, playback, VoIP and PDF work. |
| `src/translation.rs`, `src/translation/` | Translation worker, model management, download and language detection. |
| `src/mcp.rs`, `src/update.rs`, `src/link_preview.rs` | Local MCP server, verified updates and restricted network previews. |
| `src/i18n.rs`, `src/markup.rs`, `src/emoji.rs`, `src/fonts.rs`, `src/sound.rs` | Localization, text markup, emoji/font support and message chime. |
| `src/platform/`, `src/single.rs`, `src/logging.rs`, `src/qr.rs`, `src/demo/` | OS adapters, single-instance handling, logging, pairing QR and invented demo data. |
| `ui/components/`, `ui/icons/` | Slint views and bundled SVG assets; `ui/app.slint` exports the window. |

When tracing a change, follow the vertical slice: Slint callback → `bridge` → `wa::Cmd` or store/vault → return event → bridge model/property. Prefer a small interface that hides related behavior over distributing one invariant across more files. Do not extract an adapter until there is a real alternate implementation or another validated reason to change behavior at that seam. Existing paths above are implementation facts; proposed future interfaces belong in a design discussion, not in this map.
