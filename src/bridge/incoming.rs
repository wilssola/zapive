// Events from WhatsApp: groups, history chunks, messages, receipts and presence.
use super::*;

pub(super) fn group_typing_status(names: &[String], overflow: bool) -> String {
    if overflow { return t("presence.group_many"); }
    let first_names = names.iter().take(5)
        .filter_map(|name| name.split_whitespace().next()).collect::<Vec<_>>().join(", ");
    ta(if names.len() == 1 { "presence.group_one" } else { "presence.group_some" }, &[&first_names])
}

pub(super) fn typing_participants(members: &HashMap<String, (u64, String)>) -> (Vec<String>, i32) {
    let mut participants: Vec<_> = members.keys().cloned().collect();
    participants.sort();
    let extra = participants.len().saturating_sub(5).min(i32::MAX as usize) as i32;
    participants.truncate(5);
    (participants, extra)
}

impl Bridge {
    // Indexes cold chats that have messages on disk but no index yet;
    // a handful per tick so the UI never notices.
    pub(super) fn index_backfill(&mut self) {
        let Some(vault) = self.vault.take() else {
            return;
        };
        let mut seen = HashSet::new();
        let pending: Vec<String> = self.store.chats.keys().cloned()
            .chain(vault.keys("store:msgs:").into_iter()
                .map(|k| k["store:msgs:".len()..].to_string()))
            .map(|jid| self.store.canon_owned(&jid))
            .filter(|jid| seen.insert(jid.clone()) && !self.store.search.has_chat(jid))
            .take(6)
            .collect();
        for jid in &pending {
            self.store.index_from_vault(&vault, jid);
        }
        if !pending.is_empty() {
            self.store.search.save_to(&vault);
        }
        self.vault = Some(vault);
        if !pending.is_empty() {
            self.once(250, |b| b.index_backfill());
        }
    }

    pub fn on_groups(&mut self, groups: &[(String, String, bool, Option<String>)]) {
        for (jid, subject, is_community, parent) in groups {
            if subject.is_empty() {
                continue;
            }
            // Seed the sidebar with participating groups before any message.
            if !self.store.chats.contains_key(jid) {
                self.store
                    .upsert_chat(jid, Some(subject), 0, None, None, None);
            }
            self.store.set_name(jid, subject);
            if let Some(entry) = self.store.chats.get_mut(jid) {
                entry.is_community = *is_community;
                entry.community = parent.clone().unwrap_or_default();
            }
        }
        self.resolve_channel_names();
        self.schedule_refresh_chats();
    }

    pub fn on_history_chunk(&mut self, chunk: HistoryChunk) {
        for (jid, name) in &chunk.pushnames {
            self.store.upsert_contact(jid, None, Some(name));
        }
        for c in &chunk.chats {
            self.store.upsert_chat(
                &c.jid,
                c.name.as_deref(),
                c.timestamp,
                c.unread,
                c.pinned,
                c.archived,
            );
        }
        let mut added = 0usize;
        let mut added_to_current = false;
        for web in &chunk.messages {
            if let Some(stored) = crate::wa_map::from_history(&mut self.store, web) {
                let jid = stored.jid.clone();
                if stored.from_me && stored.status >= 1 { self.on_outbox_ack(&stored.id, None); }
                if let Some(vault) = &self.vault { self.store.hydrate_id(vault, &jid, &stored.id); }
                if self.store.add_message(stored) {
                    added += 1;
                    if Some(&jid) == self.current_jid.as_ref() {
                        added_to_current = true;
                    }
                }
            }
        }
        // Older messages may have arrived for the open conversation.
        if added_to_current && let Some(jid) = self.current_jid.clone() {
            if self.scroll_up_fetch {
                self.prepend_older_rows(&jid);
            } else if self.ui.get_stick_bottom() {
                self.rebuild_conversation(&jid);
                self.scroll_to_end();
                self.load_media_for_chat(&jid);
                self.queue_row_avatars(&jid);
            }
            // else: data is stored; the view catches up on next open.
        }
        self.scroll_up_fetch = false;
        self.schedule_refresh_chats();
        self.finish_loading();
        if chunk.on_demand || self.history_pending {
            self.sync_batches += 1;
        }
        self.sync_messages += added;
        if self.ui.get_sync_open() {
            self.ui.set_sync_detail(self.sync_summary().into());
        }
        if self.history_pending {
            self.set_pending(false);
            if added > 0 {
                // Keep walking back while the phone still has older messages.
                self.once(3000, |b| b.pull_older_history());
            } else {
                self.history_batches = MAX_HISTORY_BATCHES;
                println!("[history] backfill complete (no more messages)");
            }
        }
    }

