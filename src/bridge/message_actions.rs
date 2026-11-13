// Reactions and the per-message actions (reply, forward, edit, delete, copy...).
use super::*;

impl Bridge {
    // ---- reactions ----

    pub(super) fn apply_reaction(
        &mut self,
        remote_jid: &str,
        target: &str,
        reactor: &str,
        emoji: &str,
        from_me: bool,
    ) {
        let chat_jid = self.store.canon_owned(&normalize_jid(remote_jid));
        if let Some(vault) = &self.vault {
            self.store.hydrate_id(vault, &chat_jid, target);
        }
        let updated = self.store.apply_reaction(&chat_jid, target, reactor, emoji);
        if updated && Some(&chat_jid) == self.current_jid.as_ref() {
            let summary = self
                .store
                .messages_for(&chat_jid)
                .iter()
                .find(|m| m.id == target)
                .map(reaction_summary);
            if let Some((emoji, count)) = summary {
                self.patch_row(target, |row| {
                    row.reactions = emoji.into();
                    row.reactionCount = count.into();
                });
            }
        }
        // A reaction is the chat's latest activity, and WhatsApp says so in
        // the list: Reacted <emoji> to: "<what it reacted to>".
        if updated && !emoji.is_empty() {
            let snippet: String = self
                .store
                .messages_for(&chat_jid)
                .iter()
                .find(|m| m.id == target)
                .map(preview_body)
                .unwrap_or_default();
            let name = self
                .store
                .chat_name(&self.store.canon_owned(&normalize_jid(reactor)));
            let who = if from_me {
                "✓ ".to_string()
            } else if is_group(&chat_jid) && !name.is_empty() {
                format!("{}: ", name.split(' ').next().unwrap_or(""))
            } else {
                String::new()
            };
            if let Some(meta) = self.store.chats.get_mut(&chat_jid) {
                meta.preview = format!(
                    "{who}{}",
                    ta("preview.reacted", &[&clean_text(emoji), &snippet])
                );
                meta.timestamp = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs() as i64)
                    .unwrap_or(0);
            }
            self.schedule_refresh_chats();
        }
        self.schedule_save();
    }

    // ---- message actions ----

    // The participant that goes into a message key inside a group.
    pub(super) fn key_participant(&self, m: &StoredMessage) -> Option<String> {
        if !is_group(&m.jid) {
            return None;
        }
        Some(if m.from_me {
            self.self_jid.clone()
        } else {
            m.sender_jid.clone()
        })
        .filter(|p| !p.is_empty())
    }

    // "Send message viewed" (opt-in, off by default): read receipts for
    // every not-yet-acked incoming message in this chat, one Cmd::MarkRead
    // per sender so a group receipt carries the right participant.
    // Called when the chat is opened and when a message arrives while it
    // is the open chat -- there is no window-focus tracking in this app,
    // so "viewed" means "the chat is currently open", the same condition
    // that already clears the local unread badge.
    pub(super) fn send_view_receipts(&mut self, jid: &str) {
        if !self.ui.get_send_receipts() {
            return;
        }
        let pending: Vec<(Option<String>, String)> = self
            .store
            .messages_for(jid)
            .iter()
            .filter(|m| !m.from_me && !m.read_sent && m.kind != MessageKind::System)
            .map(|m| (self.key_participant(m), m.id.clone()))
            .collect();
        if pending.is_empty() {
            return;
        }
        let mut by_sender: HashMap<Option<String>, Vec<String>> = HashMap::new();
        for (sender, id) in pending {
            by_sender.entry(sender).or_default().push(id);
        }
        let mut acked = Vec::new();
        for (sender, ids) in by_sender {
            acked.extend(ids.iter().cloned());
            self.wa.send(Cmd::MarkRead {
                jid: jid.to_string(),
                sender,
                ids,
            });
        }
        self.store.mark_read_sent(jid, &acked);
    }

    // The voice-note counterpart: sent once, the moment playback actually
    // starts (see spin_audio_at, which covers both an already-decoded
    // buffer and the post-decode resume in on_audio_ready). Reuses
    // `read_sent` so a later seek/scrub on the same note doesn't re-send.
    pub(super) fn send_played_receipt(&mut self, jid: &str, id: &str) {
        if !self.ui.get_send_receipts() {
            return;
        }
        let Some(m) = self.store.messages_for(jid).iter().find(|m| m.id == id) else {
            return;
        };
        if m.from_me || m.read_sent {
            return;
        }
        let sender = self.key_participant(m);
        self.wa.send(Cmd::MarkPlayed {
            jid: jid.to_string(),
            sender,
            id: id.to_string(),
        });
        self.store.mark_read_sent(jid, &[id.to_string()]);
    }

    pub(super) fn send_reaction(&mut self, id: &str, emoji: &str) {
        let Some(m) = self.find_message(id).cloned() else {
            return;
        };
        // Reacting again with the same emoji removes it, like WhatsApp.
        let mine = m.reactions.get("me").cloned().unwrap_or_default();
        let next = if clean_text(&mine) == clean_text(emoji) {
            ""
        } else {
            emoji
        };
        self.wa.send(Cmd::React {
            jid: m.jid.clone(),
            id: id.to_string(),
            from_me: m.from_me,
            participant: self.key_participant(&m),
            emoji: next.to_string(),
        });
        if self.store.apply_reaction(&m.jid, id, "me", next) {
            let summary = self
                .store
                .messages_for(&m.jid)
                .iter()
                .find(|x| x.id == id)
                .map(reaction_summary)
                .unwrap_or_default();
            self.patch_row(id, |row| {
                row.reactions = summary.0.into();
                row.reactionCount = summary.1.into();
            });
        }
        self.schedule_save();
    }

    // Lists who reacted to a message, like WhatsApp's reaction sheet.
    pub(super) fn show_reactions(&mut self, id: &str) {
        let Some(m) = self.find_message(id) else {
            return;
        };
        let entries: Vec<(String, String)> = m
            .reactions
            .iter()
            .filter(|(_, e)| !e.is_empty())
            .map(|(r, e)| (r.clone(), e.clone()))
            .collect();
        if entries.is_empty() {
            return;
        }
        let rows: Vec<ReactionItem> = entries
            .iter()
            .map(|(reactor, emoji)| {
                let who = if reactor == "me" {
                    self.self_jid.clone()
                } else {
                    self.store.canon_owned(&normalize_jid(reactor))
                };
                let name = if reactor == "me" {
                    t("reactions.you")
                } else {
                    self.store.chat_name(&who)
                };
                let name = if name.is_empty() {
                    display_id(&who)
                } else {
                    name
                };
                let avatar = self.avatar_for(&who);
                ReactionItem {
                    own: who == "me",
                    jid: who.clone().into(),
                    name: name.clone().into(),
                    emoji: clean_text(emoji).into(),
                    hasAvatar: avatar.is_some(),
                    avatar: avatar.unwrap_or_else(empty_image),
                    initial: initial_of(&name).into(),
                    colorIdx: color_idx_of(&who),
                }
            })
            .collect();
        for (reactor, _) in &entries {
            if reactor != "me" {
                let who = self.store.canon_owned(&normalize_jid(reactor));
                self.queue_avatar(&who);
            }
        }
        let count = rows.len();
        self.reactions_model.set_vec(rows);
        self.refresh_reaction_tabs(id);
        self.ui
            .set_reactions_title(ta("reactions.count", &[&count.to_string()]).into());
        self.ui.set_reactions_open(true);
    }

    pub fn message_action_failed(&mut self, error: &str) {
        self.ui
            .set_message_dialog_title("Message action failed".into());
        self.ui.set_message_dialog_text(error.into());
        self.ui.set_message_dialog_edit(false);
        self.ui.set_message_dialog_open(true);
    }

    pub fn on_message_edit(&mut self, jid: &str, id: &str, content: &wa::Message, timestamp: i64) {
        use whatsapp_rust::proto_helpers::MessageExt as _;
        let jid = self.store.canon_owned(&normalize_jid(jid));
        let content = content.get_base_message();
        if let Some(vault) = &self.vault {
            self.store.hydrate_id(vault, &jid, id);
        }
        let text = content
            .conversation
            .clone()
            .or_else(|| {
                content
                    .extended_text_message
                    .as_option()
                    .and_then(|m| m.text.clone())
            })
            .or_else(|| {
                content
                    .image_message
                    .as_option()
                    .and_then(|m| m.caption.clone())
            })
            .or_else(|| {
                content
                    .video_message
                    .as_option()
                    .and_then(|m| m.caption.clone())
            });
        let Some(text) = text else { return };
        let Some(m) = self
            .store
            .messages
            .get_mut(&jid)
            .and_then(|v| v.iter_mut().find(|m| m.id == id))
        else {
            return;
        };
        if !m.record_edit(text, timestamp) {
            return;
        }
        let mut raw = m.raw.as_deref().cloned().unwrap_or_default();
        if m.kind == MessageKind::Text {
            raw.conversation = Some(m.text.clone());
            raw.extended_text_message = Default::default();
        } else if let Some(mut image) = raw.image_message.as_option().cloned() {
            image.caption = Some(m.text.clone());
            raw.image_message = image.into();
        } else if let Some(mut video) = raw.video_message.as_option().cloned() {
            video.caption = Some(m.text.clone());
            raw.video_message = video.into();
        }
        m.raw = Some(std::sync::Arc::new(raw));
        m.link_url.clear();
        m.link_title.clear();
        m.link_desc.clear();
        let updated = m.clone();
        self.media_inflight.remove(id);
        self.media_failed.remove(id);
        self.store.search.remove(&jid, id);
        self.store.search.add(&jid, &updated);
        if self
            .store
            .messages_for(&jid)
            .last()
            .is_some_and(|m| m.id == id)
        {
            let preview = crate::store::compute_preview(&updated, &self.store);
            if let Some(meta) = self.store.chats.get_mut(&jid) {
                meta.preview = preview;
            }
        }
        self.store.dirty_jids.insert(jid.clone());
        if self.current_jid.as_ref() == Some(&jid) {
            self.rebuild_conversation(&jid);
            self.request_automatic_media(&updated);
        }
        self.schedule_save();
        self.schedule_refresh_chats();
    }

    pub fn on_message_pin(&mut self, jid: &str, id: &str, until: i64) {
        let jid = self.store.canon_owned(&normalize_jid(jid));
        if let Some(vault) = &self.vault {
            self.store.hydrate_id(vault, &jid, id);
        }
        if let Some(m) = self
            .store
            .messages
            .get_mut(&jid)
            .and_then(|v| v.iter_mut().find(|m| m.id == id))
        {
            m.pinned_until = until;
            self.store.dirty_jids.insert(jid.clone());
            if self.current_jid.as_ref() == Some(&jid) {
                self.rebuild_conversation(&jid);
                self.refresh_pinned();
            }
            self.schedule_save();
        }
    }

    pub(super) fn refresh_message_details(&mut self) {
        if !self.ui.get_details_open() {
            return;
        }
        let id = self.ui.get_details_id().to_string();
        let Some(m) = self.find_message(&id).filter(|m| !m.deleted).cloned() else {
            self.ui.set_details_open(false);
            return;
        };
        self.ui.set_details_text(preview_body(&m).into());
        let row = self.to_row(&m, None);
        self.ui.set_details_image(row.picture.clone());
        self.ui.set_details_has_image(
            self.decoded.contains_key(&m.id) || self.media_previews.contains_key(&m.id),
        );
        let mut rows = Vec::new();
        let heading = |title: &str| DetailRow {
            kind: "heading".into(),
            title: title.into(),
            ..Default::default()
        };
        let (known, participants) = self.store.receipt_participants(&m);
        if m.from_me {
            if is_group(&m.jid) && !known {
                rows.push(DetailRow {
                    kind: "note".into(),
                    title: "Exibindo apenas confirmações disponíveis.".into(),
                    ..Default::default()
                });
            }
            for (level, label) in [
                (4, "Lida por"),
                (3, "Entregue para"),
                (0, "Sem confirmação de entrega"),
            ] {
                let group: Vec<_> = participants
                    .iter()
                    .filter(|(_, r)| {
                        if level == 4 {
                            r.status() >= 4
                        } else if level == 3 {
                            r.status() == 3
                        } else {
                            r.status() < 3
                        }
                    })
                    .collect();
                if group.is_empty() {
                    continue;
                }
                rows.push(DetailRow {
                    kind: "participant-heading".into(),
                    title: label.into(),
                    ticks: if level >= 3 { "✓✓" } else { "✓" }.into(),
                    blue: level == 4,
                    ..Default::default()
                });
                for (jid, receipt) in group {
                    let name = self
                        .store
                        .members_of(&m.jid)
                        .iter()
                        .find(|member| self.store.canon(&member.jid) == jid)
                        .filter(|member| !member.name.is_empty())
                        .map(|member| member.name.clone())
                        .unwrap_or_else(|| self.store.chat_name(jid));
                    let avatar = self.avatar_for(jid);
                    let timestamp = if level == 4 {
                        receipt.read.or(receipt.played)
                    } else {
                        receipt.delivered
                    };
                    rows.push(DetailRow {
                        kind: "person".into(),
                        title: name.clone().into(),
                        initial: initial_of(&name).into(),
                        colorIdx: color_idx_of(jid),
                        hasAvatar: avatar.is_some(),
                        avatar: avatar.unwrap_or_default(),
                        time: timestamp.map(format_stamp).unwrap_or_default().into(),
                        ..Default::default()
                    });
                }
            }
        } else {
            rows.push(DetailRow { kind: "note".into(), title: "Recibos dos demais destinatários não estão disponíveis para mensagens recebidas.".into(), ..Default::default() });
        }
        rows.push(heading("Histórico de edições"));
        if m.edit_versions.is_empty() {
            rows.push(DetailRow {
                kind: "version".into(),
                title: "Versão atual".into(),
                text: m.text.clone().into(),
                time: if m.edited {
                    "Edição anterior à observação; horário desconhecido".into()
                } else {
                    format_stamp(m.timestamp).into()
                },
                ..Default::default()
            });
        } else {
            for (index, version) in m.edit_versions.iter().enumerate().rev() {
                rows.push(DetailRow {
                    kind: "version".into(),
                    title: if index + 1 == m.edit_versions.len() {
                        "Versão atual".into()
                    } else {
                        format!("Versão {}", index + 1).into()
                    },
                    text: version.text.clone().into(),
                    time: version
                        .timestamp
                        .map(format_stamp)
                        .unwrap_or_else(|| "Horário desconhecido".into())
                        .into(),
                    ..Default::default()
                });
            }
        }
        rows.push(DetailRow {
            kind: "note".into(),
            title: format!(
                "{} · {}",
                if m.from_me {
                    t("reactions.you")
                } else {
                    m.sender.clone()
                },
                m.id
            )
            .into(),
            time: if m.pinned_until > now_secs() {
                format!("Fixada até {}", format_stamp(m.pinned_until)).into()
            } else {
                Default::default()
            },
            ..Default::default()
        });
        self.ui.set_details_rows(ModelRc::new(VecModel::from(rows)));
    }

    pub(super) fn message_action(&mut self, id: &str, action: &str) {
        if action == "translate" || action.starts_with("translation-") {
            self.translation_message_action(id, action);
            return;
        }
        let preparation_key = format!("store:preparation:{id}");
        let preparation = self
            .vault
            .as_ref()
            .and_then(|vault| vault.get(&preparation_key))
            .and_then(|json| serde_json::from_str::<crate::outbox::Preparation>(&json).ok());
        if let Some(preparation) = preparation {
            if action == "retry" {
                if self.preparations_active.contains(id) {
                    return;
                }
                self.show_preparation(&preparation, "outbox.preparing");
                if self.outbox_connected && self.preparations_active.insert(preparation.id.clone())
                {
                    self.wa.send(Cmd::PrepareAttachment(preparation));
                }
                return;
            }
            if action == "cancel" {
                if self
                    .vault
                    .as_ref()
                    .is_some_and(|vault| vault.batch_checked(&[], &[&preparation_key]).is_ok())
                {
                    self.preparation_states.remove(id);
                    self.store
                        .remove_messages(&preparation.jid, &[id.to_string()]);
                    self.rebuild_conversation(&preparation.jid);
                }
                return;
            }
        }
        if let Some(index) = self.outbox.iter().position(|entry| entry.id == id) {
            if action == "recover" {
                if let Some(message) = self.find_message(id) {
                    let text = message.text.clone();
                    self.ui
                        .invoke_set_composer(text.clone().into(), text.len() as i32);
                    self.save_draft(&text, true);
                }
                return;
            }
            if action == "retry" && self.outbox[index].state != crate::outbox::State::Sending {
                self.outbox[index].state = crate::outbox::State::Queued;
                if self.persist_outbox(&self.outbox).is_ok() {
                    self.refresh_outbox_row(id);
                    self.dispatch_outbox();
                }
                return;
            }
            if action == "cancel" {
                if self.outbox[index].state != crate::outbox::State::Queued {
                    self.message_action_failed(&t("outbox.cannot_cancel"));
                    return;
                }
                let entry = self.outbox[index].clone();
                let mut entries = self.outbox.clone();
                entries.remove(index);
                if self.persist_outbox(&entries).is_err() {
                    self.message_action_failed(&t("outbox.storage_failed"));
                    return;
                }
                self.outbox = entries;
                self.store.remove_messages(&entry.jid, &[id.to_string()]);
                self.rebuild_conversation(&entry.jid);
                return;
            }
        }
        let Some(m) = self.find_message(id).cloned() else {
            return;
        };
        if m.deleted || m.kind == MessageKind::System {
            return;
        }
        match action {
            "private" if is_group(&m.jid) && !m.from_me && !m.sender_jid.is_empty() => {
                self.open_dm(&m.sender_jid, None);
                self.reply_to = Some((m.jid.clone(), m.id.clone()));
                self.ui.set_reply_open(true);
                self.ui.set_reply_name(self.store.chat_name(&m.jid).into());
                self.ui.set_reply_text(preview_body(&m).into());
                self.ui.invoke_focus_composer();
            }
            "link" => {
                let link = message_link(&m);
                if !link.is_empty()
                    && let Err(e) = arboard::Clipboard::new().and_then(|mut c| c.set_text(link))
                {
                    self.message_action_failed(&e.to_string());
                }
            }
            "save" => {
                let Some(cached) = self.media_path.get(id) else {
                    self.request_manual_media(id);
                    return;
                };
                let Some(path) =
                    crate::media::temp_plain(&self.media_key, &std::path::PathBuf::from(cached))
                else {
                    return;
                };
                let name = if m.kind == MessageKind::Doc {
                    sanitize_file_name(&m.text)
                } else {
                    path.file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into_owned()
                };
                std::thread::spawn(move || {
                    if let Some(target) = rfd::FileDialog::new().set_file_name(&name).save_file()
                        && let Err(e) = std::fs::copy(path, target)
                    {
                        ui_apply(move |b| b.message_action_failed(&e.to_string()));
                    }
                });
            }
            "pin" => self.wa.send(Cmd::PinMessage {
                jid: m.jid.clone(),
                id: m.id.clone(),
                from_me: m.from_me,
                participant: self.key_participant(&m),
                pin: m.pinned_until <= now_secs(),
            }),
            "unread" => {
                self.close_chat();
                self.wa.send(Cmd::MarkUnread(m.jid));
            }
            "edit" if message_editable(&m) => {
                self.ui.set_message_dialog_id(m.id.into());
                self.ui.set_message_dialog_title("Edit message".into());
                self.ui.set_message_dialog_text(m.text.into());
                self.ui.set_message_dialog_edit(true);
                self.ui.set_message_dialog_open(true);
            }
            "delete" => {
                self.select_start(id);
                self.confirm_delete_selected();
            }
            "details" => {
                self.ui.set_details_id(m.id.clone().into());
                self.ui.set_details_open(true);
                self.refresh_message_details();
                let (_, participants) = self.store.receipt_participants(&m);
                for (jid, _) in participants {
                    self.queue_avatar(&jid);
                }
                self.request_automatic_media(&m);
                if is_group(&m.jid) {
                    self.ensure_group_meta(&m.jid);
                }
            }
            _ => {}
        }
    }

    pub(super) fn toggle_star(&mut self, id: &str) {
        let Some(m) = self.find_message(id).cloned() else {
            return;
        };
        let next = !m.starred;
        self.wa.send(Cmd::Star {
            jid: m.jid.clone(),
            id: id.to_string(),
            from_me: m.from_me,
            participant: self.key_participant(&m),
            starred: next,
        });
        if self.store.set_starred(&m.jid, id, next) {
            self.patch_row(id, |row| row.starred = next);
        }
    }

    pub(super) fn delete_message(&mut self, id: &str) {
        let Some(m) = self.find_message(id).cloned() else {
            return;
        };
        // Only our own messages can be taken back from everyone; the
        // rest go away here and on the phone.
        if !m.from_me {
            self.wa.send(Cmd::DeleteForMe {
                jid: m.jid.clone(),
                id: id.to_string(),
                from_me: false,
                participant: self.key_participant(&m),
                ts: m.timestamp,
            });
            if self.store.remove_messages(&m.jid, &[id.to_string()]) {
                self.rebuild_conversation(&m.jid);
                self.schedule_refresh_chats();
            }
            return;
        }
        self.wa.send(Cmd::Revoke {
            jid: m.jid.clone(),
            id: id.to_string(),
        });
        if self.store.mark_deleted(&m.jid, id) {
            self.patch_row(id, |row| {
                row.deleted = true;
                row.kind = "text".into();
                row.text = t("msg.deleted").into();
            });
            self.schedule_refresh_chats();
        }
    }

    pub(super) fn request_forward(&mut self, id: &str) {
        let Some(jid) = self.current_jid.clone() else {
            return;
        };
        let Some(m) = self.find_message(id) else {
            return;
        };
        let preview = preview_body(m);
        let image = self.decoded.get(id).map(|(img, _, _)| img.clone());
        self.reset_forward_picker();
        self.pending_forward = vec![(jid, id.to_string())];
        // Show what is being forwarded, like WhatsApp's bottom bar.
        self.ui.set_forward_preview_text(preview.into());
        self.ui.set_forward_preview_has_image(image.is_some());
        self.ui
            .set_forward_preview_image(image.unwrap_or_else(empty_image));
        self.ui.set_forward_open(true);
    }

    pub(super) fn reset_forward_picker(&mut self) {
        self.wipe_forward_picker();
        self.fill_forward_rows("");
    }

    pub(super) fn wipe_forward_picker(&mut self) {
        self.pending_forward.clear();
        self.forward_selected.clear();
        self.forward_query.clear();
        self.forward_outgoing = None;
        self.forward_history_pending = false;
        self.forward_generation = self.forward_generation.wrapping_add(1);
        self.forward_model.set_vec(Vec::new());
        self.ui.set_forward_selected(ModelRc::default());
        self.ui.set_forward_filter("recent".into());
        self.ui.set_forward_preview_text("".into());
        self.ui.set_forward_preview_has_image(false);
        self.ui.set_forward_preview_image(empty_image());
        self.ui.set_forward_open(false);
    }

    fn fill_forward_rows(&mut self, query: &str) {
        self.forward_query = query.to_string();
        let filter = self.ui.get_forward_filter();
        if filter == "recent" {
            self.load_forward_history();
        }
        let mut outgoing = self.forward_outgoing.clone().unwrap_or_default();
        if filter == "recent" {
            for (jid, messages) in &self.store.messages {
                for message in messages
                    .iter()
                    .filter(|m| m.from_me && m.kind != MessageKind::System && m.status > 0)
                {
                    outgoing
                        .entry(jid.clone())
                        .and_modify(|timestamp| {
                            *timestamp = (*timestamp).max(message.timestamp);
                        })
                        .or_insert(message.timestamp);
                }
            }
        }
        let rows: Vec<ForwardRecipient> =
            forward_candidates(&self.store, query, filter.as_str(), &outgoing)
                .into_iter()
                .map(|meta| ForwardRecipient {
                    chat: self.to_chat_row(&meta.jid, &meta.preview, meta.timestamp, 0, false),
                    selected: self.forward_selected.contains(&meta.jid),
                    favorite: meta.favorite,
                })
                .collect();
        self.forward_model.set_vec(rows);
        let selected: Vec<ChatItem> = self
            .forward_selected
            .iter()
            .filter_map(|jid| self.store.chats.get(jid))
            .map(|meta| self.to_chat_row(&meta.jid, &meta.preview, meta.timestamp, 0, false))
            .collect();
        self.ui
            .set_forward_selected(ModelRc::new(VecModel::from(selected)));
    }

    fn load_forward_history(&mut self) {
        if self.forward_outgoing.is_some() || self.forward_history_pending {
            return;
        }
        let Some(vault) = self.vault.as_ref().filter(|v| !v.locked()) else {
            return;
        };
        let reader = vault.history_reader();
        let jids: Vec<String> = self
            .store
            .chats
            .keys()
            .filter(|jid| jid.ends_with("@s.whatsapp.net") || jid.ends_with("@lid"))
            .cloned()
            .collect();
        let generation = self.forward_generation;
        let epoch = self.epoch;
        let account = self.account.clone();
        self.forward_history_pending = true;
        std::thread::spawn(move || {
            let result = reader().and_then(|vault| vault.history_latest_outgoing(&jids));
            ui_apply(move |b| {
                if b.forward_generation != generation
                    || b.epoch != epoch
                    || b.account != account
                    || b.switching
                    || b.vault.as_ref().is_none_or(|v| v.locked())
                    || !b.ui.get_forward_open()
                {
                    return;
                }
                b.forward_history_pending = false;
                if let Ok(outgoing) = result {
                    b.forward_outgoing = Some(outgoing);
                    b.fill_forward_rows(&b.forward_query.clone());
                }
            });
        });
    }

    pub(super) fn handle_forward_search(&mut self, query: &str) {
        if self.ui.get_forward_open() {
            self.fill_forward_rows(query);
        }
    }

    pub(super) fn handle_forward_filter_changed(&mut self, filter: &str) {
        if !self.ui.get_forward_open() {
            return;
        }
        let filter = match filter {
            "contacts" | "groups" | "recent" => filter,
            _ => "all",
        };
        self.ui.set_forward_filter(filter.into());
        self.fill_forward_rows(&self.forward_query.clone());
    }

    pub(super) fn handle_forward_to(&mut self, target: &str) {
        if !self.ui.get_forward_open() {
            return;
        }
        let offered = self.forward_model.iter().any(|row| row.chat.jid == target);
        if !offered && !self.forward_selected.iter().any(|jid| jid == target) {
            return;
        }
        toggle_forward_recipient(&mut self.forward_selected, target);
        self.fill_forward_rows(&self.forward_query.clone());
    }

    pub(super) fn handle_forward_clear(&mut self) {
        if !self.ui.get_forward_open() {
            return;
        }
        self.forward_selected.clear();
        self.fill_forward_rows(&self.forward_query.clone());
    }

    pub(super) fn handle_forward_confirm(&mut self) {
        if !self.ui.get_forward_open() || self.forward_selected.is_empty() {
            return;
        }
        let targets: Vec<String> = self
            .forward_selected
            .iter()
            .filter(|jid| {
                self.store
                    .chats
                    .get(*jid)
                    .is_some_and(|meta| !is_channel(jid) && meta.can_send())
            })
            .cloned()
            .collect();
        let pending = std::mem::take(&mut self.pending_forward);
        self.wipe_forward_picker();
        if targets.is_empty() {
            return;
        }
        for (source, id) in pending {
            self.warm_chat(&source);
            let raw = self
                .store
                .messages_for(&source)
                .iter()
                .find(|m| m.id == id)
                .and_then(|m| m.raw.clone());
            if let Some(message) = raw {
                for target in &targets {
                    self.wa.send(Cmd::Forward {
                        jid: target.clone(),
                        message: message.clone(),
                    });
                }
            }
        }
    }
}

