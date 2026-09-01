# Architecture

## Process model

Zapive is a single Node.js process. Slint's event loop drives the window
and, on Windows, ticks the Node event loop every 16 ms — so the Baileys
WebSocket, timers and promises keep running while the UI is displayed.
Everything resolves on the main JS thread, which is why UI models can be
updated directly inside socket handlers without synchronization.

A second, tiny process runs a PowerShell `NotifyIcon` for the system tray
and reports clicks over stdout.

```
┌──────────────────────── node (main thread) ─────────────────────────┐
│                                                                     │
│  main.ts ── vault unlock ── locale ── theme ── tray ── event loop    │
│     │                                                               │
│     ├── Db (SQLite + envelope encryption)                           │
│     ├── WhatsAppService ──── WebSocket ────► WhatsApp servers       │
│     │        │ events                                               │
│     │        ▼                                                      │
│     ├── Bridge ── Store (chats, messages, statuses, aliases)        │
│     │      │                                                        │
│     │      ├── MediaService (download, decrypt, decode, ffmpeg)     │
│     │      └── Notify (Windows toasts)                              │
│     │                                                               │
│     └── ArrayModels ────────► Slint UI (ui/app.slint)               │
└─────────────────────────────────────────────────────────────────────┘
```

## Modules

| File | Responsibility |
|---|---|
| `src/main.ts` | Boot order, vault/lock screen, locale, theme, tray, event loop |
| `src/db.ts` | SQLite vault, envelope encryption, Baileys auth-state adapter |
| `src/whatsapp.ts` | Socket lifecycle, sending, downloads, presence, app-state sync |
| `src/store.ts` | In-memory chats/messages/statuses, normalization, persistence |
| `src/bridge.ts` | The only module touching Slint: models, callbacks, orchestration |
| `src/media.ts` | Media cache, image decoding, avatars, pickers, clipboard, ffmpeg |
| `src/notify.ts` | Native toasts with burst coalescing |
| `src/qr.ts` | QR string → RGBA buffer for the login screen |
| `src/i18n.ts` | TypeScript-side translations (en/pt) |
| `src/env.ts` | Renderer selection (Skia) — must be imported before slint-ui |
| `src/paths.ts` | Per-platform data/cache directories and resource lookup |
| `src/platform.ts` | Every OS-specific call: clipboard, dialogs, theme, key store, tray |
| `src/native.ts` | Loads the addons that cannot be bundled (slint-ui, sharp, node-notifier) |
| `src/resources.ts` | Unpacks the embedded archive (UI, catalogs, native packages) on first run |
| `scripts/build.mjs` | Bundles and injects the app into a Node executable |
| `ui/app.slint` | Entire interface: theme globals, screens, overlays |
| `i18n/*.po` | Slint (`@tr`) translation catalogs, applied at load time |
| `src/slint-tr.ts` | Swaps `@tr("...")` literals for the catalog entry before `loadSource` |

## Data flow

**Inbound.** `WhatsAppService` subscribes to Baileys events and forwards
them through the `WAListener` interface. `Bridge` normalizes each payload
into `Store` and then patches the Slint `ArrayModel`s. Chat rows are
updated in place (`setRowData` plus tail push/remove) so the list never
loses its scroll position.

**Outbound.** Slint callbacks call `Bridge` methods, which call
`WhatsAppService`. Sent messages are echoed into the store immediately;
the later `messages.upsert` echo is deduplicated by message id.

**Media.** Downloads are decrypted by Baileys, re-encrypted with the vault
key and written to `media_cache/`. Images are decoded to RGBA in memory
(never written back in plaintext). Audio playback, document opening and
toast icons need a real file, so those decrypt into `media_cache/.tmp/`,
which is wiped at every launch.

## Storage layout

Everything lives in `zapive.db` (SQLite, WAL):

| Key prefix | Contents |
|---|---|
| `auth:*` | Baileys credentials, signal keys, app-state versions, LID mappings |
| `store:chats` | Chat metadata (name, preview, timestamp, unread, pin, archive) |
| `store:contacts` | jid → display name |
| `store:aliases` | LID jid → phone-number jid |
| `store:msgs:<jid>` | Last 300 messages per conversation |
| `store:statuses` | Status updates, pruned to 24 h |
| `meta:setting:*` | Theme and language (plaintext — needed before unlock) |
| `meta:dk`, `meta:pin_salt` | Wrapped data key and PIN salt |

## Notable design decisions

**History backfill.** This account's pairing history sync arrives empty, so
Zapive walks backwards with `fetchMessageHistory` — batches on connect and
a targeted fetch when a thin conversation is opened or scrolled to the top.

**LID identities.** WhatsApp v7 addresses contacts by both `@lid` and phone
number, which produced duplicate chats. Aliases are learned from
`remoteJidAlt`/`participantAlt` and from Baileys' own LID mapping store,
then duplicates are merged (messages, unread counts, pins) and persisted.

**App-state keys.** Pinned and archived chats arrive as encrypted app-state
patches. When the phone never shared the sync keys, Baileys logs
`failed to find key <id>`; a custom pino stream detects that and requests
those keys from the phone, after which the parked collections resume.

**Reactive theming.** A Slint `global Theme` derives every color from a
single `dark` boolean, so switching themes repaints instantly without
reloading the UI.


## Platforms

Windows, macOS and Linux run the same code; only `src/platform.ts` differs
per system, and it is the only module allowed to shell out to the OS.

| Concern | Windows | macOS | Linux |
|---|---|---|---|
| Vault key | DPAPI (`CurrentUser`) | login keychain (`security`) | Secret Service (`secret-tool`) |
| Clipboard | PowerShell `Set-/Get-Clipboard` | `pbcopy` / `pbpaste` | `wl-copy` or `xclip` |
| File dialog | WinForms `OpenFileDialog` | `osascript choose file` | `zenity` or `kdialog` |
| Open links | `cmd /c start` | `open` | `xdg-open` |
| System theme | registry `AppsUseLightTheme` | `defaults read -g AppleInterfaceStyle` | `gsettings color-scheme` |
| Microphone | ffmpeg `dshow` | ffmpeg `avfoundation` | ffmpeg `pulse` / `alsa` |
| Tray | PowerShell `NotifyIcon` | — (window owns the lifetime) | — |
| Notifications | SnoreToast | `terminal-notifier` | `notify-send` |

Where a tool is missing the feature degrades instead of failing: without a
key store the vault warns and keeps the key unbound; without `zenity` the
attachment button reports that no dialog is available.

## Where the data lives

| | Vault (`zapive.db`) | Media cache |
|---|---|---|
| Windows | `%APPDATA%\Zapive` | `%LOCALAPPDATA%\Zapive\Cache\media` |
| macOS | `~/Library/Application Support/Zapive` | `~/Library/Caches/Zapive/media` |
| Linux | `$XDG_DATA_HOME/zapive` | `$XDG_CACHE_HOME/zapive/media` |

The executable also unpacks its embedded resources into
`<cache>/runtime/<content-hash>/`, which is disposable: deleting it only
costs one unpack on the next start.

Nothing is written next to the executable. A database left in the working
directory by an earlier build is moved on the first run.
