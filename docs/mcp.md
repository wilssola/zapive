# MCP server (AI integration)

Zapive can run a [Model Context Protocol](https://modelcontextprotocol.io)
server inside the app. Local AI clients such as Claude Desktop, Claude
Code or Cursor can then list, read and search your chats and act on them
through the running Zapive.

> [!CAUTION]
> With the server on, **any program that has the token can read all your
> chats and send messages, reactions and read receipts as you.** It is off
> by default. Enable it only if you understand the risks below.

## Enable

1. **Settings → AI integration → Enable MCP server.**
2. The section shows the status ("Running at http://127.0.0.1:47823/mcp"),
   the **server URL** and the **access token** (hidden until you press
   **Show token**).
3. Press **Copy client configuration** and paste the JSON into your MCP
   client's configuration:

```json
{
  "mcpServers": {
    "zapive": {
      "type": "http",
      "url": "http://127.0.0.1:47823/mcp",
      "headers": { "Authorization": "Bearer <64-hex-character token>" }
    }
  }
}
```

The server starts once the vault is open and only runs while Zapive does.
While the PIN lock screen is up, tools answer with an error asking you to
unlock. The on/off switch and the port are saved for the installation;
the token is stored encrypted in the vault, one per account (switching
accounts restarts the server with that account's token). Tools always act
on the account on screen, even though the other accounts stay connected.

## Tools

| Tool | Kind | Arguments | Result |
| --- | --- | --- | --- |
| `list_chats` | read-only | `query?`, `label?`, `limit?` (1–200, default 50) | Chats, most recent first, with their `chat_id` and labels |
| `read_messages` | read-only | `chat_id`, `limit?`, `before?` (message_id for paging) | Messages in chronological order, with reactions |
| `search_messages` | read-only | `query`, `chat_id?`, `limit?` | Full-text matches from the local index |
| `get_chat_info` | read-only | `chat_id` | Name, type, unread count, mute/archive/pin state, labels, community, group members |
| `list_labels` | read-only | none | Your labels (the lists in Zapive) with `label_id` and chat count |
| `find_unanswered_chats` | read-only | `label?`, `min_wait_minutes?` (default 60), `max_age_days?` (1–365, default 30), `include_groups?` (default false), `limit?` | Chats whose latest message is from the other person with no reply from you, longest waiting first |
| `create_label` | **writes** | `name` | Creates a label, or returns the existing one with that name |
| `set_chat_label` | **writes** | `chat_id`, `label`, `labeled?` (default true) | Files a chat under a label, or removes it; syncs to your WhatsApp |
| `send_message` | **writes** | `chat_id`, `text` (WhatsApp formatting), `reply_to?` | Sends text as you to an existing chat |
| `mark_as_read` | **writes** | `chat_id` | Clears the unread count; sends read receipts only if **Send read receipts** is on |
| `react_to_message` | **writes** | `chat_id`, `message_id`, `emoji` (empty removes) | Reacts as you |

### Labels and unanswered chats

Labels are the same lists you see in the chat list. Wherever a tool takes
a `label`, give its `label_id` or its exact name (case and accents are
ignored); `list_labels` shows both. WhatsApp's built-in filters are not
listed.

`find_unanswered_chats` looks at the last 30 messages of each chat, skips
system notices and deleted messages, and reports a chat when everything
after your last reply came from the other person. `waiting_since` is the
first of those messages and `pending_messages` counts them (up to 30). To
check only customers, file their chats under a label such as "Clientes"
and pass it as `label`. It reads direct chats only, unless
`include_groups` is on, and checks at most the 500 most recent matching
chats per call.

Limits: up to 200 items per call, text up to 64 KiB, queries up to 256
characters, and a 20-second timeout per tool call. Tools can only reach
chats Zapive already knows. `send_message` refuses chats where you can't
send, for example a group you left or one where only admins can send.

## Transport and protection

- **Streamable HTTP** on `127.0.0.1` only, default port **47823**. If that
  port is busy, Zapive picks a free one and saves it, so the copied
  configuration stays valid across restarts.
- Every request needs `Authorization: Bearer <token>`. The token is 32
  random bytes in hex, compared in constant time.
- Requests from a browser origin other than localhost are refused, and so
  is a `Host` header that is not `127.0.0.1` or `localhost` with the
  server's port. This blocks DNS-rebinding attacks from web pages.
- Tool calls run on the UI thread, through the same code paths as the
  interface.

## Risks and recommendations

1. **The token is a password for your WhatsApp.** Don't paste it into
   shared configs, screenshots or issue reports. If it leaks, press
   **Regenerate token**: the old one stops working immediately.
2. **Prompt injection.** Messages are written by other people. A message
   can contain text that tries to make your assistant send messages,
   forward content or reveal chats. Use clients that ask for confirmation
   before tools that are not read-only (`send_message`, `react_to_message`,
   `mark_as_read`, `create_label`, `set_chat_label`), and keep that
   confirmation on.
3. **Other local software.** Any process running as your user can try to
   reach `127.0.0.1`. Without the token it gets `401`, but malware that
   reads your client's config file gets the token too.
4. **Account safety.** Automated or bulk sending can get a WhatsApp
   account restricted. Don't use the server for mass messaging.
5. **Turn it off** when you don't need it: the server stops at once, and
   requests already in flight get `503`.

## Troubleshooting

| Symptom | Cause and fix |
| --- | --- |
| Status "Could not start the server: …" | The port could not be bound; check firewall or security software that blocks localhost listeners. |
| Client gets 401 | Wrong or old token: copy the configuration again. |
| Client gets 403 | The request came from a non-local browser origin or an unexpected `Host` header. |
| Client gets 503 | The server was just switched off. |
| "Zapive is locked" | Unlock Zapive with your PIN. |
| Connection refused | Zapive is closed or the server is off. |
| Tools time out | The UI thread was busy for over 20 s (for example during a large sync); try again. |
