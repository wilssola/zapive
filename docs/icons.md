# Tabler icon assets

The bundled icon SVGs live in `ui/icons/`. [Tabler Icons](https://github.com/tabler/tabler-icons) are MIT-licensed; preserve the upstream copyright and license notice when importing assets (see the [Tabler Icons repository](https://github.com/tabler/tabler-icons) for its license and notices). The project currently identifies Tabler Icons under third-party components in [README.md](../README.md); do not treat that mention as a complete per-icon provenance record or a substitute for upstream notices. Some checked-in SVGs have been edited (for example, `ui/icons/heart-filled.svg` has a custom fill), so do not claim every file is an unmodified upstream export.

## Obtain and place an icon

1. Find the icon's canonical name in the [Tabler Icons catalog](https://tabler.io/icons); confirm whether the project already has an equivalent under `ui/icons/`.
2. Download the SVG from the upstream icon page or the corresponding file in the [Tabler Icons repository](https://github.com/tabler/tabler-icons). Prefer the project's existing 24×24 outline SVG style and keep the original icon name (for example `archive.svg`). If the design requires modifications, review the difference and preserve upstream copyright and license notices; do not mix unrelated icon sets without checking their licenses.
3. Place the asset in `ui/icons/<icon-name>.svg`. Refer to it from a Slint file in `ui/components/` using `@image-url("../icons/<icon-name>.svg")`. Check actual relative paths from other directories; do not copy SVG markup into multiple components.
4. Inspect the icon at the intended size in light/dark and glass/opaque themes; ensure the renderer handles its SVG features. Include new assets and their usage in the same change. Do not fetch assets at runtime or download arbitrary unreviewed files into the app's data directory.

This document records the asset workflow, not a claim that every icon is traceable to an exact upstream revision. For reproducible provenance of a specific imported or modified icon, record its upstream name and revision in the change description.
