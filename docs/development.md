# Development

## Requirements

- **Node.js 24+** — required by `slint-ui`; also runs TypeScript natively
- **Bun** — package manager and script runner
- **ffmpeg** — audio/video playback, waveforms and microphone capture.
  Windows: `winget install Gyan.FFmpeg` (also auto-discovered under the
  winget packages folder); macOS: `brew install ffmpeg`; Linux: the
  distribution package
- **Linux only** — `zenity` (or `kdialog`) for file dialogs, `wl-clipboard`
  or `xclip` for the clipboard, `libsecret-tools` for the key store

## Scripts

```bash
bun install       # dependencies
bun run start     # launch the app (node src/main.ts)
bun run check     # tsc --noEmit
bun run i18n      # compile i18n/*.po to gettext .mo catalogs
bun run build     # package dist/Zapive (executable + resources)
```

## Packaging

`bun run build` produces a single executable for the host platform:

```
dist/Zapive/Zapive.exe      # ~120 MB, nothing beside it
```

Everything rides inside it. `scripts/build.mjs` compiles the catalogs,
bundles `src/main.ts` with esbuild into CommonJS, gathers the files that
must exist on disk at runtime — `ui/`, `i18n/` and the three packages
carrying native binaries (slint-ui, sharp, node-notifier) — and packs
them into one gzipped archive (`ZPAK1`: magic, JSON manifest, payload).
The bundle and the archive go into a Node SEA blob, which `postject`
injects into a copy of the Node binary.

Native addons cannot be loaded from memory, and Slint reads the markup
and the icons from disk, so on first run `src/resources.ts` unpacks the
archive into `<cache>/runtime/<content-hash>/` and points `nativeRequire`
and the UI loader at it. The hash names the directory, so a new build
unpacks beside the old one and then deletes it; later runs of the same
build skip the step entirely.

Only the host platform's prebuilt binaries are packed, and the archive
compresses 51 MB down to 22 MB.

The icon lives in `ui/zapive.png`; `node scripts/make-ico.mjs` derives
`ui/zapive.ico` from it (16–256 px). The window and taskbar use the PNG
(Slint's `Window.icon`), the tray and the executable use the ICO — the
build applies it with rcedit *before* injecting the SEA blob, because
rewriting the PE resource table afterwards would displace the asset —
and toasts without a contact photo fall back to the PNG.

Close a running packaged build before rebuilding: Windows keeps the
executable locked and the clean step stops with a note saying so.

Build on the platform you are targeting — the Node binary and the native
addons are platform-specific. On macOS the executable is re-signed ad hoc
so Gatekeeper allows it locally; a real distribution needs a Developer ID
signature and notarization.

## Why Node and not Bun

`slint-ui` and `sharp` are napi modules that need Node ≥24, and Baileys is
unreliable on Bun's WebSocket implementation (reconnect loops that can
trip WhatsApp's ban heuristics). Bun stays as the toolchain: fast installs
and script running. **Never `bun run src/main.ts`.**

## TypeScript without a build step

Node 24 strips types natively, so there is no bundler. The constraints:

- Only erasable syntax — no `enum`, no `namespace`, no constructor
  parameter properties
- Relative imports must include the `.ts` extension
- `tsconfig.json` is type-check only (`noEmit`, `erasableSyntaxOnly`)

## Slint notes

Hard-won details that are easy to trip over again:

- Dashed identifiers map to **underscores** in JS (`status-text` →
  `status_text`), and component instances are **not extensible** — every
  property/callback must exist in the `.slint` file
- Wrapped `Text` needs an explicit `width: min(self.preferred-width, max)`
  or its height is computed wrong (bubbles collapse or overlap)
- A `Rectangle`'s bound height is not its `preferred-height`; counting
  padding twice makes rows overlap
- Emoji followed by a variation selector (U+FE0F) render as tofu — see
  `cleanText()`; dingbats like ✕ ➤ are missing from the font, so the UI
  uses Tabler SVGs instead
- `runEventLoop({ quitOnLastWindowClosed: false })` enables tray behavior
- `capture-key-pressed` on a `FocusScope` sees Ctrl+V even while a
  `LineEdit` has focus (used for paste-to-send)
- Programmatic scrolling must be re-applied a few times: rows grow after
  the first layout pass when images and wrapped text resolve

## Baileys notes

- `syncFullHistory: true` breaks registration on 7.0.0-rc14 with an
  instant 428 loop — keep it `false` and pass
  `shouldSyncHistoryMessage: () => true`
- The pairing history sync can arrive empty; use `fetchMessageHistory`
  to walk backwards instead
- Pinned/archived chats need `resyncAppState`; when the phone withheld
  sync keys, request them with an `APP_STATE_SYNC_KEY_REQUEST` protocol
  message to your own jid (`category: "peer"`)
- Force-killing the app mid-handshake can invalidate the session and
  require re-pairing

## Adding a translation string

1. Slint markup: wrap the literal in `@tr("…")`
2. TypeScript: add the key to both tables in `src/i18n.ts`
3. Add `msgid`/`msgstr` to `i18n/pt_BR.po`
4. Run `bun run i18n`

## Commit conventions

Conventional Commits in English, imperative subject ≤ 72 chars, body only
for non-obvious *why*. One concern per commit.

## Troubleshooting

| Symptom | Cause / fix |
|---|---|
| `Could not connect to socket` on commit | 1Password SSH agent locked — open 1Password |
| Endless `Connection Terminated` (428) | `syncFullHistory` enabled, or too many rapid reconnects; wait a few minutes |
| Chats stuck empty after pairing | History sync arrived empty; the backfill fills it in over the next minutes |
| Pins/archives missing | App-state keys not shared yet; keep the phone online, the key request retries |
| Voice button does nothing | ffmpeg not found or no DirectShow microphone |
