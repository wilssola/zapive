# Roadmap

Goal: reach practical parity with WhatsApp Desktop while staying a single
lightweight native process.

## Next up

**Replies (quoted messages)**
Render `contextInfo.quotedMessage` above the bubble and send with
`quoted:`, including jumping to the quoted message.

**Reacting from the app**
Right-click a bubble to pick an emoji — receiving reactions already works,
sending does not.

## Planned

- **Groups in common** in the contact info panel
- **Following and leaving channels** (listing and reading already work)
- **Message search inside a conversation**, plus global search across
  stored messages
- **Starred messages** and per-chat mute settings
- **Mark as read / unread**, and sending read receipts explicitly
- **Group management** — participant list, add/remove, admin actions
- **Drag and drop** files onto the conversation
- **In-app audio player** with a waveform instead of the system player
- **Video playback in-window** (currently opens externally)
- **Packaging** — signed installer with a proper app icon, autostart
  option, and a portable build

## Under consideration

- **Windows Hello / TPM** key protection via a napi module, removing the
  PIN prompt (see [security.md](security.md#threat-model))
- **Multi-account** support with separate vaults
- **Linux and macOS builds** — the stack is portable; the Windows-specific
  pieces are DPAPI, the tray helper, file dialogs and ffmpeg's DirectShow
  input
- **Message export** (per chat, encrypted or plain)

## Known limitations

| Limitation | Reason |
|---|---|
| Flag emoji render as letters (🇧🇷 → BR) | Slint font fallback lacks regional indicator sequences |
| Video and audio open in the system player | No embedded media player yet |
| History only goes back as far as the phone serves | WhatsApp on-demand sync returns ~50 messages per request |
| Sending a GIF forwards an existing one | Uploading a local mp4 as gif-playback is wired but not exposed in the UI |
| Calls are not supported | Baileys does not implement WhatsApp calling |
| Windows only | DPAPI, NotifyIcon, WinForms dialogs and DirectShow |

## Non-goals

- Bots, automation or bulk messaging — this is a personal client
- Web or mobile versions
- Anything that requires bundling a browser engine
