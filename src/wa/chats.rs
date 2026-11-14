// Chat-level state: archive, mute, ephemeral timer, labels, favorites, clearing and history.
use super::*;

const NEWSLETTER_PAGE_SIZE: u32 = 50;
const NEWSLETTER_BOOTSTRAP_LIMIT: usize = 100;

#[derive(Default)]
pub(super) struct NewsletterHistory {
    epoch: u64,
    pages: std::collections::HashMap<String, NewsletterCursor>,
}

#[derive(Default)]
struct NewsletterCursor {
    before: Option<u64>,
    exhausted: bool,
}

impl NewsletterCursor {
    fn advance(&mut self, ids: impl Iterator<Item = u64>) {
        let next = ids.filter(|id| *id > 0).min();
        self.exhausted = next.is_none()
            || self
                .before
                .is_some_and(|old| next.is_some_and(|n| n >= old));
        if !self.exhausted {
            self.before = next;
        }
    }
}

fn newsletter_web(
    jid: &str,
    id: String,
    timestamp: u64,
    from_me: bool,
    message: Option<wa::Message>,
) -> Option<wa::WebMessageInfo> {
    if id.is_empty() || timestamp > i64::MAX as u64 {
        return None;
    }
    Some(wa::WebMessageInfo {
        key: Some(wa::MessageKey {
            remote_jid: Some(jid.to_string()),
            id: Some(id),
            from_me: Some(from_me),
            ..Default::default()
        })
        .into(),
        message: message?.into(),
        message_timestamp: Some(timestamp),
        ..Default::default()
    })
}

async fn newsletter_page(ctx: &Ctx, jid: String, requested: bool) {
    let mut history = ctx.newsletters.lock().await;
    if ctx.epoch != SESSION_EPOCH.load(Ordering::SeqCst) {
        return;
    }
    if history.epoch != ctx.epoch {
        history.epoch = ctx.epoch;
        history.pages.clear();
    }
    let cursor = history.pages.entry(jid.clone()).or_default();
    let messages = if cursor.exhausted {
        Some(Vec::new())
    } else if let Some(target) = parse_jid(&jid) {
        match tokio::time::timeout(
            std::time::Duration::from_secs(20),
            ctx.client
                .newsletter()
                .get_messages(target, NEWSLETTER_PAGE_SIZE, cursor.before),
        )
        .await
        {
            Ok(Ok(mut page)) => {
                page.truncate(NEWSLETTER_PAGE_SIZE as usize);
                cursor.advance(page.iter().map(|m| m.server_id));
                Some(
                    page.into_iter()
                        .filter_map(|m| {
                            newsletter_web(&jid, m.message_id, m.timestamp, m.is_sender, m.message)
                        })
                        .collect(),
                )
            }
            _ => None,
        }
    } else {
        None
    };
    crate::bridge::wa_apply_at(ctx.epoch, move |b| {
        b.on_newsletter_history(&jid, messages, requested)
    });
}

