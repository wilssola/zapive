// Contact and group info, chat settings, shared history and the member picker.
use super::*;

// How long a fetched roster is trusted before the next open refreshes it.
pub(super) const GROUP_META_TTL: Duration = Duration::from_secs(10 * 60);

impl Bridge {
    // ---- share group history with a newcomer ----

    pub(super) fn open_share_dialog(&mut self, group: &str, member: &str) {
        let name = self.store.chat_name(member);
        let number = format_number(member);
        let avatar = self.avatar_for(member);
        self.queue_avatar(member);
        self.share_target = Some((group.to_string(), member.to_string()));
        self.ui.set_share_name(
            if number.is_empty() || name == number {
                name.clone()
            } else {
                number.clone()
            }
            .into(),
        );
        self.ui.set_share_has_avatar(avatar.is_some());
        self.ui.set_share_avatar(avatar.unwrap_or_else(empty_image));
        self.ui.set_share_initial(initial_of(&name).into());
        self.ui.set_share_color_idx(color_idx_of(member));
        self.ui.set_share_sending(false);
        self.ui.set_share_open(true);
    }

    pub(super) fn share_history_send(&mut self) {
        let Some((group, member)) = self.share_target.clone() else {
            return;
        };
        self.warm_chat(&group);
        // The last few days of the group, newest 100 messages, as the
        // WebMessageInfo records a history payload is made of.
        let cutoff = now_secs() - 7 * 24 * 3600;
        let mut messages: Vec<wa::WebMessageInfo> = self
            .store
            .messages_for(&group)
            .iter()
            .rev()
            .filter(|m| m.kind != MessageKind::System && !m.deleted && m.timestamp >= cutoff)
            .filter_map(|m| {
                let raw = m.raw.as_deref()?;
                use whatsapp_rust::waproto::buffa::MessageField;
                Some(wa::WebMessageInfo {
                    key: MessageField::some(wa::MessageKey {
                        remote_jid: Some(group.clone()),
                        from_me: Some(m.from_me),
                        id: Some(m.id.clone()),
                        participant: Some(if m.from_me {
                            self.self_jid.clone()
                        } else {
                            m.sender_jid.clone()
                        }),
                    }),
                    message: MessageField::some(raw.clone()),
                    message_timestamp: Some(m.timestamp as u64),
                    push_name: if m.sender.is_empty() {
                        None
                    } else {
                        Some(m.sender.clone())
                    },
                    ..Default::default()
                })
            })
            .take(100)
            .collect();
        messages.reverse();
        if messages.is_empty() {
            self.share_target = None;
            self.ui.set_share_open(false);
            return;
        }
        self.ui.set_share_sending(true);
        self.wa.send(Cmd::ShareHistory {
            group,
            member,
            messages,
        });
    }

    pub fn on_share_result(&mut self, ok: bool) {
        self.ui.set_share_sending(false);
        self.ui.set_share_open(false);
        if let Some((group, member)) = self.share_target.take()
            && ok
        {
            let who = crate::wa_map::notice_name(&self.store, &member);
            let body = ta("notice.history_shared", &[&who]);
            self.add_system_message(
                &group,
                &format!("sys:shared:{}", now_secs()),
                now_secs(),
                body,
            );
        }
    }

    // ---- channels & info panel ----

    // Channels arrive as jids only; ask the server for their names.
    pub(super) fn resolve_channel_names(&mut self) {
        let wanted: Vec<String> = self
            .store
            .sorted_chats()
            .into_iter()
            .filter(|meta| is_channel(&meta.jid) && meta.name.is_empty())
            .map(|meta| meta.jid.clone())
            .filter(|jid| !self.requested_channels.contains(jid))
            .collect();
        for jid in wanted {
            self.requested_channels.insert(jid.clone());
            self.wa.send(Cmd::FetchChannel(jid));
        }
    }

    pub fn on_channel_meta(
        &mut self,
        jid: &str,
        name: &str,
        avatar: Option<crate::media::Decoded>,
    ) {
        if !name.is_empty() {
            self.store.set_name(jid, name);
        }
        if let Some(decoded) = avatar {
            let image = image_of(&decoded);
            self.avatars.insert(jid.to_string(), Some(image.clone()));
            self.patch_avatar_everywhere(jid, &image);
        }
        self.schedule_refresh_chats();
    }

