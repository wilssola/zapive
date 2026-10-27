//! Emoji picker palette.
//!
//! Data from Unicode `emoji-test.txt` (latest version), © 2026 Unicode,
//! Inc. — https://www.unicode.org/terms_of_use.html. Filtered to
//! fully-qualified default sequences; skin-tone variants are excluded.

use crate::EmojiRow;
use slint::{ModelRc, SharedString, VecModel};

const DATA: &str = include_str!("../ui/emoji/emoji.txt");
const CELLS: usize = 8;

/// Canonical Unicode group name → i18n key, in canonical order.
static GROUP_KEYS: &[(&str, &str)] = &[
    ("Smileys & Emotion", "emoji.smileys"),
    ("People & Body", "emoji.people"),
    ("Animals & Nature", "emoji.animals"),
    ("Food & Drink", "emoji.food"),
    ("Travel & Places", "emoji.travel"),
    ("Activities", "emoji.activities"),
    ("Objects", "emoji.objects"),
    ("Symbols", "emoji.symbols"),
    ("Flags", "emoji.flags"),
];

/// Parse `DATA` into picker rows: section headers carry a translated label
/// and no cells; emoji lines are chunked into rows of at most `CELLS` cells.
fn rows() -> Vec<(String, Vec<SharedString>)> {
    let mut out: Vec<(String, Vec<SharedString>)> = Vec::new();
    for line in DATA.lines() {
        if line.is_empty() {
            continue;
        }
        if let Some(header) = line.strip_prefix("# ") {
            if let Some((_, key)) = GROUP_KEYS.iter().find(|(name, _)| *name == header) {
                out.push((crate::i18n::t(key), Vec::new()));
            }
            continue;
        }
        let cells: Vec<SharedString> = line.split_whitespace().map(SharedString::from).collect();
        for chunk in cells.chunks(CELLS) {
            out.push((String::new(), chunk.to_vec()));
        }
    }
    out
}

/// The picker model: one `EmojiRow` per header or chunk of `CELLS` cells.
pub fn palette_rows() -> ModelRc<EmojiRow> {
    let rows: Vec<EmojiRow> = rows()
        .into_iter()
        .map(|(label, cells)| EmojiRow {
            label: label.into(),
            cells: ModelRc::new(VecModel::from(cells)),
        })
        .collect();
    ModelRc::new(VecModel::from(rows))
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXPECTED_KEYS: &[&str] = &[
        "emoji.smileys",
        "emoji.people",
        "emoji.animals",
        "emoji.food",
        "emoji.travel",
        "emoji.activities",
        "emoji.objects",
        "emoji.symbols",
        "emoji.flags",
    ];

    #[test]
    fn sections_in_canonical_order() {
        let keys: Vec<&str> = GROUP_KEYS.iter().map(|(_, key)| *key).collect();
        assert_eq!(keys, EXPECTED_KEYS);
        let parsed: Vec<&str> = rows()
            .iter()
            .filter(|(label, _)| !label.is_empty())
            .map(|(label, _)| {
                GROUP_KEYS
                    .iter()
                    .map(|(_, key)| *key)
                    .find(|key| crate::i18n::t(key) == *label)
                    .expect("section label maps to a known key")
            })
            .collect();
        assert_eq!(parsed, EXPECTED_KEYS);
    }

    #[test]
    fn no_empty_section() {
        let rows = rows();
        let headers: Vec<usize> = rows
            .iter()
            .enumerate()
            .filter(|(_, (label, _))| !label.is_empty())
            .map(|(i, _)| i)
            .collect();
        assert_eq!(headers.len(), 9);
        for (k, &start) in headers.iter().enumerate() {
            let end = headers.get(k + 1).copied().unwrap_or(rows.len());
            assert!(start + 1 < end, "empty section at row {start}");
            assert!(
                rows[start + 1..end]
                    .iter()
                    .all(|(_, cells)| !cells.is_empty()),
                "empty data row in section at row {start}"
            );
        }
    }

    #[test]
    fn total_cell_count_in_range() {
        let total: usize = rows().iter().map(|(_, cells)| cells.len()).sum();
        assert!(
            (1500..2200).contains(&total),
            "total emoji cells out of range: {total}"
        );
    }

    #[test]
    fn every_cell_non_empty() {
        for (label, cells) in rows() {
            if label.is_empty() {
                assert!(!cells.is_empty(), "data row with no cells");
                assert!(cells.iter().all(|c| !c.is_empty()));
            }
        }
    }

    #[test]
    fn rows_within_cell_limit() {
        for (_, cells) in rows() {
            assert!(cells.len() <= CELLS);
        }
    }

    #[test]
    fn labels_translated() {
        for &(_, key) in GROUP_KEYS {
            let label = crate::i18n::t(key);
            assert!(!label.is_empty(), "missing translation for {key}");
            assert_ne!(label, key, "untranslated label for {key}");
        }
    }
}
