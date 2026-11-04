# Notifications and sound

Zapive uses each operating system's own notification system. Settings are
in **Settings → Notifications**:

| Setting | Default | Effect |
| --- | --- | --- |
| **Notifications** | On | Banners for new messages and incoming calls. When off, no banners appear, but unread counts still update. |
| **Show message previews** | **Off** | On: the banner shows the chat name, the text (with the sender in groups) and a thumbnail for photos, videos and stickers. Off: it says only "Zapive · New message". |
| **Message sounds** | On | A short chime for incoming messages, **even when banners or OS notifications are off**. |

## When a banner appears

- For an incoming message in a chat that is **not open**. If the chat is
  open, the message goes straight into the conversation.
- **Muted chats** stay silent unless the message **mentions you**.
- **Bursts are grouped:** messages that arrive within about a second
  become one banner, such as "3 new messages from Ana". If they come from
  several chats, the banner lists up to three names.
- Nothing is shown while the vault is locked (PIN screen). Pending banners
  are dropped instead of revealing content.
- **Incoming calls** show a banner (if notifications are on) besides the
  ring and the call window, which always appear.

## Clicking a banner

| Platform | Click | Actions |
| --- | --- | --- |
| Windows 10/11 | Brings Zapive to the front and **opens that chat** | **Reply** (type in the banner and the reply is sent to that chat), **Play** for voice notes, played in the background |
| Linux (D-Bus, also inside Flatpak) | Opens that chat | **Open**, **Reply** (opens the chat so you can type), **Play** for voice notes, where the notification server supports actions |
| macOS | Shows the banner only (no click-through or actions yet) | — |

For a grouped banner, a click opens the chat when all the messages are
from **one chat**. Otherwise it just brings the window to the front. The
link between a banner and its message expires after about a minute.

On Windows, banners carry the Zapive app identity and icon, and a
thumbnail when previews are on. Windows **Focus / Do not disturb** and the
per-app notification settings still apply.

## The message sound

- A two-note chime synthesised by the app (no audio file), played through
  the default output device with `cpal`. It **does not depend on OS
  notifications**, so you hear it even with banners off or Windows
  notifications disabled.
- Played once per burst: messages that arrive within 1.5 s chime once.
- Not played:
  - when **Message sounds** is off;
  - for your own messages;
  - for messages older than 30 s (history sync or catch-up after
    reconnecting);
  - for muted chats, unless you are mentioned;
  - for the chat you have open while the Zapive window has focus;
  - while the vault is locked.
- The setting is stored as `message_sound` (`1` or `0`) and is on by
  default.

## Tray and background (Windows)

Closing the window **hides it to the tray**, so Zapive keeps receiving
messages, notifying and ringing. Use the tray menu to open or exit it.
With **Start at login**, Zapive starts directly in the tray. On Linux and
macOS, closing the window quits the app.

## Troubleshooting

| Problem | Check |
| --- | --- |
| No banners on Windows | Settings → System → Notifications → **Zapive** is allowed, Focus/Do not disturb is off, and Zapive's **Notifications** setting is on. |
| Banners show only "New message" | Turn on **Show message previews**. |
| A banner click doesn't open the chat | More than a minute passed, or the banner grouped several chats; on macOS this is not supported yet. |
| No banners on Linux | A notification daemon must be running (most desktops have one); actions depend on it. |
| No chime | **Message sounds** is on, a default output device exists, the chat isn't muted, and it isn't the chat you are looking at. |