    pub(super) fn open_contact_info(&mut self) {
        let Some(jid) = self.current_jid.clone() else {
            return;
        };
        let group = is_group(&jid);
        let name = self.store.chat_name(&jid);
        let avatar = self.avatar_for(&jid);
        let archived = self
            .store
            .chats
            .get(&jid)
            .map(|c| c.archived)
            .unwrap_or(false);
        self.ui.set_info_name(name.clone().into());
        self.ui.set_info_id(display_id(&jid).into());
        self.ui.set_info_about("".into());
        self.ui.set_info_desc("".into());
        self.ui.set_info_is_group(group);
        self.ui.set_info_members("".into());
        self.ui.set_info_common_groups(ModelRc::default());
        self.ui
            .set_info_common_groups_state(if group { "" } else { "loading" }.into());
        if !group {
            let contact = self.store.canon_owned(&jid);
            let cached = self.common_groups_cache.get(&contact).cloned();
            if let Some((_, groups)) = &cached {
                self.paint_common_groups(groups);
            }
            let fresh = cached
                .as_ref()
                .is_some_and(|(at, _)| at.elapsed() < Duration::from_secs(600));
            let pending = self
                .common_groups_pending
                .get(&contact)
                .is_some_and(|at| at.elapsed() < Duration::from_secs(60));
            if !fresh && !pending {
                let mut identities = vec![jid.clone(), contact.clone()];
                identities.extend(
                    self.store
                        .aliases
                        .iter()
                        .filter(|(_, phone)| **phone == contact)
                        .map(|(lid, _)| lid.clone()),
                );
                identities.sort();
                identities.dedup();
                self.common_groups_pending.insert(contact, Instant::now());
                self.wa.send(Cmd::FetchCommonGroups {
                    jid: jid.clone(),
                    identities,
                });
            }
        }
        self.ui.set_info_archived(archived);
        self.ui.set_info_has_avatar(avatar.is_some());
        self.ui.set_info_avatar(avatar.unwrap_or_else(empty_image));
        self.ui.set_info_initial(initial_of(&name).into());
        self.ui.set_info_color_idx(color_idx_of(&jid));
        self.ui.set_chat_menu_open(false);
        self.info_member_model.set_vec(Vec::new());
        // The 140px portrait wants a bigger cut than the list keeps.
        self.wa.send(Cmd::AvatarLarge(jid.clone()));
        if group {
            self.fill_info_members(&jid);
            self.group_meta_at.remove(&jid);
            self.ensure_group_meta(&jid);
        }
        // Shared media: the last 12 pictures already decoded.
        let cells: Vec<StickerCell> = self
            .store
            .messages_for(&jid)
            .iter()
            .rev()
            .filter(|m| m.kind == MessageKind::Image && !m.sticker)
            .filter_map(|m| {
                self.decoded.get(&m.id).map(|(img, _, _)| StickerCell {
                    id: m.id.clone().into(),
                    pic: img.clone(),
                    ready: true,
                })
            })
            .take(12)
            .collect();
        let grid: Vec<ModelRc<StickerCell>> = cells
            .chunks(4)
            .map(|chunk| ModelRc::from(Rc::new(VecModel::from(chunk.to_vec()))))
            .collect();
        self.info_media_model.set_vec(grid);
        self.ui.set_info_open(true);
        self.wa.send(Cmd::FetchChatInfo { jid, group });
    }

    pub fn on_chat_info(&mut self, jid: &str, about: &str, desc: &str, members: usize) {
        if Some(&jid.to_string()) != self.current_jid.as_ref() || !self.ui.get_info_open() {
            return;
        }
        self.ui.set_info_about(about.into());
        self.ui.set_info_desc(desc.into());
        if members > 0 {
            self.ui
                .set_info_members(ta("info.members", &[&members.to_string()]).into());
        }
    }

