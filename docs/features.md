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
- **@ marker** on chats where an unread message mentions you (by number or `@all`)
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
- **WhatsApp formatting** — *bold*, _italic_, ~strike~ and ```monospace```
  render as styled text
- **Link previews** with thumbnail, title, description and host; the whole
  card opens the link, and pages that expose no metadata get no card
- **Mentions** resolve to contact names and open that chat when clicked;
  **links** open in the default browser
- **Text selection** by dragging (blue selection) plus a context menu with
  Copy and Forward
- **Scroll memory** per conversation, floating jump-to-latest button, and
  loading older messages when reaching the top
- **Sync banner** while older history is being fetched

## Media

| Type | Receive | Send |
|---|---|---|
| Images | Inline thumbnail (≤330×380) + full-screen lightbox | Picker or Ctrl+V, with caption preview |
| Video / GIF | GIFs loop in the bubble; videos play in an in-app overlay | GIF tab with keyless search (Openverse) |
| Audio / voice notes | **In-app player**: waveform, seek, elapsed time, speed (1x–3x), mini player when you leave the chat | **Microphone recording** (ffmpeg → mono opus, ptt), optional **view-once** |
| Documents | Filename + open button | Attach menu, any file type |
| Stickers | Animated, drawn without a bubble | Tabs for stickers you sent and starred ones; any image converted to 512 px webp |

- Group messages show the sender avatar beside the first bubble of a run
- **Attach menu** (`+`): Document, Photos, Audio, Sticker
- **Emoji picker** with a curated palette that renders correctly in Slint
- Clipboard images (Ctrl+V) open the same confirmation preview as attachments

## Contact and group info

- Clicking the conversation header opens a side panel with the large
  avatar, phone number (contacts) or participant count (groups)
- **About** text for contacts and **description** for groups, fetched live
- **Shared media** grid built from thumbnails already cached locally
- Archive / unarchive the conversation, or clear its local messages

## Calls

Baileys can detect and decline calls, but never answer them — so Zapive
makes sure you never miss one instead:

- **Incoming call popup** with the caller's photo and voice/video label,
  raising the window even when the app sits in the tray
- **Native notification** alongside it, so the call is visible anywhere
- **Decline** ends the call from the desktop; **Dismiss** only hides the
  popup (the phone keeps ringing)
- **Calls tab** in the rail with the history: voice/video, and whether the
  call was answered elsewhere, declined, missed or ended
- Clicking an entry opens that conversation

## Channels and communities

- **Channels** (`@newsletter`) have their own rail tab; names are resolved
  through newsletter metadata and posts render like any conversation
- **Communities** tab groups the community shells and their linked groups,
  detected from `linkedParent` / `isCommunity` in group metadata

## Status (stories)

- Rail tab listing authors with accent-ring avatars
- Viewer overlay for text, image and video (thumbnail) updates
- 24-hour lifetime, pruned automatically and persisted between launches

## Desktop integration

- **Native Windows toasts** with the contact's **round photo**, coalescing
  bursts into a single summary notification; clicking one opens that
  conversation and raises the window
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
