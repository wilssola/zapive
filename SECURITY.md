# Security policy

Zapive stores WhatsApp conversations, media and session credentials on the user's computer, so security issues deserve careful handling.

## Supported versions

Only the **latest release** published on [Releases](https://github.com/wilssola/zapive/releases) receives security fixes. The app updates itself, so please confirm the problem on the latest version before reporting it.

## Reporting a vulnerability

**Do not open a public issue.** Use GitHub's private reporting:

1. Open <https://github.com/wilssola/zapive/security/advisories/new>.
2. Describe the impact, the steps to reproduce, the version and the operating system.
3. If you can, suggest a fix.

**Never send** real conversations, phone numbers, tokens, the contents of `vault.db`/`wa.db`, or keys. Use invented data: `zapive --demo` runs with sample data and never connects to WhatsApp.

## What to expect

- An acknowledgement within 7 days.
- An assessment and a fix plan within 30 days, depending on severity.
- Credit in the security advisory, if you want it.
- Coordinated disclosure: the advisory is published together with the release that fixes the problem.

## Scope

In scope: the local vault and on-disk encryption, the search index, the updater (SHA-256 verification), the local MCP server (token, listening only on `127.0.0.1`), handling of received media and links, the installers and the release pipeline.

Out of scope: flaws in WhatsApp itself or its servers, flaws in the operating system, and attacks that require physical access to a computer that is already unlocked.

See [docs/privacy-and-security.md](docs/privacy-and-security.md) for the threat model.

## Dependencies

CI runs `cargo audit` on every pull request and weekly, and Dependabot proposes updates to dependencies and actions.