    pub fn on_common_groups(&mut self, jid: &str, groups: Option<Vec<(String, String)>>) {
        let contact = self.store.canon_owned(jid);
        self.common_groups_pending.remove(&contact);
        if let Some(groups) = &groups {
            self.common_groups_cache
                .insert(contact.clone(), (Instant::now(), groups.clone()));
        }
        let visible = self
            .current_jid
            .as_deref()
            .is_some_and(|current| self.store.canon(current) == contact)
            && self.ui.get_info_open()
            && !self.ui.get_info_is_group();
        if !visible {
            return;
        }
        if let Some(groups) = groups {
            self.paint_common_groups(&groups);
        } else if !self.common_groups_cache.contains_key(&contact) {
            self.ui.set_info_common_groups_state("error".into());
        }
    }

    pub(super) fn paint_common_groups(&self, groups: &[(String, String)]) {
        let mut rows: Vec<MemberItem> = groups
            .iter()
            .map(|(jid, name)| self.to_member_row(jid, name, "", false, false))
            .collect();
        rows.sort_by_key(|row| row.name.to_lowercase());
        self.ui
            .set_info_common_groups(ModelRc::from(Rc::new(VecModel::from(rows))));
        self.ui.set_info_common_groups_state("ready".into());
    }

    pub(super) fn toggle_archive(&mut self) {
        let Some(jid) = self.current_jid.clone() else {
            return;
        };
        let next = !self
            .store
            .chats
            .get(&jid)
            .map(|c| c.archived)
            .unwrap_or(false);
        if let Some(meta) = self.store.chats.get_mut(&jid) {
            meta.archived = next;
        }
        self.ui.set_info_archived(next);
        self.wa.send(Cmd::Archive {
            jid,
            archived: next,
        });
        self.schedule_refresh_chats();
    }

    pub(super) fn clear_current_chat(&mut self) {
        let Some(jid) = self.current_jid.clone() else {
            return;
        };
        self.clear_chat(&jid, true);
        self.ui.set_info_open(false);
    }

    // Empties a conversation here and, when asked, on the phone too.
    pub(super) fn clear_chat(&mut self, jid: &str, remote: bool) {
        self.translation_cache.retain(|key, _| key.0 != jid);
        self.store.clear_messages(jid);
        if Some(jid) == self.current_jid.as_deref() {
            self.messages_model.set_vec(Vec::new());
            self.select_end();
        }
        if remote {
            self.wa.send(Cmd::ClearChat(jid.to_string()));
        }
        self.schedule_refresh_chats();
    }

    // The phone cleared or deleted a chat; mirror it.
    pub fn on_chat_cleared(&mut self, jid: &str, deleted: bool) {
        let jid = self.store.canon_owned(&normalize_jid(jid));
        if deleted {
            self.translation_cache.retain(|key, _| key.0 != jid);
            self.store.delete_chat(&jid);
            if Some(&jid) == self.current_jid.as_ref() {
                self.close_chat();
            }
            self.schedule_refresh_chats();
        } else {
            self.clear_chat(&jid, false);
        }
    }

    pub(super) fn close_chat(&mut self) {
        self.save_composer(true);
        self.ui.invoke_set_composer("".into(), 0);
        self.clear_reply();
        self.mentions.clear();
        self.ui.set_mention_open(false);
        self.schedule_refresh_chats();
        self.ui.set_message_menu_open(false);
        self.ui.set_details_open(false);
        self.ui.set_message_dialog_open(false);
        self.select_end();
        self.ui.set_chat_menu_open(false);
        self.ui.set_chat_search_open(false);
        self.ui.set_info_open(false);
        self.ui.set_chat_open(false);
        self.ui.set_selected_jid("".into());
        self.cancel_preview();
        self.current_jid = None;
        self.ui.set_typing_members(ModelRc::default());
        self.ui.set_typing_extra(0);
        self.unread_boundary = None;
        self.messages_model.set_vec(Vec::new());
        self.sync_mini_player();
    }

    // ---- chat settings (mute, favorite, lists, disappearing) ----