    pub fn on_history_requested(&mut self, ok: bool) {
        if !ok {
            self.set_pending(false);
        } else {
            // If the phone never answers, unblock after a while.
            self.once(20_000, |b| {
                if b.history_pending {
                    b.set_pending(false);
                }
            });
        }
    }

    pub fn on_messages(&mut self, batch: &MessageBatch) {
        for inbound in batch.iter() {
            let info = &inbound.info;
            let chat = info.source.chat.to_non_ad_string();
            if chat == "status@broadcast" {
                if let Some(entry) = crate::wa_map::status_entry(&mut self.store, inbound)
                    && self.store.add_status(entry)
                {
                    self.refresh_statuses();
                    self.schedule_save();
                }
                continue;
            }
            use whatsapp_rust::proto_helpers::MessageExt as _;
            let content = inbound.message.get_base_message();
            if let Some(pm) = content.protocol_message.as_option()
                && pm.r#type == Some(wa::message::protocol_message::Type::MESSAGE_EDIT)
                && let Some(key) = pm.key.as_option()
                && let Some(id) = key.id.as_deref()
                && let Some(edited) = pm.edited_message.as_option()
            {
                let jid = self.store.canon_owned(&normalize_jid(&chat));
                if let Some(vault) = &self.vault { self.store.hydrate_id(vault, &jid, id); }
                let sender = self.store.canon_owned(&info.source.sender.to_non_ad_string());
                let valid = self.store.messages_for(&jid).iter().find(|m| m.id == id).is_some_and(|m| {
                    m.from_me == info.source.is_from_me && (m.from_me || self.store.canon(&m.sender_jid) == sender)
                });
                if valid { self.on_message_edit(&chat, id, edited, pm.timestamp_ms.map(|t| t / 1000).unwrap_or_else(|| info.timestamp.timestamp())); }
                continue;
            }
            if let Some(pin) = content.pin_in_chat_message.as_option()
                && let Some(id) = pin.key.as_option().and_then(|k| k.id.as_deref())
            {
                let duration = content.message_context_info.as_option().and_then(|c| c.message_add_on_duration_in_secs).unwrap_or(604800);
                let until = if pin.r#type == Some(wa::message::pin_in_chat_message::Type::PinForAll) { info.timestamp.timestamp() + i64::from(duration) } else { 0 };
                self.on_message_pin(&chat, id, until);
                continue;
            }
            // A disappearing-messages change in a one-to-one chat comes as
            // a protocol message; it shows as a notice, like on the phone.
            if let Some(pm) = content.protocol_message.as_option()
                && pm.r#type == Some(wa::message::protocol_message::Type::EPHEMERAL_SETTING)
            {
                let chat_jid = self.store.canon_owned(&normalize_jid(&chat));
                let secs = pm.ephemeral_expiration.unwrap_or(0);
                let actor = if info.source.is_from_me {
                    self.self_jid.clone()
                } else {
                    info.source.sender.to_non_ad_string()
                };
                let body = crate::wa_map::group_notice(
                    &self.store,
                    "ephemeral",
                    Some(&actor),
                    &[],
                    "",
                    secs,
                );
                self.add_system_message(&chat_jid, &info.id, info.timestamp.timestamp(), body);
                if let Some(meta) = self.store.chats.get_mut(&chat_jid) {
                    meta.ephemeral = secs;
                }
                self.apply_chat_settings_ui();
                continue;
            }
            // Deleted-for-everyone arrives as a protocol REVOKE. Applied
            // destructively (today's behaviour) when "apply deletions" is
            // on, or when it came from one of this account's own devices
            // (phone or another linked device) so they all stay
            // consistent; otherwise the message is kept and only
            // flagged, so the reader can still see it and the sender
            // can't quietly erase it here.
            if let Some(pm) = content.protocol_message.as_option()
                && pm.r#type == Some(wa::message::protocol_message::Type::Revoke)
                && let Some(target) = pm.key.as_option().and_then(|k| k.id.clone())
            {
                let chat_jid = self.store.canon_owned(&normalize_jid(&chat));
                if let Some(vault) = &self.vault { self.store.hydrate_id(vault, &chat_jid, &target); }
                let destructive = self.ui.get_apply_deletions() || info.source.is_from_me;
                let changed = if destructive {
                    self.store.mark_deleted(&chat_jid, &target)
                } else {
                    self.store.mark_revoked(&chat_jid, &target)
                };
                if changed && Some(&chat_jid) == self.current_jid.as_ref() {
                    self.patch_row(&target, |row| {
                        if destructive {
                            row.deleted = true;
                            row.kind = "text".into();
                            row.text = t("msg.deleted").into();
                        }
                        row.revoked = true;
                    });
                }
                self.schedule_refresh_chats();
                continue;
            }
            if let Some(rm) = content.reaction_message.as_option()
                && let Some(target) = rm.key.as_option().and_then(|k| k.id.clone())
            {
                let reactor = if info.source.is_from_me {
                    "me".to_string()
                } else {
                    info.source.sender.to_non_ad_string()
                };
                self.apply_reaction(
                    &chat,
                    &target,
                    &reactor,
                    rm.text.as_deref().unwrap_or(""),
                    info.source.is_from_me,
                );
                continue;
            }
            let Some(stored) = crate::wa_map::from_live(&mut self.store, inbound) else {
                continue;
            };
            let jid = stored.jid.clone();
            let from_me = stored.from_me;
            if from_me { self.on_outbox_ack(&stored.id, None); }
            if let Some(vault) = &self.vault { self.store.hydrate_id(vault, &jid, &stored.id); }
            let mentions_me = stored.mentions_me;
            let system_message = stored.kind == MessageKind::System;
            let sender = stored.sender_jid.clone();
            let translation_id = stored.id.clone();
            if !self.store.add_message(stored) {
                continue;
            }
            self.request_translation(&jid, &translation_id, true);
            if !from_me {
                self.clear_participant_typing(&jid, &sender);
                self.play_message_sound(&jid, mentions_me, info.timestamp.timestamp(), system_message);
            }
            if Some(&jid) == self.current_jid.as_ref() {
                self.push_message_row(&jid);
                self.send_view_receipts(&jid);
            } else if !from_me {
                let mut muted = false;
                if let Some(meta) = self.store.chats.get_mut(&jid) {
                    meta.unread += 1;
                    if mentions_me {
                        meta.mentioned = true;
                    }
                    muted = meta.is_muted();
                }
                // A muted chat still counts unread; it just stays quiet,
                // unless someone tagged us.
                if muted && !mentions_me {
                    continue;
                }
                if let Some(m) = self.store.messages_for(&jid).last().cloned() {
                    let body = self.store.named_mentions(&notification_body(&m));
                    let title = self.store.chat_name(&jid);
                    let text = if is_group(&jid) && !m.sender.is_empty() {
                        format!("{}: {body}", m.sender)
                    } else {
                        body
                    };
                    self.push_notification(title, text, jid.clone(), m.id.clone());
                }
            }
        }
        self.schedule_refresh_chats();
    }

