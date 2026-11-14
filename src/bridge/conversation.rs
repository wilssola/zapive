// The open conversation: header, rows, links and scrolling.
use super::*;

// A run of non-breaking spaces appended to the styled text (and its
// plain overlay) so the text engine wraps the last line early and
// leaves the bottom-right corner free for the floating time/ticks.
pub(super) const META_PAD: &str = "\u{00a0}\u{00a0}\u{00a0}\u{00a0}\u{00a0}\u{00a0}\u{00a0}\u{00a0}\u{00a0}\u{00a0}\u{00a0}\u{00a0}\u{00a0}\u{00a0}";

// Room the time cluster takes at the end of a bubble's text, as no-break
// spaces. A pinned mark or the "edited" tag lengthens the label, so they get
// more; keep the tiers in step with `meta-pad` in app-window.slint.
pub(super) fn meta_pad(m: &StoredMessage) -> String {
    let extra = if m.edited { 12 } else { 0 } + if m.pinned_until > now_secs() { 3 } else { 0 };
    META_PAD.to_string() + &"\u{00a0}".repeat(extra)
}

impl Bridge {
    // ---- conversation ----

    pub(super) fn apply_header(&mut self, jid: &str) {
        let name = self.store.chat_name(jid);
        let avatar = self.avatar_for(jid);
        self.ui.set_current_jid(jid.into());
        // Calls are one-to-one: groups, channels and status cannot ring.
        self.ui
            .set_current_callable(!is_group(jid) && !is_channel(jid) && jid != "status@broadcast");
        self.ui.set_current_chat_name(name.clone().into());
        self.ui.set_current_avatar_has(avatar.is_some());
        self.ui
            .set_current_avatar(avatar.unwrap_or_else(empty_image));
        self.ui.set_current_initial(initial_of(&name).into());
        self.ui.set_current_color_idx(color_idx_of(jid));
        self.update_typing_status();
    }

    // Opens (or starts) the conversation with a jid; used by the chat list
    // and by clicking a sender name inside a group.
    // Hydrates a chat and rotates the warm set, evicting the coldest.
    pub(super) fn warm_chat(&mut self, jid: &str) {
        let Some(vault) = self.vault.as_ref() else {
            return;
        };
        let jid = self.store.canon_owned(jid);
        self.store.hydrate(vault, &jid);
        self.warm_order.retain(|j| j != &jid);
        self.warm_order.push_back(jid);
        while self.warm_order.len() > WARM_MAX {
            let evictable = self.warm_order.iter().position(|j| {
                Some(j.as_str()) != self.current_jid.as_deref()
                    && !self.store.dirty_jids.contains(j)
            });
            let Some(idx) = evictable else { break };
            if let Some(old) = self.warm_order.remove(idx) {
                self.store.evict(&old);
            }
        }
    }

    // Mouse resting on a chat row usually precedes a click; preload the
    // tail so the open feels instant.
    pub fn chat_hover(&mut self, jid: &str) {
        if self.hover_jid == jid || self.store.hydrated.contains(self.store.canon(jid)) {
            return;
        }
        self.hover_jid = jid.to_string();
        let jid = jid.to_string();
        self.once(120, move |b| {
            if b.hover_jid == jid {
                b.warm_chat(&jid);
            }
        });
    }

    pub fn open_dm(&mut self, jid_raw: &str, jump_to: Option<&str>) {
        self.warm_chat(jid_raw);
        if jid_raw.is_empty() {
            return;
        }
        let jid = self.store.canon_owned(jid_raw);
        if !self.store.chats.contains_key(&jid) {
            self.store.upsert_chat(&jid, None, 0, None, None, None);
            self.refresh_chats();
        }
        self.open_jid(&jid, jump_to);
    }

