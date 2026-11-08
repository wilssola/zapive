// The chat list: rows, filters, search and labels.
use super::*;

pub(super) const SEARCH_LIMIT: usize = 60;

// Whether a message has something to download or decode for its bubble.
pub(super) fn chat_matches_archive(archived: bool, tab: &str, searching: bool) -> bool {
    if tab == "archived" {
        archived
    } else {
        !archived || searching
    }
}

impl Bridge {
    // ---- chat list ----

    pub(super) fn to_chat_row(
        &self,
        jid: &str,
        preview: &str,
        timestamp: i64,
        unread: u32,
        mentioned: bool,
    ) -> ChatItem {
        let name = self.store.chat_name(jid);
        let avatar = self.avatar_for(jid);
        let meta = self.store.chats.get(jid);
        let activity = self
            .chat_typing
            .get(jid)
            .and_then(|members| members.values().max_by_key(|(token, _)| token))
            .map(|(_, text)| text.as_str());
        let draft = self.store.drafts.get(jid).filter(|draft| {
            self.current_jid.as_deref() != Some(jid) && !draft.text.trim().is_empty()
        });
        let draft_preview =
            draft.map(|draft| format!("{} {}", t("preview.draft"), clean_text(&draft.text)));
        let preview = draft_preview.as_deref().or(activity).unwrap_or(preview);
        let (preview, ticks, ticks_blue) = if let Some(body) = preview.strip_prefix("✓ ") {
            let (ticks, blue) = self
                .store
                .messages
                .get(jid)
                .and_then(|messages| messages.iter().max_by_key(|m| m.timestamp))
                .filter(|m| m.from_me && m.timestamp == timestamp)
                .map(ticks_for)
                .unwrap_or(("✓", false));
            (body, ticks, blue)
        } else {
            (preview, "", false)
        };
        ChatItem {
            jid: jid.into(),
            name: name.clone().into(),
            preview: preview.into(),
            typing: activity.is_some() && draft.is_none(),
            draft: draft.is_some(),
            ticks: ticks.into(),
            ticksBlue: ticks_blue,
            time: format_time(timestamp).into(),
            hasAvatar: avatar.is_some(),
            avatar: avatar.unwrap_or_else(empty_image),
            initial: initial_of(&name).into(),
            colorIdx: color_idx_of(jid),
            unread: unread as i32,
            mentioned: mentioned && unread > 0,
            pinned: meta.map(|c| c.pinned > 0).unwrap_or(false),
            muted: meta.map(|c| c.is_muted()).unwrap_or(false),
            silenced_call: self.silenced_for(jid),
        }
    }

    pub(super) fn visible_chats(&self) -> Vec<(String, String, i64, u32, bool)> {
        let search = crate::search::fold(&self.search_text);
        let label_chats = self
            .tab
            .strip_prefix("label:")
            .and_then(|id| self.store.label_chats.get(id));
        self.store
            .sorted_chats()
            .into_iter()
            .filter(|meta| {
                let channel = is_channel(&meta.jid);
                if self.view == "channels" {
                    if !channel {
                        return false;
                    }
                } else if self.view == "communities" {
                    if !meta.is_community && meta.community.is_empty() {
                        return false;
                    }
                } else if channel || meta.is_community {
                    // Regular chat list: channels and community shells live
                    // in their own tabs.
                    return false;
                }
                if !chat_matches_archive(meta.archived, &self.tab, !search.is_empty()) {
                    return false;
                }
                if self.tab != "archived" {
                    match self.tab.as_str() {
                        "unread" if meta.unread == 0 => return false,
                        "favorites" if !meta.favorite => return false,
                        "groups" if !is_group(&meta.jid) => return false,
                        _ => {}
                    }
                    if let Some(set) = label_chats
                        && !set.contains(&meta.jid)
                    {
                        return false;
                    }
                }
                if !search.is_empty()
                    && !crate::search::fold(&self.store.chat_name(&meta.jid)).contains(&search)
                    && !meta.jid.contains(&search)
                {
                    return false;
                }
                true
            })
            .map(|meta| {
                (
                    meta.jid.clone(),
                    meta.preview.clone(),
                    meta.timestamp,
                    meta.unread,
                    meta.mentioned,
                )
            })
            .collect()
    }

    pub(super) fn schedule_refresh_chats(&mut self) {
        // A background account has no list on screen; what matters is
        // that its changes reach the vault.
        if self.headless {
            self.schedule_save();
            return;
        }
        if self.refresh_queued {
            return;
        }
        self.refresh_queued = true;
        self.once(100, |b| {
            b.refresh_queued = false;
            b.refresh_chats();
        });
    }

