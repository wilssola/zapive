# Flatpak / Flathub

Everything here is **prepared but not yet published**. The manifest builds the
app offline the way Flathub requires; the `flatpak` GitHub workflow
(`.github/workflows/flatpak.yml`, manual trigger) produces a `zapive.flatpak`
bundle you can install locally to test:

```sh
flatpak install --user zapive.flatpak
flatpak run io.github.wilssola.Zapive
```

## Files

- `io.github.wilssola.Zapive.yml` — the manifest (GNOME 48 runtime +
  rust-stable SDK extension, offline cargo build).
- `../linux/io.github.wilssola.Zapive.desktop` — desktop entry (shared with
  the AppImage).
- `../linux/io.github.wilssola.Zapive.metainfo.xml` — AppStream metainfo
  (shared with the AppImage).
- `cargo-sources.json` — **generated, not committed**: created from
  `Cargo.lock` by [flatpak-cargo-generator](https://github.com/flatpak/flatpak-builder-tools/tree/master/cargo)
  so cargo can resolve every crate without network access. The workflow
  regenerates it on every run; regenerate whenever `Cargo.lock` changes.

Self-update is disabled automatically inside Flatpak (the app checks
`FLATPAK_ID`); updates then come from the store, as Flathub mandates.

## Submitting to Flathub (when we decide to publish)

Follow <https://docs.flathub.org/docs/for-app-authors/submission>:

1. **Make the repo public** (or publish release tarballs): Flathub builders
   must be able to fetch the source. Then replace the `type: dir` source in
   the manifest with the release tag, e.g.:

   ```yaml
   sources:
     - type: git
       url: https://github.com/wilssola/zapive.git
       tag: v0.3.0
       commit: <full sha>
     - cargo-sources.json
   ```

2. **Metainfo**: add real screenshots to
   `io.github.wilssola.Zapive.metainfo.xml` (required for desktop apps) and
   keep the `<releases>` entry current. Validate with
   `flatpak run org.freedesktop.appstream-glib validate *.metainfo.xml`.

3. **Local check**: `flatpak-builder --user --install-deps-from=flathub
   --force-clean builddir io.github.wilssola.Zapive.yml` must succeed, and
   `flatpak run flathub org.flatpak.Builder --show-manifest` / the
   `flatpak-builder-lint` tool must pass:
   `flatpak run --command=flatpak-builder-lint org.flatpak.Builder manifest io.github.wilssola.Zapive.yml`.

4. **Open the PR**: fork <https://github.com/flathub/flathub>, create a branch
   off `new-pr` named `io.github.wilssola.Zapive`, add the manifest,
   `cargo-sources.json` and a `flathub.json` if needed, and open a PR against
   the `new-pr` branch. A reviewer will run the test build and comment.

5. After approval the app gets its own `flathub/io.github.wilssola.Zapive`
   repo; future updates are PRs there (or automated via flathub bot).

Note: `io.github.wilssola.*` is the correct app-id shape for a GitHub-hosted
project per Flathub's app-id requirements; changing it later is painful, so
keep it stable.
