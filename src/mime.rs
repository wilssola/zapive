// What a document is, decided from its name and, failing that, its first
// bytes. WhatsApp shows anything without a real mimetype as a "BIN" file,
// so documents must never go out as application/octet-stream when the
// type can be known.

pub const FALLBACK: &str = "application/octet-stream";

// Extension (lowercase, no dot) -> mimetype.
const BY_EXTENSION: &[(&str, &str)] = &[
    ("pdf", "application/pdf"),
    ("doc", "application/msword"),
    (
        "docx",
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
    ),
    ("xls", "application/vnd.ms-excel"),
    (
        "xlsx",
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
    ),
    ("ppt", "application/vnd.ms-powerpoint"),
    (
        "pptx",
        "application/vnd.openxmlformats-officedocument.presentationml.presentation",
    ),
    ("odt", "application/vnd.oasis.opendocument.text"),
    ("ods", "application/vnd.oasis.opendocument.spreadsheet"),
    ("odp", "application/vnd.oasis.opendocument.presentation"),
    ("rtf", "application/rtf"),
    ("txt", "text/plain"),
    ("csv", "text/csv"),
    ("md", "text/markdown"),
    ("html", "text/html"),
    ("htm", "text/html"),
    ("xml", "application/xml"),
    ("json", "application/json"),
    ("zip", "application/zip"),
    ("rar", "application/vnd.rar"),
    ("7z", "application/x-7z-compressed"),
    ("tar", "application/x-tar"),
    ("gz", "application/gzip"),
    ("apk", "application/vnd.android.package-archive"),
    ("epub", "application/epub+zip"),
    ("vcf", "text/vcard"),
    ("ics", "text/calendar"),
    ("jpg", "image/jpeg"),
    ("jpeg", "image/jpeg"),
    ("png", "image/png"),
    ("gif", "image/gif"),
    ("webp", "image/webp"),
    ("bmp", "image/bmp"),
    ("svg", "image/svg+xml"),
    ("heic", "image/heic"),
    ("mp3", "audio/mpeg"),
    ("m4a", "audio/mp4"),
    ("aac", "audio/aac"),
    ("wav", "audio/wav"),
    ("ogg", "audio/ogg"),
    ("opus", "audio/ogg; codecs=opus"),
    ("flac", "audio/flac"),
    ("mp4", "video/mp4"),
    ("m4v", "video/mp4"),
    ("mov", "video/quicktime"),
    ("mkv", "video/x-matroska"),
    ("webm", "video/webm"),
    ("avi", "video/x-msvideo"),
    ("3gp", "video/3gpp"),
];

pub fn from_extension(file_name: &str) -> Option<&'static str> {
    let ext = std::path::Path::new(file_name)
        .extension()?
        .to_str()?
        .to_ascii_lowercase();
    BY_EXTENSION
        .iter()
        .find(|(known, _)| *known == ext)
        .map(|(_, mime)| *mime)
}

// Magic numbers of the formats people actually send as documents.
pub fn sniff(data: &[u8]) -> Option<&'static str> {
    const SIGNATURES: &[(&[u8], &str)] = &[
        (b"\x89PNG\r\n\x1a\n", "image/png"),
        (b"\xff\xd8\xff", "image/jpeg"),
        (b"GIF8", "image/gif"),
        (b"PK\x03\x04", "application/zip"),
        (b"Rar!\x1a\x07", "application/vnd.rar"),
        (b"7z\xbc\xaf\x27\x1c", "application/x-7z-compressed"),
        (b"\xd0\xcf\x11\xe0\xa1\xb1\x1a\xe1", "application/msword"),
        (b"OggS", "audio/ogg"),
        (b"ID3", "audio/mpeg"),
    ];
    // PDF readers accept up to 1024 bytes of junk before the header.
    let head = &data[..data.len().min(1024)];
    if head.windows(5).any(|window| window == b"%PDF-") {
        return Some("application/pdf");
    }
    SIGNATURES
        .iter()
        .find(|(magic, _)| data.starts_with(magic))
        .map(|(_, mime)| *mime)
}

