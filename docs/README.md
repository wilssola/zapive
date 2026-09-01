# Zapive

A native desktop WhatsApp client for Windows, built to be **lighter and
faster than the official app** — no Chromium, no web view, no Electron.

The official WhatsApp Desktop ships a browser engine to render a web app.
Zapive talks to WhatsApp over the same WebSocket protocol the phone uses
(via [Baileys](https://baileys.wiki/)) and draws its interface with
[Slint](https://slint.dev/), a compiled, GPU-accelerated toolkit. The
result is a single Node process with a native window instead of a browser
stack.

| | Official WhatsApp Desktop | Zapive |
|---|---|---|
| Rendering | Chromium (Electron/WebView2) | Slint (Skia, compiled UI) |
| Processes | Browser + renderer + GPU + helpers | One Node process (+ a tray helper) |
| Local storage | IndexedDB inside the browser profile | Single SQLite vault, always encrypted |
| Extra protection | OS account only | Optional PIN (scrypt KEK) + DPAPI binding |

## Documentation map

| Document | What it covers |
|---|---|
| [architecture.md](architecture.md) | Module layout, data flow, threading model |
| [features.md](features.md) | Everything implemented, with behavior notes |
| [security.md](security.md) | Vault design, key wrapping, threat model |
| [development.md](development.md) | Setup, scripts, conventions, troubleshooting |
| [roadmap.md](roadmap.md) | What's planned and known limitations |

## Quick start

```bash
bun install          # dependencies (bun is the toolchain)
bun run start        # runs on Node 24 — never `bun run` the app itself
```

First launch shows a QR code (or pairing code). After linking, the session
lives in `zapive.db` and the app reconnects automatically.

## Stack

- **[Baileys](https://baileys.wiki/) 7.x** — WhatsApp Web protocol over WebSocket
- **[Slint](https://slint.dev/) 1.17** — declarative UI compiled to native code
- **Node.js 24** — runtime; TypeScript runs through native type stripping
- **Bun** — package manager and script runner (not the runtime; see
  [development.md](development.md#why-node-and-not-bun))
- **node:sqlite** — built-in database, no native dependency
- **sharp** — image decoding, thumbnails, sticker conversion
- **ffmpeg** — microphone capture for voice notes (optional)
