# Architecture

Zapive is a native Rust binary with a Slint UI compiled from `ui/app.slint` by `build.rs`. This is a map of the current implementation, not a claim that every module already has a narrow interface. See [modules.md](modules.md) for responsibilities and [project-conventions.md](project-conventions.md) for design standards.

## Runtime flow

```text
Slint AppWindow -> bridge::Bridge (UI thread) -> store -> vault (SQLite)
       callbacks          |                    |       encrypted data
                          +-> wa::Cmd (tokio mpsc) -> wa::WaService
                                                     whatsapp-rust client
                                 events -> UI-thread scheduling -> Bridge
```

`bridge::Bridge` owns `AppWindow`, store state and Slint models. UI callbacks are registered in `src/bridge/wiring.rs`; bridge methods send `wa::Cmd` through `WaService::send`. `src/wa/mod.rs` owns the protocol client and the command channel; `src/wa/executor.rs` dispatches commands to private `wa` modules and `src/wa/pump.rs` maps protocol events to bridge calls. The protocol event path invokes concrete bridge methods; it is **not** an interchangeable event-adapter interface. Every linked account has its own `WaService` (client, executor and command channel), and each service gets a unique epoch (`WaService::epoch`). Everything the `wa` side sends back goes through `bridge::wa_apply_at` with that epoch (`src/wa/pump.rs`, `src/wa/executor.rs`, and the other private `wa` modules via `Ctx::epoch`). `wa_apply_at` queues work while the vault is locked, drops it once the account is gone, and otherwise runs it for the account the epoch belongs to (`src/bridge/mod.rs`). `bridge::wa_apply` targets whichever account is on screen; `bridge::ui_apply` schedules work without any of these guards. Preserve the intended epoch and vault behavior when changing any event path.

## Accounts

All paired accounts stay connected. The bridge keeps one account's state in its own fields (the account on screen), and every other account's state in an `AccountState` (`src/bridge/accounts.rs`) with matching fields. Switching swaps those fields; no client reconnects. An event for a background account runs **headless**: `Bridge::run_headless` swaps that account's state in, together with a hidden `AppWindow` and unbound models (`Views`), sets the vault's account scope, runs the regular handler, and swaps everything back. The same handlers therefore store, index, notify and count unread for every account. Rules for code on event paths:

- Use `self.once` for delayed work: a timer scheduled while headless fires for the same account.
- Check `self.headless` before anything that must only happen for the account on screen (the switcher, translation, ringing, the chat list refresh).
- A setting that handlers read from the window must be mirrored into the hidden one (`mirror_settings`), and per-account screen state that lives on the window travels with the account (`Carry`).
- A new per-account `Bridge` field belongs in `AccountState` and `swap_account`, or it leaks between accounts.

`src/wa_map.rs` normalizes protocol messages into `StoredMessage` for live traffic and history sync. `src/store.rs` maintains chats and message history in memory, hydrating conversations on demand and keeping at most eight warm chats; `src/vault.rs` persists encrypted records via SQLite. `src/search.rs` maintains a separate FTS5 index. The bridge runs on the Slint UI thread; the WhatsApp client and other async jobs run on tokio. Translation, PDF processing, audio, video and camera work may use dedicated threads. Never block the UI thread with network, disk or decoding work.

## UI and native integrations

`ui/app.slint` exports `AppWindow` from `ui/components/app-window.slint`. Related Slint files live in `ui/components/`; `theme.slint` provides theme tokens. The bridge currently sets window properties and registers callbacks directly; this is a broad interface, not a set of independent UI globals. See [icons.md](icons.md) for icon provenance and placement.

OS-specific operations live in `src/platform/` (`windows.rs`, `macos.rs`, `linux.rs`). Dependencies and exact versions are in `Cargo.toml` and `Cargo.lock`; build commands and platform requirements are in [building.md](building.md).

## Real seams and constraints

- `WaService` exposes a `Cmd` sender to the bridge, but the return path is tied to concrete bridge methods. Do not describe a demo/production `WaClient` trait or `WaEvent` enum as existing code.
- `wa_map` is the mapping seam between protocol messages and stored messages. Preserve normalization for both incoming messages and history.
- The vault is the persistence and encryption seam; keep its record format and unlock requirements intact unless a migration is planned.
- Demo mode (`src/demo/`, behind the `demo` feature) uses invented data; consult [demo-mode.md](demo-mode.md) before testing it. Never use real chats, contacts or credentials as fixtures.
- Consult [privacy-and-security.md](privacy-and-security.md) before touching storage, network requests or logging.
