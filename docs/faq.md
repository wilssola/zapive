# FAQ

### Is Zapive official?
No. It is an independent client and is not affiliated with or endorsed by
WhatsApp or Meta. It connects as a linked device through
[whatsapp-rust](https://github.com/jlucaso1/whatsapp-rust).

### Can my account be banned?
Unofficial clients can break WhatsApp's terms, and WhatsApp may restrict
accounts that use them. Zapive behaves as a normal companion device and
doesn't send in bulk, but the risk isn't zero. Use a secondary number if
that matters to you. Don't use the MCP server for automated mass
messaging.

### Does my phone need to stay online?
No more than with WhatsApp Web: linked devices work on their own once
linked. The phone is still needed to link and to provide older history.

### Is it Electron or a web view?
No. Zapive is one native Rust binary with a Slint UI and no browser
engine. It uses about 140–180 MB of RAM with roughly 900 chats loaded.

### Which systems are supported?
Windows 10/11 x64 (the main platform), Linux x86_64 (AppImage) and macOS 11+
on Apple silicon (DMG). Linux and macOS are built and tested by CI but get
less hands-on testing.

### Are my messages encrypted on disk?
Yes. Messages, chats, media and avatars are encrypted with AES-256-GCM
under a key protected by the OS key store and, optionally, by your PIN.
WhatsApp's session store, the basic preferences and the log are not; see
[privacy-and-security.md](privacy-and-security.md).

### Why don't my contacts see blue ticks?
Read receipts are **off by default**. Turn on **Settings → Privacy → Send
read receipts**.

### Someone deleted a message but I still see it. Why?
By default, a message the sender deletes for everyone is kept and marked
"deleted". Turn on **Apply messages deleted by the sender** to erase such
messages instead.

### Does translation send my messages to a server?
No. Translation runs on your CPU with CTranslate2. Only the one-time model
download uses the network; see [translation.md](translation.md).

### Can I use Zapive with ChatGPT, Claude or Cursor?
Yes, with any MCP client, through the opt-in local MCP server. Read
[mcp.md](mcp.md) first: whoever has the token can read and send messages
as you.

### Can I make calls?
Yes, one-to-one voice and video calls. Group calls aren't supported yet.

### Can I use several numbers?
Yes, up to five accounts, all connected at the same time. Switch between
them from the left rail; each shows its unread chats.

### Can I vote in polls or create them?
Yes. Create a poll from the attach menu, vote by clicking an option, and
use **View votes** to see who picked what; see [features.md](features.md#polls).

### Are there keyboard shortcuts?
Yes. Press `Ctrl+/` (`Cmd+/` on macOS) for the full list; see
[features.md](features.md#keyboard).

### How do I get updates?
Zapive updates itself from GitHub Releases, after checking each download
against its SHA-256 checksum; see [releases.md](releases.md#self-update).

### How do I take a screenshot for a bug report without exposing my chats?
Use [demo mode](demo-mode.md):
`cargo run --features demo -- --demo`.

### What language is the interface in?
English or Portuguese, following the system by default (**Settings →
General → Language**). Contributions of new languages are welcome.

### What's the license?
Zapive is open source under the [MIT License](../LICENSE). Bundled third-party components keep their own
licenses.
