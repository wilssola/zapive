# Troubleshooting

When asking for help, include the OS, the Zapive version (**Settings →
About**) and what you expected to happen. **Never attach** `vault.db`,
`wa.db`, `search.db`, the cache, your MCP token or screenshots of real
chats. Use [demo mode](demo-mode.md) for screenshots, and remove names,
numbers and paths from log excerpts.

## Logs

- File: `zapive.log` in the data folder (`%APPDATA%\Zapive`,
  `~/Library/Application Support/Zapive`, `~/.local/share/zapive`). It
  rotates at 4 MB, and the previous run is kept as `zapive.log.old`.
- More detail: start with `ZAPIVE_LOG=debug` (PowerShell:
  `$env:ZAPIVE_LOG="debug"; & "C:\Program Files\Zapive\zapive.exe"`). Debug
  logs include protocol traffic and **may contain identifiers**, so review
  them before sharing.
- Panics are written to the log with a backtrace.

## Installing and starting

| Problem | Fix |
| --- | --- |
| Windows SmartScreen / "unknown publisher" | The build is self-signed: **More info → Run anyway**. |
| macOS "cannot be opened because the developer cannot be verified" | Right-click → **Open**, or allow it in System Settings → Privacy & Security. |
| Starting Zapive again does nothing | Only one instance runs at a time; the running window is brought to the front. On Windows, check the tray. |
| Linux: warning "secret-tool store failed; storing with weak protection" | Install `secret-tool` (libsecret) and run a Secret Service keyring. To move the key into the keyring, log out with **Erase everything** and link again (or start with an empty data folder). |
| The window is not translucent | See [appearance.md](appearance.md): Liquid Glass needs Windows 11 22H2+, macOS, or KDE Plasma on Wayland, and is dropped while the window is unfocused. |

## Linking and connection

| Problem | Fix |
| --- | --- |
| The QR code expires or won't scan | Wait for the new code, raise screen brightness, or use **link with phone number** instead. |
| "Failed to generate pairing code" | Check the number has the country code, and that the connection works; try again. |
| Chats appear but old messages are missing | History arrives in the background. Keep WhatsApp open on the phone. Scroll up in a chat to request older messages; the sync dialog shows progress. Anything older stays on the phone. |
| The device was removed on the phone | Zapive returns to the login screen. Link again; with "keep conversations" the local history is reused. |
| Messages stuck as pending | They are queued in the outbox and are sent when the connection returns. |

## PIN and vault

| Problem | Fix |
| --- | --- |
| Wrong PIN, and attempts are slowed | After 3 wrong attempts each try waits longer (up to 8 s). |
| Forgot the PIN | It **can't be recovered**. Close Zapive, move the data folder aside (or delete `vault.db`), start again and link the device. History is fetched again from the phone. |
| Moved to a new computer or Windows profile | The vault key is bound to the OS key store of the old account, so a copied `vault.db` can't be opened elsewhere. Link the new installation as a new device. |

## Notifications and sound

See [notifications.md](notifications.md#troubleshooting).

## Calls

| Problem | Fix |
| --- | --- |
| "No audio devices available" / "No camera available" | Connect the device and choose it in **Settings → Calls**. Changes apply to the next call. |
| Can't start a group call | Not supported yet; only one-to-one calls. |
| A call arriving during another call is declined | That is intentional: Zapive declines it to keep the current call. |
| Call fails to connect | Collect `zapive.log` with `ZAPIVE_LOG=debug`. Firewalls that block UDP can stop the media relay. |

## Media and documents

| Problem | Fix |
| --- | --- |
| A video shows as a document without preview | Only H.264/MP4 (and GIF) play in the app. Use **Open with another app**. |
| "The PDF engine could not be downloaded" | The viewer downloads pdfium (about 7 MB) once from GitHub; check the connection and retry, or open the PDF with another app. |
| Media doesn't download automatically | Check **Settings → Downloads**. |
| Disk usage grows | The cache can be deleted safely (with Zapive closed); media downloads again when needed. |

## Translation and MCP

- Translation: see [translation.md](translation.md#troubleshooting).
- MCP: see [mcp.md](mcp.md#troubleshooting).

## Updates

| Problem | Fix |
| --- | --- |
| No update offered | Checks run 10 s after start and every hour; use **Check for updates**. While the repository is private, set `ZAPIVE_GH_TOKEN`. Flatpak installs don't self-update. |
| "Update failed" | Usually a download cut short or a checksum mismatch. Try again; the old binary stays in place. |
| Translation stopped working after an update (Windows) | The DLLs next to `zapive.exe` are missing or stale: update again or reinstall with the setup file. |

## Resetting everything

Close Zapive, then delete the data folder and the cache folder (paths in
[privacy-and-security.md](privacy-and-security.md#what-is-stored-where)).
Also remove the device from **Linked devices** on the phone.
