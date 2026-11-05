// The command loop: runs every Cmd against the live session.
use super::*;

pub(super) async fn executor(
    mut session: Session,
    mut cmd_rx: mpsc::UnboundedReceiver<Cmd>,
    cmd_tx: mpsc::UnboundedSender<Cmd>,
) {
    let mut media_key = crate::vault::KeyHandle::default();
    let media_sem = Arc::new(tokio::sync::Semaphore::new(3));
    let avatar_sem = Arc::new(tokio::sync::Semaphore::new(8));
    let link_cache = Arc::new(std::sync::Mutex::new(crate::link_preview::Cache::default()));
    let link_sem = Arc::new(tokio::sync::Semaphore::new(2));
    while let Some(cmd) = cmd_rx.recv().await {
        let epoch = SESSION_EPOCH.load(Ordering::SeqCst);
        match cmd {
            Cmd::PrepareAttachment(preparation) => {
                let client = session.client.clone();
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
            Cmd::MediaKey(key) => {
                media_key = key;
            }
            Cmd::Media {
                id,
                mimetype,
                message,
                want,
            } => {
                let client = session.client.clone();
                let key = media_key.clone();
                let sem = media_sem.clone();
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
                            let decoded = crate::media::read_cached(&key, &path).and_then(|d| {
                                crate::media::decode_bytes(&d, crate::media::BUBBLE_PX)
                            });
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
                let cache = link_cache.clone();
                let sem = link_sem.clone();
                tokio::spawn(async move {
                    let permit = tokio::time::timeout(
                        std::time::Duration::from_secs(20),
                        sem.acquire_owned(),
                    )
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
            Cmd::React {
                jid,
                id,
                from_me,
                participant,
                emoji,
            } => {
                let client = session.client.clone();
                tokio::spawn(async move {
                    let Some(chat) = parse_jid(&jid) else { return };
                    let key = whatsapp_rust::message_key(
                        id,
                        &chat,
                        from_me,
                        participant.and_then(|p| parse_jid(&p)).as_ref(),
                    );
                    if let Err(e) = client.send_reaction(chat, key, &emoji).await {
                        eprintln!("[wa] reaction failed: {e}");
                    }
                });
            }
            Cmd::Revoke { jid, id } => {
                let client = session.client.clone();
                tokio::spawn(async move {
                    let Some(chat) = parse_jid(&jid) else {
                        action_failed(epoch, "Invalid destination JID");
                        return;
                    };
                    if let Err(e) = client
                        .revoke_message(chat, id, whatsapp_rust::send::RevokeType::Sender)
                        .await
                    {
                        action_failed(epoch, format!("[wa] revoke failed: {e}"));
                    }
                });
            }
            Cmd::Edit { jid, id, text } => {
                let client = session.client.clone();
                tokio::spawn(async move {
                    let Some(chat) = parse_jid(&jid) else { return };
                    let message = wa::Message {
                        conversation: Some(text),
                        ..Default::default()
                    };
                    match client.edit_message(chat, id.clone(), message.clone()).await {
                        Ok(_) => {
                            let timestamp = crate::store::now_secs();
                            ui_apply(move |b| b.on_message_edit(&jid, &id, &message, timestamp));
                        }
                        Err(e) => ui_apply(move |b| b.message_action_failed(&e.to_string())),
                    }
                });
            }
            Cmd::PinMessage {
                jid,
                id,
                from_me,
                participant,
                pin,
            } => {
                let client = session.client.clone();
                tokio::spawn(async move {
                    let Some(chat) = parse_jid(&jid) else { return };
                    let key = wa::MessageKey {
                        remote_jid: Some(jid.clone()),
                        id: Some(id.clone()),
                        from_me: Some(from_me),
                        participant,
                    };
                    let result = if pin {
                        client
                            .pin_message(chat, key, whatsapp_rust::send::PinDuration::Days7)
                            .await
                    } else {
                        client.unpin_message(chat, key).await
                    };
                    match result {
                        Ok(_) => ui_apply(move |b| {
                            b.on_message_pin(
                                &jid,
                                &id,
                                if pin {
                                    crate::store::now_secs() + 604800
                                } else {
                                    0
                                },
                            )
                        }),
                        Err(e) => ui_apply(move |b| b.message_action_failed(&e.to_string())),
                    }
                });
            }
            Cmd::CreatePoll {
                jid,
                question,
                options,
                selectable,
            } => {
                let client = session.client.clone();
                tokio::spawn(async move {
                    let Some(chat) = parse_jid(&jid) else {
                        action_failed(epoch, "Invalid destination JID");
                        return;
                    };
                    match client
                        .polls()
                        .create(chat, &question, &options, selectable)
                        .await
                    {
                        Ok((sent, secret)) => {
                            let id = sent.message_id.clone();
                            let message = (*sent.message).clone();
                            crate::bridge::wa_apply_at(epoch, move |b| {
                                b.on_poll_created(&jid, &id, message, &secret)
                            });
                        }
                        Err(e) => action_failed(epoch, format!("[wa] poll failed: {e}")),
                    }
                });
            }
            Cmd::DecryptVote {
                jid,
                poll_id,
                creator,
                voter,
                secret,
                payload,
                iv,
                timestamp_ms,
            } => {
                let client = session.client.clone();
                tokio::spawn(async move {
                    let (Some(creator_jid), Some(voter_jid)) =
                        (parse_jid(&creator), parse_jid(&voter))
                    else {
                        return;
                    };
                    let ciphertext = whatsapp_rust::features::PollVoteCiphertext {
                        enc_payload: &payload,
                        enc_iv: &iv,
                    };
                    match client
                        .polls()
                        .decrypt_vote(ciphertext, &secret, &poll_id, &creator_jid, &voter_jid)
                        .await
                    {
                        Ok(hashes) => {
                            let picked: Vec<String> = hashes
                                .iter()
                                .map(|hash| hash.iter().map(|byte| format!("{byte:02x}")).collect())
                                .collect();
                            crate::bridge::wa_apply_at(epoch, move |b| {
                                b.on_poll_vote(&jid, &poll_id, &voter, picked, timestamp_ms)
                            });
                        }
                        Err(e) => eprintln!("[wa] poll vote could not be opened: {e}"),
                    }
                });
            }
            Cmd::PollVote {
                jid,
                id,
                creator,
                secret,
                options,
            } => {
                let client = session.client.clone();
                tokio::spawn(async move {
                    let (Some(chat), Some(creator_jid)) = (parse_jid(&jid), parse_jid(&creator))
                    else {
                        action_failed(epoch, "Invalid destination JID");
                        return;
                    };
                    match client
                        .polls()
                        .vote(chat, &id, &creator_jid, &secret, &options)
                        .await
                    {
                        Ok(_) => crate::bridge::wa_apply_at(epoch, move |b| {
                            b.on_poll_vote_sent(&jid, &id, &options)
                        }),
                        Err(e) => action_failed(epoch, format!("[wa] poll vote failed: {e}")),
                    }
                });
            }
            Cmd::MarkUnread(jid) => {
                let client = session.client.clone();
                tokio::spawn(async move {
                    let Some(chat) = parse_jid(&jid) else { return };
                    match client
                        .chat_actions()
                        .mark_chat_as_read(&chat, false, None)
                        .await
                    {
                        Ok(_) => ui_apply(move |b| b.on_mark_read(&jid, false)),
                        Err(e) => ui_apply(move |b| b.message_action_failed(&e.to_string())),
                    }
                });
            }
            Cmd::Star {
                jid,
                id,
                from_me,
                participant,
                starred,
            } => {
                let client = session.client.clone();
                tokio::spawn(async move {
                    let Some(chat) = parse_jid(&jid) else { return };
                    let participant = participant.and_then(|p| parse_jid(&p));
                    let actions = client.chat_actions();
                    let result = if starred {
                        actions
                            .star_message(&chat, participant.as_ref(), &id, from_me)
                            .await
                    } else {
                        actions
                            .unstar_message(&chat, participant.as_ref(), &id, from_me)
                            .await
                    };
                    if let Err(e) = result {
                        eprintln!("[wa] star failed: {e}");
                    }
                });
            }
            Cmd::MarkRead { jid, sender, ids } => {
                let client = session.client.clone();
                tokio::spawn(async move {
                    let Some(chat) = parse_jid(&jid) else { return };
                    let sender = sender.and_then(|p| parse_jid(&p));
                    let id_refs: Vec<&str> = ids.iter().map(String::as_str).collect();
                    if let Err(e) = client.mark_as_read(&chat, sender.as_ref(), &id_refs).await {
                        eprintln!("[wa] read receipt failed: {e}");
                    }
                });
            }
            Cmd::MarkPlayed { jid, sender, id } => {
                let client = session.client.clone();
                tokio::spawn(async move {
                    let Some(chat) = parse_jid(&jid) else { return };
                    let sender = sender.and_then(|p| parse_jid(&p));
                    if let Err(e) = client
                        .mark_as_played(&chat, sender.as_ref(), &[id.as_str()])
                        .await
                    {
                        eprintln!("[wa] played receipt failed: {e}");
                    }
                });
            }
            Cmd::Forward { jid, message } => {
                tokio::spawn(async move {
                    if parse_jid(&jid).is_none() {
                        action_failed(epoch, "Invalid destination JID");
                        return;
                    };
                    use whatsapp_rust::proto_helpers::MessageExt as _;
                    let message = message.get_base_message().prepare_for_forward();
                    crate::bridge::wa_apply_at(epoch, move |b| {
                        b.enqueue_prepared(&jid, message, None);
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

            Cmd::SendImage {
                jid,
                path,
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
                        kind: "image".into(),
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
            Cmd::SendDocument { jid, path, origin } => {
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
                        kind: "document".into(),
                        filename: path
                            .file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_else(|| "file".into()),
                        caption: None,
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
            Cmd::SendAudioFile { jid, path, origin } => {
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
                        kind: "audio".into(),
                        filename: path
                            .file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_else(|| "file".into()),
                        caption: None,
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
            Cmd::SendSticker { jid, path } => {
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
                        kind: "sticker".into(),
                        filename: path
                            .file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_else(|| "file".into()),
                        caption: None,
                        origin: Some(path.to_string_lossy().into_owned()),
                        data,
                        in_rate: 0,
                        view_once: false,
                    };
                    crate::bridge::wa_apply_at(epoch, move |b| {
                        b.stage_attachment(preparation);
                    });
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

            Cmd::ZoomFrames { id, path } => {
                let key = media_key.clone();
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
            Cmd::RejectCall { id, from } => {
                let client = session.client.clone();
                tokio::spawn(async move {
                    // The parked offer carries the creator and the ringing
                    // generation, which a bare id cannot reconstruct.
                    if let Some(offer) = take_offer(&id) {
                        if let Err(e) = client.voip().reject(&offer).await {
                            log::warn!("[wa] call reject failed: {e}");
                        }
                        return;
                    }
                    let Some(peer) = parse_jid(&from) else { return };
                    if let Err(e) = client.voip().reject_call(&id, &peer, &peer).await {
                        log::warn!("[wa] call reject failed: {e}");
                    }
                });
            }
            Cmd::AcceptCall { id, video } => {
                let client = session.client.clone();
                CANCEL_PENDING.store(false, Ordering::SeqCst);
                tokio::spawn(async move {
                    let Some(offer) = take_offer(&id) else {
                        fail_call(&t("call.gone"));
                        return;
                    };
                    let Some(audio) = open_audio().await else {
                        // No devices means no call; decline rather than
                        // leave the caller listening to nothing.
                        if let Err(e) = client.voip().reject(&offer).await {
                            log::warn!("[wa] call reject failed: {e}");
                        }
                        fail_call(&t("call.no_devices"));
                        return;
                    };
                    // Answering an audio-only offer with video is not on
                    // the table: the peer never offered to receive it.
                    let vision = match video {
                        true => open_camera().await,
                        false => None,
                    };
                    let voip = client.voip();
                    let mut builder = voip.accept(&offer).audio(audio.mic(), audio.speaker());
                    if let Some((camera, sink)) = &vision {
                        builder = builder.video(camera.source(), sink.clone());
                    }
                    // preaccept -> callKey decrypt -> accept -> relay.
                    match builder.start().await {
                        Ok(handle) => {
                            run_call(handle, audio, vision.map(|(c, _)| c), false).await;
                        }
                        Err(e) => {
                            log::error!("[wa] call accept failed: {e}");
                            fail_call(&t("call.failed"));
                        }
                    }
                });
            }
            Cmd::StartCall { jid, video } => {
                let client = session.client.clone();
                CANCEL_PENDING.store(false, Ordering::SeqCst);
                tokio::spawn(async move {
                    let Some(peer) = parse_jid(&jid) else {
                        fail_call(&t("call.failed"));
                        return;
                    };
                    let Some(audio) = open_audio().await else {
                        fail_call(&t("call.no_devices"));
                        return;
                    };
                    let vision = match video {
                        true => open_camera().await,
                        false => None,
                    };
                    if video && vision.is_none() {
                        fail_call(&t("call.no_camera"));
                        return;
                    }
                    let voip = client.voip();
                    let mut builder = voip.call(&peer).audio(audio.mic(), audio.speaker());
                    if let Some((camera, sink)) = &vision {
                        builder = builder.video(camera.source(), sink.clone());
                    }
                    log::info!(
                        "[wa] placing a {} call to {peer}",
                        if vision.is_some() { "video" } else { "voice" }
                    );
                    match builder.start().await {
                        Ok(handle) => {
                            // The call id is minted here, so the screen
                            // only learns it now.
                            let id = handle.call_id().to_string();
                            log::info!("[wa] offer sent for call {id}; waiting for the relay");
                            ui_apply(move |b| b.on_outgoing_call(&id));
                            run_call(handle, audio, vision.map(|(c, _)| c), true).await;
                        }
                        Err(e) => {
                            log::error!("[wa] outgoing call failed: {e}");
                            fail_call(&t("call.failed"));
                        }
                    }
                });
            }
            Cmd::HangupCall => {
                let client = session.client.clone();
                tokio::spawn(async move {
                    let Some(live) = take_live() else {
                        // Nothing connected yet: leave the note for the
                        // setup task to find.
                        CANCEL_PENDING.store(true, Ordering::SeqCst);
                        return;
                    };
                    // terminate() tears the local media down as well, but
                    // hangup() is what guarantees it when the stanza
                    // cannot be sent.
                    if let Err(e) = client
                        .voip()
                        .terminate(&live.id, &live.peer, &live.creator)
                        .await
                    {
                        log::warn!("[wa] call terminate failed: {e}");
                    }
                    live.handle.hangup_local().await;
                });
            }
            Cmd::SetCallVideo(on) => {
                let handle = LIVE_CALL.lock().ok().and_then(|live| {
                    live.as_ref()
                        .map(|call| (call.handle.clone(), call.id.clone()))
                });
                let Some((handle, id)) = handle else { continue };
                tokio::spawn(async move {
                    if !on {
                        if let Err(e) = handle.stop_video().await {
                            log::warn!("[wa] stopping video failed: {e}");
                        }
                        if let Ok(mut live) = LIVE_CALL.lock()
                            && let Some(call) = live.as_mut()
                        {
                            // Dropping the feed is what releases the camera.
                            call.camera = None;
                            call.upgrade = None;
                        }
                        ui_apply(move |b| b.on_call_video(&id, false));
                        return;
                    }
                    let Some((camera, sink)) = open_video() else {
                        fail_call(&t("call.no_camera"));
                        return;
                    };
                    // A parked request from the peer has to be answered with
                    // its own token; anything else is a fresh upgrade.
                    let token = LIVE_CALL
                        .lock()
                        .ok()
                        .and_then(|mut live| live.as_mut().and_then(|call| call.upgrade.take()));
                    let started = match token {
                        Some(token) => {
                            handle
                                .accept_video(token, camera.source(), sink.clone())
                                .await
                        }
                        None => handle.start_video(camera.source(), sink.clone()).await,
                    };
                    if let Err(e) = started {
                        log::error!("[wa] starting video failed: {e}");
                        fail_call(&t("call.no_camera"));
                        return;
                    }
                    if let Ok(mut live) = LIVE_CALL.lock()
                        && let Some(call) = live.as_mut()
                    {
                        call.camera = Some(camera);
                    }
                    ui_apply(move |b| b.on_call_video(&id, true));
                });
            }
            Cmd::SetCallMuted(muted) => {
                // set_muted announces <mute_v2> to the peer, so it has to be
                // awaited; the capture side reads the flag either way.
                let live = LIVE_CALL
                    .lock()
                    .ok()
                    .and_then(|live| live.as_ref().map(|c| (c.handle.clone(), c.muted.clone())));
                let Some((handle, flag)) = live else { continue };
                flag.store(muted, Ordering::SeqCst);
                tokio::spawn(async move {
                    if let Err(e) = handle.set_muted(muted).await {
                        log::warn!("[wa] the peer was not told about the mute: {e}");
                    }
                });
            }
            Cmd::FetchChannel(jid) => {
                let client = session.client.clone();
                tokio::spawn(async move {
                    let Some(target) = parse_jid(&jid) else {
                        return;
                    };
                    match client.newsletter().get_metadata(&target).await {
                        Ok(meta) => {
                            let name = meta.name.clone();
                            let url = meta.picture_url.clone().or(meta.preview_url.clone());
                            let avatar = match url {
                                Some(url) => tokio::task::spawn_blocking(move || {
                                    let mut res = ureq::get(&url).call().ok()?;
                                    let bytes = res.body_mut().read_to_vec().ok()?;
                                    crate::media::decode_cover(&bytes, 64)
                                })
                                .await
                                .ok()
                                .flatten(),
                                None => None,
                            };
                            ui_apply(move |b| b.on_channel_meta(&jid, &name, avatar));
                        }
                        Err(e) => eprintln!("[wa] channel metadata failed for {jid}: {e}"),
                    }
                });
            }
            Cmd::FetchChatInfo { jid, group } => {
                let client = session.client.clone();
                tokio::spawn(async move {
                    let Some(target) = parse_jid(&jid) else {
                        return;
                    };
                    if group {
                        match client.groups().get_metadata(&target).await {
                            Ok(meta) => {
                                let desc = meta.description.clone().unwrap_or_default();
                                let members = meta
                                    .size
                                    .map(|s| s as usize)
                                    .unwrap_or(meta.participants.len());
                                ui_apply(move |b| b.on_chat_info(&jid, "", &desc, members));
                            }
                            Err(e) => eprintln!("[wa] group info failed: {e}"),
                        }
                    } else {
                        match client
                            .contacts()
                            .get_user_info(std::slice::from_ref(&target))
                            .await
                        {
                            Ok(map) => {
                                let about = map
                                    .get(&target)
                                    .and_then(|info| info.status.clone())
                                    .unwrap_or_default();
                                ui_apply(move |b| b.on_chat_info(&jid, &about, "", 0));
                            }
                            Err(e) => eprintln!("[wa] user info failed: {e}"),
                        }
                    }
                });
            }
            Cmd::Archive { jid, archived } => {
                let client = session.client.clone();
                tokio::spawn(async move {
                    let Some(target) = parse_jid(&jid) else {
                        return;
                    };
                    let actions = client.chat_actions();
                    let result = if archived {
                        actions.archive_chat(&target, None).await
                    } else {
                        actions.unarchive_chat(&target, None).await
                    };
                    if let Err(e) = result {
                        eprintln!("[wa] archive failed: {e}");
                    }
                });
            }
            Cmd::FetchGroupMeta(jid) => {
                let client = session.client.clone();
                tokio::spawn(async move {
                    let Some(target) = parse_jid(&jid) else {
                        return;
                    };
                    match client.groups().get_metadata(&target).await {
                        Ok(meta) => {
                            let members = meta
                                .participants
                                .iter()
                                .map(|p| crate::store::Member {
                                    jid: p.jid.to_non_ad_string(),
                                    phone: p
                                        .phone_number
                                        .as_ref()
                                        .map(|j| j.to_non_ad_string())
                                        .unwrap_or_default(),
                                    name: p
                                        .details
                                        .as_ref()
                                        .and_then(|d| d.display_name.as_ref())
                                        .map(|n| n.to_string())
                                        .unwrap_or_default(),
                                    admin: p.is_admin(),
                                })
                                .collect();
                            let snap = GroupSnapshot {
                                jid,
                                subject: meta.subject.clone(),
                                desc: meta.description.clone().unwrap_or_default(),
                                announce: meta.is_announcement,
                                ephemeral: meta
                                    .ephemeral
                                    .as_ref()
                                    .and_then(|e| e.expiration)
                                    .unwrap_or(0),
                                members,
                            };
                            ui_apply(move |b| b.on_group_meta(snap));
                        }
                        Err(e) => eprintln!("[wa] group metadata failed for {jid}: {e}"),
                    }
                });
            }
            Cmd::Mute { jid, until_ms } => {
                let client = session.client.clone();
                tokio::spawn(async move {
                    let Some(target) = parse_jid(&jid) else {
                        return;
                    };
                    let actions = client.chat_actions();
                    let result = if until_ms > 0 {
                        actions.mute_chat_until(&target, until_ms).await
                    } else {
                        actions.mute_chat(&target).await
                    };
                    if let Err(e) = result {
                        eprintln!("[wa] mute failed: {e}");
                    }
                });
            }
            Cmd::Unmute(jid) => {
                let client = session.client.clone();
                tokio::spawn(async move {
                    let Some(target) = parse_jid(&jid) else {
                        return;
                    };
                    if let Err(e) = client.chat_actions().unmute_chat(&target).await {
                        eprintln!("[wa] unmute failed: {e}");
                    }
                });
            }
            Cmd::SetEphemeral { jid, group, secs } => {
                let client = session.client.clone();
                tokio::spawn(async move {
                    let Some(target) = parse_jid(&jid) else {
                        action_failed(epoch, "Invalid destination JID");
                        return;
                    };
                    if group {
                        if let Err(e) = client.groups().set_ephemeral(target, secs).await {
                            action_failed(epoch, format!("[wa] ephemeral setting failed: {e}"));
                        }
                        return;
                    }
                    // One-to-one chats carry the timer as a protocol
                    // message, the way the phone announces it.
                    use whatsapp_rust::waproto::buffa::MessageField;
                    let pm = wa::message::ProtocolMessage {
                        r#type: Some(wa::message::protocol_message::Type::EPHEMERAL_SETTING),
                        ephemeral_expiration: Some(secs),
                        ..Default::default()
                    };
                    let message = wa::Message {
                        protocol_message: MessageField::some(pm),
                        ..Default::default()
                    };
                    if let Err(e) = client.send_message(target, message).await {
                        action_failed(epoch, format!("[wa] ephemeral setting failed: {e}"));
                    }
                });
            }
            Cmd::LeaveGroup(jid) => {
                let client = session.client.clone();
                tokio::spawn(async move {
                    let Some(target) = parse_jid(&jid) else {
                        return;
                    };
                    match client.groups().leave(target).await {
                        Ok(()) => ui_apply(move |b| b.on_group_left(&jid)),
                        Err(e) => eprintln!("[wa] leave group failed: {e}"),
                    }
                });
            }
            Cmd::AddMembers { jid, members } => {
                let client = session.client.clone();
                tokio::spawn(async move {
                    let Some(target) = parse_jid(&jid) else {
                        return;
                    };
                    let who: Vec<whatsapp_rust::Jid> =
                        members.iter().filter_map(|m| parse_jid(m)).collect();
                    match client.groups().add_participants(target, &who).await {
                        Ok(results) => {
                            let failed: Vec<String> = results
                                .iter()
                                .filter(|r| r.error.is_some())
                                .map(|r| r.jid.to_non_ad_string())
                                .collect();
                            ui_apply(move |b| b.on_members_added(&jid, &failed));
                        }
                        Err(e) => eprintln!("[wa] add members failed: {e}"),
                    }
                });
            }
            Cmd::ClearChat(jid) => {
                let client = session.client.clone();
                tokio::spawn(async move {
                    let Some(target) = parse_jid(&jid) else {
                        return;
                    };
                    if let Err(e) = client
                        .chat_actions()
                        .clear_chat(&target, false, false, None)
                        .await
                    {
                        eprintln!("[wa] clear chat failed: {e}");
                    }
                });
            }
            Cmd::DeleteChat(jid) => {
                let client = session.client.clone();
                tokio::spawn(async move {
                    let Some(target) = parse_jid(&jid) else {
                        return;
                    };
                    if let Err(e) = client
                        .chat_actions()
                        .delete_chat(&target, false, None)
                        .await
                    {
                        eprintln!("[wa] delete chat failed: {e}");
                    }
                });
            }
            Cmd::DeleteForMe {
                jid,
                id,
                from_me,
                participant,
                ts,
            } => {
                let client = session.client.clone();
                tokio::spawn(async move {
                    let Some(chat) = parse_jid(&jid) else { return };
                    let participant = participant.and_then(|p| parse_jid(&p));
                    if let Err(e) = client
                        .chat_actions()
                        .delete_message_for_me(
                            &chat,
                            participant.as_ref(),
                            &id,
                            from_me,
                            false,
                            Some(ts * 1000),
                        )
                        .await
                    {
                        eprintln!("[wa] delete for me failed: {e}");
                    }
                });
            }
            Cmd::LabelCreate {
                id,
                name,
                color,
                order,
            } => {
                let client = session.client.clone();
                tokio::spawn(async move {
                    use whatsapp_rust::waproto::buffa::MessageField;
                    let value = wa::SyncActionValue {
                        label_edit_action: MessageField::some(
                            wa::sync_action_value::LabelEditAction {
                                name: Some(name),
                                color: Some(color),
                                predefined_id: None,
                                deleted: Some(false),
                                order_index: Some(order),
                                is_active: Some(true),
                                ..Default::default()
                            },
                        ),
                        timestamp: Some(now_ms()),
                        ..Default::default()
                    };
                    if let Err(e) = client
                        .send_app_state_action(&whatsapp_rust::schemas::LABEL_EDIT, &[&id], &value)
                        .await
                    {
                        eprintln!("[wa] label create failed: {e}");
                    }
                });
            }
            Cmd::LabelDelete(id) => {
                let client = session.client.clone();
                tokio::spawn(async move {
                    use whatsapp_rust::waproto::buffa::MessageField;
                    let value = wa::SyncActionValue {
                        label_edit_action: MessageField::some(
                            wa::sync_action_value::LabelEditAction {
                                deleted: Some(true),
                                ..Default::default()
                            },
                        ),
                        timestamp: Some(now_ms()),
                        ..Default::default()
                    };
                    if let Err(e) = client
                        .send_app_state_action(&whatsapp_rust::schemas::LABEL_EDIT, &[&id], &value)
                        .await
                    {
                        eprintln!("[wa] label delete failed: {e}");
                    }
                });
            }
            Cmd::LabelChat { label_id, jid, on } => {
                let client = session.client.clone();
                tokio::spawn(async move {
                    use whatsapp_rust::waproto::buffa::MessageField;
                    let value = wa::SyncActionValue {
                        label_association_action: MessageField::some(
                            wa::sync_action_value::LabelAssociationAction {
                                labeled: Some(on),
                                ..Default::default()
                            },
                        ),
                        timestamp: Some(now_ms()),
                        ..Default::default()
                    };
                    if let Err(e) = client
                        .send_app_state_action(
                            &whatsapp_rust::schemas::LABEL_JID,
                            &[&label_id, &jid],
                            &value,
                        )
                        .await
                    {
                        eprintln!("[wa] label assignment failed: {e}");
                    }
                });
            }
            Cmd::Favorites(jids) => {
                let client = session.client.clone();
                tokio::spawn(async move {
                    use whatsapp_rust::waproto::buffa::MessageField;
                    let favorites = jids
                        .into_iter()
                        .map(|id| wa::sync_action_value::favorites_action::Favorite {
                            id: Some(id),
                        })
                        .collect();
                    let value = wa::SyncActionValue {
                        favorites_action: MessageField::some(
                            wa::sync_action_value::FavoritesAction { favorites },
                        ),
                        timestamp: Some(now_ms()),
                        ..Default::default()
                    };
                    if let Err(e) = client
                        .send_app_state_action(&whatsapp_rust::schemas::FAVORITES, &[], &value)
                        .await
                    {
                        eprintln!("[wa] favorites sync failed: {e}");
                    }
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
                let key = media_key.clone();
                tokio::spawn(async move {
                    let wave = tokio::task::spawn_blocking(move || {
                        crate::media::temp_plain(&key, &path)
                            .and_then(|p| crate::audio::waveform(&p))
                    })
                    .await
                    .ok()
                    .flatten();
                    if let Some(img) = wave {
                        ui_apply(move |b| b.on_wave(&id, img));
                    }
                });
            }
            Cmd::SendVoice {
                jid,
                samples,
                in_rate,
                view_once,
            } => {
                let preparation = crate::outbox::Preparation {
                    id: crate::outbox::new_id(),
                    jid,
                    kind: "voice".into(),
                    filename: "voice.f32le".into(),
                    caption: None,
                    origin: None,
                    data: samples
                        .iter()
                        .flat_map(|sample| sample.to_le_bytes())
                        .collect(),
                    in_rate,
                    view_once,
                };
                crate::bridge::wa_apply_at(epoch, move |b| {
                    b.stage_attachment(preparation);
                });
            }

            Cmd::FetchAvatar(jid) => {
                let client = session.client.clone();
                let sem = avatar_sem.clone();
                let key = media_key.clone();
                tokio::spawn(async move {
                    let path = crate::media::avatar_cache_path(&jid);
                    // Disk first: the sealed cache fills the list instantly;
                    // the network only refreshes stale entries.
                    let hit = {
                        let key = key.clone();
                        let path = path.clone();
                        tokio::task::spawn_blocking(move || {
                            let meta = std::fs::metadata(&path).ok()?;
                            let fresh = meta
                                .modified()
                                .ok()
                                .and_then(|m| m.elapsed().ok())
                                .map(|age| age.as_secs() < 7 * 24 * 3600)
                                .unwrap_or(false);
                            if meta.len() == 0 {
                                return Some((None, fresh)); // remembered "no picture"
                            }
                            let bytes = crate::media::read_cached(&key, &path)?;
                            let img = crate::media::decode_cover(&bytes, crate::media::AVATAR_PX)?;
                            // A preview-sized file counts as stale, so the
                            // full picture replaces it.
                            Some((Some(img), fresh && crate::media::avatar_is_full(&bytes)))
                        })
                        .await
                        .ok()
                        .flatten()
                    };
                    if let Some((img, fresh)) = hit {
                        let jid2 = jid.clone();
                        ui_apply(move |b| b.on_avatar(&jid2, img, true));
                        if fresh {
                            return;
                        }
                    }
                    let _permit = sem.acquire_owned().await;
                    let Some(target) = parse_jid(&jid) else {
                        return;
                    };
                    // The preview the server offers is a 96px thumbnail
                    // that blurs at any real size; the full picture is
                    // what WhatsApp Web shows, so fetch that and cut it.
                    let result = picture_url(&client, &target).await;
                    match result {
                        Ok(Some(url)) => {
                            let img = tokio::task::spawn_blocking(move || {
                                let mut res = ureq::get(&url).call().ok()?;
                                let bytes = res.body_mut().read_to_vec().ok()?;
                                let img =
                                    crate::media::decode_cover(&bytes, crate::media::AVATAR_PX)?;
                                let _ = std::fs::write(&path, key.encrypt_bytes(&bytes));
                                Some(img)
                            })
                            .await
                            .ok()
                            .flatten();
                            match img {
                                Some(img) => ui_apply(move |b| b.on_avatar(&jid, Some(img), true)),
                                // The URL fetch failed: transient, retryable.
                                None => ui_apply(move |b| b.on_avatar(&jid, None, false)),
                            }
                        }
                        // Confirmed: this jid has no picture (or hides it).
                        Ok(None) => {
                            let _ = std::fs::write(&path, []);
                            ui_apply(move |b| b.on_avatar(&jid, None, true));
                        }
                        Err(_) => ui_apply(move |b| b.on_avatar(&jid, None, false)),
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(25)).await;
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
                let key = media_key.clone();
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
            Cmd::ShareHistory {
                group,
                member,
                messages,
            } => {
                let client = session.client.clone();
                tokio::spawn(async move {
                    use whatsapp_rust::waproto::buffa::{Message as _, MessageField};
                    let Some(to) = parse_jid(&group) else {
                        action_failed(epoch, "Invalid destination JID");
                        crate::bridge::wa_apply_at(epoch, |b| b.on_share_result(false));
                        return;
                    };
                    let count = messages.len() as i64;
                    let oldest = messages
                        .iter()
                        .filter_map(|m| m.message_timestamp)
                        .min()
                        .unwrap_or(0) as i64;
                    let bundle = wa::GroupHistory {
                        messages,
                        ..Default::default()
                    };
                    let bytes = bundle.encode_to_vec();
                    let upload = match client
                        .upload(
                            bytes,
                            whatsapp_rust::wacore::download::MediaType::History,
                            Default::default(),
                        )
                        .await
                    {
                        Ok(u) => u,
                        Err(e) => {
                            action_failed(epoch, format!("[wa] history bundle upload failed: {e}"));
                            crate::bridge::wa_apply_at(epoch, |b| b.on_share_result(false));
                            return;
                        }
                    };
                    let message = wa::Message {
                        message_history_bundle: MessageField::some(
                            wa::message::MessageHistoryBundle {
                                mimetype: Some("application/x-protobuf".into()),
                                file_sha256: Some(upload.file_sha256.to_vec()),
                                media_key: Some(upload.media_key.to_vec()),
                                file_enc_sha256: Some(upload.file_enc_sha256.to_vec()),
                                direct_path: Some(upload.direct_path),
                                media_key_timestamp: Some(upload.media_key_timestamp),
                                message_history_metadata: MessageField::some(
                                    wa::message::MessageHistoryMetadata {
                                        history_receivers: vec![member.clone()],
                                        oldest_message_timestamp_in_window: Some(oldest),
                                        message_count: Some(count),
                                        oldest_message_timestamp_in_bundle: Some(oldest),
                                        ..Default::default()
                                    },
                                ),
                                ..Default::default()
                            },
                        ),
                        ..Default::default()
                    };
                    match client.send_message(to, message).await {
                        Ok(_) => crate::bridge::wa_apply_at(epoch, |b| b.on_share_result(true)),
                        Err(e) => {
                            action_failed(epoch, format!("[wa] history bundle send failed: {e}"));
                            crate::bridge::wa_apply_at(epoch, |b| b.on_share_result(false));
                        }
                    }
                });
            }
            Cmd::StatusPoster { id, path } => {
                let key = media_key.clone();
                tokio::spawn(async move {
                    let frame = tokio::task::spawn_blocking(move || {
                        crate::media::temp_plain(&key, &path)
                            .map(|p| crate::video::frames(&p, 1080, 1.0, 1))
                            .and_then(|f| f.into_iter().next())
                    })
                    .await
                    .ok()
                    .flatten();
                    if let Some(img) = frame {
                        ui_apply(move |b| b.on_status_poster(&id, img));
                    }
                });
            }
            Cmd::AvatarLarge(jid) => {
                let client = session.client.clone();
                let key = media_key.clone();
                tokio::spawn(async move {
                    let path = crate::media::avatar_cache_path(&jid);
                    let cached = {
                        let (key, path) = (key.clone(), path.clone());
                        tokio::task::spawn_blocking(move || {
                            crate::media::read_cached(&key, &path).and_then(|b| {
                                crate::media::decode_cover(&b, crate::media::AVATAR_LARGE_PX)
                            })
                        })
                        .await
                        .ok()
                        .flatten()
                    };
                    if let Some(img) = cached {
                        ui_apply(move |b| b.on_avatar_large(&jid, img));
                        return;
                    }
                    let Some(target) = parse_jid(&jid) else {
                        return;
                    };
                    let Ok(Some(url)) = picture_url(&client, &target).await else {
                        return;
                    };
                    let img = tokio::task::spawn_blocking(move || {
                        let mut res = ureq::get(&url).call().ok()?;
                        let bytes = res.body_mut().read_to_vec().ok()?;
                        let img =
                            crate::media::decode_cover(&bytes, crate::media::AVATAR_LARGE_PX)?;
                        let _ = std::fs::write(&path, key.encrypt_bytes(&bytes));
                        Some(img)
                    })
                    .await
                    .ok()
                    .flatten();
                    if let Some(img) = img {
                        ui_apply(move |b| b.on_avatar_large(&jid, img));
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
                let client = session.client.clone();
                let key = media_key.clone();
                let sem = media_sem.clone();
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
            Cmd::Start | Cmd::Resume => {
                let client = session.client.clone();
                tokio::spawn(async move { client.run().await });
            }
            Cmd::PairWithCode(digits) => {
                let client = session.client.clone();
                tokio::spawn(async move {
                    let options = PairCodeOptions {
                        phone_number: digits,
                        ..Default::default()
                    };
                    match client.pair_with_code(options).await {
                        Ok(code) => {
                            let pretty = format_pair_code(&code);
                            ui_apply(move |b| b.on_pairing_code(&pretty));
                        }
                        Err(e) => {
                            eprintln!("[wa] pair code request failed: {e}");
                            ui_apply(|b| b.on_pairing_failed());
                        }
                    }
                });
            }
            Cmd::SendQueued { id, jid, message } => {
                let client = session.client.clone();
                tokio::spawn(async move {
                    let result = match parse_jid(&jid) {
                        Some(to) => client
                            .send_message_with_options(
                                to,
                                (*message).clone(),
                                whatsapp_rust::send::SendOptions::default()
                                    .with_message_id(id.clone()),
                            )
                            .await
                            .map(|_| ())
                            .map_err(|e| e.to_string()),
                        None => Err("Invalid destination JID".into()),
                    };
                    crate::bridge::wa_apply_at(epoch, move |b| {
                        b.on_outbox_result(&jid, &id, result)
                    });
                });
            }
            Cmd::FetchHistory {
                jid,
                oldest_id,
                from_me,
                ts_ms,
            } => {
                let client = session.client.clone();
                tokio::spawn(async move {
                    let Some(chat) = parse_jid(&jid) else { return };
                    let ok = client
                        .fetch_message_history(&chat, &oldest_id, from_me, ts_ms, 50)
                        .await
                        .is_ok();
                    ui_apply(move |b| b.on_history_requested(ok));
                });
            }
            Cmd::SubscribePresence(jid) => {
                let client = session.client.clone();
                tokio::spawn(async move {
                    if let Some(target) = parse_jid(&jid) {
                        let _ = client.presence().subscribe(target).await;
                    }
                });
            }
            Cmd::FetchCommonGroups { jid, identities } => {
                let client = session.client.clone();
                tokio::spawn(async move {
                    let result = client.groups().get_participating().await;
                    let groups = match result {
                        Ok(groups) => Some(
                            groups
                                .into_iter()
                                .filter_map(|(group_jid, meta)| {
                                    let shared = meta.participants.iter().any(|participant| {
                                        std::iter::once(&participant.jid)
                                            .chain(participant.phone_number.iter())
                                            .chain(participant.lid.iter())
                                            .any(|identity| {
                                                identities.contains(&identity.to_non_ad_string())
                                            })
                                    });
                                    shared.then(|| (group_jid.to_non_ad_string(), meta.subject))
                                })
                                .collect::<Vec<_>>(),
                        ),
                        Err(error) => {
                            log::warn!("fetch common groups failed: {error}");
                            None
                        }
                    };
                    ui_apply(move |b| b.on_common_groups(&jid, groups));
                });
            }
            Cmd::FetchGroups => {
                let client = session.client.clone();
                tokio::spawn(async move {
                    match client.groups().get_participating().await {
                        Ok(groups) => {
                            let list: Vec<(String, String, bool, Option<String>)> = groups
                                .iter()
                                .map(|(jid, meta)| {
                                    (
                                        jid.to_non_ad_string(),
                                        meta.subject.clone(),
                                        meta.is_parent_group,
                                        meta.parent_group_jid
                                            .as_ref()
                                            .map(|j| j.to_non_ad_string()),
                                    )
                                })
                                .collect();
                            ui_apply(move |b| b.on_groups(&list));
                        }
                        Err(e) => eprintln!("[wa] fetch groups failed: {e}"),
                    }
                });
            }
            Cmd::Logout => {
                // Dispatches Event::LoggedOut, which routes back here as
                // ResetSession.
                session.client.logout().await;
            }
            Cmd::HaltRetries => {
                session.client.disconnect().await;
                ui_apply(|b| b.on_fatal("offline"));
            }
            Cmd::SwitchAccount(account) => {
                // A call cannot follow the user to another account.
                if let Some(live) = take_live() {
                    let _ = session
                        .client
                        .voip()
                        .terminate(&live.id, &live.peer, &live.creator)
                        .await;
                    live.handle.hangup_local().await;
                }
                session.client.disconnect().await;
                let previous = session.account.clone();
                drop(session);
                SESSION_EPOCH.fetch_add(1, Ordering::SeqCst);
                let opened = match build_session(cmd_tx.clone(), &account).await {
                    Ok(next) => Ok(next),
                    Err(e) => {
                        eprintln!("[wa] cannot open account {account:?}: {e}");
                        build_session(cmd_tx.clone(), &previous).await
                    }
                };
                match opened {
                    Ok(next) => {
                        session = next;
                        let account = session.account.clone();
                        let registered = session.client.pn().is_some();
                        let epoch = SESSION_EPOCH.load(Ordering::SeqCst);
                        // Not through the epoch-stamped path on purpose:
                        // this is the message that opens the new epoch.
                        crate::bridge::ui_apply(move |b| {
                            b.on_account_ready(&account, registered, epoch)
                        });
                    }
                    Err(e) => {
                        eprintln!("[wa] session rebuild failed: {e}");
                        let msg = t("status.connect_failed");
                        crate::bridge::ui_apply(move |b| b.on_status(&msg));
                        return;
                    }
                }
            }
            Cmd::ResetSession => {
                session.client.disconnect().await;
                let account = session.account.clone();
                drop(session);
                crate::paths::remove_db(&wa_session_path(&account));
                match build_session(cmd_tx.clone(), &account).await {
                    Ok(next) => {
                        session = next;
                        ui_apply(|b| b.on_logged_out());
                        let client = session.client.clone();
                        tokio::spawn(async move { client.run().await });
                    }
                    Err(e) => {
                        eprintln!("[wa] session rebuild failed: {e}");
                        let msg = t("status.connect_failed");
                        ui_apply(move |b| b.on_status(&msg));
                        return;
                    }
                }
            }
            Cmd::Shutdown => {
                // Leaving a call ringing on the peer's phone after the app
                // is gone would be the worst kind of goodbye.
                if let Some(live) = take_live() {
                    let _ = session
                        .client
                        .voip()
                        .terminate(&live.id, &live.peer, &live.creator)
                        .await;
                    live.handle.hangup_local().await;
                }
                session.client.disconnect().await;
                return;
            }
        }
    }
}