    pub(super) fn open_jid(&mut self, jid: &str, jump_to: Option<&str>) {
        self.ui.invoke_clear_sidebar_search();
        if !self.search_text.is_empty() {
            self.search_text.clear();
            self.refresh_chats();
        }
        self.ui.set_message_menu_open(false);
        self.ui.set_details_open(false);
        self.ui.set_message_dialog_open(false);
        if let Some(current) = self.current_jid.clone()
            && current != jid
        {
            // Someone reading at the bottom expects the newest message next
            // time, not the offset that happened to be the end back then.
            if self.ui.get_stick_bottom() || self.truncated.contains(&current) {
                self.scroll_pos.remove(&current);
            } else {
                self.scroll_pos
                    .insert(current.clone(), self.ui.get_conv_scroll());
            }
            self.truncated.remove(&current);
            self.clear_reply();
        }
        let switching_chat = self.current_jid.as_deref() != Some(jid);
        if switching_chat {
            self.save_composer(true);
            self.clear_staged();
            self.cancel_preview();
            let unread = self.store.chats.get(jid).map_or(0, |meta| meta.unread) as usize;
            self.unread_boundary = if unread > 0 {
                let incoming: Vec<_> = self
                    .store
                    .messages_for(jid)
                    .iter()
                    .filter(|m| !m.from_me && m.kind != MessageKind::System)
                    .collect();
                incoming
                    .len()
                    .checked_sub(unread)
                    .and_then(|index| incoming.get(index))
                    .map(|m| m.id.clone())
            } else {
                None
            };
            self.select_end();
            self.ui.set_chat_search_open(false);
            self.chat_search_model.set_vec(Vec::new());
            self.ui.set_mention_open(false);
            self.mentions.clear();
        }
        self.current_jid = Some(jid.to_string());
        if switching_chat {
            let draft = self.store.drafts.get(jid).cloned().unwrap_or_default();
            self.mentions = draft.mentions;
            let cursor = draft.text.len() as i32;
            self.ui.invoke_set_composer(draft.text.into(), cursor);
            self.refresh_composer_mentions(None);
        }
        self.ui.set_selected_jid(jid.into());
        self.refresh_pinned();
        self.refresh_silenced_calls();
        self.ui.set_stick_bottom(true);
        self.ui.set_chat_menu_open(false);
        if let Some(meta) = self.store.chats.get_mut(jid) {
            meta.unread = 0;
            meta.mentioned = false;
        }
        self.send_view_receipts(jid);
        self.peer_online = false;
        self.update_typing_status();
        self.wa.send(Cmd::SubscribePresence(jid.to_string()));
        self.apply_header(jid);
        self.apply_chat_settings_ui();
        self.refresh_labels();
        self.ensure_group_meta(jid);
        let jump_loaded = jump_to.map(|target| {
            self.vault
                .as_ref()
                .is_some_and(|vault| self.store.load_around(vault, jid, target))
        });
        let at = jump_to.and_then(|target| {
            self.store
                .messages_for(jid)
                .iter()
                .position(|m| m.id == target)
        });
        if jump_to.is_some() && (jump_loaded == Some(false) || at.is_none()) {
            self.message_action_failed(&t("history.unavailable"));
        }
        let list = self.store.messages_for(jid);
        // Jumping to a message means starting the list there: the row
        // lands at the top exactly, instead of chasing a pixel offset
        // inside a virtualized list that estimates unmeasured heights.
        let from = at.unwrap_or(0);
        if at.is_some() {
            self.truncated.insert(jid.to_string());
        } else {
            self.truncated.remove(jid);
        }
        let rows: Vec<MessageItem> = list
            .iter()
            .enumerate()
            .skip(from)
            .map(|(i, m)| self.to_row(m, if i > 0 { list.get(i - 1) } else { None }))
            .collect();
        self.messages_model.set_vec(rows);
        self.initialize_message_focus(false);
        self.ui.set_chat_open(true);
        self.ui.invoke_focus_composer();
        self.ui.set_conv_ready(false);
        if at.is_some() {
            self.ui.set_stick_bottom(false);
            println!("[jump] {jid} at row {from}/{}", list.len());
        }
        self.apply_cached_stickers(jid);
        let anchored_top = at.is_some();
        self.ui.set_scroll_armed(!anchored_top);
        let saved = if anchored_top {
            Some(0.0)
        } else if jump_to.is_some() {
            None
        } else {
            self.scroll_pos.get(jid).copied()
        };
        // Anchor across a few layout passes while hidden, then reveal.
        for delay in [0u64, 40, 90] {
            let jid = jid.to_string();
            self.once(delay, move |b| {
                if b.ui.get_selected_jid() != jid {
                    return;
                }
                match saved {
                    Some(y) if y <= 0.0 => b.ui.invoke_set_conversation_scroll(y),
                    _ => b.ui.invoke_scroll_conversation_end(),
                }
            });
        }
        let selected_jid = jid.to_string();
        self.once(120, move |b| {
            if b.ui.get_selected_jid() != selected_jid {
                return;
            }
            b.initialize_message_focus(true);
            b.ui.set_conv_ready(true);
            // Clear anything the transition re-armed while the list still
            // held the previous chat's offset.
            b.ui.set_scroll_armed(!anchored_top);
        });
        self.schedule_refresh_chats();
        self.auto_translate_chat(jid);
        self.load_media_for_chat(jid);
        self.queue_row_avatars(jid);
        self.queue_avatar(jid);
        // Playback follows the user: the mini bar takes over elsewhere.
        self.sync_mini_player();
        // Thin conversation: ask the phone for this chat's older messages.
        let list = self.store.messages_for(jid);
        if is_channel(jid) && list.len() < 20 && !self.history_pending {
            self.set_pending(true);
            self.wa.send(Cmd::FetchHistory {
                jid: jid.to_string(),
                oldest_id: String::new(),
                from_me: false,
                ts_ms: 0,
            });
        } else if list.len() < 20
            && !self.history_pending
            && let Some(first) = list.first().filter(|m| m.raw.is_some())
        {
            let (id, from_me, ts) = (first.id.clone(), first.from_me, first.timestamp);
            self.set_pending(true);
            self.wa.send(Cmd::FetchHistory {
                jid: jid.to_string(),
                oldest_id: id,
                from_me,
                ts_ms: ts * 1000,
            });
        }
    }

