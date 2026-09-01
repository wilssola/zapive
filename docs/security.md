# Security

Zapive stores a linked-device session plus message history on disk. That
data is protected with the envelope-encryption model password managers
use, so the vault survives a stolen disk and never keeps the PIN around.

## Envelope encryption

```
PIN ──scrypt(N=2^17, r=8, p=1)──► KEK ──AES-256-GCM──┐
                                                     ├──► wrapped DK ──DPAPI──► meta.dk
                    random 256-bit Data Key (DK) ────┘
                              │
                              ├── AES-256-GCM ──► every kv row (auth, chats, messages)
                              └── AES-256-GCM ──► every file in media_cache/ (ZENC1 header)
```

- The **data key** is random and never derived from the PIN, so changing
  or removing the PIN only re-wraps it — no re-encryption of data.
- The **KEK** comes from scrypt with a memory-hard parameter set
  (~130 MB), making offline PIN guessing expensive.
- **DPAPI** (`CryptProtectData`, CurrentUser scope) wraps the result, the
  same OS facility Chrome and Signal Desktop use. Copying `zapive.db` to
  another machine or Windows account makes it unreadable.
- Data is **always encrypted**, PIN or not. Without a PIN the DK is
  protected by DPAPI alone, so the app opens automatically for that
  Windows user.

## PIN handling

- Never stored, not even hashed. Verification is implicit: a wrong PIN
  produces a wrong KEK, and AES-GCM authentication fails when unwrapping.
- After three failures, retries back off exponentially (0.5 s → 8 s).
- `lock()` zeroes the key buffer in memory.
- Legacy vaults (direct scrypt-derived key) are migrated on first unlock.

## Media files

Cached media carries a `ZENC1` header followed by IV, auth tag and
ciphertext. Images are decrypted straight into memory for display.
Playback, document opening and toast icons need real files, so those are
decrypted into `media_cache/.tmp/`, which is deleted at every launch.

## Threat model

**Protected against**
- Disk theft or backup exfiltration (data unreadable without the Windows
  account, and without the PIN when set)
- Another Windows account on the same machine reading the vault
- Casual local access while the app is locked

**Not protected against**
- Malware running as your Windows user while the vault is unlocked
- A keylogger capturing the PIN
- Anything on the phone side — Zapive is a linked device and inherits
  whatever the account exposes

**Known gap.** Windows Hello / TPM-bound keys would remove the PIN from
the equation, but require a native module (`KeyCredentialManager`). DPAPI
is the practical baseline until then.

## Data hygiene

- `.gitignore` excludes `zapive.db*`, `media_cache/`, and legacy backups
- Logout deletes credentials and stored conversations
- No telemetry, no network calls beyond WhatsApp itself


## Key protection per platform

The wrapped data key is handed to whatever the system offers:

- **Windows** — DPAPI (`CryptProtectData`, `CurrentUser` scope), the same
  facility Chrome and Signal Desktop use for their local keys
- **macOS** — the login keychain, via `security add-generic-password`
- **Linux** — the Secret Service (GNOME Keyring, KWallet) via `secret-tool`

If none is reachable the key is stored base64 with a warning on stdout,
and a PIN remains the only thing protecting the vault at rest.