    // In-place diff: only changed rows are rewritten, which is what keeps
    // the ListView's scroll position stable.
    pub(super) fn refresh_chats(&mut self) {
        self.canonicalize_typing_keys();
        let (chats, channels, communities) = self.store.sidebar_unread_counts();
        self.ui.set_chats_unread_count(chats);
        self.ui.set_channels_unread_count(channels);
        self.ui.set_communities_unread_count(communities);
        let archived_unread = self
            .store
            .chats
            .values()
            .filter(|chat| chat.archived && chat.unread > 0)
            .count();
        let archived_mentioned = self
            .store
            .chats
            .values()
            .any(|chat| chat.archived && chat.unread > 0 && chat.mentioned);
        self.ui
            .set_archived_unread_count(archived_unread.min(i32::MAX as usize) as i32);
        self.ui.set_archived_mentioned(archived_mentioned);
        let rows: Vec<ChatItem> = self
            .visible_chats()
            .iter()
            .map(|(jid, preview, ts, unread, mentioned)| {
                self.to_chat_row(jid, preview, *ts, *unread, *mentioned)
            })
            .collect();
        for row in &rows {
            let jid = row.jid.as_str();
            if !is_group(jid)
                && !is_channel(jid)
                && self.presence_subscribed.insert(jid.to_string())
            {
                self.wa.send(Cmd::SubscribePresence(jid.to_string()));
            }
        }
        let model = &self.chats_model;
        let common = model.row_count().min(rows.len());
        for (i, next) in rows.iter().take(common).enumerate() {
            let cur = model.row_data(i);
            let differs = cur
                .map(|cur| {
                    cur.jid != next.jid
                        || cur.name != next.name
                        || cur.preview != next.preview
                        || cur.typing != next.typing
                        || cur.draft != next.draft
                        || cur.ticks != next.ticks
                        || cur.ticksBlue != next.ticksBlue
                        || cur.time != next.time
                        || cur.unread != next.unread
                        || cur.mentioned != next.mentioned
                        || cur.pinned != next.pinned
                        || cur.muted != next.muted
                        || cur.hasAvatar != next.hasAvatar
                })
                .unwrap_or(true);
            if differs {
                model.set_row_data(i, next.clone());
            }
        }
        if rows.len() > model.row_count() {
            for row in rows.into_iter().skip(model.row_count()) {
                model.push(row);
            }
        } else {
            while model.row_count() > rows.len() {
                model.remove(model.row_count() - 1);
            }
        }
        self.refresh_search_hits();
        self.ensure_avatars();
        self.schedule_save();
    }

    // Messages matching the sidebar search, across every chat.
    pub(super) fn refresh_search_hits(&mut self) {
        if self.search_text.chars().count() < 2 {
            if self.search_model.row_count() > 0 {
                self.search_model.set_vec(Vec::new());
            }
            return;
        }
        let hits = self
            .store
            .search
            .search(&self.search_text, None, SEARCH_LIMIT);
        let rows: Vec<SearchHit> = hits.iter().map(|h| self.to_hit(h, true)).collect();
        for h in &hits {
            self.queue_avatar(&h.jid);
        }
        self.search_model.set_vec(rows);
    }

    pub(super) fn to_hit(&self, h: &crate::search::Hit, with_chat: bool) -> SearchHit {
        let name = if with_chat {
            self.store.chat_name(&h.jid)
        } else if h.from_me {
            t("reactions.you")
        } else if h.sender.is_empty() {
            self.store.chat_name(&h.jid)
        } else {
            h.sender.clone()
        };
        // Inside a chat the row is about the author, so it wears their
        // picture; across chats it is about the conversation.
        let face = if with_chat {
            h.jid.clone()
        } else if h.from_me {
            self.self_jid.clone()
        } else if h.sender_jid.is_empty() {
            h.jid.clone()
        } else {
            self.store.canon_owned(&h.sender_jid)
        };
        let avatar = self.avatar_for(&face);
        let who = if with_chat && is_group(&h.jid) {
            if h.from_me {
                t("reactions.you")
            } else {
                h.sender.clone()
            }
        } else {
            String::new()
        };
        SearchHit {
            jid: h.jid.clone().into(),
            face: face.clone().into(),
            id: h.id.clone().into(),
            name: name.clone().into(),
            sender: who.into(),
            snippet: self.store.named_mentions(&h.snippet).into(),
            time: format_time(h.ts).into(),
            hasAvatar: avatar.is_some(),
            avatar: avatar.unwrap_or_else(empty_image),
            initial: initial_of(&name).into(),
            colorIdx: color_idx_of(&face),
        }
    }

