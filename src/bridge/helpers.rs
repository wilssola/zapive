// Pure helpers shared by the bridge modules.
use super::*;

pub(super) fn image_of(d: &crate::media::Decoded) -> slint::Image {
    let mut buf = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::new(d.w, d.h);
    buf.make_mut_bytes().copy_from_slice(&d.rgba);
    slint::Image::from_rgba8(buf)
}

// A blurhash-like stand-in for a privacy-blurred face: the picture shrunk to
// a few pixels keeps its dominant colours (so contacts stay tellable apart)
// but no detail. Scaled back up with smooth sampling it reads as a soft
// blur, computed once per avatar instead of a backdrop blur every frame.
pub(super) fn privacy_blurred_of(d: &crate::media::Decoded) -> slint::Image {
    const GRID: u32 = 6;
    const OUT: u32 = 48;
    let Some(src) = image::RgbaImage::from_raw(d.w, d.h, d.rgba.clone()) else {
        return image_of(d);
    };
    let tiny = image::imageops::resize(&src, GRID, GRID, image::imageops::FilterType::Triangle);
    let soft = image::imageops::resize(&tiny, OUT, OUT, image::imageops::FilterType::CatmullRom);
    let mut buf = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::new(OUT, OUT);
    buf.make_mut_bytes().copy_from_slice(soft.as_raw());
    slint::Image::from_rgba8(buf)
}

pub(super) fn card_thumbnail(bytes: &[u8]) -> Option<slint::Image> {
    if bytes.len() > 2 * 1024 * 1024 {
        return None;
    }
    let mut reader = image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .ok()?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(2048);
    limits.max_image_height = Some(2048);
    limits.max_alloc = Some(16 * 1024 * 1024);
    reader.limits(limits);
    let rgba = reader.decode().ok()?.thumbnail(480, 320).to_rgba8();
    let buffer = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::clone_from_slice(
        rgba.as_raw(),
        rgba.width(),
        rgba.height(),
    );
    Some(slint::Image::from_rgba8(buffer))
}

pub(super) fn image_bytes(image: &slint::Image) -> usize {
    let size = image.size();
    size.width as usize * size.height as usize * 4
}

// A video's box when only its thumbnail is known: the thumbnail's shape
// at the size a clip gets, scaled up as well as down.
pub(super) fn poster_box(w: i32, h: i32) -> (i32, i32) {
    let (w, h) = (w.max(1) as f64, h.max(1) as f64);
    let scale = (330.0 / w).min(380.0 / h);
    (
        ((w * scale).round() as i32).max(1),
        ((h * scale).round() as i32).max(1),
    )
}

pub(super) fn bubble_fit(w: i32, h: i32) -> (i32, i32) {
    let scale = (330.0 / w as f64).min(380.0 / h as f64).min(1.0);
    (
        ((w as f64 * scale).round() as i32).max(1),
        ((h as f64 * scale).round() as i32).max(1),
    )
}

pub(super) fn quote_chat(m: &StoredMessage) -> &str {
    if m.quote_jid.is_empty() {
        &m.jid
    } else {
        &m.quote_jid
    }
}

pub(super) fn message_link(m: &StoredMessage) -> String {
    if !m.link_url.is_empty() {
        return m.link_url.clone();
    }
    crate::link_preview::first_url(&m.text)
}

pub(super) fn document_type(m: &StoredMessage) -> String {
    if m.kind != MessageKind::Doc {
        return String::new();
    }
    if let Some((_, ext)) = m.text.trim().rsplit_once('.')
        && !ext.is_empty()
        && ext.len() <= 10
        && ext.bytes().all(|b| b.is_ascii_alphanumeric())
    {
        return ext.to_ascii_uppercase();
    }
    match m.mimetype.split(';').next().unwrap_or_default().trim() {
        "application/pdf" => "PDF",
        "application/vnd.ms-excel" => "XLS",
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet" => "XLSX",
        "application/msword" => "DOC",
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document" => "DOCX",
        "application/vnd.ms-powerpoint" => "PPT",
        "application/vnd.openxmlformats-officedocument.presentationml.presentation" => "PPTX",
        "text/plain" => "TXT",
        "text/csv" => "CSV",
        "application/zip" => "ZIP",
        "application/x-7z-compressed" => "7Z",
        "application/vnd.rar" | "application/x-rar-compressed" => "RAR",
        _ => "FILE",
    }
    .into()
}

