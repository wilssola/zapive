// Small helpers shared by the executor and the pump.
use super::*;

// Where a jid's full-size picture can be fetched from. Channels answer
// through their metadata, groups through the group picture query when
// the contact query has nothing, people through the contact query.
// Ok(None) means "no picture"; Err means "ask again later".
pub(super) async fn picture_url(
    client: &Arc<Client>,
    jid: &whatsapp_rust::Jid,
) -> Result<Option<String>, ()> {
    if jid.to_string().ends_with("@newsletter") {
        return match client.newsletter().get_metadata(jid).await {
            Ok(meta) => Ok(meta.picture_url.clone().or(meta.preview_url.clone())),
            Err(_) => Err(()),
        };
    }
    let is_group = jid.to_string().ends_with("@g.us");
    match client.contacts().get_profile_picture(jid, false).await {
        Ok(Some(pic)) => return Ok(Some(pic.url.clone())),
        Ok(None) if !is_group => return Ok(None),
        Err(_) if !is_group => return Err(()),
        _ => {}
    }
    use whatsapp_rust::features::PictureType;
    match client
        .groups()
        .get_profile_pictures(vec![jid.clone()], PictureType::Image)
        .await
    {
        Ok(list) => Ok(list.into_iter().next().and_then(|p| p.url)),
        Err(_) => Err(()),
    }
}

pub(super) fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

pub(super) fn urlencode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

// Small jpeg preview embedded in outgoing image messages.
pub(super) fn make_jpeg_thumb(data: &[u8]) -> Option<Vec<u8>> {
    let img = image::load_from_memory(data).ok()?;
    let small = img
        .resize(72, 72, image::imageops::FilterType::Triangle)
        .to_rgb8();
    let mut out = Vec::new();
    let mut cursor = std::io::Cursor::new(&mut out);
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut cursor, 70)
        .encode_image(&small)
        .ok()?;
    Some(out)
}

// A view-once wrapper around any content message.
pub(super) fn wrap_view_once(inner: wa::Message) -> wa::Message {
    use whatsapp_rust::waproto::buffa::MessageField;
    let wrapper = wa::message::FutureProofMessage {
        message: MessageField::some(inner),
    };
    wa::Message {
        view_once_message_v2: MessageField::some(wrapper),
        ..Default::default()
    }
}

pub(super) fn format_pair_code(code: &str) -> String {
    if code.len() == 8 {
        format!("{}-{}", &code[..4], &code[4..])
    } else {
        code.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_encoding_uses_form_spaces_and_utf8_bytes() {
        assert_eq!(urlencode(""), "");
        assert_eq!(urlencode("Az09-_.~"), "Az09-_.~");
        assert_eq!(urlencode("a b+c/&"), "a+b%2Bc%2F%26");
        assert_eq!(urlencode("é"), "%C3%A9");
    }

    #[test]
    fn pairing_code_only_formats_eight_bytes() {
        assert_eq!(format_pair_code("12345678"), "1234-5678");
        assert_eq!(format_pair_code(""), "");
        assert_eq!(format_pair_code("1234567"), "1234567");
        assert_eq!(format_pair_code("123456789"), "123456789");
    }

    #[test]
    fn view_once_wrapper_preserves_inner_content() {
        let wrapped = wrap_view_once(wa::Message {
            conversation: Some("secret".into()),
            ..Default::default()
        });
        assert!(wrapped.conversation.is_none());
        let inner = wrapped
            .view_once_message_v2
            .as_option()
            .unwrap()
            .message
            .as_option()
            .unwrap();
        assert_eq!(inner.conversation.as_deref(), Some("secret"));
    }

    #[test]
    fn thumbnail_rejects_invalid_images() {
        assert!(make_jpeg_thumb(&[]).is_none());
        assert!(make_jpeg_thumb(b"not an image").is_none());
    }
}
