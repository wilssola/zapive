# Features

Everything below is implemented and working against a live account.

## Connection and session

- **QR login** rendered inside the window (RGBA buffer, no temp files)
- **Pairing code** login with a phone number, as an alternative to the QR
- **Persistent session** — relaunching goes straight to the chat list
- **Auto-reconnect** with exponential backoff (2 s → 60 s)
- **Logout** unlinks the device server-side (`remove-companion-device`)
  and wipes local conversations
- Runs with `markOnlineOnConnect: false` so the phone keeps notifying;
  `syncFullHistory` stays off because it breaks registration on 7.0.0-rc14

## Chat list

- Avatars (profile pictures, cached and encrypted) with colored initials
  as fallback, retried on transient failures
- Real group names via `groupFetchAllParticipating`
- Filters: **All**, **Unread**, **Archived**; live search by name
- **Pinned chats** first, with a pin icon; **unread badges** in accent color
- Previews prefixed with the sender in groups, `✓` for your own messages
- Duplicate `@lid`/phone chats merged into one conversation
- In-place row updates, so refreshes never reset the scroll position

## Conversations

- Message bubbles grouped by run (tight spacing for consecutive messages)
- **Day separators** — Today / Yesterday / date
- **Delivery ticks** — sent (✓), delivered (✓✓), read (✓✓ blue)
- **Presence** — "typing…", "recording audio…", "online" under the name
- **Reactions** shown as a pill attached to the bubble
- **Forwarded** and **deleted** ("This message was deleted") indicators
- Group sender names are clickable — opens or starts that DM
- **Text selection** by dragging (blue selection) plus a context menu with
  Copy and Forward
- **Scroll memory** per conversation, floating jump-to-latest button, and
  loading older messages when reaching the top
- **Sync banner** while older history is being fetched

## Media

| Type | Receive | Send |
|---|---|---|
| Images | Inline thumbnail (≤330×380) + full-screen lightbox | Picker or Ctrl+V, with caption preview |
| Video / GIF | Embedded thumbnail with play badge; opens externally | GIF tab in the picker |
| Audio / voice notes | Play button (system player) | **Microphone recording** (ffmpeg → mono opus, ptt), optional **view-once** |
| Documents | Filename + open button | Attach menu, any file type |
| Stickers | Rendered like images | Recent-stickers tab; any image converted to 512 px webp |

- **Attach menu** (`+`): Document, Photos, Audio, Sticker
- **Emoji picker** with a curated palette that renders correctly in Slint
- Clipboard images (Ctrl+V) open the same confirmation preview as attachments

## Status (stories)

- Rail tab listing authors with accent-ring avatars
- Viewer overlay for text, image and video (thumbnail) updates
- 24-hour lifetime, pruned automatically and persisted between launches

## Desktop integration

- **Native Windows toasts** with the contact's **round photo**, coalescing
  bursts into a single summary notification
- **System tray**: closing the window keeps the app connected; the icon
  reopens it and offers Quit
- Notifications are suppressed for the conversation currently open

## Appearance and language

- **Dark**, **Light** and **System** themes (registry-polled, live switch)
- **English**, **Portuguese** and **System** language (Slint `@tr` gettext
  catalog + TypeScript dictionary)
- **Tabler icons** throughout, colorized per theme

## Security

- All local data encrypted at rest (see [security.md](security.md))
- Optional PIN with lock screen; media files encrypted too