pub(super) async fn run(ctx: &Ctx, cmd: Cmd) {
    let epoch = ctx.epoch;
    match cmd {
        Cmd::SyncNewsletters => {
            let ctx = ctx.clone();
            tokio::spawn(async move {
                if epoch != SESSION_EPOCH.load(Ordering::SeqCst) {
                    return;
                }
                let Ok(Ok(channels)) = tokio::time::timeout(
                    std::time::Duration::from_secs(30),
                    ctx.client.newsletter().list_subscribed(),
                )
                .await
                else {
                    return;
                };
                let channels: Vec<_> = channels
                    .into_iter()
                    .map(|c| (c.jid.to_non_ad_string(), c.name))
                    .filter(|(jid, _)| crate::store::is_channel(jid))
                    .collect();
                let metadata = channels.clone();
                crate::bridge::wa_apply_at(epoch, move |b| b.on_newsletters(&metadata));
                for (jid, _) in channels.into_iter().take(NEWSLETTER_BOOTSTRAP_LIMIT) {
                    if epoch != SESSION_EPOCH.load(Ordering::SeqCst) {
                        break;
                    }
                    {
                        let mut history = ctx.newsletters.lock().await;
                        if epoch != SESSION_EPOCH.load(Ordering::SeqCst) {
                            break;
                        }
                        if history.epoch != epoch {
                            history.epoch = epoch;
                            history.pages.clear();
                        }
                        history.pages.remove(&jid);
                    }
                    newsletter_page(&ctx, jid, false).await;
                }
            });
        }
        Cmd::Archive { jid, archived } => {
            let client = ctx.client.clone();
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
        Cmd::PinChat { jid, pinned } => {
            let client = ctx.client.clone();
            tokio::spawn(async move {
                let Some(target) = parse_jid(&jid) else {
                    return;
                };
                let actions = client.chat_actions();
                let result = if pinned {
                    actions.pin_chat(&target).await
                } else {
                    actions.unpin_chat(&target).await
                };
                if let Err(e) = result {
                    eprintln!("[wa] pin failed: {e}");
                }
            });
        }
        Cmd::Mute { jid, until_ms } => {
            let client = ctx.client.clone();
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
            let client = ctx.client.clone();
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
            let client = ctx.client.clone();
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
        Cmd::ClearChat(jid, newest) => {
            let client = ctx.client.clone();
            tokio::spawn(async move {
                let Some(target) = parse_jid(&jid) else {
                    return;
                };
                if let Err(e) = client
                    .chat_actions()
                    .clear_chat(&target, false, false, cleared_range(newest))
                    .await
                {
                    eprintln!("[wa] clear chat failed: {e}");
                }
            });
        }
        Cmd::DeleteChat(jid, newest) => {
            let client = ctx.client.clone();
            tokio::spawn(async move {
                let Some(target) = parse_jid(&jid) else {
                    return;
                };
                if let Err(e) = client
                    .chat_actions()
                    .delete_chat(&target, false, cleared_range(newest))
                    .await
                {
                    eprintln!("[wa] delete chat failed: {e}");
                }
            });
        }
        Cmd::LabelCreate {
            id,
            name,
            color,
            order,
        } => {
            let client = ctx.client.clone();
            tokio::spawn(async move {
                use whatsapp_rust::waproto::buffa::MessageField;
                let value = wa::SyncActionValue {
                    label_edit_action: MessageField::some(wa::sync_action_value::LabelEditAction {
                        name: Some(name),
                        color: Some(color),
                        predefined_id: None,
                        deleted: Some(false),
                        order_index: Some(order),
                        is_active: Some(true),
                        ..Default::default()
                    }),
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
            let client = ctx.client.clone();
            tokio::spawn(async move {
                use whatsapp_rust::waproto::buffa::MessageField;
                let value = wa::SyncActionValue {
                    label_edit_action: MessageField::some(wa::sync_action_value::LabelEditAction {
                        deleted: Some(true),
                        ..Default::default()
                    }),
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
            let client = ctx.client.clone();
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
            let client = ctx.client.clone();
            tokio::spawn(async move {
                use whatsapp_rust::waproto::buffa::MessageField;
                let favorites = jids
                    .into_iter()
                    .map(|id| wa::sync_action_value::favorites_action::Favorite { id: Some(id) })
                    .collect();
                let value = wa::SyncActionValue {
                    favorites_action: MessageField::some(wa::sync_action_value::FavoritesAction {
                        favorites,
                    }),
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
        Cmd::ShareHistory {
            group,
            member,
            messages,
        } => {
            let client = ctx.client.clone();
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
                    message_history_bundle: MessageField::some(wa::message::MessageHistoryBundle {
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
                    }),
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
        Cmd::FetchHistory {
            jid,
            oldest_id,
            from_me,
            ts_ms,
        } => {
            if crate::store::is_channel(&jid) {
                let ctx = ctx.clone();
                tokio::spawn(async move { newsletter_page(&ctx, jid, true).await });
                return;
            }
            let client = ctx.client.clone();
            tokio::spawn(async move {
                let Some(chat) = parse_jid(&jid) else { return };
                let ok = client
                    .fetch_message_history(&chat, &oldest_id, from_me, ts_ms, 50)
                    .await
                    .is_ok();
                crate::bridge::wa_apply_at(epoch, move |b| b.on_history_requested(ok));
            });
        }
        _ => unreachable!("not a chats command"),
    }
}

#[cfg(test)]
mod newsletter_tests {
    use super::*;

    #[test]
    fn newsletter_cursor_uses_minimum_numeric_id_and_stops_without_progress() {
        let mut cursor = NewsletterCursor::default();
        cursor.advance([100, 9, 40, 0].into_iter());
        assert_eq!(cursor.before, Some(9));
        assert!(!cursor.exhausted);
        cursor.advance([8, 2].into_iter());
        assert_eq!(cursor.before, Some(2));
        cursor.advance([2, 8].into_iter());
        assert!(cursor.exhausted);
        assert_eq!(cursor.before, Some(2));
    }

    #[test]
    fn newsletter_empty_or_missing_server_ids_exhaust_page() {
        for ids in [vec![], vec![0, 0]] {
            let mut cursor = NewsletterCursor::default();
            cursor.advance(ids.into_iter());
            assert!(cursor.exhausted);
            assert_eq!(cursor.before, None);
        }
    }

    #[test]
    fn newsletter_history_preserves_wire_id_and_deduplicates_without_unread() {
        let jid = "123456@newsletter";
        let message = wa::Message {
            conversation: Some("Invented channel post".into()),
            ..Default::default()
        };
        let web = newsletter_web(jid, "wire-id".into(), 1700000000, false, Some(message)).unwrap();
        assert_eq!(web.key.as_option().unwrap().id.as_deref(), Some("wire-id"));
        let mut store = crate::store::Store::default();
        store.upsert_chat(jid, Some("Invented Channel"), 0, Some(0), None, None);
        let mapped = crate::wa_map::from_history(&mut store, &web).unwrap();
        assert_eq!(mapped.jid, jid);
        assert_eq!(mapped.id, "wire-id");
        assert_eq!(mapped.timestamp, 1700000000);
        assert!(store.add_message(mapped));
        let duplicate = crate::wa_map::from_history(&mut store, &web).unwrap();
        assert!(!store.add_message(duplicate));
        assert_eq!(store.messages_for(jid).len(), 1);
        assert_eq!(store.chats[jid].unread, 0);
    }

    #[test]
    fn newsletter_unmappable_posts_are_not_given_synthetic_ids() {
        assert!(
            newsletter_web(
                "123456@newsletter",
                String::new(),
                1,
                false,
                Some(wa::Message::default())
            )
            .is_none()
        );
        assert!(newsletter_web("123456@newsletter", "wire".into(), 1, false, None).is_none());
        assert!(
            newsletter_web(
                "123456@newsletter",
                "wire".into(),
                u64::MAX,
                false,
                Some(wa::Message::default())
            )
            .is_none()
        );
    }
}
