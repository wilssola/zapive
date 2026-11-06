// Sending and acting on messages: reactions, edits, revokes, pins, receipts, forwards.
use super::*;

pub(super) async fn run(ctx: &Ctx, cmd: Cmd) {
    let epoch = ctx.epoch;
    match cmd {
        Cmd::React {
            jid,
            id,
            from_me,
            participant,
            emoji,
        } => {
            let client = ctx.client.clone();
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
            let client = ctx.client.clone();
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
            let client = ctx.client.clone();
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
            let client = ctx.client.clone();
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
        Cmd::MarkUnread(jid) => {
            let client = ctx.client.clone();
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
            let client = ctx.client.clone();
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
            let client = ctx.client.clone();
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
            let client = ctx.client.clone();
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
        Cmd::DeleteForMe {
            jid,
            id,
            from_me,
            participant,
            ts,
        } => {
            let client = ctx.client.clone();
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

        Cmd::SendQueued { id, jid, message } => {
            let client = ctx.client.clone();
            tokio::spawn(async move {
                let result = match parse_jid(&jid) {
                    Some(to) => client
                        .send_message_with_options(
                            to,
                            (*message).clone(),
                            whatsapp_rust::send::SendOptions::default().with_message_id(id.clone()),
                        )
                        .await
                        .map(|_| ())
                        .map_err(|e| e.to_string()),
                    None => Err("Invalid destination JID".into()),
                };
                crate::bridge::wa_apply_at(epoch, move |b| b.on_outbox_result(&jid, &id, result));
            });
        }
        _ => unreachable!("not a messaging command"),
    }
}
