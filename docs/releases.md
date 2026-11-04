# Releases, versioning and changelog

Every push to `master` can produce a release. The version, the release
notes and the update shown inside the app all come from the commit history,
so **commit messages are the changelog**. The binding rules are in
[AGENTS.md](../AGENTS.md) and [CONTRIBUTING.md](../CONTRIBUTING.md); this
page explains how they are applied.

## Commit format

[Conventional Commits](https://www.conventionalcommits.org):
`type(scope): description`. The type and scope are in English and
lower-case. The description is in Portuguese, preferably in the
infinitive, with no final period.

```text
feat(updates): exibir novidades antes de baixar a atualização
fix(messages): preservar o texto quando o envio falhar
feat(storage)!: alterar o formato do histórico local
```

- **Atomic commits:** one logical change each, including its translations
  and tests, and each one must compile.
- **Clear descriptions:** the title, without its prefix, becomes the
  changelog line. Avoid vague titles such as "ajustes".
- **Body:** optional, for motivation and limits. For `feat`, `fix`,
  `perf` and `security`, its **first paragraph** appears in the notes as a
  sub-item (up to 400 characters), so write it for end users.
- **Breaking changes:** put `!` in the title **and** a `BREAKING CHANGE:`
  paragraph with the migration in the body.

## Types → changelog sections

| Type | Section in the notes | Version bump |
| --- | --- | --- |
| `feat` | Novidades (new features) | minor |
| `fix` | Correções (fixes) | patch |
| `perf`, `refactor` | Melhorias (improvements) | patch |
| `security` | Segurança (security) | patch |
| `chore` | hidden | **none** |
| `ci`, `build`, `test`, `docs`, `style` | hidden | patch (they still count) |
| any type with `!` | **Alterações importantes** (breaking changes, each with "Migração:", the migration) | **major** |
| unknown type / non-conventional | Outras alterações (other changes) | patch |

The section names are shown as the generator writes them, in Portuguese.

Don't label a feature as `chore` to hide it. Don't bump versions or create
`chore(release)` commits by hand: the pipeline does that.

## The pipeline (`.github/workflows/release.yml`)

1. **Version.** It takes the commit subjects since the last `v*` tag.
   `chore:` subjects are dropped, and if nothing is left, there is no
   release. Then: a `!` (or `BREAKING CHANGE` in a subject) → major, any
   `feat` → minor, anything else → patch. The bump only looks at
   **subjects**, which is why the `!` is mandatory.
2. **Build** on Windows (x86_64), Linux (x86_64) and macOS (aarch64) with
   `cargo build --release --locked`. `fail-fast` is off, so one broken
   platform doesn't discard the others.
   - Windows: the self-signed `zapive.exe`, then the Inno Setup installer
     (also signed).
   - Linux: the AppImage.
   - macOS: the `.app` bundle and DMG, with a self-signed codesign.
   - All: `zapive-<target>.zip`, the self-updater payload. On Windows it
     also contains the CTranslate2 DLLs.
3. **Release.** It runs the generator's unit tests, then
   `generate_changelog.py`, and publishes a GitHub release with the
   notes, every artifact and `SHA256SUMS.txt`.

## Release-notes generator (`.github/scripts/generate_changelog.py`)

Standard library only. It reads the git range (the previous `v*` tag up to
the target) and renders Markdown with **Portuguese headings**:

```text
## Alterações importantes          breaking changes, each with "Migração:" (migration)
## Novidades                       new features
## Melhorias                       improvements
## Correções                       fixes
## Segurança                       security
## Outras alterações               other changes
## Contribuidores                  contributors; bots excluded, GitHub handles from noreply e-mails
**Changelog completo**: full compare link
## Downloads e verificação (SHA-256)   downloads and verification; only with --assets-dir
```

- The **scope** becomes a bold label: `- **messages**: preservar o texto…`.
- Each line links to its commit and, when known, its PR. The PR number
  comes from the GitHub API or from a `(#123)` suffix in the title.
- Merge commits are skipped, and duplicate titles are merged (a breaking
  version wins).
- `@mentions` in commit text are neutralised, so release notes don't ping
  people.
- With `--assets-dir` it also writes `SHA256SUMS.txt` (`sha256sum` format,
  which the updater parses) and a download table with verification steps.

The in-app **What's new** view removes every section whose heading mentions
SHA-256 or checksums, so don't rename that heading without checking
`plain_release_notes` in `src/update.rs`.

Preview the notes locally:

```sh
python3 .github/scripts/generate_changelog.py --to-tag HEAD --output notes.md
python3 -m unittest discover -s .github/scripts -p 'test_*.py'
```

Options: `--repo`, `--from-tag`, `--to-tag`, `--repository owner/name`,
`--assets-dir`, `--checksums-output`, `--output`.

## Self-update

- Zapive checks GitHub Releases **10 seconds after start and then every
  hour**. **Settings → Updates → Check for updates** runs a check now.
- When a newer release exists, a **banner** offers to download it. The
  updater:
  1. downloads `zapive-<target>.zip`;
  2. checks it against the release's `SHA256SUMS.txt`, and refuses a zip
     that isn't listed or doesn't match;
  3. unpacks the binary next to the running one and swaps it with two
     renames in the same directory, so a full disk can't leave you without
     an executable. On Windows it also refreshes the CTranslate2 DLLs if
     they are missing or different.
- The new version runs after **Restart**. AppImage installs replace the
  `.AppImage` file itself. **Flatpak installs never self-update**; the
  store handles them.
- **What's new** lists the release history from the same notes, flagging
  releases newer than yours. It is cached in the vault for offline
  viewing.
- While the repository is private, set `ZAPIVE_GH_TOKEN` so the updater
  can reach the assets.

## Supported versions

Only the latest release receives fixes, security fixes included (see
[SECURITY.md](../SECURITY.md)).
