# Local translation

Zapive translates messages **on your computer**. A
[CTranslate2](https://github.com/OpenNMT/CTranslate2) engine runs inside the
app on the CPU with the open
[Argos Translate](https://github.com/argosopentech/argos-translate)
OPUS-MT models. Once a model is installed, translating needs no network
and sends no text anywhere.

## Languages

You can translate from and to **English, Portuguese, Spanish, French,
German, Chinese, Japanese, Russian, Italian and Arabic**. The source can
be **auto**, which detects the language of each message with
[lingua](https://github.com/pemistahl/lingua-rs) trained on those same ten
languages.

When no direct model exists for a pair, Zapive plans a route through an
intermediate language, for example Portuguese → English → French. It
prefers a direct model first, then routes with fewer models still to
download, then English as the middle step.

## Set up

1. **Settings → Translation**.
2. **Start** the translation service. It runs in the background, with no
   window, and you can **Stop** it at any time to free memory.
3. Choose the **source language** (or *auto*) and the **target language**.
4. Under **Translation models**, press **Refresh** to load the catalog of
   language pairs, then **Install model** for the pairs you need. The
   progress is shown while the model downloads and unpacks.
5. Installed models are listed and can be removed.

Models are stored in `translation/models` in the data folder (see
[privacy-and-security.md](privacy-and-security.md#what-is-stored-where)).

## Use

The message menu offers:

| Action | Effect |
| --- | --- |
| **Translate message** | Shows the translation under the original ("Translated from …") |
| **Copy translation** | Copies the translated text |
| **Hide translation** | Hides it again |
| **Always translate from \<language\>** | Translates new messages in that language automatically as they arrive |
| **Never translate from \<language\>** | Stops automatic translation for that language and hides its automatic translations |

Only text, and the captions of photos and videos, can be translated, up to
16,000 characters. Your own messages are never translated automatically.
Deleted messages and stickers are skipped.

## Privacy

- **Translating:** fully local. The text goes to the engine thread in the
  same process and nowhere else.
- **Downloading models:** the catalog comes from the Argos package index on
  `raw.githubusercontent.com/argosopentech`, and models from
  `argos-net.com` or Argos GitHub releases. Any other host, and any
  redirect to a private address, is refused. The catalog is capped at 8 MB
  and a model at 512 MB.
- **Settings and per-language rules** are stored encrypted in the vault.
  Translations are kept in memory only.

## Platform notes

| OS | Engine |
| --- | --- |
| Windows | The official CTranslate2 4.8.4 DLL (`ctranslate2.dll` + `libiomp5md.dll`, next to `zapive.exe`), loaded lazily. If it is missing, only translation fails, with a readable error. |
| Linux, macOS | CTranslate2 is compiled from source and linked statically (CPU, `ruy` backend). |

## Troubleshooting

| Message | What to do |
| --- | --- |
| "Install a model for this language pair in Settings." | Install the pair, or a route through English. |
| "Choose the source language in Settings." | Auto-detection was unsure; pick the source language. |
| "The translation engine could not start." | Restart the service. On Windows, check that the two DLLs are next to `zapive.exe` (reinstall, or let the updater restore them). |
| "Translation timed out." | Long texts on slow CPUs can take a while (the limit is 180 s); restart the service. |
| "The model file is missing or invalid." | Remove the model and install it again. |
| Download blocked or too large | Refresh the catalog; update Zapive if the catalog changed hosts. |
