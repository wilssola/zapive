// Downloading and decrypting WhatsApp media.
use super::*;

pub(super) async fn run(ctx: &Ctx, cmd: Cmd) {
    match cmd {
        Cmd::Media {
            id,
            mimetype,
            message,
            want,
        } => {
            let client = ctx.client.clone();
            let key = ctx.media_key.clone();
            let sem = ctx.media_sem.clone();
            tokio::spawn(async move {
                let Some(path) =
                    crate::media::ensure_cached(&client, &key, &id, &mimetype, &message, &sem)
                        .await
                else {
                    ui_apply(move |b| b.on_media_missing(&id));
                    return;
                };
                match want {
                    MediaWant::File => ui_apply(move |b| b.on_media_file(&id, &path)),
                    MediaWant::Video => {
                        // Playable the moment the file is here; the
                        // poster follows when it is decoded.
                        {
                            let (id, path) = (id.clone(), path.clone());
                            ui_apply(move |b| b.on_media_file(&id, &path));
                        }
                        let poster = tokio::task::spawn_blocking(move || {
                            crate::media::temp_plain(&key, &path)
                                .and_then(|p| crate::video::poster(&p, crate::media::BUBBLE_PX))
                        })
                        .await
                        .ok()
                        .flatten();
                        if let Some(img) = poster {
                            ui_apply(move |b| b.on_video_poster(&id, img));
                        }
                    }
                    MediaWant::Image => {
                        // Bubble size only; the lightbox asks for a
                        // full decode when it opens.
                        let decoded = crate::media::read_cached(&key, &path)
                            .and_then(|d| crate::media::decode_bytes(&d, crate::media::BUBBLE_PX));
                        match decoded {
                            Some(img) => ui_apply(move |b| b.on_media_image(&id, &path, img)),
                            None => ui_apply(move |b| b.on_media_missing(&id)),
                        }
                    }
                    MediaWant::Sticker => {
                        let frames = crate::media::read_cached(&key, &path)
                            .map(|d| crate::media::sticker_frames(&d, 180, 24))
                            .unwrap_or_default();
                        ui_apply(move |b| b.on_media_sticker(&id, &path, frames));
                    }
                    MediaWant::Gif => {
                        let frames = tokio::task::spawn_blocking(move || {
                            crate::media::temp_plain(&key, &path)
                                .map(|p| crate::video::frames(&p, 320, 15.0, 45))
                                .map(|f| (path, f))
                        })
                        .await
                        .ok()
                        .flatten();
                        match frames {
                            Some((path, frames)) if !frames.is_empty() => {
                                ui_apply(move |b| b.on_media_gif(&id, &path, frames));
                            }
                            Some((path, _)) => ui_apply(move |b| b.on_media_file(&id, &path)),
                            None => ui_apply(move |b| b.on_media_missing(&id)),
                        }
                    }
                }
            });
        }
        Cmd::LinkThumb {
            id,
            direct_path,
            media_key: mk,
            sha256,
            enc_sha256,
            fallback,
        } => {
            let epoch = ctx.epoch;
            let client = ctx.client.clone();
            let key = ctx.media_key.clone();
            let sem = ctx.media_sem.clone();
            tokio::spawn(async move {
                let path = crate::media::link_thumb_path(&id);
                let mut bytes = {
                    let (key, path) = (key.clone(), path.clone());
                    tokio::task::spawn_blocking(move || crate::media::read_cached(&key, &path))
                        .await
                        .ok()
                        .flatten()
                };
                if bytes.is_none() {
                    let params = whatsapp_rust::download::DownloadParams::encrypted(
                        direct_path,
                        &mk,
                        &sha256,
                        &enc_sha256,
                        0,
                        whatsapp_rust::wacore::download::MediaType::LinkThumbnail,
                    );
                    crate::media::wait_for_backoff().await;
                    let mut attempt = {
                        let _permit = sem.acquire().await;
                        client.download(&params).await
                    };
                    if attempt
                        .as_ref()
                        .is_err_and(|e| crate::media::rate_limited(&e.to_string()))
                    {
                        crate::media::note_rate_limit();
                        crate::media::wait_for_backoff().await;
                        let _permit = sem.acquire().await;
                        attempt = client.download(&params).await;
                    }
                    match attempt {
                        Ok(data) => {
                            let sealed = key.encrypt_bytes(&data);
                            let _ = tokio::fs::write(&path, sealed).await;
                            bytes = Some(data);
                        }
                        Err(e) => eprintln!("[media] link thumbnail failed for {id}: {e}"),
                    }
                }
                let data = bytes.unwrap_or(fallback);
                let img = tokio::task::spawn_blocking(move || {
                    crate::media::decode_bytes(&data, crate::media::CARD_PX)
                })
                .await
                .ok()
                .flatten();
                match img {
                    Some(img) => {
                        crate::bridge::wa_apply_at(epoch, move |b| b.on_thumb(&id, true, img))
                    }
                    None => crate::bridge::wa_apply_at(epoch, move |b| b.on_media_missing(&id)),
                }
            });
        }
        _ => unreachable!("not a media command"),
    }
}
