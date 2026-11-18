# Agent instructions

Work in English: source code, identifiers, comments, documentation, issue and pull-request text, and commit messages. Portuguese belongs in app localization (`i18n/pt/LC_MESSAGES/zapive.po`); every user-visible string needs a semantic id in both English and Portuguese catalogs. Preserve existing product names and historical release artifacts when citing them.

## Read the relevant guide before changing code

- Architecture, runtime flow, and existing seams: [docs/architecture.md](docs/architecture.md).
- Module responsibilities and where a change belongs: [docs/modules.md](docs/modules.md).
- Design, privacy, tests, and contribution standards: [docs/project-conventions.md](docs/project-conventions.md).
- Importing or modifying icons: [docs/icons.md](docs/icons.md).
- Build commands and platform requirements: [docs/building.md](docs/building.md).
- Releases and changelog behavior: [docs/releases.md](docs/releases.md).

## CI, dependency updates, and validation

- Before diagnosing a CI failure, identify the exact workflow run, failing job and step, commit SHA, and branch or pull request. Read that run's diagnostics; do not assume a failed check means tests ran or that another run has the same cause. Distinguish code failures from runner availability and other infrastructure failures.
- Compare the local Rust and Clippy versions with those used by the failing job. The `stable` channel changes over time; a local pass on an older toolchain does not establish compatibility with the CI toolchain. Do not pin or change toolchains just to bypass a diagnostic.
- Inspect the relevant workflow and reproduce its feature flags and target coverage. For the current lint-and-test gate, run `cargo fmt --all -- --check`, `cargo clippy --locked --all-targets --features demo -- -D warnings`, and `cargo test --locked --features demo` (under `xvfb-run -a` on headless Linux). Run relevant script tests as well. Do not treat `cargo check` or a filtered test run as a substitute for the full gate.
- Treat deprecation warnings and Clippy diagnostics as release blockers when CI uses `-D warnings`. Migrate to supported APIs rather than adding warning suppressions, weakening CI, or downgrading dependencies to hide the problem.
- For dependency updates, inspect both the manifest requirement and the exact resolved version in `Cargo.lock`. Review migration guidance and affected call sites. Validate against the proposed dependency version, not only the current base branch; passing checks with the old version do not validate the update. Do not change branches or discard unrelated work to perform this validation.
- When changing cryptography, serialization, or persisted formats, preserve compatibility and add regression tests using existing-format fixtures or known vectors. Check malformed input handling; avoid introducing panics for invalid stored data.
- Before reporting a fix as validated, state which commands passed, which toolchain and dependency version were tested, and any checks that were blocked or not run. Do not claim remote CI, another platform, or release publication succeeded based only on local checks.

## Commits and publication

Use Conventional Commits: `type(scope): description`, all in English. Keep the type lowercase, the scope short, and the description specific and imperative, with no trailing period. Examples: `feat(updates): show release notes before download`, `fix(messages): preserve unsent text`, `docs(architecture): describe WhatsApp modules`.

Make one logical change per commit, including the translations and tests it needs; keep each commit buildable. `feat` is a feature, not a `chore`. Use `!` in the subject for breaking changes and explain the incompatibility and migration in a `BREAKING CHANGE:` body. Do not manually bump versions or create release commits; the pipeline owns releases. See [docs/releases.md](docs/releases.md) for type categories and versioning.

Only commit when asked and push only when explicitly asked. Before committing, review the diff and staged files: exclude unrelated work and secrets, tokens, credentials, and `.env` files. Run relevant checks and report failures or checks not run. Never force-push, bypass hooks, or amend someone else's commit.
