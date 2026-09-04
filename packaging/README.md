# Packaging

| Platform | Artifact | Built by |
| --- | --- | --- |
| Windows | `Zapive-Setup-<version>.exe` | Inno Setup, `windows/zapive.iss` |
| Linux | `Zapive-<version>-x86_64.AppImage` | `linux/make-appimage.sh` |
| macOS | `Zapive-<version>.dmg` (contains `Zapive.app`) | `macos/make-bundle.sh` |
| all | `zapive-<target>.zip` | release workflow — self-updater payload |

The zip names are load-bearing: the in-app updater matches on
`windows-x86_64` / `linux-x86_64` / `macos-aarch64`, so no other asset may
contain those strings.

Flatpak lives in `flatpak/` and is prepared but unpublished; see
`flatpak/README.md`.

## Windows signing

```pwsh
pwsh packaging/windows/make-self-signed-cert.ps1
```

Repository secrets: `WINDOWS_PFX_BASE64`, `WINDOWS_PFX_PASSWORD`.
`sign.ps1` signs both `zapive.exe` (before the installer is built, so the
installer and the updater zip both carry it) and the finished installer.
Verification reports `UnknownError` for a self-signed root, so the script
checks that a signer certificate was embedded instead of the status.

## macOS signing

```sh
bash packaging/macos/make-self-signed-cert.sh
```

Repository secrets: `MACOS_P12_BASE64`, `MACOS_P12_PASSWORD`. The workflow
imports the p12 into a temporary keychain and signs with the identity
`Zapive Self-Signed`. The updater payload binary is signed with
`--identifier io.github.wilssola.Zapive` so that swapping it into an
installed bundle keeps the bundle launchable.

Users still get the unidentified-developer prompt on first launch
(right-click -> Open, or System Settings -> Privacy & Security).

## Azure Artifact Signing

The commented-out steps in `release.yml` cover the real thing. Blocker as of
2026-09-03: Public Trust identity validation is geo-restricted — individuals
in the US/Canada only, organizations in the US, Canada, EU, UK, AU, NZ, JP,
KR, SG, CH, NO and IL. Brazil is not eligible.