fn forward_candidates<'a>(
    store: &'a Store,
    query: &str,
    filter: &str,
    outgoing: &HashMap<String, i64>,
) -> Vec<&'a crate::store::ChatMeta> {
    let q = query.trim().to_lowercase();
    let digits: String = q.chars().filter(|c| c.is_ascii_digit()).collect();
    let recent = filter == "recent";
    let mut chats = store.sorted_chats();
    if recent {
        chats.retain(|meta| {
            (meta.jid.ends_with("@s.whatsapp.net") || meta.jid.ends_with("@lid"))
                && outgoing.contains_key(&meta.jid)
        });
        chats.sort_by(|a, b| {
            outgoing[&b.jid]
                .cmp(&outgoing[&a.jid])
                .then_with(|| a.jid.cmp(&b.jid))
        });
    }
    chats
        .into_iter()
        .filter(|meta| !is_channel(&meta.jid))
        .filter(|meta| match filter {
            "contacts" => !is_group(&meta.jid),
            "groups" => is_group(&meta.jid),
            _ => true,
        })
        .take(if recent { 20 } else { usize::MAX })
        .filter(|meta| {
            q.is_empty()
                || store.chat_name(&meta.jid).to_lowercase().contains(&q)
                || (!digits.is_empty() && meta.jid.contains(&digits))
        })
        .take(200)
        .collect()
}

