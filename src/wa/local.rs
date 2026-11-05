// Work that only borrows the runtime: decoding, link previews, GIF search, updates, PDF engine.
use super::*;

pub(super) async fn run(ctx: &Ctx, cmd: Cmd) {
    let epoch = ctx.epoch;
    match cmd {
        Cmd::DecodePreview { id, bytes } => {
            tokio::spawn(async move {
                let img = tokio::task::spawn_blocking(move || {
                    let image = image::load_from_memory(&bytes).ok()?.blur(8.0).to_rgba8();
                    Some(crate::media::Decoded {
                        w: image.width(),
                        h: image.height(),
                        rgba: image.into_raw(),
                    })
                })
                .await
                .ok()
                .flatten();
                ui_apply(move |b| b.on_media_preview(&id, img));
            });
        }
        Cmd::LinkPreview { jid, id, text, url } => {
            let cache = ctx.link_cache.clone();
            let sem = ctx.link_sem.clone();
            tokio::spawn(async move {
                let permit =
                    tokio::time::timeout(std::time::Duration::from_secs(20), sem.acquire_owned())
                        .await;
                let preview = if let Ok(Ok(permit)) = permit {
                    tokio::task::spawn_blocking(move || {
                        let _permit = permit;
                        if SESSION_EPOCH.load(Ordering::SeqCst) != epoch {
                            return None;
                        }
                        cache.lock().ok()?.get(&url, epoch)
                    })
                    .await
                    .ok()
                    .flatten()
                } else {
                    None
                };
                crate::bridge::wa_apply_at(epoch, move |b| {
                    b.on_link_preview(&jid, &id, &text, preview)
                });
            });
        }
        Cmd::DecodeThumb { id, bytes, link } => {
            tokio::spawn(async move {
                if let Some(img) = tokio::task::spawn_blocking(move || {
                    crate::media::decode_bytes(&bytes, crate::media::BUBBLE_PX)
                })
                .await
                .ok()
                .flatten()
                {
                    crate::bridge::wa_apply_at(epoch, move |b| b.on_thumb(&id, link, img));
                } else if link {
                    crate::bridge::wa_apply_at(epoch, move |b| b.on_media_missing(&id));
                }
            });
        }
        Cmd::GifSearch(query) => {
            tokio::spawn(async move {
                let results = tokio::task::spawn_blocking(move || {
                    let q = if query.trim().is_empty() {
                        "funny"
                    } else {
                        query.trim()
                    };
                    let url = format!(
                        "https://api.openverse.org/v1/images/?extension=gif&page_size=20&q={}",
                        urlencode(q)
                    );
                    let mut res = ureq::get(&url).header("User-Agent", "Zapive").call().ok()?;
                    let text = res.body_mut().read_to_string().ok()?;
                    let json: serde_json::Value = serde_json::from_str(&text).ok()?;
                    let mut out = Vec::new();
                    for item in json.get("results")?.as_array()?.iter().take(12) {
                        let id = item.get("id")?.as_str()?.to_string();
                        let gif_url = item.get("url")?.as_str()?.to_string();
                        let thumb_url = item
                            .get("thumbnail")
                            .and_then(|t| t.as_str())
                            .unwrap_or(&gif_url)
                            .to_string();
                        let Some(mut thumb) = ureq::get(&thumb_url)
                            .header("User-Agent", "Zapive")
                            .call()
                            .ok()
                        else {
                            continue;
                        };
                        let Some(bytes) = thumb.body_mut().read_to_vec().ok() else {
                            continue;
                        };
                        if let Some(img) = crate::media::decode_cover(&bytes, 84) {
                            out.push((id, gif_url, img));
                        }
                    }
                    Some(out)
                })
                .await
                .ok()
                .flatten()
                .unwrap_or_default();
                ui_apply(move |b| b.on_gif_results(results));
            });
        }
        Cmd::ZoomFrames { id, path } => {
            let key = ctx.media_key.clone();
            tokio::spawn(async move {
                let frames = tokio::task::spawn_blocking(move || {
                    crate::media::temp_plain(&key, &path)
                        .map(|p| crate::video::frames(&p, 720, 15.0, 90))
                })
                .await
                .ok()
                .flatten()
                .unwrap_or_default();
                if !frames.is_empty() {
                    ui_apply(move |b| b.on_zoom_frames(&id, frames));
                }
            });
        }
        Cmd::CheckUpdate => {
            tokio::spawn(async move {
                let result = tokio::task::spawn_blocking(crate::update::check)
                    .await
                    .unwrap_or_else(|_| Err("update check task failed".into()));
                // Direct apply: the banner must show on the lock
                // and login screens, not wait in the backlog.
                crate::bridge::ui_apply(move |b| b.on_update_checked(result));
            });
        }
        Cmd::ApplyUpdate => {
            tokio::spawn(async move {
                let result = tokio::task::spawn_blocking(crate::update::apply)
                    .await
                    .unwrap_or_else(|e| Err(e.to_string()));
                crate::bridge::ui_apply(move |b| b.on_update_applied(result));
            });
        }
        Cmd::AudioDecode {
            id,
            plain,
            rate_idx,
            rate,
        } => {
            tokio::spawn(async move {
                let buffer = tokio::task::spawn_blocking(move || {
                    crate::audio::decode_with_tempo(&plain, rate)
                })
                .await
                .ok()
                .flatten();
                match buffer {
                    Some(buffer) => ui_apply(move |b| b.on_audio_ready(&id, rate_idx, buffer)),
                    None => eprintln!("[audio] decode failed for {id}"),
                }
            });
        }
        Cmd::Waveform { id, path } => {
            let key = ctx.media_key.clone();
            tokio::spawn(async move {
                let wave = tokio::task::spawn_blocking(move || {
                    crate::media::temp_plain(&key, &path).and_then(|p| crate::audio::waveform(&p))
                })
                .await
                .ok()
                .flatten();
                if let Some(img) = wave {
                    ui_apply(move |b| b.on_wave(&id, img));
                }
            });
        }
        Cmd::PdfEngineInstall => {
            tokio::spawn(async move {
                let result = tokio::task::spawn_blocking(crate::pdf::install_engine)
                    .await
                    .unwrap_or_else(|e| Err(e.to_string()));
                ui_apply(move |b| b.on_pdf_engine_installed(result));
            });
        }
        Cmd::DecodeFull { id, path } => {
            let key = ctx.media_key.clone();
            tokio::spawn(async move {
                let img = tokio::task::spawn_blocking(move || {
                    crate::media::read_cached(&key, &path)
                        .and_then(|d| crate::media::decode_bytes(&d, crate::media::FULL_PX))
                })
                .await
                .ok()
                .flatten();
                if let Some(img) = img {
                    ui_apply(move |b| b.on_full_image(&id, img));
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
        _ => unreachable!("not a local command"),
    }
}