    // List chips: every label, flagged when the open chat is filed there.
    pub(super) fn refresh_labels(&mut self) {
        let current = self.current_jid.clone().unwrap_or_default();
        let rows: Vec<LabelItem> = self
            .store
            .sorted_labels()
            .into_iter()
            .map(|l| LabelItem {
                id: l.id.clone().into(),
                name: l.name.clone().into(),
                active: self
                    .store
                    .label_chats
                    .get(&l.id)
                    .map(|set| set.contains(&current))
                    .unwrap_or(false),
            })
            .collect();
        self.label_chip_model.set_vec(
            rows.iter()
                .filter(|row| {
                    self.store
                        .labels
                        .get(row.id.as_str())
                        .is_some_and(|label| !label.is_builtin_filter())
                })
                .cloned()
                .collect::<Vec<_>>(),
        );
        self.label_model.set_vec(rows);
    }

    pub(super) fn schedule_save(&mut self) {
        self.refresh_message_details();
        if self.save_queued || self.vault.is_none() {
            return;
        }
        self.save_queued = true;
        self.once(2000, |b| {
            b.save_queued = false;
            if let Some(vault) = b.vault.take() {
                // Never write through a locked vault: the store isn't
                // loaded yet and every set() would be dropped anyway.
                if !vault.locked() {
                    b.store.save_to(&vault);
                }
                b.vault = Some(vault);
            }
        });
    }

    // ---- search ----

    pub(super) fn open_chat_search(&mut self) {
        self.ui.set_chat_menu_open(false);
        self.ui.set_info_open(false);
        self.ui.set_chat_search_open(true);
        self.chat_search(&self.chat_search_query.clone());
    }

    pub(super) fn chat_search(&mut self, query: &str) {
        self.chat_search_query = query.to_string();
        self.chat_search_pos = 0;
        let Some(jid) = self.current_jid.clone() else {
            return;
        };
        if query.trim().is_empty() {
            self.chat_search_model.set_vec(Vec::new());
            self.ui.set_chat_search_count("".into());
            self.ui.set_highlight_id("".into());
            return;
        }
        let hits = self.store.search.search(query, Some(&jid), 200);
        let count = hits.len();
        let rows: Vec<SearchHit> = hits.iter().map(|h| self.to_hit(h, false)).collect();
        let faces: HashSet<String> = hits
            .iter()
            .map(|h| {
                if h.from_me {
                    self.self_jid.clone()
                } else if h.sender_jid.is_empty() {
                    h.jid.clone()
                } else {
                    self.store.canon_owned(&h.sender_jid)
                }
            })
            .collect();
        for face in faces {
            self.queue_avatar(&face);
        }
        self.chat_search_model.set_vec(rows);
        self.ui.set_chat_search_count(
            if count == 0 {
                t("search.none")
            } else {
                ta("search.results", &[&count.to_string()])
            }
            .into(),
        );
    }

    // Up/down through the hits, newest first.
    pub(super) fn chat_search_step(&mut self, delta: i32) {
        let count = self.chat_search_model.row_count();
        if count == 0 {
            return;
        }
        let next = (self.chat_search_pos as i32 + delta).rem_euclid(count as i32) as usize;
        self.chat_search_pos = next;
        if let Some(row) = self.chat_search_model.row_data(next) {
            let (jid, id) = (row.jid.to_string(), row.id.to_string());
            self.open_hit(&jid, &id);
        }
    }

    // Jumps to a message and flashes it.
    pub(super) fn open_hit(&mut self, jid: &str, id: &str) {
        self.open_dm(jid, Some(id));
        self.ui.set_highlight_id(id.into());
        let id = id.to_string();
        self.once(2500, move |b| {
            if b.ui.get_highlight_id() == id.as_str() {
                b.ui.set_highlight_id("".into());
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sidebar_search_includes_archived_chats_without_changing_archive_filter() {
        for tab in ["all", "unread", "favorites", "groups", "label:work"] {
            assert!(chat_matches_archive(false, tab, false));
            assert!(!chat_matches_archive(true, tab, false));
            assert!(chat_matches_archive(false, tab, true));
            assert!(chat_matches_archive(true, tab, true));
        }
        for searching in [false, true] {
            assert!(chat_matches_archive(true, "archived", searching));
            assert!(!chat_matches_archive(false, "archived", searching));
        }
    }
}