    // Pushes the open chat's flags to the header menu.
    pub(super) fn apply_chat_settings_ui(&mut self) {
        let Some(jid) = self.current_jid.clone() else {
            return;
        };
        let meta = self.store.chats.get(&jid).cloned().unwrap_or_default();
        self.ui.set_current_is_group(is_group(&jid));
        self.ui
            .set_current_can_send(meta.can_send() && !is_channel(&jid));
        self.ui.set_current_left(meta.left);
        self.ui.set_current_muted(meta.is_muted());
        self.ui.set_current_mute_label(
            if meta.muted == -1 {
                t("mute.always")
            } else if meta.is_muted() {
                ta("mute.until", &[&format_stamp(meta.muted)])
            } else {
                String::new()
            }
            .into(),
        );
        self.ui.set_current_favorite(meta.favorite);
        self.ui.set_current_ephemeral(meta.ephemeral as i32);
        self.ui.set_current_ephemeral_label(
            if meta.ephemeral > 0 {
                crate::wa_map::ephemeral_label(meta.ephemeral)
            } else {
                String::new()
            }
            .into(),
        );
        self.ui.set_current_admin(meta.admin);
    }

    // Groups are refreshed on open, at most once per TTL.
    pub(super) fn ensure_group_meta(&mut self, jid: &str) {
        if !is_group(jid) {
            return;
        }
        let fresh = self
            .group_meta_at
            .get(jid)
            .is_some_and(|at| at.elapsed() < GROUP_META_TTL);
        if fresh {
            return;
        }
        self.group_meta_at.insert(jid.to_string(), Instant::now());
        self.wa.send(Cmd::FetchGroupMeta(jid.to_string()));
    }

    pub fn on_group_meta(&mut self, snap: GroupSnapshot) {
        let jid = self.store.canon_owned(&normalize_jid(&snap.jid));
        if !snap.subject.is_empty() {
            self.store.set_name(&jid, &snap.subject);
        }
        // The roster pairs every LID with its phone number: that is what
        // turns "@2785...@lid" mentions into names.
        let mut learned = false;
        let mut with_phone = 0;
        for m in &snap.members {
            if m.jid.ends_with("@lid") && !m.phone.is_empty() {
                with_phone += 1;
                learned |= self.store.learn_alias(&m.jid, &m.phone);
            }
            // A server-side display name fills in for people who never
            // wrote here, so their mentions read as names.
            if !m.name.is_empty() {
                let canon = self.store.canon_owned(&m.jid);
                if !self.store.contacts.contains_key(&canon)
                    && !self.store.push_names.contains_key(&canon)
                {
                    self.store.push_names.insert(canon, m.name.clone());
                    learned = true;
                }
            }
        }
        println!(
            "[group] {} roster: {} members, {} with a phone, announce={}",
            jid,
            snap.members.len(),
            with_phone,
            snap.announce
        );
        self.store.set_members(&jid, snap.members);
        self.refresh_message_details();
        self.schedule_save();
        let admin = self.store.self_is_admin(&jid);
        if let Some(meta) = self.store.chats.get_mut(&jid) {
            meta.announce = snap.announce;
            meta.admin = admin;
            meta.ephemeral = snap.ephemeral;
            meta.left = false;
        }
        if Some(&jid) == self.current_jid.as_ref() {
            self.apply_header(&jid);
            self.apply_chat_settings_ui();
            if self.ui.get_info_open() {
                self.ui.set_info_desc(snap.desc.clone().into());
                self.fill_info_members(&jid);
            }
            // Newly paired identities may name mentions that were drawn
            // as bare numbers.
            if learned && self.ui.get_stick_bottom() {
                self.rebuild_conversation(&jid);
                self.scroll_to_end();
            }
        }
        self.schedule_refresh_chats();
    }