pub(super) fn message_editable(m: &StoredMessage) -> bool {
    m.from_me
        && !m.deleted
        && !m.revoked
        && m.kind == MessageKind::Text
        && !is_channel(&m.jid)
        && m.raw.is_some()
        && (0..900).contains(&(now_secs() - m.timestamp))
}

pub(super) fn needs_media(m: &StoredMessage) -> bool {
    !m.deleted
        && (m.kind != MessageKind::Text && m.kind != MessageKind::System
            || !m.link_title.is_empty()
            || !m.link_url.is_empty()
            || (m.kind == MessageKind::Text && !crate::link_preview::first_url(&m.text).is_empty()))
}

// What a message becomes in a copy or an export: its text, or a
// placeholder for media the way WhatsApp writes "<Media omitted>".
pub(super) fn copy_body(m: &StoredMessage) -> String {
    if m.deleted {
        return t("msg.deleted");
    }
    match m.kind {
        MessageKind::Text | MessageKind::System => m.text.clone(),
        MessageKind::Image if m.sticker => t("preview.sticker"),
        MessageKind::Image | MessageKind::Video if !m.text.is_empty() => {
            format!("{} {}", t("export.media_omitted"), m.text)
        }
        MessageKind::Doc => format!("{} ({})", t("export.media_omitted"), m.text),
        _ => t("export.media_omitted"),
    }
}

// Scales media dimensions into the bubble's 330x380 box (stickers use a
// smaller square), falling back to a sane default when unknown.
pub(super) fn media_box(m: &StoredMessage) -> (i32, i32) {
    if m.kind != MessageKind::Image && m.kind != MessageKind::Video {
        return (0, 0);
    }
    let (max_w, max_h) = if m.sticker {
        (180.0, 180.0)
    } else {
        (330.0, 380.0)
    };
    let w = if m.media_w > 0 {
        m.media_w as f64
    } else {
        260.0
    };
    let h = if m.media_h > 0 {
        m.media_h as f64
    } else {
        200.0
    };
    let scale = (max_w / w).min(max_h / h).min(1.0);
    (
        ((w * scale).round() as i32).max(1),
        ((h * scale).round() as i32).max(1),
    )
}

// The content hash that identifies a sticker across messages.
pub(super) fn sticker_key(m: &StoredMessage) -> Option<String> {
    crate::media::sticker_key(m.raw.as_deref()?)
}

// A file name safe for the temp directory and the save dialog.
pub(super) fn sanitize_file_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') {
                '_'
            } else {
                c
            }
        })
        .collect();
    if cleaned.trim().is_empty() {
        "document".into()
    } else {
        cleaned.trim().to_string()
    }
}

pub(super) fn host_of(url: &str) -> String {
    if url.is_empty() {
        return String::new();
    }
    url::Url::parse(url)
        .ok()
        .and_then(|u| {
            u.host_str()
                .map(|h| h.trim_start_matches("www.").to_string())
        })
        .unwrap_or_default()
}

pub(super) fn initial_of(name: &str) -> String {
    name.trim()
        .chars()
        .next()
        .map(|c| c.to_uppercase().to_string())
        .unwrap_or_else(|| "?".to_string())
}

