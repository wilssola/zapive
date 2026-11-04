// Source-language detection. Script heuristics first (they are exact for
// scripts that identify one language), then lingua in low-accuracy mode with
// confidence and margin thresholds; anything doubtful is `source_required`
// so the user picks the language instead of getting a wrong translation.
use lingua::{LanguageDetector, LanguageDetectorBuilder};
use unicode_script::{Script, UnicodeScript};

const MIN_LETTERS: usize = 6;
const SHORT_LETTERS: usize = 12;

#[derive(Debug, PartialEq, Eq)]
pub(super) enum Verdict {
    Language(&'static str),
    Statistical { short: bool },
}

pub(super) fn heuristic(text: &str) -> Result<Verdict, String> {
    let letters: Vec<char> = text.chars().filter(|c| c.is_alphabetic()).collect();
    // Common/Inherited letters (the prolonged sound mark, for one) belong to
    // no script of their own and must not mask the script of their neighbours.
    let scripts: Vec<Script> = {
        let mut found: Vec<Script> = letters
            .iter()
            .map(|c| c.script())
            .filter(|s| !matches!(s, Script::Common | Script::Inherited))
            .collect();
        found.sort_by_key(|s| s.full_name());
        found.dedup();
        found
    };
    if letters.len() >= 2 {
        let kana = scripts
            .iter()
            .any(|s| matches!(s, Script::Hiragana | Script::Katakana));
        if kana
            && scripts
                .iter()
                .all(|s| matches!(s, Script::Hiragana | Script::Katakana | Script::Han))
        {
            return Ok(Verdict::Language("ja"));
        }
        for (script, code) in [
            (Script::Han, "zh"),
            (Script::Hangul, "ko"),
            (Script::Greek, "el"),
            (Script::Armenian, "hy"),
            (Script::Georgian, "ka"),
            (Script::Thai, "th"),
        ] {
            if scripts == [script] {
                return Ok(Verdict::Language(code));
            }
        }
    }
    let short = letters.len() < SHORT_LETTERS;
    if letters.len() < MIN_LETTERS || (short && words(text) < 2) {
        return Err("source_required".into());
    }
    Ok(Verdict::Statistical { short })
}

fn words(text: &str) -> usize {
    let mut count = 0;
    let mut inside = false;
    for c in text.chars() {
        let letter = c.is_alphabetic();
        if letter && !inside {
            count += 1;
        }
        inside = letter;
    }
    count
}

/// `scores` are the confidence values sorted from best to worst.
pub(super) fn accepted(scores: &[f64], short: bool) -> bool {
    let (confidence, margin) = if short { (0.9, 0.3) } else { (0.7, 0.2) };
    match scores {
        [] => false,
        [top] => *top >= confidence,
        [top, second, ..] => *top >= confidence && top - second >= margin,
    }
}

pub(super) fn build() -> LanguageDetector {
    LanguageDetectorBuilder::from_all_languages()
        .with_low_accuracy_mode()
        .build()
}

pub(super) fn detect(
    text: &str,
    detector: &mut Option<LanguageDetector>,
) -> Result<String, String> {
    let short = match heuristic(text)? {
        Verdict::Language(code) => return Ok(code.into()),
        Verdict::Statistical { short } => short,
    };
    let detector = detector.get_or_insert_with(build);
    let scores = detector.compute_language_confidence_values(text);
    let values: Vec<f64> = scores.iter().map(|(_, v)| *v).collect();
    if !accepted(&values, short) {
        return Err("source_required".into());
    }
    Ok(scores[0].0.iso_code_639_1().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn verdict(text: &str) -> Result<Verdict, String> {
        heuristic(text)
    }

    #[test]
    fn single_script_languages_are_recognised_without_statistics() {
        assert_eq!(verdict("日本語のテキストです"), Ok(Verdict::Language("ja")));
        assert_eq!(verdict("ありがとう"), Ok(Verdict::Language("ja")));
        assert_eq!(verdict("コーヒー"), Ok(Verdict::Language("ja")));
        assert_eq!(verdict("你好，世界"), Ok(Verdict::Language("zh")));
        assert_eq!(verdict("안녕하세요"), Ok(Verdict::Language("ko")));
        assert_eq!(verdict("Καλημέρα κόσμε"), Ok(Verdict::Language("el")));
        assert_eq!(verdict("สวัสดีครับ"), Ok(Verdict::Language("th")));
        assert_eq!(verdict("Բարեւ աշխարհ"), Ok(Verdict::Language("hy")));
        assert_eq!(verdict("გამარჯობა"), Ok(Verdict::Language("ka")));
    }

    #[test]
    fn mixed_scripts_are_never_guessed_from_one_of_them() {
        let mixed = verdict("hello 你好 world friends");
        assert_eq!(mixed, Ok(Verdict::Statistical { short: false }));
        assert_eq!(
            verdict("日本語 and Korean 한국어 mix"),
            Ok(Verdict::Statistical { short: false })
        );
    }

    #[test]
    fn short_or_wordless_input_asks_for_the_source() {
        for text in ["", "ok", "12345", "hello", "😀😀😀", "a b c"] {
            assert_eq!(verdict(text), Err("source_required".into()), "{text:?}");
        }
        assert_eq!(
            verdict("good morning"),
            Ok(Verdict::Statistical { short: true })
        );
        assert_eq!(
            verdict("good morning to everyone here"),
            Ok(Verdict::Statistical { short: false })
        );
    }

    #[test]
    fn thresholds_depend_on_length_and_margin() {
        assert!(!accepted(&[], false));
        assert!(accepted(&[0.7], false));
        assert!(!accepted(&[0.69], false));
        assert!(!accepted(&[0.89], true));
        assert!(accepted(&[0.9, 0.6], true));
        assert!(!accepted(&[0.9, 0.7], true));
        assert!(accepted(&[0.8, 0.6], false));
        assert!(!accepted(&[0.8, 0.7], false));
    }

    #[test]
    fn statistical_detection_on_clear_text() {
        let mut detector = None;
        let cases = [
            (
                "The weather is really nice today and we are going to the park.",
                "en",
            ),
            (
                "O tempo está muito bonito hoje e nós vamos ao parque passear.",
                "pt",
            ),
            (
                "Das Wetter ist heute wirklich schön und wir gehen in den Park.",
                "de",
            ),
        ];
        for (text, expected) in cases {
            assert_eq!(
                detect(text, &mut detector).as_deref(),
                Ok(expected),
                "{text}"
            );
        }
        assert_eq!(
            detect("日本語です", &mut detector).as_deref(),
            Ok("ja"),
            "heuristics must not need the statistical models"
        );
        assert_eq!(detect("hi", &mut detector), Err("source_required".into()));
    }
}
