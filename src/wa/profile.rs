// Pictures, chat info, channels, statuses and presence of other users.
use super::*;

pub(super) async fn run(ctx: &Ctx, cmd: Cmd) {
    let epoch = ctx.epoch;
    match cmd {
        Cmd::FetchChannel(jid) => {
            let client = ctx.client.clone();
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
                        wa_apply_at(epoch, move |b| b.on_channel_meta(&jid, &name, avatar));
                    }
                    Err(e) => eprintln!("[wa] channel metadata failed for {jid}: {e}"),
                }
            });
        }
        Cmd::FetchChatInfo { jid, group } => {
            let client = ctx.client.clone();
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
                            wa_apply_at(epoch, move |b| b.on_chat_info(&jid, "", &desc, members));
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
                            wa_apply_at(epoch, move |b| b.on_chat_info(&jid, &about, "", 0));
                        }
                        Err(e) => eprintln!("[wa] user info failed: {e}"),
                    }
                }
            });
        }
        Cmd::FetchAvatar(jid) => {
            let client = ctx.client.clone();
            let sem = ctx.avatar_sem.clone();
            let key = ctx.media_key.clone();
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
                    wa_apply_at(epoch, move |b| b.on_avatar(&jid2, img, true));
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
                            let img = crate::media::decode_cover(&bytes, crate::media::AVATAR_PX)?;
                            let _ = std::fs::write(&path, key.encrypt_bytes(&bytes));
                            Some(img)
                        })
                        .await
                        .ok()
                        .flatten();
                        match img {
                            Some(img) => {
                                wa_apply_at(epoch, move |b| b.on_avatar(&jid, Some(img), true))
                            }
                            // The URL fetch failed: transient, retryable.
                            None => wa_apply_at(epoch, move |b| b.on_avatar(&jid, None, false)),
                        }
                    }
                    // Confirmed: this jid has no picture (or hides it).
                    Ok(None) => {
                        let _ = std::fs::write(&path, []);
                        wa_apply_at(epoch, move |b| b.on_avatar(&jid, None, true));
                    }
                    Err(_) => wa_apply_at(epoch, move |b| b.on_avatar(&jid, None, false)),
                }
                tokio::time::sleep(std::time::Duration::from_millis(25)).await;
            });
        }
        Cmd::StatusPoster { id, path } => {
            let key = ctx.media_key.clone();
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
                    wa_apply_at(epoch, move |b| b.on_status_poster(&id, img));
                }
            });
        }
        Cmd::AvatarLarge(jid) => {
            let client = ctx.client.clone();
            let key = ctx.media_key.clone();
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
                    wa_apply_at(epoch, move |b| b.on_avatar_large(&jid, img));
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
                    let img = crate::media::decode_cover(&bytes, crate::media::AVATAR_LARGE_PX)?;
                    let _ = std::fs::write(&path, key.encrypt_bytes(&bytes));
                    Some(img)
                })
                .await
                .ok()
                .flatten();
                if let Some(img) = img {
                    wa_apply_at(epoch, move |b| b.on_avatar_large(&jid, img));
                }
            });
        }
        Cmd::SubscribePresence(jid) => {
            let client = ctx.client.clone();
            tokio::spawn(async move {
                if let Some(target) = parse_jid(&jid) {
                    let _ = client.presence().subscribe(target).await;
                }
            });
        }
        _ => unreachable!("not a profile command"),
    }
}