    pub(super) fn rebuild_conversation(&mut self, jid: &str) {
        let focused_id = self.ui.invoke_snapshot_message_focus();
        let list = self.store.messages_for(jid);
        let rows: Vec<MessageItem> = list
            .iter()
            .enumerate()
            .map(|(i, m)| self.to_row(m, if i > 0 { list.get(i - 1) } else { None }))
            .collect();
        self.messages_model.set_vec(rows);
        self.apply_cached_stickers(jid);
        let jid = jid.to_string();
        self.once(40, move |b| {
            if b.ui.get_selected_jid() != jid {
                return;
            }
            b.restore_message_focus(focused_id.as_str());
        });
    }

    fn initialize_message_focus(&self, visible: bool) {
        let ids: Vec<_> = (0..self.messages_model.row_count())
            .filter_map(|i| self.messages_model.row_data(i))
            .map(|row| row.id.to_string())
            .collect();
        let index = initial_message_index(
            &ids,
            &self.row_geom,
            self.geom_epoch,
            self.ui.get_message_visible_height(),
            visible,
        );
        if let Some(index) = index {
            self.ui
                .invoke_initialize_message_focus(index as i32, ids[index].as_str().into());
        }
    }

    fn restore_message_focus(&self, id: &str) {
        if let Some(index) = message_focus_index(
            (0..self.messages_model.row_count())
                .filter_map(|i| self.messages_model.row_data(i))
                .map(|row| row.id.to_string()),
            id,
        ) {
            self.ui
                .invoke_restore_message_focus(index as i32, id.into());
        }
    }

    // Inserts freshly fetched history above the current rows and shifts
    // the viewport by exactly the height that was added, so the message
    // the user was reading stays put.
    pub(super) fn prepend_older_rows(&mut self, jid: &str) {
        let list = self.store.messages_for(jid);
        let first_shown = self.messages_model.row_data(0).map(|r| r.id.to_string());
        let cut = first_shown.and_then(|id| list.iter().position(|m| m.id == id));
        let Some(cut) = cut.filter(|&c| c > 0) else {
            return;
        };

        let focused_id = self.ui.invoke_snapshot_message_focus();
        let before_y = self.ui.get_conv_scroll();
        let before_h = self.ui.get_conv_viewport_h();
        self.ui.set_conv_ready(false);

        let older: Vec<MessageItem> = list
            .iter()
            .enumerate()
            .take(cut)
            .map(|(i, m)| self.to_row(m, if i > 0 { list.get(i - 1) } else { None }))
            .collect();
        for (i, row) in older.iter().enumerate() {
            self.messages_model.insert(i, row.clone());
        }
        // The row that used to be first now has a predecessor.
        let boundary = self.to_row(&list[cut], list.get(cut - 1));
        self.messages_model.set_row_data(cut, boundary);

        let jid_owned = jid.to_string();
        self.once(40, move |b| {
            if b.ui.get_selected_jid() != jid_owned {
                return;
            }
            let added = b.ui.get_conv_viewport_h() - before_h;
            b.ui.invoke_set_conversation_scroll(before_y - added);
            b.ui.set_conv_ready(true);
            b.restore_message_focus(focused_id.as_str());
            b.load_media_for_chat(&jid_owned);
            b.queue_row_avatars(&jid_owned);
        });
    }