    // A group notification: a notice in the conversation plus whatever
    // it changes about the roster or the settings.
    pub fn on_group_update(&mut self, change: GroupChange) {
        let jid = self.store.canon_owned(&normalize_jid(&change.jid));
        let who: Vec<String> = change
            .who
            .iter()
            .map(|(j, pn)| {
                if let Some(pn) = pn {
                    self.store.learn_alias(j, pn);
                }
                self.store.canon_owned(j)
            })
            .collect();
        if let (Some(actor), Some(pn)) = (&change.actor, &change.actor_pn) {
            self.store.learn_alias(actor, pn);
        }
        let actor = change.actor.as_ref().map(|a| self.store.canon_owned(a));
        let me_involved = who.iter().any(|w| self.store.self_jids.contains(w));
        match change.kind {
            "add" | "remove" | "promote" | "demote" => {
                if !self.store.patch_members(&jid, &who, change.kind) {
                    self.group_meta_at.remove(&jid);
                    self.ensure_group_meta(&jid);
                }
                if me_involved {
                    let admin = self.store.self_is_admin(&jid);
                    if let Some(meta) = self.store.chats.get_mut(&jid) {
                        meta.admin = admin;
                        if change.kind == "remove" {
                            meta.left = true;
                        } else if change.kind == "add" {
                            meta.left = false;
                        }
                    }
                }
            }
            "subject" => self.store.set_name(&jid, &change.text),
            "announce" | "not_announce" => {
                if let Some(meta) = self.store.chats.get_mut(&jid) {
                    meta.announce = change.kind == "announce";
                }
            }
            "ephemeral" => {
                if let Some(meta) = self.store.chats.get_mut(&jid) {
                    meta.ephemeral = change.number;
                }
            }
            _ => {}
        }
        let kind = if change.kind == "add" && change.reason == "invite" {
            "invite"
        } else {
            change.kind
        };
        let body = crate::wa_map::group_notice(
            &self.store,
            kind,
            actor.as_deref(),
            &who,
            &change.text,
            change.number,
        );
        if !body.is_empty() {
            let invite =
                change.kind == "invite" || (change.kind == "add" && change.reason == "invite");
            let id = if invite && who.len() == 1 && !me_involved {
                format!("sys:invite:{}:{}", change.ts, who[0])
            } else {
                format!("sys:{}:{}:{}", change.ts, change.kind, who.join(","))
            };
            self.add_system_message(&jid, &id, change.ts, body);
        }
        if Some(&jid) == self.current_jid.as_ref() {
            self.apply_header(&jid);
            self.apply_chat_settings_ui();
            if self.ui.get_info_open() {
                self.fill_info_members(&jid);
            }
        }
        self.schedule_refresh_chats();
    }

    // Stores a notice and shows it if the chat is open.
    pub(super) fn add_system_message(&mut self, jid: &str, id: &str, ts: i64, body: String) {
        if !self.store.chats.contains_key(jid) {
            self.store.upsert_chat(jid, None, 0, None, None, None);
        }
        let ts = if ts > 0 { ts } else { now_secs() };
        let stored = crate::wa_map::system_message(jid, id, ts, body);
        if self.store.add_message(stored) && Some(jid) == self.current_jid.as_deref() {
            self.push_message_row(jid, id);
        }
    }

    pub fn on_group_left(&mut self, jid: &str) {
        let jid = self.store.canon_owned(&normalize_jid(jid));
        if let Some(meta) = self.store.chats.get_mut(&jid) {
            meta.left = true;
        }
        let body = t("notice.you_left");
        self.add_system_message(&jid, &format!("sys:left:{}", now_secs()), now_secs(), body);
        if Some(&jid) == self.current_jid.as_ref() {
            self.apply_chat_settings_ui();
        }
        self.schedule_refresh_chats();
    }

    pub fn on_mute(&mut self, jid: &str, muted: i64) {
        let jid = self.store.canon_owned(&normalize_jid(jid));
        self.store.set_muted(&jid, muted);
        if Some(&jid) == self.current_jid.as_ref() {
            self.apply_chat_settings_ui();
        }
        self.schedule_refresh_chats();
    }

    // `hours`: 0 unmutes, -1 mutes for good, otherwise mutes that long.
    pub(super) fn mute_current(&mut self, hours: i32) {
        let Some(jid) = self.current_jid.clone() else {
            return;
        };
        self.ui.set_chat_menu_open(false);
        let muted = match hours {
            0 => 0,
            h if h < 0 => -1,
            h => now_secs() + h as i64 * 3600,
        };
        if let Some(meta) = self.store.chats.get_mut(&jid) {
            meta.muted = muted;
        }
        match muted {
            0 => self.wa.send(Cmd::Unmute(jid)),
            -1 => self.wa.send(Cmd::Mute { jid, until_ms: 0 }),
            until => self.wa.send(Cmd::Mute {
                jid,
                until_ms: until * 1000,
            }),
        }
        self.apply_chat_settings_ui();
        self.schedule_refresh_chats();
    }

