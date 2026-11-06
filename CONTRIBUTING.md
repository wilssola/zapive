# Contributing to Zapive

Thanks for wanting to help. This guide covers how to build, check and submit changes. By taking part, you agree to the [Code of Conduct](CODE_OF_CONDUCT.md). Security issues follow [SECURITY.md](SECURITY.md).

> **Golden rule:** never use real data in issues, tests, logs or screenshots, and never send real messages, reactions, statuses or calls to test something. Use demo mode.

## License

Zapive is open source under the [MIT License](LICENSE). By submitting a contribution (code, translations, documentation or assets), you agree that it is licensed under the MIT License as well, and you confirm that you have the right to contribute it.

## Building

Prerequisites: [Rust](https://rustup.rs) (`rust-toolchain.toml` selects the `stable` channel with `rustfmt` and `clippy`).

| Platform | Extra |
| --- | --- |
| Windows | MSVC Build Tools and `./scripts/fetch-ctranslate2.ps1` (downloads the official CTranslate2 DLL into `vendor/`) |
| Linux | `libasound2-dev libgtk-3-dev libxdo-dev cmake build-essential` |
| macOS | `cmake` (CTranslate2 is compiled from source) |

```sh
cargo build --locked          # development
cargo run --locked            # run the app
cargo build --release --locked
```

More details are in [docs/building.md](docs/building.md).

## Checks before opening a PR

```sh
cargo fmt --all -- --check    # formatting (see the note below)
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
python3 -m unittest discover -s .github/scripts -p 'test_*.py'   # release-notes generator
```

The CI `fmt and clippy` job is **blocking**: the code must pass `cargo fmt --all -- --check` and `cargo clippy --locked --all-targets -- -D warnings` (also with `--features demo`). Fix warnings at the source; use a local `#[allow]` only with a short reason.

CI (`.github/workflows/build.yml`) runs the build and tests on Linux, macOS and Windows, plus `cargo audit` and the release-notes generator tests.

## Demo mode

`zapive --demo` opens the app with invented data in a temporary data directory and **never connects to WhatsApp**. Build it with `cargo run --features demo -- --demo`. Use it to debug the UI and to take screenshots for issues and PRs without exposing real conversations. See [docs/demo-mode.md](docs/demo-mode.md).

## UI text and i18n

Every visible string goes through i18n with semantic ids, in `i18n/en_US/LC_MESSAGES/zapive.po` and `i18n/pt/LC_MESSAGES/zapive.po`. Don't hard-code text in `.slint` or `.rs` files.

## Code

- Identifiers and code comments are in English.
- Follow the style of the surrounding code, and avoid comments that only repeat what the code says.
- Check the existing dependencies before adding new ones, and justify each new dependency in the PR.

## Commits

The full convention is in [AGENTS.md](AGENTS.md). In short:

- Conventional Commits: `type(scope): description`. The **type** and **scope** are in lower-case English. Write the **description in English**, using a specific imperative phrase with no final period (for example `fix(messages): preserve unsent text`).
- `feat`, `fix`, `perf`, `refactor` and `security` appear in the changelog. `chore`, `ci`, `build`, `test`, `docs` and `style` are left out of the notes.
- Make one commit per logical change, keep each one compilable, and include its implementation, translations and tests.
- For breaking changes, put `!` in the title and a `BREAKING CHANGE: ...` paragraph in the body that explains the migration.
- Don't change versions or create `chore(release)` commits: the pipeline does that.

### How commits become release notes

Every push to `master` produces a release automatically (`.github/workflows/release.yml`). The better the commit, the better the note:

- The **scope** becomes a bold label (`**messages**: ...`).
- For `feat`, `fix`, `perf` and `security`, the **first paragraph of the body** appears as a sub-item. Write it for end users.
- The `BREAKING CHANGE:` text appears in the breaking-changes section, together with the migration.
- Links to the commit and the PR, the contributor list and the compare link are added automatically.
- Each release ships per-platform installers, `SHA256SUMS.txt` and verification instructions.

See [docs/releases.md](docs/releases.md) for the details. To preview the result locally:

```sh
python3 .github/scripts/generate_changelog.py --to-tag HEAD --output notes.md
```

## Pull requests

- Branch from `master`, keep the PR focused, and fill in the template.
- Describe what you tested and on which systems. Visual changes need screenshots, taken with `--demo`.
- Don't include secrets, `.env` files, certificates, `vault.db`, `wa.db` or unsanitised logs.