    pub fn on_receipt(&mut self, chat: &str, ids: &[String], participant: &str, alternate: Option<&str>, status: u32, timestamp: i64) {
        if let Some(alternate) = alternate {
            if participant.ends_with("@lid") { self.store.learn_alias(participant, alternate); }
            else if alternate.ends_with("@lid") { self.store.learn_alias(alternate, participant); }
        }
        let jid = self.store.canon_owned(&normalize_jid(chat));
        let mut changed = Vec::new();
        for id in ids {
            if status >= 3 { self.on_outbox_ack(id, None); }
            if let Some(vault) = &self.vault { self.store.hydrate_id(vault, &jid, id); }
            if self.store.apply_receipt(&jid, id, participant, status, timestamp) {
                changed.push(id.clone());
            }
        }
        if Some(&jid) == self.current_jid.as_ref() {
            let patches: Vec<(String, &'static str, bool)> = changed
                .iter()
                .filter_map(|id| {
                    self.store
                        .messages_for(&jid)
                        .iter()
                        .find(|m| &m.id == id)
                        .map(|m| {
                            let (ticks, blue) = ticks_for(m);
                            (id.clone(), ticks, blue)
                        })
                })
                .collect();
            for (id, ticks, blue) in patches {
                self.patch_row(&id, |row| {
                    row.ticks = ticks.into();
                    row.ticksBlue = blue;
                });
            }
        }
        if !changed.is_empty() {
            self.schedule_refresh_chats();
        }
        self.schedule_save();
    }

    pub fn on_chat_presence(&mut self, chat: &str, sender: &str, state: &str, media: &str) {
        let jid = self.store.canon_owned(&normalize_jid(chat));
        let sender = self.store.canon_owned(&normalize_jid(sender));
        if sender.is_empty() || self.store.self_jids.contains(&sender) { return; }
        if !state.contains("Composing") {
            self.clear_participant_typing(&jid, &sender);
            return;
        }
        let text = if media.contains("Audio") {
            t("presence.recording")
        } else {
            t("presence.typing")
        };
        self.typing_gen += 1;
        let generation = self.typing_gen;
        self.chat_typing.entry(jid.clone()).or_default().insert(sender.clone(), (generation, text));
        self.update_typing_status();
        self.schedule_refresh_chats();
        self.once(25_000, move |b| {
            if b.chat_typing.get(&jid).and_then(|members| members.get(&sender)).is_some_and(|(token, _)| *token == generation) {
                b.clear_participant_typing(&jid, &sender);
            }
        });
    }

    pub(super) fn update_typing_status(&mut self) {
        let text = if let Some(jid) = self.current_jid.as_ref().filter(|jid| is_group(jid)) {
            let typing = self.chat_typing.get(jid);
            if let Some(members) = typing.filter(|members| !members.is_empty()) {
                let (participants, extra) = typing_participants(members);
                let names: Vec<_> = participants.iter().map(|jid| self.store.chat_name(jid)).collect();
                group_typing_status(&names, extra > 0)
            } else {
                self.store.members_of(jid).iter().map(|member| {
                    let canon = self.store.canon(&member.jid);
                    let name = self.store.chat_name(canon);
                    if name == display_id(canon) && !member.name.trim().is_empty() {
                        clean_text(&member.name)
                    } else { name }
                }).collect::<Vec<_>>().join(", ")
            }
        } else {
            self.current_jid.as_ref()
            .and_then(|jid| self.chat_typing.get(jid))
            .and_then(|members| members.values().max_by_key(|(token, _)| token))
            .map(|(_, text)| text.clone())
            .unwrap_or_else(|| if self.peer_online { t("presence.online") } else { String::new() })
        };
        self.ui.set_current_status(text.into());
        self.refresh_typing_members();
    }

    pub(super) fn refresh_typing_members(&mut self) {
        let (participants, extra) = self.current_jid.as_ref().filter(|jid| is_group(jid))
            .and_then(|jid| self.chat_typing.get(jid))
            .map(typing_participants).unwrap_or_default();
        let mut rows = Vec::new();
        for jid in participants.into_iter().take(5) {
            let name = self.store.chat_name(&jid);
            rows.push(self.to_member_row(&jid, &name, "", false, false));
            self.queue_avatar(&jid);
        }
        self.ui.set_typing_members(ModelRc::new(VecModel::from(rows)));
        self.ui.set_typing_extra(extra);
    }

    pub(super) fn clear_participant_typing(&mut self, jid: &str, sender: &str) {
        let sender = self.store.canon_owned(&normalize_jid(sender));
        if let Some(members) = self.chat_typing.get_mut(jid) {
            members.remove(&sender);
            if members.is_empty() { self.chat_typing.remove(jid); }
        }
        self.schedule_refresh_chats();
        self.update_typing_status();
    }

    pub(super) fn clear_chat_typing(&mut self, jid: &str) {
        if self.chat_typing.remove(jid).is_some() {
            self.schedule_refresh_chats();
        }
        self.update_typing_status();
    }

    pub fn on_presence(&mut self, from: &str, available: bool) {
        let jid = self.store.canon_owned(&normalize_jid(from));
        if Some(&jid) == self.current_jid.as_ref() {
            self.peer_online = available;
        }
        if !available {
            let groups: Vec<_> = self.chat_typing.iter()
                .filter(|(chat, members)| is_group(chat) && members.contains_key(&jid))
                .map(|(chat, _)| chat.clone()).collect();
            for group in groups { self.clear_participant_typing(&group, &jid); }
            self.clear_chat_typing(&jid);
        } else {
            self.update_typing_status();
        }
    }

    pub fn on_chat_flag(&mut self, jid: &str, pinned: Option<i64>, archived: Option<bool>) {
        let jid = self.store.canon_owned(&normalize_jid(jid));
        self.store.upsert_chat(&jid, None, 0, None, pinned, archived);
        self.schedule_refresh_chats();
    }

    pub fn on_star(&mut self, jid: &str, id: &str, starred: bool) {
        let jid = self.store.canon_owned(&normalize_jid(jid));
        if let Some(vault) = &self.vault { self.store.hydrate_id(vault, &jid, id); }
        if self.store.set_starred(&jid, id, starred) && Some(&jid) == self.current_jid.as_ref() {
            self.patch_row(id, |row| row.starred = starred);
        }
        self.schedule_save();
    }

    // Another device of ours read (or played) these messages: nothing is
    // unread here for that chat any more.
    pub fn on_read_elsewhere(&mut self, chat: &str, ids: &[String]) {
        let jid = self.store.canon_owned(&normalize_jid(chat));
        if ids.is_empty() { return; }
        if let Some(vault) = &self.vault { for id in ids { self.store.hydrate_id(vault, &jid, id); } }
        let newest_incoming = self.store.messages_for(&jid).iter().rev().find(|m| !m.from_me && m.kind != MessageKind::System).map(|m| m.id.clone());
        if let Some(newest) = newest_incoming && ids.contains(&newest) {
            self.on_mark_read(&jid, true);
        } else if let Some(meta) = self.store.chats.get_mut(&jid) {
            meta.unread = meta.unread.saturating_sub(ids.len() as u32);
            if meta.unread == 0 { meta.mentioned = false; }
            self.schedule_refresh_chats();
        }
    }

    pub fn on_mark_read(&mut self, jid: &str, read: bool) {
        let jid = self.store.canon_owned(&normalize_jid(jid));
        if let Some(meta) = self.store.chats.get_mut(&jid) {
            if read {
                meta.unread = 0;
                meta.mentioned = false;
            } else if meta.unread == 0 {
                meta.unread = 1;
            }
        }
        self.schedule_refresh_chats();
    }

    pub fn on_contact(&mut self, jid: &str, name: &str) {
        self.store
            .upsert_contact(&normalize_jid(jid), Some(name), None);
        self.update_typing_status();
        self.schedule_refresh_chats();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn group_header_typing_uses_first_names_and_plural_forms() {
        let names = vec!["Ana Silva".to_string(), "  Bruno Costa".to_string()];
        assert_eq!(group_typing_status(&names[..1], false), ta("presence.group_one", &["Ana"]));
        assert_eq!(group_typing_status(&names, false), ta("presence.group_some", &["Ana, Bruno"]));
        assert_eq!(group_typing_status(&names, true), t("presence.group_many"));
        let names = vec!["Ana Silva".to_string(); 5];
        assert_eq!(group_typing_status(&names, false), ta("presence.group_some", &["Ana, Ana, Ana, Ana, Ana"]));
    }

    #[test]
    fn typing_bubbles_limit_avatars_and_count_remaining_people() {
        for count in [0, 1, 5, 6, 12] {
            let members = (0..count).rev().map(|i| {
                (format!("{i:02}@s.whatsapp.net"), (i as u64, String::new()))
            }).collect();
            let (visible, extra) = typing_participants(&members);
            assert_eq!(visible.len(), count.min(5));
            assert_eq!(extra, count.saturating_sub(5) as i32);
            assert!(visible.windows(2).all(|pair| pair[0] < pair[1]));
        }
    }

    #[test]
    fn typing_members_refresh_independently() {
        let mut members = HashMap::from([
            ("a".to_string(), (1, String::new())),
            ("b".to_string(), (2, String::new())),
        ]);
        members.insert("a".into(), (3, String::new()));
        assert_eq!(members.len(), 2);
        members.remove("a");
        assert_eq!(typing_participants(&members).0, vec!["b".to_string()]);
    }
}
