// In-process CTranslate2 engine for Argos packages: SentencePiece tokenization
// on both sides, paragraph/sentence chunking and beam search on the CPU.
use ct2rs::sys::{self, TranslationOptions};
use ct2rs::{ComputeType, Config, Device};
use sentencepiece::SentencePieceProcessor;
use std::path::Path;

const CHUNK_CHARS: usize = 400;
const BEAM_SIZE: usize = 4;
const MAX_BATCH: usize = 32;
const MAX_DECODING: usize = 1024;

pub(super) struct Engine {
    translator: sys::Translator,
    sentencepiece: SentencePieceProcessor,
    target_prefix: String,
}

// Windows links ctranslate2.dll lazily (see build.rs). Loading it here, by
// absolute path from the executable's directory, means a missing or broken
// DLL becomes an error the user can read, and the delay-load helper later
// finds the module already in the process instead of raising an exception.
// The handle is kept for the life of the process on purpose.
#[cfg(windows)]
fn runtime_ready() -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt as _;
    use std::sync::OnceLock;
    use windows::Win32::System::LibraryLoader::{
        LOAD_LIBRARY_SEARCH_DEFAULT_DIRS, LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR, LoadLibraryExW,
    };
    static READY: OnceLock<Result<(), String>> = OnceLock::new();
    READY
        .get_or_init(|| {
            let exe = std::env::current_exe().map_err(|_| "runtime_missing")?;
            let dir = exe.parent().ok_or("runtime_missing")?;
            for name in RUNTIME_DLLS {
                let path = dir.join(name);
                if !path.is_file() {
                    log::warn!("[translation] {name} is missing next to the executable");
                    return Err("runtime_missing".into());
                }
                if *name != RUNTIME_DLLS[0] {
                    continue;
                }
                let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
                let flags = LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_DEFAULT_DIRS;
                if let Err(error) = unsafe {
                    LoadLibraryExW(windows::core::PCWSTR(wide.as_ptr()), None, flags)
                } {
                    log::warn!("[translation] {name} could not be loaded: {error}");
                    return Err("runtime_missing".into());
                }
            }
            Ok(())
        })
        .clone()
}

#[cfg(not(windows))]
fn runtime_ready() -> Result<(), String> {
    Ok(())
}

/// The DLLs the Windows build needs next to the executable, the one the
/// executable imports first.
#[cfg(windows)]
const RUNTIME_DLLS: &[&str] = &["ctranslate2.dll", "libiomp5md.dll"];

impl Engine {
    pub(super) fn load(dir: &Path, target_prefix: String) -> Result<Self, String> {
        runtime_ready()?;
        let spm = dir.join("sentencepiece.model");
        // Read here rather than via `open`: the C++ side takes a narrow path
        // that breaks on Windows profiles with non-ASCII names.
        let bytes = std::fs::read(&spm).map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                "model_unsupported"
            } else {
                "invalid_model_file"
            }
        })?;
        let sentencepiece = SentencePieceProcessor::from_serialized_proto(&bytes)
            .map_err(|_| "invalid_model_file")?;
        let config = Config {
            device: Device::CPU,
            compute_type: ComputeType::INT8,
            num_threads_per_replica: 2,
            ..Default::default()
        };
        let translator = sys::Translator::new(dir.join("model"), &config).map_err(|error| {
            log::warn!("[translation] model load failed: {error}");
            "invalid_model_file"
        })?;
        Ok(Self {
            translator,
            sentencepiece,
            target_prefix,
        })
    }

    pub(super) fn translate(
        &self,
        text: &str,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<String, String> {
        let mut paragraphs = Vec::new();
        for paragraph in text.split('\n') {
            check()?;
            if paragraph.trim().is_empty() {
                paragraphs.push(paragraph.to_string());
                continue;
            }
            let chunks = chunks(paragraph);
            let translated = self.translate_chunks(&chunks)?;
            paragraphs.push(translated.join(" "));
        }
        Ok(paragraphs.join("\n"))
    }

    fn translate_chunks(&self, chunks: &[&str]) -> Result<Vec<String>, String> {
        if chunks.is_empty() {
            return Ok(Vec::new());
        }
        let tokenized = chunks
            .iter()
            .map(|chunk| {
                self.sentencepiece
                    .encode(chunk)
                    .map(|pieces| pieces.into_iter().map(|p| p.piece).collect::<Vec<_>>())
                    .map_err(|_| "runtime_failed".to_string())
            })
            .collect::<Result<Vec<_>, _>>()?;
        let options = TranslationOptions {
            beam_size: BEAM_SIZE,
            replace_unknowns: true,
            max_batch_size: MAX_BATCH,
            max_input_length: 0,
            max_decoding_length: MAX_DECODING,
            ..Default::default()
        };
        let results = if self.target_prefix.is_empty() {
            self.translator.translate_batch(&tokenized, &options, None)
        } else {
            let prefixes = vec![vec![self.target_prefix.clone()]; tokenized.len()];
            self.translator
                .translate_batch_with_target_prefix(&tokenized, &prefixes, &options, None)
        }
        .map_err(|error| {
            log::warn!("[translation] inference failed: {error}");
            "runtime_failed".to_string()
        })?;
        if results.len() != chunks.len() {
            return Err("runtime_failed".into());
        }
        results
            .into_iter()
            .map(|result| {
                let tokens = result.hypotheses.into_iter().next().unwrap_or_default();
                let decoded = self
                    .sentencepiece
                    .decode_pieces(&tokens)
                    .map_err(|_| "runtime_failed".to_string())?;
                Ok(strip_prefix(&decoded, &self.target_prefix).trim().to_string())
            })
            .collect()
    }
}