    pub(super) fn push_message_row(&mut self, jid: &str, id: &str) {
        let shown: Vec<String> = (0..self.messages_model.row_count())
            .filter_map(|i| self.messages_model.row_data(i))
            .map(|row| row.id.to_string())
            .collect();
        let list = self.store.messages_for(jid);
        match plan_row(list, &shown, id) {
            RowPlan::Skip => return,
            RowPlan::Rebuild => {
                let from_me = list.iter().any(|m| m.id == id && m.from_me);
                self.rebuild_conversation(jid);
                if from_me || self.ui.get_stick_bottom() {
                    self.scroll_to_end();
                }
                return;
            }
            RowPlan::Append => {}
        }
        let Some(stored) = list.last() else { return };
        let prev = list.len().checked_sub(2).and_then(|i| list.get(i));
        let row = self.to_row(stored, prev);
        let from_me = stored.from_me;
        let needs_media = needs_media(stored);
        let stored = stored.clone();
        self.messages_model.push(row);
        // Don't yank the view down while the user is reading older rows.
        if from_me || self.ui.get_stick_bottom() {
            self.scroll_to_end();
        }
        if needs_media {
            self.request_automatic_media(&stored);
        }
        self.queue_row_avatars(jid);
    }

    pub(super) fn scroll_to_end(&mut self) {
        for delay in [0u64, 60, 220] {
            self.once(delay, |b| b.ui.invoke_scroll_conversation_end());
        }
    }

    // Formatted or mention-carrying messages render as styled text; plain
    // ones keep the selectable input.
    // The styled rendering, whether there is one, the same words as plain
    // text for the selectable overlay, and where the links are in those.
    pub(super) fn styled_for(
        &self,
        m: &StoredMessage,
    ) -> (slint::StyledText, bool, String, Vec<LinkSpan>) {
        let body = if m.deleted { "" } else { m.text.as_str() };
        let none = || {
            (
                slint::StyledText::from_plain_text(""),
                false,
                String::new(),
                Vec::new(),
            )
        };
        // File names and durations are literal, underscores and all.
        if matches!(
            m.kind,
            MessageKind::Doc | MessageKind::Audio | MessageKind::System
        ) {
            return none();
        }
        if body.is_empty() || !has_markup(body) {
            return none();
        }
        let resolve = self.mention_resolver(m);
        let md = format!(
            "{}{pad}",
            crate::markup::bubble_markdown(body, &resolve),
            pad = meta_pad(m)
        );
        match slint::StyledText::from_markdown(&md) {
            Ok(styled) => {
                let mut plain = render_plain(body, &resolve);
                let links = plain
                    .spans
                    .iter()
                    .map(|(start, end, target)| LinkSpan {
                        start: *start as i32,
                        end: *end as i32,
                        mention: !target.contains("://")
                            && ["@s.whatsapp.net", "@lid", "@g.us"]
                                .iter()
                                .any(|suffix| target.ends_with(suffix)),
                    })
                    .collect();
                // The overlay text must end with the same run as the styled
                // text above, or the glyphs drift apart.
                plain.text.push_str(&meta_pad(m));
                (styled, true, plain.text, links)
            }
            Err(_) => none(),
        }
    }