fn toggle_forward_recipient(selected: &mut Vec<String>, target: &str) {
    if let Some(index) = selected.iter().position(|jid| jid == target) {
        selected.remove(index);
    } else if !target.is_empty() {
        selected.push(target.to_string());
    }
}

#[cfg(test)]
mod forward_tests {
    use super::*;

    fn forward_candidates<'a>(
        store: &'a Store,
        query: &str,
        filter: &str,
    ) -> Vec<&'a crate::store::ChatMeta> {
        let outgoing = HashMap::from([
            ("5500000000001@s.whatsapp.net".to_string(), 10),
            ("5500000000002@s.whatsapp.net".to_string(), 20),
            ("100@g.us".to_string(), 100),
            ("100@newsletter".to_string(), 200),
        ]);
        super::forward_candidates(store, query, filter, &outgoing)
    }

    fn fixture() -> Store {
        let mut store = Store::default();
        for (jid, name, timestamp) in [
            ("5500000000001@s.whatsapp.net", "Example Contact", 30),
            ("5500000000002@s.whatsapp.net", "Other Contact", 0),
            ("100@g.us", "Example Group", 20),
            ("100@newsletter", "Example Channel", 40),
        ] {
            store.upsert_chat(jid, Some(name), timestamp, None, None, None);
        }
        store
    }

    #[test]
    fn filters_exclude_channels_and_distinguish_contacts_groups_and_recent() {
        let store = fixture();
        assert_eq!(forward_candidates(&store, "", "all").len(), 3);
        assert_eq!(forward_candidates(&store, "", "contacts").len(), 2);
        let groups = forward_candidates(&store, "", "groups");
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].jid, "100@g.us");
        let recent = forward_candidates(&store, "", "recent");
        assert_eq!(recent.len(), 2);
        assert_eq!(recent[0].jid, "5500000000002@s.whatsapp.net");
        assert_eq!(recent[1].jid, "5500000000001@s.whatsapp.net");
    }

    #[test]
    fn search_preserves_case_insensitive_names_and_formatted_phone_matching() {
        let store = fixture();
        assert_eq!(forward_candidates(&store, " EXAMPLE ", "all").len(), 2);
        let phone = forward_candidates(&store, "+55 (000) 0000-0001", "all");
        assert_eq!(phone.len(), 1);
        assert_eq!(phone[0].jid, "5500000000001@s.whatsapp.net");
        assert!(forward_candidates(&store, "Channel", "all").is_empty());
    }

    #[test]
    fn recent_is_limited_before_search_and_ignores_incoming_pins_and_drafts() {
        let mut store = fixture();
        let mut outgoing = HashMap::new();
        for index in 0..25 {
            let jid = format!("recent-{index}@s.whatsapp.net");
            store.upsert_chat(
                &jid,
                Some("Recent Example"),
                100 + index,
                None,
                Some(index),
                None,
            );
            outgoing.insert(jid.clone(), 100 - index);
            store.drafts.insert(
                jid,
                crate::store::Draft {
                    text: "Example draft".into(),
                    promoted: index as u64,
                    ..Default::default()
                },
            );
        }
        let expected: Vec<_> = (0..20)
            .map(|index| format!("recent-{index}@s.whatsapp.net"))
            .collect();
        let actual: Vec<_> = super::forward_candidates(&store, "", "recent", &outgoing)
            .into_iter()
            .map(|meta| meta.jid.clone())
            .collect();
        assert_eq!(actual, expected);
        assert!(super::forward_candidates(&store, "Other", "recent", &outgoing).is_empty());
        assert!(
            super::forward_candidates(&store, "Example Contact", "recent", &outgoing).is_empty()
        );
        assert!(super::forward_candidates(&store, "recent-24", "recent", &outgoing).is_empty());
        assert_eq!(
            super::forward_candidates(&store, "recent-0", "recent", &outgoing).len(),
            2
        );
    }

    #[test]
    fn selection_toggles_without_duplicates_and_survives_refiltering() {
        let store = fixture();
        let mut selected = Vec::new();
        toggle_forward_recipient(&mut selected, "100@g.us");
        toggle_forward_recipient(&mut selected, "5500000000001@s.whatsapp.net");
        let _ = forward_candidates(&store, "Other", "contacts");
        assert_eq!(selected.len(), 2);
        toggle_forward_recipient(&mut selected, "100@g.us");
        assert_eq!(selected, ["5500000000001@s.whatsapp.net"]);
        toggle_forward_recipient(&mut selected, "");
        assert_eq!(selected.len(), 1);
        selected.clear();
        assert!(selected.is_empty());
    }
}
