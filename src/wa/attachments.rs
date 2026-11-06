// Encrypting and uploading an outgoing attachment.
use super::*;

pub(super) async fn prepare_attachment(
    client: &Arc<Client>,
    preparation: crate::outbox::Preparation,
) -> Result<wa::Message, String> {
    use whatsapp_rust::wacore::download::MediaType;
    use whatsapp_rust::waproto::buffa::MessageField;

    if parse_jid(&preparation.jid).is_none() {
        return Err("Invalid destination JID".into());
    }
    if preparation.id.is_empty()
        || preparation.id.len() > 128
        || !preparation.id.bytes().all(|c| c.is_ascii_alphanumeric())
    {
        return Err("Invalid preparation ID".into());
    }
    let media_type = match preparation.kind.as_str() {
        "image" => MediaType::Image,
        "document" => MediaType::Document,
        "audio" | "voice" => MediaType::Audio,
        "sticker" => MediaType::Sticker,
        "gif" => MediaType::Video,
        _ => return Err("Unsupported attachment kind".into()),
    };
    let kind = preparation.kind.clone();
    let filename = preparation.filename.clone();
    let converted = tokio::task::spawn_blocking(move || -> Result<_, String> {
        struct Temporary(std::path::PathBuf);
        impl Drop for Temporary {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let mut data = preparation.data;
        let mut thumb = None;
        let mut seconds = None;
        let mut waveform = None;
        match kind.as_str() {
            "image" => thumb = make_jpeg_thumb(&data),
            "sticker" => {
                data = crate::media::to_webp_sticker(&data)
                    .ok_or_else(|| "Sticker encode failed".to_string())?;
            }
            "audio" | "gif" | "voice" => {
                let dir = std::env::temp_dir().join(format!(
                    "zapive_attachment_{}_{}",
                    preparation.id,
                    crate::outbox::new_id()
                ));
                std::fs::create_dir(&dir).map_err(|e| e.to_string())?;
                let temporary = Temporary(dir);
                match kind.as_str() {
                    "audio" => {
                        let extension = std::path::Path::new(&filename)
                            .extension()
                            .and_then(|e| e.to_str())
                            .unwrap_or("");
                        let extension = if extension.bytes().all(|c| c.is_ascii_alphanumeric())
                            && extension.len() <= 16
                        {
                            extension
                        } else {
                            "audio"
                        };
                        let path = temporary.0.join(format!("source.{extension}"));
                        std::fs::write(&path, &data).map_err(|e| e.to_string())?;
                        seconds = crate::audio::probe_duration(&path);
                    }
                    "gif" => {
                        let url = std::str::from_utf8(&data).map_err(|e| e.to_string())?;
                        let mut response = ureq::get(url)
                            .header("User-Agent", "Zapive")
                            .call()
                            .map_err(|e| e.to_string())?;
                        let bytes = response
                            .body_mut()
                            .read_to_vec()
                            .map_err(|e| e.to_string())?;
                        let source = temporary.0.join("source.gif");
                        let output = temporary.0.join("output.mp4");
                        std::fs::write(&source, bytes).map_err(|e| e.to_string())?;
                        crate::video::gif_to_mp4(&source, &output)
                            .ok_or_else(|| "GIF conversion failed".to_string())?;
                        data = std::fs::read(&output).map_err(|e| e.to_string())?;
                    }
                    "voice" => {
                        if !data.len().is_multiple_of(4) || preparation.in_rate == 0 {
                            return Err("Invalid voice source".into());
                        }
                        let samples: Vec<f32> = data
                            .as_chunks::<4>()
                            .0
                            .iter()
                            .map(|bytes| {
                                f32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
                            })
                            .collect();
                        if samples.iter().any(|sample| !sample.is_finite()) {
                            return Err("Invalid voice samples".into());
                        }
                        waveform = Some(crate::audio::message_waveform(&samples));
                        let output = temporary.0.join("output.ogg");
                        seconds = Some(
                            crate::audio::encode_voice_ogg(&samples, preparation.in_rate, &output)
                                .ok_or_else(|| "Voice encode failed".to_string())?,
                        );
                        data = std::fs::read(&output).map_err(|e| e.to_string())?;
                    }
                    _ => unreachable!(),
                }
            }
            _ => {}
        }
        Ok((data, thumb, seconds, waveform))
    })
    .await
    .map_err(|e| e.to_string())??;
    let (data, thumb, seconds, waveform) = converted;
    let len = data.len() as u64;
    // Decided before the upload consumes the bytes: the name says what the
    // file is, and the first bytes settle it when the name does not.
    let document_fields = (preparation.kind == "document")
        .then(|| crate::mime::document_fields(&preparation.filename, &data));
    let upload = client
        .upload(data, media_type, Default::default())
        .await
        .map_err(|e| e.to_string())?;
    let mut message = match preparation.kind.as_str() {
        "image" => whatsapp_rust::media::image_message(
            upload,
            whatsapp_rust::media::ImageOptions {
                caption: preparation.caption.filter(|c| !c.is_empty()),
                jpeg_thumbnail: thumb,
                ..Default::default()
            },
        ),
        "document" => {
            let fields = document_fields
                .unwrap_or_else(|| crate::mime::document_fields(&preparation.filename, &[]));
            whatsapp_rust::media::document_message(
                upload,
                whatsapp_rust::media::DocumentOptions {
                    mimetype: Some(fields.mimetype),
                    file_name: Some(fields.file_name),
                    title: Some(fields.title),
                    ..Default::default()
                },
            )
        }
        "audio" | "voice" => {
            let mimetype = match std::path::Path::new(&preparation.filename)
                .extension()
                .and_then(|e| e.to_str())
            {
                Some("ogg") | Some("opus") => "audio/ogg; codecs=opus",
                Some("m4a") | Some("aac") => "audio/mp4",
                Some("wav") => "audio/wav",
                _ => "audio/mpeg",
            };
            whatsapp_rust::media::audio_message(
                upload,
                whatsapp_rust::media::AudioOptions {
                    ptt: Some(preparation.kind == "voice"),
                    duration_seconds: seconds,
                    waveform,
                    mimetype: (preparation.kind == "audio").then(|| mimetype.to_string()),
                    ..Default::default()
                },
            )
        }
        "gif" => whatsapp_rust::media::video_message(
            upload,
            whatsapp_rust::media::VideoOptions {
                gif_playback: Some(true),
                ..Default::default()
            },
        ),
        "sticker" => {
            let sticker = wa::message::StickerMessage {
                url: Some(upload.url),
                direct_path: Some(upload.direct_path),
                media_key: Some(upload.media_key.to_vec()),
                file_enc_sha256: Some(upload.file_enc_sha256.to_vec()),
                file_sha256: Some(upload.file_sha256.to_vec()),
                file_length: Some(len),
                mimetype: Some("image/webp".into()),
                width: Some(512),
                height: Some(512),
                media_key_timestamp: Some(upload.media_key_timestamp),
                ..Default::default()
            };
            wa::Message {
                sticker_message: MessageField::some(sticker),
                ..Default::default()
            }
        }
        _ => unreachable!(),
    };
    if preparation.view_once {
        message = wrap_view_once(message);
    }
    Ok(message)
}

pub(super) async fn run(ctx: &Ctx, cmd: Cmd) {
    let epoch = ctx.epoch;
    match cmd {
        Cmd::PrepareAttachment(preparation) => {
            let client = ctx.client.clone();
            tokio::spawn(async move {
                if SESSION_EPOCH.load(Ordering::SeqCst) != epoch {
                    return;
                }
                let id = preparation.id.clone();
                let result = prepare_attachment(&client, preparation).await;
                crate::bridge::wa_apply_at(epoch, move |b| {
                    b.on_attachment_prepared(&id, result);
                });
            });
        }
        Cmd::ResendMedia { jid, message } => {
            tokio::spawn(async move {
                use whatsapp_rust::proto_helpers::MessageExt as _;
                if parse_jid(&jid).is_none() {
                    action_failed(epoch, "Invalid destination JID");
                    return;
                };
                // A plain send, not forward_message: prepare_for_forward
                // always stamps `is_forwarded`, which is wrong here --
                // this is a fresh message that merely reuses an
                // already-uploaded CDN blob. Clearing context_info
                // strips it (and any stale quote/mention data) so
                // neither side sees it as forwarded.
                let mut message = (*message).clone();
                message.set_context_info(wa::ContextInfo::default());
                crate::bridge::wa_apply_at(epoch, move |b| {
                    b.enqueue_prepared(&jid, message, None);
                });
            });
        }

        Cmd::SendFile {
            jid,
            path,
            kind,
            origin,
            caption,
        } => {
            tokio::spawn(async move {
                let data = match tokio::fs::read(&path).await {
                    Ok(data) => data,
                    Err(e) => {
                        action_failed(epoch, format!("Failed to read media: {e}"));
                        return;
                    }
                };
                let preparation = crate::outbox::Preparation {
                    id: crate::outbox::new_id(),
                    jid,
                    kind: kind.as_str().into(),
                    filename: path
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_else(|| "file".into()),
                    caption,
                    origin: origin.or_else(|| Some(path.to_string_lossy().into_owned())),
                    data,
                    in_rate: 0,
                    view_once: false,
                };
                crate::bridge::wa_apply_at(epoch, move |b| {
                    b.stage_attachment(preparation);
                });
            });
        }

        Cmd::SendGifUrl { jid, url } => {
            let preparation = crate::outbox::Preparation {
                id: crate::outbox::new_id(),
                jid,
                kind: "gif".into(),
                filename: "animation.gif".into(),
                caption: None,
                origin: None,
                data: url.into_bytes(),
                in_rate: 0,
                view_once: false,
            };
            crate::bridge::wa_apply_at(epoch, move |b| {
                b.stage_attachment(preparation);
            });
        }

        _ => unreachable!("not a attachments command"),
    }
}