    // Names and chats for the "@number" tokens of one message.
    pub(super) fn mention_resolver<'a>(
        &'a self,
        m: &'a StoredMessage,
    ) -> impl Fn(&str) -> MentionTarget + 'a {
        let store = &self.store;
        move |num: &str| -> MentionTarget {
            // Tagging the whole group carries the group's own id.
            if m.jid.starts_with(num) && is_group(&m.jid) {
                return MentionTarget {
                    name: Some(t("mention.all")),
                    jid: m.jid.clone(),
                };
            }
            // The id is either a phone number or a LID; try both before
            // falling back on the shape of the number itself.
            for form in [format!("{num}@s.whatsapp.net"), format!("{num}@lid")] {
                let canon = store.canon_owned(&form);
                let name = store
                    .contacts
                    .get(&canon)
                    .or_else(|| {
                        store
                            .chats
                            .get(&canon)
                            .filter(|c| !c.name.is_empty())
                            .map(|c| &c.name)
                    })
                    .or_else(|| store.push_names.get(&canon));
                if let Some(name) = name {
                    return MentionTarget {
                        name: Some(clean_text(name)),
                        jid: canon,
                    };
                }
            }
            let guess = if num.len() > 13 {
                format!("{num}@lid")
            } else {
                format!("{num}@s.whatsapp.net")
            };
            MentionTarget {
                name: None,
                jid: store.canon_owned(&guess),
            }
        }
    }

    // A click (no drag) on the selectable overlay of a styled message:
    // opens whatever link or mention sits under that byte offset.
    pub(super) fn open_link_at(&mut self, id: &str, offset: usize) {
        let Some(m) = self.find_message(id).cloned() else {
            return;
        };
        let target = target_at(&m.text, offset, &self.mention_resolver(&m));
        if let Some(target) = target {
            if target.starts_with("http://") || target.starts_with("https://") {
                crate::platform::open_path(&target);
            } else {
                self.open_dm(&target, None);
            }
        }
    }

    pub(super) fn to_row(&self, m: &StoredMessage, prev: Option<&StoredMessage>) -> MessageItem {
        let group = is_group(&m.jid);
        // A run also breaks after a pause, so a long stream from one
        // sender still shows who is talking, like WhatsApp does.
        const RUN_GAP: i64 = 5 * 60;
        let first_of_run = prev
            .map(|p| {
                p.from_me != m.from_me
                    || (group && p.sender != m.sender)
                    || m.timestamp - p.timestamp > RUN_GAP
            })
            .unwrap_or(true);
        let sender_jid = if m.sender_jid.is_empty() {
            m.jid.clone()
        } else {
            self.store.canon_owned(&m.sender_jid)
        };
        let group_indent = group && !m.from_me && m.kind != MessageKind::System;
        let voice_jid = if m.is_voice_message() {
            if m.from_me {
                self.self_jid.clone()
            } else {
                sender_jid.clone()
            }
        } else {
            String::new()
        };
        let (pic_w, pic_h) = media_box(m);
        let (ticks, ticks_blue) = ticks_for(m);
        let (reactions, reaction_count) = reaction_summary(m);
        let day = format_day(m.timestamp);
        let day_label = if prev.map(|p| format_day(p.timestamp) != day).unwrap_or(true) {
            day.clone()
        } else {
            String::new()
        };
        let card = crate::wa_map::special_card(m);
        let (poll_options, poll_multi, poll_total, poll_can_vote) = self.poll_rows(m);
        let card_picture = card.thumbnail.and_then(card_thumbnail);
        let (styled, has_styled, plain, links) = self.styled_for(m);
        let link_host = host_of(&m.link_url);
        let system = m.kind == MessageKind::System;
        let outgoing = self.outbox.iter().find(|entry| entry.id == m.id);
        let preparing = self.preparation_states.get(&m.id).copied();
        let tr = self.translation_entry(m);
        let source = tr.map(|e| e.source.as_str()).unwrap_or("");
        MessageItem {
            poll_options: slint::ModelRc::new(VecModel::from(poll_options)),
            poll_multi,
            poll_total,
            poll_can_vote,
            translationText: tr
                .filter(|e| !e.hidden)
                .map(|e| e.text.as_str())
                .unwrap_or("")
                .into(),
            translationLabel: tr
                .filter(|e| !e.text.is_empty())
                .map(|e| {
                    format!(
                        "{} → {}",
                        translation_language_name(&e.source),
                        translation_language_name(&self.translation_settings.target)
                    )
                })
                .unwrap_or_default()
                .into(),
            translationBusy: tr.is_some_and(|e| e.busy),
            translationError: tr
                .filter(|e| !e.hidden)
                .map(|e| e.error.as_str())
                .unwrap_or("")
                .into(),
            translationSource: source.into(),
            translationPolicy: self
                .translation_settings
                .policies
                .get(source)
                .map(String::as_str)
                .unwrap_or("")
                .into(),
            sendPending: outgoing.is_some() || preparing.is_some(),
            sendRetry: preparing == Some(true)
                || outgoing.is_some_and(|entry| {
                    matches!(
                        entry.state,
                        crate::outbox::State::Failed | crate::outbox::State::Uncertain
                    )
                }),
            sendState: outgoing
                .map(|entry| {
                    t(match entry.state {
                        crate::outbox::State::Queued => "outbox.queued",
                        crate::outbox::State::Sending => "outbox.sending",
                        crate::outbox::State::Uncertain => "outbox.uncertain",
                        crate::outbox::State::Failed => "outbox.failed",
                    })
                })
                .unwrap_or_else(|| {
                    preparing
                        .map(|failed| {
                            t(if failed {
                                "outbox.failed"
                            } else {
                                "outbox.preparing"
                            })
                        })
                        .unwrap_or_default()
                })
                .into(),
            cardKind: card.kind.into(),
            cardInitial: initial_of(&card.title).into(),
            cardTitle: card.title.into(),
            cardBody: card.body.into(),
            cardDetail: card.detail.into(),
            cardOptions: ModelRc::new(VecModel::from(
                card.options
                    .into_iter()
                    .map(SharedString::from)
                    .collect::<Vec<_>>(),
            )),
            cardHasPicture: card_picture.is_some(),
            cardPicture: card_picture.unwrap_or_else(empty_image),
            id: m.id.clone().into(),
            kind: (if m.deleted {
                "text"
            } else {
                match m.kind {
                    MessageKind::Text => "text",
                    MessageKind::Image => "image",
                    MessageKind::Audio => "audio",
                    MessageKind::Doc => "doc",
                    MessageKind::Video => "video",
                    MessageKind::System => "system",
                }
            })
            .into(),
            text: if m.deleted {
                t("msg.deleted").into()
            } else {
                m.text.clone().into()
            },
            fromMe: m.from_me,
            sender: m.sender.clone().into(),
            showSender: group && !m.from_me && first_of_run && !system,
            dayFull: day.into(),
            selected: self.selected.iter().any(|s| s == &m.id),
            // The collapsed roster notice opens the group info on click;
            // an invite-link join offers to share the recent history.
            clickable: system
                && (m.text == t("notice.members_changed") || m.id.starts_with("sys:invite:")),
            // Saved contacts show their address-book name without the ~.
            senderLabel: if group && !sender_jid.is_empty() && !self.store.is_saved(&sender_jid) {
                format!("~ {}", self.store.chat_name(&sender_jid)).into()
            } else {
                self.store
                    .chat_name(if sender_jid.is_empty() {
                        &m.jid
                    } else {
                        &sender_jid
                    })
                    .into()
            },
            firstOfRun: first_of_run,
            time: format!(
                "{}{}{}",
                if m.pinned_until > now_secs() {
                    "⌖ "
                } else {
                    ""
                },
                format_clock(m.timestamp),
                if m.edited { " · edited" } else { "" }
            )
            .into(),
            // Rebuilt rows keep already-decoded media so scrollback and
            // re-opens draw instantly.
            picture: self
                .decoded
                .get(&m.id)
                .map(|(img, _, _)| img.clone())
                .or_else(|| self.media_previews.get(&m.id).cloned())
                .unwrap_or_else(empty_image),
            picW: self.decoded.get(&m.id).map(|&(_, w, _)| w).unwrap_or(pic_w),
            picH: self.decoded.get(&m.id).map(|&(_, _, h)| h).unwrap_or(pic_h),
            mediaPath: self
                .media_path
                .get(&m.id)
                .cloned()
                .unwrap_or_default()
                .into(),
            documentType: document_type(m).into(),
            mediaKind: "".into(),
            mediaOffer: false,
            mediaProgress: -1.0,
            mediaSize: m
                .raw
                .as_ref()
                .and_then(|raw| crate::media::expected_bytes(raw))
                .filter(|&bytes| bytes > 0)
                .map(crate::media::format_size)
                .unwrap_or_default()
                .into(),
            mediaReady: self.media_path.contains_key(&m.id),
            mediaLoading: self.media_inflight.contains(&m.id),
            reactions: reactions.into(),
            reactionCount: reaction_count.into(),
            dayLabel: day_label.into(),
            unread_boundary: self.unread_boundary.as_deref() == Some(m.id.as_str()),
            ticks: if outgoing.is_some() || preparing.is_some() {
                "pending"
            } else {
                ticks
            }
            .into(),
            ticksBlue: ticks_blue,
            senderHasAvatar: self.avatar_for(&sender_jid).is_some(),
            senderAvatar: self.avatar_for(&sender_jid).unwrap_or_else(empty_image),
            senderInitial: initial_of(if m.sender.is_empty() { "?" } else { &m.sender }).into(),
            senderColorIdx: color_idx_of(if sender_jid.is_empty() {
                &m.jid
            } else {
                &sender_jid
            }),
            voiceMessage: m.is_voice_message(),
            audioFilename: if m.kind == MessageKind::Audio
                || (m.kind == MessageKind::Doc && m.mimetype.starts_with("audio/"))
            {
                let source = self
                    .vault
                    .as_ref()
                    .and_then(|vault| vault.setting_get(&format!("media-source:{}", m.id)));
                m.audio_filename(source.as_deref()).into()
            } else {
                "".into()
            },
            voiceJid: voice_jid.clone().into(),
            voiceHasAvatar: self.avatar_for(&voice_jid).is_some(),
            voiceAvatar: self.avatar_for(&voice_jid).unwrap_or_else(empty_image),
            voiceInitial: initial_of(
                if m.from_me {
                    let name = self.store.chat_name(&self.self_jid);
                    if name.is_empty() {
                        "?".to_string()
                    } else {
                        name
                    }
                } else if m.sender.is_empty() {
                    "?".to_string()
                } else {
                    m.sender.clone()
                }
                .as_str(),
            )
            .into(),
            voiceColorIdx: color_idx_of(if voice_jid.is_empty() {
                &m.jid
            } else {
                &voice_jid
            }),
            groupIndent: group_indent,
            showAvatar: group_indent && first_of_run,
            // Unsaved senders show their number next to the push name.
            senderNumber: if group_indent
                && !sender_jid.is_empty()
                && !self.store.is_saved(&sender_jid)
            {
                format_number(&sender_jid).into()
            } else {
                "".into()
            },
            sticker: m.sticker,
            gif: m.gif,
            videoLen: if m.kind == MessageKind::Video && !m.gif && m.duration_sec > 0 {
                crate::store::format_duration(m.duration_sec).into()
            } else {
                "".into()
            },
            styled,
            hasStyled: has_styled && card.kind.is_empty(),
            links: ModelRc::new(VecModel::from(links)),
            plain: plain.into(),
            linkTitle: m.link_title.trim().into(),
            linkDesc: m.link_desc.trim().into(),
            linkHost: link_host.into(),
            linkUrl: message_link(m).into(),
            hasLink: !m.link_title.is_empty() || !m.link_url.is_empty(),
            linkThumb: empty_image(),
            hasLinkThumb: false,
            hasLinkOg: false,
            linkThumbW: 0,
            linkThumbH: 0,
            hasWave: self.waves.contains_key(&m.id),
            wave: self.waves.get(&m.id).cloned().unwrap_or_else(empty_image),
            playing: self
                .audio
                .as_ref()
                .map(|a| a.id == m.id && !a.paused && a.player.is_some())
                .unwrap_or(false),
            progress: 0.0,
            posLabel: "".into(),
            senderJid: sender_jid.into(),
            forwarded: m.forwarded,
            deleted: m.deleted,
            revoked: m.revoked,
            starred: m.starred,
            pinned: m.pinned_until > now_secs(),
            editable: message_editable(m),
            privateReply: is_group(&m.jid) && !m.from_me && !m.sender_jid.is_empty(),
            hasQuote: !m.quote_text.is_empty() || !m.quote_id.is_empty(),
            quoteName: if is_group(&m.quote_jid) && m.quote_jid != m.jid {
                self.store.chat_name(&m.quote_jid).into()
            } else if m.quote_author.is_empty() {
                t("reactions.you").into()
            } else {
                let name = self.store.chat_name(&m.quote_author);
                if name.is_empty() {
                    display_id(&m.quote_author).into()
                } else {
                    name.into()
                }
            },
            quoteText: self.store.named_mentions(&m.quote_text).into(),
            quoteId: m.quote_id.clone().into(),
            quoteJid: quote_chat(m).into(),
        }
    }

    pub(super) fn patch_row(&self, id: &str, patch: impl FnOnce(&mut MessageItem)) {
        for i in 0..self.messages_model.row_count() {
            if let Some(mut row) = self.messages_model.row_data(i)
                && row.id == id
            {
                patch(&mut row);
                self.messages_model.set_row_data(i, row);
                return;
            }
        }
    }

    pub(super) fn find_message(&self, id: &str) -> Option<&StoredMessage> {
        let jid = self.current_jid.as_ref()?;
        self.store.messages_for(jid).iter().find(|m| m.id == id)
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum RowPlan {
    Skip,
    Append,
    Rebuild,
}

// The store keeps messages sorted by timestamp, so a message that arrives
// out of order (phone clock skew, echo vs. server time, late delivery) is
// not the last one. Appending `list.last()` then showed a neighbour twice.
pub(super) fn plan_row(list: &[StoredMessage], shown: &[String], id: &str) -> RowPlan {
    if shown.iter().any(|s| s == id) {
        return RowPlan::Skip;
    }
    match list.iter().position(|m| m.id == id) {
        None => RowPlan::Skip,
        Some(i) if i + 1 == list.len() => RowPlan::Append,
        Some(_) => RowPlan::Rebuild,
    }
}

fn message_focus_index(ids: impl IntoIterator<Item = String>, focused_id: &str) -> Option<usize> {
    if focused_id.is_empty() {
        return None;
    }
    ids.into_iter().position(|id| id == focused_id)
}

fn initial_message_index(
    ids: &[String],
    geometry: &HashMap<String, (f32, f32, u32)>,
    epoch: u32,
    height: f32,
    visible: bool,
) -> Option<usize> {
    if visible {
        let first_visible = ids.iter().enumerate().filter_map(|(index, id)| {
            let &(y, h, row_epoch) = geometry.get(id)?;
            (row_epoch == epoch && h > 0.0 && y < height && y + h > 0.0).then_some((index, y))
        });
        if let Some((index, _)) = first_visible.min_by(|a, b| a.1.total_cmp(&b.1)) {
            return Some(index);
        }
    }
    ids.len().checked_sub(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(id: &str, ts: i64) -> StoredMessage {
        let mut m = crate::wa_map::system_message("a@s.whatsapp.net", id, ts, "x".to_string());
        m.kind = MessageKind::Text;
        m
    }

    #[test]
    fn message_focus_tracks_identity_across_prepend_and_reset() {
        let ids = ["older", "first", "focused", "last"].map(String::from);
        assert_eq!(message_focus_index(ids.clone(), "focused"), Some(2));
        assert_eq!(message_focus_index(ids, ""), None);
        assert_eq!(message_focus_index(["other".to_string()], "focused"), None);
        assert_eq!(
            message_focus_index(["focused".to_string(), "last".to_string()], "focused"),
            Some(0)
        );
    }

    #[test]
    fn initial_focus_prefers_saved_visible_rows_without_seeking() {
        let ids = ["old", "visible", "recent"].map(String::from);
        let geometry = HashMap::from([
            ("old".to_string(), (-100.0, 30.0, 4)),
            ("visible".to_string(), (-10.0, 40.0, 4)),
            ("recent".to_string(), (30.0, 40.0, 4)),
        ]);
        assert_eq!(
            initial_message_index(&ids, &geometry, 4, 100.0, true),
            Some(1)
        );
        assert_eq!(
            initial_message_index(&ids, &geometry, 4, 100.0, false),
            Some(2)
        );
        assert_eq!(
            initial_message_index(&ids, &geometry, 5, 100.0, true),
            Some(2)
        );
        assert_eq!(initial_message_index(&[], &geometry, 4, 100.0, true), None);
    }

    #[test]
    fn out_of_order_message_does_not_duplicate_the_last_row() {
        let mut store = crate::store::Store::default();
        assert!(store.add_message(msg("new", 100)));
        let shown = vec!["new".to_string()];
        assert!(store.add_message(msg("late", 50)));
        let list = store.messages_for("a@s.whatsapp.net");
        // Old behaviour appended list.last() == "new", which is already shown.
        assert_eq!(list.last().unwrap().id, "new");
        assert_eq!(plan_row(list, &shown, "late"), RowPlan::Rebuild);
        assert_eq!(plan_row(list, &shown, "new"), RowPlan::Skip);
    }

    #[test]
    fn in_order_message_is_appended_once() {
        let mut store = crate::store::Store::default();
        store.add_message(msg("a", 1));
        store.add_message(msg("b", 2));
        let list = store.messages_for("a@s.whatsapp.net");
        assert_eq!(plan_row(list, &["a".to_string()], "b"), RowPlan::Append);
        assert_eq!(
            plan_row(list, &["a".to_string(), "b".to_string()], "b"),
            RowPlan::Skip
        );
    }
}