pub(super) fn color_idx_of(jid: &str) -> i32 {
    let mut h: i32 = 0;
    for b in jid.encode_utf16() {
        h = h.wrapping_add(b as i32);
    }
    h.abs() % 8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn document_types_use_extension_and_mime_fallback() {
        let mut m =
            crate::wa_map::system_message("peer@s.whatsapp.net", "id", 1, "report.Xlsx".into());
        m.kind = MessageKind::Doc;
        assert_eq!(document_type(&m), "XLSX");
        m.text = "report".into();
        m.mimetype = "application/pdf".into();
        assert_eq!(document_type(&m), "PDF");
        m.text = "report.invalid/ext".into();
        assert_eq!(document_type(&m), "PDF");
        m.mimetype.clear();
        assert_eq!(document_type(&m), "FILE");
        m.kind = MessageKind::Text;
        assert!(document_type(&m).is_empty());
    }

    #[test]
    fn quotes_navigate_to_explicit_chat_not_author() {
        let mut m = crate::wa_map::system_message("peer@s.whatsapp.net", "id", 1, String::new());
        m.quote_author = "author@s.whatsapp.net".into();
        assert_eq!(quote_chat(&m), "peer@s.whatsapp.net");
        m.quote_jid = "source@g.us".into();
        assert_eq!(quote_chat(&m), "source@g.us");
        m.quote_jid = m.jid.clone();
        assert_eq!(quote_chat(&m), "peer@s.whatsapp.net");
    }

    #[test]
    fn quote_context_round_trips_source_and_ordinary_replies() {
        use whatsapp_rust::proto_helpers::MessageBuilderExt as _;
        let mut store = Store::default();
        let sender = "author@s.whatsapp.net".parse().unwrap();
        let target = "peer@s.whatsapp.net".parse().unwrap();
        let meta = crate::wa_map::LiveMeta {
            chat: "peer@s.whatsapp.net".into(),
            sender: "author@s.whatsapp.net".into(),
            sender_alt: None,
            recipient_alt: None,
            id: "reply",
            from_me: false,
            push_name: "",
            timestamp: 1,
        };
        for source in ["source@g.us", "peer@s.whatsapp.net"] {
            let ctx = whatsapp_rust::proto_helpers::build_quote_context_with_info(
                "original",
                &sender,
                &source.parse().unwrap(),
                &target,
                &wa::Message::text("quoted"),
            );
            assert_eq!(
                ctx.remote_jid.as_deref(),
                (source != meta.chat).then_some(source)
            );
            let message = wa::Message::text_with_context("reply", ctx);
            let stored =
                crate::wa_map::normalize(&mut store, &meta, &message, &HashMap::new(), 0).unwrap();
            assert_eq!(stored.quote_id, "original");
            assert_eq!(stored.quote_text, "quoted");
            assert_eq!(stored.quote_author, "author@s.whatsapp.net");
            assert_eq!(quote_chat(&stored), source);
            let mut json = serde_json::to_value(&stored).unwrap();
            let restored: StoredMessage = serde_json::from_value(json.clone()).unwrap();
            assert_eq!(restored.quote_jid, stored.quote_jid);
            json.as_object_mut().unwrap().remove("quote_jid");
            let legacy: StoredMessage = serde_json::from_value(json).unwrap();
            assert!(legacy.quote_jid.is_empty());
            assert_eq!(quote_chat(&legacy), meta.chat);
        }
    }

    #[test]
    fn edit_requires_own_recent_live_text() {
        let mut m =
            crate::wa_map::system_message("a@s.whatsapp.net", "id", now_secs(), "hello".into());
        m.kind = MessageKind::Text;
        m.from_me = true;
        m.raw = Some(std::sync::Arc::new(wa::Message::default()));
        assert!(message_editable(&m));
        m.timestamp -= 901;
        assert!(!message_editable(&m));
        m.timestamp = now_secs();
        m.from_me = false;
        assert!(!message_editable(&m));
        m.from_me = true;
        m.revoked = true;
        assert!(!message_editable(&m));
    }

    #[test]
    fn links_can_be_copied_without_a_preview() {
        let mut m = crate::wa_map::system_message(
            "a@s.whatsapp.net",
            "id",
            1,
            "See https://example.com/page.".into(),
        );
        assert_eq!(message_link(&m), "https://example.com/page");
        m.text = "no link".into();
        assert!(message_link(&m).is_empty());
    }
}
