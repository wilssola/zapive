# Project conventions

These rules guide changes to the current code; they do not imply that all existing modules already follow them. For runtime flow see [architecture.md](architecture.md); for path ownership see [modules.md](modules.md). For build commands see [building.md](building.md).

## Design and testing

A **module** has an **interface** and an **implementation**. The interface includes types, ordering constraints, invariants, errors and runtime costs, not just callable names. Prefer **depth**: substantial behavior behind a small interface. Place a **seam** where behavior must vary, and add an **adapter** only when the alternative is real or the need is validated. Optimize **leverage** for callers and **locality** for maintainers: changes to one concept should not require editing unrelated files. Evaluate a proposed extraction with the deletion test: removing the old code should reduce what callers must understand. A seam with one adapter is hypothetical; two adapters demonstrate real variation. Test behavior through the module interface whenever possible, including failure and lifecycle cases, instead of exposing implementation details for tests.

Respect the existing UI-thread/tokio split and account epoch filtering described in [architecture.md](architecture.md). Keep network, file access, decoding and translation off the UI thread. Follow the style of surrounding code; check `Cargo.toml` before adding dependencies and explain new dependencies in the pull request. Put OS-specific implementation in `src/platform/`. Do not introduce a database, queue, trait or abstraction solely for anticipated use.

## Data and safety

Protect private message and account data: follow [privacy-and-security.md](privacy-and-security.md) for storage, encryption, logging and network behavior. Never use live chats, contact names or phone numbers as test fixtures. Do not send messages, reactions, statuses or calls to test a change. Use invented demo data where applicable ([demo-mode.md](demo-mode.md)). Preserve persisted formats unless you implement and test a migration.

## Language and assets

Write code, identifiers, comments, documentation, issue text, PR text and commit messages in English. App localization is the exception: add user-visible text with semantic ids to both `i18n/en_US/LC_MESSAGES/zapive.po` and `i18n/pt/LC_MESSAGES/zapive.po`. Do not hard-code UI text in Slint or Rust. Existing historical releases may retain their original language; do not silently rewrite them. Icon sourcing and placement are in [icons.md](icons.md).

## Checks and delivery

Use the relevant checks from [building.md](building.md) and report their actual results and any checks not run. Inspect the final diff and avoid unrelated files. Keep each logical change and its tests and translations together; follow [AGENTS.md](../AGENTS.md) for commit safety and [releases.md](releases.md) for versioning. Never publish a build, commit or push without authorization.
