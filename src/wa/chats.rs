// Chat-level state: archive, mute, ephemeral timer, labels, favorites, clearing and history.
use super::*;

pub(super) async fn run(ctx: &Ctx, cmd: Cmd) {
    let epoch = ctx.epoch;
    match cmd {
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
        Cmd::ClearChat(jid) => {
            let client = ctx.client.clone();
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
            let client = ctx.client.clone();
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
            let client = ctx.client.clone();
            tokio::spawn(async move {
                let Some(chat) = parse_jid(&jid) else { return };
                let ok = client
                    .fetch_message_history(&chat, &oldest_id, from_me, ts_ms, 50)
                    .await
                    .is_ok();
                ui_apply(move |b| b.on_history_requested(ok));
            });
        }
        _ => unreachable!("not a chats command"),
    }
}