    pub(super) fn set_ephemeral_current(&mut self, secs: u32) {
        let Some(jid) = self.current_jid.clone() else {
            return;
        };
        self.ui.set_chat_menu_open(false);
        if let Some(meta) = self.store.chats.get_mut(&jid) {
            meta.ephemeral = secs;
        }
        let body = crate::wa_map::group_notice(
            &self.store,
            "ephemeral",
            Some(&self.self_jid.clone()),
            &[],
            "",
            secs,
        );
        self.add_system_message(&jid, &format!("sys:eph:{}", now_secs()), now_secs(), body);
        self.wa.send(Cmd::SetEphemeral {
            jid,
            group: is_group(self.current_jid.as_deref().unwrap_or("")),
            secs,
        });
        self.apply_chat_settings_ui();
    }

    pub(super) fn toggle_favorite(&mut self) {
        let Some(jid) = self.current_jid.clone() else {
            return;
        };
        self.ui.set_chat_menu_open(false);
        let next = !self
            .store
            .chats
            .get(&jid)
            .map(|c| c.favorite)
            .unwrap_or(false);
        if let Some(meta) = self.store.chats.get_mut(&jid) {
            meta.favorite = next;
        }
        let all: Vec<String> = self
            .store
            .chats
            .values()
            .filter(|c| c.favorite)
            .map(|c| c.jid.clone())
            .collect();
        self.wa.send(Cmd::Favorites(all));
        self.apply_chat_settings_ui();
        self.schedule_refresh_chats();
    }

    pub(super) fn toggle_list(&mut self, id: &str) {
        let Some(jid) = self.current_jid.clone() else {
            return;
        };
        let on = !self
            .store
            .label_chats
            .get(id)
            .map(|set| set.contains(&jid))
            .unwrap_or(false);
        if self.store.set_label_chat(id, &jid, on) {
            self.wa.send(Cmd::LabelChat {
                label_id: id.to_string(),
                jid,
                on,
            });
        }
        self.refresh_labels();
        self.schedule_refresh_chats();
    }

    pub(super) fn create_list(&mut self, name: &str) {
        let name = name.trim();
        if name.is_empty() {
            return;
        }
        let id = self.store.next_label_id();
        let order = self.store.labels.len() as i32;
        let label = Label {
            id: id.clone(),
            name: name.to_string(),
            color: order % 20,
            order,
            list_type: Some(5),
        };
        self.store.upsert_label(label);
        self.wa.send(Cmd::LabelCreate {
            id: id.clone(),
            name: name.to_string(),
            color: order % 20,
            order,
        });
        // A list made from a chat's menu starts with that chat in it.
        if let Some(jid) = self.current_jid.clone()
            && self.store.set_label_chat(&id, &jid, true)
        {
            self.wa.send(Cmd::LabelChat {
                label_id: id,
                jid,
                on: true,
            });
        }
        self.refresh_labels();
        self.schedule_refresh_chats();
    }

    pub(super) fn delete_list(&mut self, id: &str) {
        self.store.remove_label(id);
        self.wa.send(Cmd::LabelDelete(id.to_string()));
        if self.tab == format!("label:{id}") {
            self.tab = "all".into();
            self.ui.set_chat_tab("all".into());
        }
        self.refresh_labels();
        self.schedule_refresh_chats();
    }

    pub fn on_label(
        &mut self,
        id: &str,
        name: &str,
        color: i32,
        order: i32,
        deleted: bool,
        list_type: Option<i32>,
    ) {
        if deleted {
            self.store.remove_label(id);
        } else if !name.is_empty() {
            let list_type =
                list_type.or_else(|| self.store.labels.get(id).and_then(|l| l.list_type));
            self.store.upsert_label(Label {
                id: id.to_string(),
                name: name.to_string(),
                color,
                order,
                list_type,
            });
        }
        self.refresh_labels();
        self.schedule_refresh_chats();
    }

    pub fn on_label_chat(&mut self, id: &str, jid: &str, on: bool) {
        let jid = self.store.canon_owned(&normalize_jid(jid));
        self.store.set_label_chat(id, &jid, on);
        self.refresh_labels();
        self.schedule_refresh_chats();
    }

    // ---- member picker (add to group) ----

    pub(super) fn open_member_picker(&mut self) {
        let Some(jid) = self.current_jid.clone() else {
            return;
        };
        if !is_group(&jid) {
            return;
        }
        self.ui.set_chat_menu_open(false);
        self.member_picked.clear();
        self.ui.set_member_count(0);
        self.fill_member_rows("");
        self.ui.set_member_pick_open(true);
    }

