# Demo mode

Demo mode runs the **real Zapive UI on invented data**: fictional people
and groups, generated artwork, and a conversation that shows every kind of
bubble. Use it to develop the interface, record bugs and take screenshots
**without exposing a real account**.

## Guarantees

When started with `--demo` (or any `--demo-*` flag), Zapive:

- redirects every per-user directory (`APPDATA`, `LOCALAPPDATA`,
  `XDG_DATA_HOME`, `XDG_CACHE_HOME`, `XDG_CONFIG_HOME`, and `HOME` outside
  Windows) to a scratch folder `zapive-demo-<pid>` in the system temp
  directory, **before anything reads a path**. If any data directory still
  points outside that folder, it refuses to run (exit code 2);
- **never** opens the real vault or cache, takes the single-instance lock,
  creates a WhatsApp client, opens a network connection, registers a tray
  icon, checks for updates, starts the MCP server or raises notifications;
- doesn't install the bridge, so no UI action can reach the WhatsApp
  service;
- deletes the scratch folder on exit.

So you can run it next to your real Zapive.

## Build

Demo mode is compiled only with the `demo` feature, so release builds
don't carry it:

```sh
cargo run --features demo -- --demo
```

Without the feature, `--demo` prints "this build has no demo mode;
rebuild with `--features demo`" and exits.

## Options

| Flag | Values | Default |
| --- | --- | --- |
| `--demo` | open the demo window | — |
| `--demo-page <page>` | `chat`, `settings`, `info`, `login`, `splash` | `chat` |
| `--demo-theme <theme>` | `dark`, `light` | `dark` |
| `--demo-style <style>` | `glass`, `opaque` | `glass` |
| `--demo-lang <lang>` | `en`, `pt` | `en` |
| `--demo-size <WxH>` | width 400–4000, height 300–3000 | `1440x900` |
| `--demo-group` | open a group chat instead of a direct chat (implied by `info`) | — |
| `--demo-shot <file.png>` | render once, save a PNG and exit | — |
| `--demo-shots-all <dir>` | render the full screenshot set into `<dir>` | — |

The scale factor is forced to 1 (`SLINT_SCALE_FACTOR=1`), so screenshots
are pixel-identical whatever your monitor scaling is.

## Regenerating the documentation screenshots

```sh
cargo run --features demo -- --demo-shots-all docs/screenshots
```

This runs one process per screenshot, so their states stay apart, and
writes:

| File | Page | Theme | Style |
| --- | --- | --- | --- |
| `chat-dark-glass.png` | chat | dark | glass |
| `chat-light-glass.png` | chat | light | glass |
| `chat-dark-opaque.png` | chat | dark | opaque |
| `chat-light-opaque.png` | chat | light | opaque |
| `settings.png` | settings | dark | glass |
| `info-panel.png` | info | dark | glass |
| `login.png` | login | dark | glass |
| `splash.png` | splash | dark | glass |

The README and these docs reference those names, so keep them stable.
`--demo-size` and `--demo-lang` also apply to the whole set.

## Screenshots for issues and PRs

Always use demo mode for screenshots you share. Never post captures of your
real chats, names or numbers. Example of a single capture:

```sh
cargo run --features demo -- --demo-page settings --demo-theme light --demo-shot settings-light.png
```
