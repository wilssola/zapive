# Agent instructions

Work in English: source code, identifiers, comments, documentation, issue and pull-request text, and commit messages. Portuguese belongs in app localization (`i18n/pt/LC_MESSAGES/zapive.po`); every user-visible string needs a semantic id in both English and Portuguese catalogs. Preserve existing product names and historical release artifacts when citing them.

## Read the relevant guide before changing code

- Architecture, runtime flow, and existing seams: [docs/architecture.md](docs/architecture.md).
- Module responsibilities and where a change belongs: [docs/modules.md](docs/modules.md).
- Design, privacy, tests, and contribution standards: [docs/project-conventions.md](docs/project-conventions.md).
- Importing or modifying icons: [docs/icons.md](docs/icons.md).
- Build commands and platform requirements: [docs/building.md](docs/building.md).
- Releases and changelog behavior: [docs/releases.md](docs/releases.md).

## Commits and publication

Use Conventional Commits: `type(scope): description`, all in English. Keep the type lowercase, the scope short, and the description specific and imperative, with no trailing period. Examples: `feat(updates): show release notes before download`, `fix(messages): preserve unsent text`, `docs(architecture): describe WhatsApp modules`.

Make one logical change per commit, including the translations and tests it needs; keep each commit buildable. `feat` is a feature, not a `chore`. Use `!` in the subject for breaking changes and explain the incompatibility and migration in a `BREAKING CHANGE:` body. Do not manually bump versions or create release commits; the pipeline owns releases. See [docs/releases.md](docs/releases.md) for type categories and versioning.

Only commit when asked and push only when explicitly asked. Before committing, review the diff and staged files: exclude unrelated work and secrets, tokens, credentials, and `.env` files. Run relevant checks and report failures or checks not run. Never force-push, bypass hooks, or amend someone else's commit.