    // Saved contacts not yet in the group, narrowed by the search box.
    pub(super) fn fill_member_rows(&mut self, query: &str) {
        let Some(jid) = self.current_jid.clone() else {
            return;
        };
        let q = crate::search::fold(query.trim());
        let digits: String = q.chars().filter(|c| c.is_ascii_digit()).collect();
        let in_group: HashSet<String> = self
            .store
            .members_of(&jid)
            .iter()
            .flat_map(|m| [self.store.canon_owned(&m.jid), m.phone.clone()])
            .collect();
        let mut candidates: Vec<String> = self
            .store
            .saved_contacts
            .iter()
            .chain(self.store.chats.keys())
            .filter(|j| j.ends_with("@s.whatsapp.net") && !in_group.contains(*j))
            .cloned()
            .collect();
        candidates.sort();
        candidates.dedup();
        let rows: Vec<MemberItem> = candidates
            .iter()
            .filter(|j| {
                if q.is_empty() {
                    return true;
                }
                crate::search::fold(&self.store.chat_name(j)).contains(&q)
                    || (!digits.is_empty() && j.contains(&digits))
            })
            .take(300)
            .map(|j| {
                let name = self.store.chat_name(j);
                self.to_member_row(
                    j,
                    &name,
                    &format_number(j),
                    false,
                    self.member_picked.contains(j),
                )
            })
            .collect();
        for row in &rows {
            self.queue_avatar(row.jid.as_str());
        }
        self.member_model.set_vec(rows);
    }

    pub(super) fn confirm_add_members(&mut self) {
        let Some(jid) = self.current_jid.clone() else {
            return;
        };
        self.ui.set_member_pick_open(false);
        let members: Vec<String> = self.member_picked.drain().collect();
        if members.is_empty() {
            return;
        }
        self.wa.send(Cmd::AddMembers { jid, members });
    }

    pub fn on_members_added(&mut self, jid: &str, failed: &[String]) {
        let jid = self.store.canon_owned(&normalize_jid(jid));
        self.group_meta_at.remove(&jid);
        self.ensure_group_meta(&jid);
        if !failed.is_empty() {
            let names: Vec<String> = failed.iter().map(|j| self.store.chat_name(j)).collect();
            self.ui
                .set_status_text(ta("members.add_failed", &[&names.join(", ")]).into());
        }
    }

    // Roster shown in the group info panel.
    pub(super) fn fill_info_members(&mut self, jid: &str) {
        let members: Vec<Member> = self.store.members_of(jid).to_vec();
        let mut rows: Vec<MemberItem> = members
            .iter()
            .map(|m| {
                let canon = self.store.canon_owned(&m.jid);
                let name = if self.store.self_jids.contains(&canon) {
                    t("reactions.you")
                } else {
                    self.store.chat_name(&canon)
                };
                let number = if m.phone.is_empty() {
                    format_number(&canon)
                } else {
                    format_number(&m.phone)
                };
                self.to_member_row(&canon, &name, &number, m.admin, false)
            })
            .collect();
        rows.sort_by(|a, b| {
            b.admin
                .cmp(&a.admin)
                .then(a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        });
        for row in &rows {
            self.queue_avatar(row.jid.as_str());
        }
        let count = rows.len();
        self.info_member_model.set_vec(rows);
        if count > 0 {
            self.ui
                .set_info_members(ta("info.members", &[&count.to_string()]).into());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn mute_for_a_chat_not_yet_in_the_store_is_kept() {
        const GROUP: &str = "120363000000000003@g.us";
        super::ui_thread::run(|done| {
            let ui = AppWindow::new().expect("create Slint component");
            wiring::install(&ui, WaService::detached());
            let kept = Cell::new(false);
            apply_now(|b| {
                b.on_mute(GROUP, -1);
                kept.set(b.store.chats.get(GROUP).is_some_and(|meta| meta.is_muted()));
            });
            drop(ui);
            done(if kept.get() {
                Ok(())
            } else {
                Err("mute for an unknown chat was dropped".to_string())
            });
        });
    }
}
