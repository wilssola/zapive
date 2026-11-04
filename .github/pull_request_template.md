## Why

What problem does this solve? Link the issue, if there is one (`Closes #123`).

## What changed

A summary of the change. If anything is incompatible, explain the migration.

## How it was verified

Commands you ran and platforms you tested. For UI changes, include screenshots taken with `zapive --demo` (never with real data).

- [ ] `cargo fmt --all -- --check`
- [ ] `cargo clippy --locked --all-targets -- -D warnings` (also with `--features demo`)
- [ ] `cargo test --locked`
- [ ] New strings go through i18n (`i18n/en_US` and `i18n/pt`, semantic ids)
- [ ] Commits follow `AGENTS.md` (`type(scope): description`, English type and Portuguese description; `!` and `BREAKING CHANGE:` if incompatible)
- [ ] No secrets, tokens, conversations, names or real numbers in the diff, logs or screenshots
