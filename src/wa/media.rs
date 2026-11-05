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
        _ => unreachable!("not a media command"),
    }
}
