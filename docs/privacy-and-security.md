# Privacy and security

This page describes what Zapive stores, how it is protected, what goes over
the network, and the limits of that protection. Report vulnerabilities
privately as described in [SECURITY.md](../SECURITY.md).

## Account safety

Zapive is an **unofficial** client. It uses WhatsApp's companion-device
protocol through [whatsapp-rust](https://github.com/jlucaso1/whatsapp-rust),
the same model WhatsApp Web uses. WhatsApp can still restrict accounts that
use unofficial clients. If you can't accept that risk, use a secondary
number or an official app.

## Defaults

| Behaviour | Default | Where |
| --- | --- | --- |
| Send read and played receipts | **Off** | Settings → Privacy |
| Erase a message when its sender deletes it for everyone | **Off**: the message stays, marked as deleted | Settings → Privacy |
| Show message previews in notifications | **Off**: the banner says only "Zapive · New message" | Settings → Notifications |
| PIN on launch | Off | Settings → Security |
| MCP server | **Off** | Settings → AI integration |
| Blur names, photos, text, media | Off | Settings → Privacy |

With **blur**, hovering an item or giving it keyboard focus reveals only
that item. It helps when someone is looking at your screen or you are
sharing it.

## The vault (envelope encryption)

Everything Zapive writes about your conversations goes through one key
hierarchy:

1. A random **256-bit data key (DK)** encrypts each record and each cached
   file with **AES-256-GCM** (random 96-bit IV, authenticated). Encrypted
   files start with the `ZENC1` marker.
2. The DK is stored **wrapped**:
   - **With a PIN:** under a key derived from the PIN with **scrypt**
     (N = 2^17, r = 8, p = 1). The PIN itself is never stored, and changing
     it only re-wraps the DK.
   - **Always:** again by the **OS key store** (see below), which binds the
     vault to your user account on this computer.
3. While the app runs, the DK lives in memory only (and is zeroed when the
   vault is locked). Purpose-specific keys, such as the search index key,
   are derived from it and never stored.

With a PIN set, Zapive asks for it at launch. After three wrong attempts,
each new try waits longer, up to 8 seconds.

> [!WARNING]
> There is no PIN recovery. If you forget the PIN, the encrypted data
> cannot be read. Log out (erasing the data) and link the device again.
> Recent history comes back from the phone.

### OS key store

| OS | Where the wrapped key goes | Fallback |
| --- | --- | --- |
| Windows | DPAPI, bound to your Windows user | weak (`raw:`) if DPAPI fails |
| macOS | Login Keychain, service `Zapive`, account `vault` (`security` CLI) | weak if the keychain refuses |
| Linux | Secret Service through `secret-tool` (GNOME Keyring, KWallet, KeePassXC…) | weak if `secret-tool` is missing or fails |

"Weak" means the key is stored base64-encoded next to the data, so anyone
who can read your files can decrypt them. Zapive prints a warning to its
console when this happens. On Linux, install `secret-tool` **before** the
first launch.

## What is stored where

| Data | File (Windows · macOS · Linux) | Protection |
| --- | --- | --- |
| Messages, chats, contacts, drafts, outbox, MCP token, translation settings, release history | `vault.db` in the data folder | AES-256-GCM per record |
| App preferences (theme, language, toggles, MCP port, account list with the number and display name) | `vault.db`, `meta` table | **Plain**: they are needed before unlock |
| Search index | `search.db` (`search-<id>.db` for extra accounts) | Keyed hashes plus encrypted snippets; see below |
| WhatsApp session (Signal keys, device identity) | `wa.db` (`wa-<id>.db`) | **Plain SQLite** owned by whatsapp-rust, protected by file permissions |
| Media and avatar cache | cache folder `media/` | AES-256-GCM per file |
| Temporary decrypted copies (opening a file in another app) | cache folder `media/.tmp` | Plain; deleted at the next launch |
| Notification thumbnails (only with previews on) | cache folder `.toast` | Plain; removed about a minute after the banner, and at launch |
| Translation models, PDF engine | data folder `translation/models`, `pdfium/` | Plain; public files |
| Log | `zapive.log` in the data folder (4 MB, one previous run kept as `.log.old`) | Plain |

Data folder: `%APPDATA%\Zapive` · `~/Library/Application Support/Zapive` ·
`$XDG_DATA_HOME/zapive` (default `~/.local/share/zapive`).
Cache folder: `%LOCALAPPDATA%\Zapive\Cache` · `~/Library/Caches/Zapive` ·
`$XDG_CACHE_HOME/zapive` (default `~/.cache/zapive`).

Deleting the cache never loses the account. Deleting `search.db` only
means the index is rebuilt. Deleting `wa.db` means linking again.

> [!TIP]
> Use full-disk encryption (BitLocker, FileVault, LUKS) for what Zapive
> cannot encrypt itself: `wa.db`, the log, temporary files, swap and
> backups.

## Search index

Full-text search needs SQLite FTS5 to see tokens, so words can't simply be
AES blobs. Instead:

- Each word is lower-cased, its Latin accents are removed, and it becomes
  a **keyed, prefix-preserving term** before it reaches SQLite. The key is
  derived from the DK and never stored, so as-you-type prefix search works
  but no word is readable without the vault key.
- The snippet shown in results is stored separately as **AES-256-GCM
  ciphertext** that FTS5 never sees.
- A wrong or rotated key is detected when the index opens. The index is
  then dropped and rebuilt instead of returning garbage.

**Known leakage**, as with any deterministic searchable index: the length
of words and how often a word (or a pair of words) appears.

## Network traffic

Zapive only connects to:

| Destination | Why | When |
| --- | --- | --- |
| WhatsApp servers and media CDN | Messaging, media, calls | While connected |
| Websites linked in messages | Link preview title and thumbnail. Private, loopback and LAN addresses are refused | When a message with a link and no embedded preview is shown |
| `api.openverse.org` | GIF search | When you search GIFs |
| `api.github.com` / GitHub release assets | Update check and download | At start, then hourly |
| `raw.githubusercontent.com/argosopentech`, `argos-net.com`, GitHub release assets | Translation model catalog and models (strict host allow-list) | Only when you refresh or install models |
| `github.com/bblanchon/pdfium-binaries` | PDF engine | Once, the first time you open a PDF |

There is no telemetry and no analytics. The MCP server listens on
`127.0.0.1` only.

## Receipts and deletions

- With **Send read receipts** off, Zapive never sends read or played
  receipts, including when an MCP client marks a chat as read. The phone
  and other devices still behave as they are configured.
- With **Apply messages deleted by the sender** off, a "delete for
  everyone" from someone else keeps the message and adds a marker. Your
  own deletions always apply.

## MCP server risks

The optional MCP server lets local AI clients **read your chats and send
messages as you**. It is off by default. When it is on:

- Every request must carry a 256-bit bearer token, compared in constant
  time.
- It listens on `127.0.0.1` only. Browser origins other than localhost and
  unexpected `Host` headers are refused, which blocks DNS rebinding.
- **Anyone or any program with the token can read all your chats and send
  messages, reactions and read receipts as you.** Treat it like a
  password, regenerate it if it leaks, and only paste it into clients you
  trust.
- Prompt injection is real: a message someone sends you can contain
  instructions aimed at your AI assistant. Prefer clients that ask before
  running non-read-only tools.

Details: [mcp.md](mcp.md).

## Threat model in short

**Protected:** a stolen or copied disk or backup (the vault, media and
index are unreadable without the OS key store, and also without the PIN
if one is set), and other user accounts on the same computer.

**Not protected:** malware or anyone running as **your** user while you
are logged in (the OS key store unlocks for them too); the plain files
listed above; and screenshots or screen sharing (use blur).

## Demo mode

`zapive --demo` (built with `--features demo`) redirects every data
directory to a temporary folder before anything is read, never opens a
WhatsApp connection, and deletes the folder on exit. Use it for
screenshots and bug reports; see [demo-mode.md](demo-mode.md).