// The extension wins (it is what the user chose); a file with none, or one
// we do not know, is sniffed; only then is it opaque.
pub fn document_mimetype(file_name: &str, data: &[u8]) -> String {
    from_extension(file_name)
        .or_else(|| sniff(data))
        .unwrap_or(FALLBACK)
        .to_string()
}

// What a sent document message carries besides the bytes. Both the attach
// button and a dropped file build their message from this, so they cannot
// drift apart.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DocumentFields {
    pub mimetype: String,
    pub file_name: String,
    pub title: String,
}

pub fn document_fields(file_name: &str, data: &[u8]) -> DocumentFields {
    let title = std::path::Path::new(file_name)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .filter(|stem| !stem.is_empty())
        .unwrap_or(file_name)
        .to_string();
    DocumentFields {
        mimetype: document_mimetype(file_name, data),
        file_name: file_name.to_string(),
        title,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn common_document_extensions_map_to_real_types() {
        for (name, mime) in [
            ("PROPOSTA final.pdf", "application/pdf"),
            ("a.PDF", "application/pdf"),
            (
                "a.docx",
                "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
            ),
            (
                "a.xlsx",
                "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
            ),
            (
                "a.pptx",
                "application/vnd.openxmlformats-officedocument.presentationml.presentation",
            ),
            ("a.zip", "application/zip"),
            ("a.txt", "text/plain"),
            ("a.csv", "text/csv"),
            ("a.mp3", "audio/mpeg"),
            ("a.mp4", "video/mp4"),
        ] {
            assert_eq!(from_extension(name), Some(mime), "{name}");
        }
    }

    #[test]
    fn unknown_or_missing_extensions_are_not_guessed() {
        assert_eq!(from_extension("README"), None);
        assert_eq!(from_extension("a.unknownext"), None);
        assert_eq!(from_extension(".hidden"), None);
    }

    #[test]
    fn magic_bytes_identify_a_pdf_without_an_extension() {
        assert_eq!(sniff(b"%PDF-1.7\n%..."), Some("application/pdf"));
        assert_eq!(sniff(b"\n\n%PDF-1.4"), Some("application/pdf"));
        assert_eq!(sniff(b"PK\x03\x04rest"), Some("application/zip"));
        assert_eq!(sniff(b"plain text"), None);
        assert_eq!(sniff(b""), None);
    }

    #[test]
    fn extension_wins_then_sniffing_then_octet_stream() {
        assert_eq!(document_mimetype("a.pdf", b"not a pdf"), "application/pdf");
        assert_eq!(
            document_mimetype("PROPOSTA", b"%PDF-1.7"),
            "application/pdf"
        );
        assert_eq!(document_mimetype("a.weird", b"%PDF-1.7"), "application/pdf");
        assert_eq!(document_mimetype("a.weird", b"???"), FALLBACK);
        assert_eq!(document_mimetype("noext", b""), FALLBACK);
    }

    #[test]
    fn attach_and_drop_build_identical_document_fields() {
        // Both paths hand the same file name and bytes to document_fields.
        let bytes = b"%PDF-1.7 body";
        let attached = document_fields("PROPOSTA 2026.pdf", bytes);
        let dropped = document_fields("PROPOSTA 2026.pdf", bytes);
        assert_eq!(attached, dropped);
        assert_eq!(attached.mimetype, "application/pdf");
        assert_eq!(attached.file_name, "PROPOSTA 2026.pdf");
        assert_eq!(attached.title, "PROPOSTA 2026");
    }

    #[test]
    fn a_pdf_is_never_sent_as_a_binary_file() {
        assert_ne!(document_fields("x.pdf", b"").mimetype, FALLBACK);
        assert_ne!(document_fields("x", b"%PDF-1.5").mimetype, FALLBACK);
    }
}