fn strip_prefix<'a>(value: &'a str, prefix: &str) -> &'a str {
    if prefix.is_empty() {
        value
    } else {
        value.strip_prefix(prefix).unwrap_or(value)
    }
}

/// Splits after sentence-ending punctuation followed by whitespace; the
/// equivalent of the look-behind split `(?<=[.!?。！？])\s+`. The whitespace
/// run is dropped and empty pieces are kept, like `re.split`.
pub(super) fn split_sentences(paragraph: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut start = 0;
    let mut previous: Option<char> = None;
    let mut chars = paragraph.char_indices().peekable();
    while let Some((index, c)) = chars.next() {
        if c.is_whitespace() && previous.is_some_and(|p| ".!?。！？".contains(p)) {
            let mut end = index + c.len_utf8();
            while let Some(&(next, n)) = chars.peek() {
                if !n.is_whitespace() {
                    break;
                }
                end = next + n.len_utf8();
                chars.next();
            }
            parts.push(&paragraph[start..index]);
            start = end;
            previous = None;
            continue;
        }
        previous = Some(c);
    }
    parts.push(&paragraph[start..]);
    parts
}

/// Sentences cut into pieces of at most `CHUNK_CHARS` characters.
pub(super) fn chunks(paragraph: &str) -> Vec<&str> {
    chunk_by(paragraph, CHUNK_CHARS)
}

fn chunk_by(paragraph: &str, size: usize) -> Vec<&str> {
    let mut out = Vec::new();
    for sentence in split_sentences(paragraph) {
        let mut start = 0;
        let mut count = 0;
        for (index, _) in sentence.char_indices() {
            if count == size {
                out.push(&sentence[start..index]);
                start = index;
                count = 0;
            }
            count += 1;
        }
        if start < sentence.len() {
            out.push(&sentence[start..]);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sentences_split_after_terminal_punctuation_and_whitespace() {
        assert_eq!(
            split_sentences("Hello world. How are you?  Fine! Ok"),
            ["Hello world.", "How are you?", "Fine!", "Ok"]
        );
        assert_eq!(split_sentences("no punctuation here"), ["no punctuation here"]);
        assert_eq!(split_sentences("3.14 is pi"), ["3.14 is pi"]);
        assert_eq!(split_sentences("a.b c"), ["a.b c"]);
        assert_eq!(split_sentences("Wait... what?"), ["Wait...", "what?"]);
        assert_eq!(
            split_sentences("こんにちは。 元気ですか？ はい"),
            ["こんにちは。", "元気ですか？", "はい"]
        );
        assert_eq!(split_sentences("Mr. Smith\tleft.\u{a0}Bye"), ["Mr.", "Smith\tleft.", "Bye"]);
    }

    #[test]
    fn splitting_keeps_edge_pieces_like_re_split() {
        assert_eq!(split_sentences("Hello. "), ["Hello.", ""]);
        assert_eq!(split_sentences(" Hello."), [" Hello."]);
        assert_eq!(split_sentences(""), [""]);
        assert_eq!(split_sentences("a. b. c."), ["a.", "b.", "c."]);
    }

    #[test]
    fn chunks_cut_long_sentences_by_characters_and_skip_empty_pieces() {
        assert!(chunks("").is_empty());
        assert_eq!(chunks("Hello. "), ["Hello."]);
        assert_eq!(chunk_by("abcdefg", 3), ["abc", "def", "g"]);
        assert_eq!(chunk_by("abcdef", 3), ["abc", "def"]);
        assert_eq!(chunk_by("ab. cdefgh", 3), ["ab.", "cde", "fgh"]);
        let long = "é".repeat(CHUNK_CHARS * 2 + 1);
        let pieces = chunks(&long);
        assert_eq!(pieces.len(), 3);
        assert!(pieces[..2].iter().all(|p| p.chars().count() == CHUNK_CHARS));
        assert_eq!(pieces[2].chars().count(), 1);
        assert_eq!(pieces.concat(), long);
    }

    #[test]
    fn target_prefix_is_removed_only_when_present() {
        assert_eq!(strip_prefix("__pt__ olá", "__pt__"), " olá");
        assert_eq!(strip_prefix("olá", "__pt__"), "olá");
        assert_eq!(strip_prefix("olá", ""), "olá");
    }

    #[test]
    fn missing_tokenizer_is_unsupported_not_a_panic() {
        let dir = std::env::temp_dir().join(format!(
            "zapive-engine-{}-{}",
            std::process::id(),
            super::super::models::random_hex()
        ));
        std::fs::create_dir_all(dir.join("model")).unwrap();
        assert_eq!(
            Engine::load(&dir, String::new()).err().as_deref(),
            Some("model_unsupported")
        );
        std::fs::write(dir.join("sentencepiece.model"), b"not a model").unwrap();
        assert_eq!(
            Engine::load(&dir, String::new()).err().as_deref(),
            Some("invalid_model_file")
        );
        let _ = std::fs::remove_dir_all(dir);
    }
}
