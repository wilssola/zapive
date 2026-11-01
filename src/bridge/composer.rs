// The composer: drafts, sending text, mentions and replies.
use super::*;

impl Bridge {
    // ---- composing ----

    pub(super) fn save_draft(&mut self, text: &str, promote: bool) {
        let Some(jid) = self.current_jid.clone() else { return; };
        if text.is_empty() {
            if self.store.drafts.remove(&jid).is_some() {
                self.schedule_refresh_chats();
            }
            return;
        }
        if !self.store.chats.contains_key(&jid) { return; }
        let previous = self.store.drafts.get(&jid).map_or(0, |draft| draft.promoted);
        let promoted = if promote && !text.trim().is_empty() {
            self.draft_sequence = self.draft_sequence
                .max(self.store.drafts.values().map(|draft| draft.promoted).max().unwrap_or(0)) + 1;
            self.draft_sequence
        } else if text.trim().is_empty() { 0 } else { previous };
        self.store.drafts.insert(jid, crate::store::Draft {
            text: text.to_string(),
            mentions: self.mentions.clone(),
            promoted,
        });
        self.schedule_refresh_chats();
    }

    pub(super) fn save_composer(&mut self, promote: bool) {
        let text = self.ui.get_composer_text().to_string();
        self.save_draft(&text, promote);
    }

    pub(super) fn handle_send_text(&mut self, text: &str) {
        let mut body = text.trim().to_string();
        let Some(jid) = self.current_jid.clone() else {
            return;
        };
        if body.is_empty() {
            return;
        }
        if !self
            .store
            .chats
            .get(&jid)
            .map(|c| c.can_send())
            .unwrap_or(true)
        {
            return;
        }
        let quote = self.reply_to.clone().and_then(|(chat, id)| {
            let m = self.store.messages_for(&chat).iter().find(|m| m.id == id)?;
            let raw = m.raw.clone()?;
            Some(QuoteRef {
                id: m.id.clone(),
                chat_jid: m.jid.clone(),
                sender_jid: if m.from_me {
                    self.self_jid.clone()
                } else {
                    m.sender_jid.clone()
                },
                message: raw,
            })
        });
        // "@Name" in the composer goes out as "@<number>" plus the jid in
        // the context, which is how every client renders a mention.
        let mut mentions: Vec<String> = Vec::new();
        let mut mention_all = None;
        let picked = self.mentions.clone();
        for (label, target) in picked {
            let token = format!("@{label}");
            if !body.contains(&token) {
                continue;
            }
            if target == "all" {
                let user = jid.split('@').next().unwrap_or("").to_string();
                body = body.replace(&token, &format!("@{user}"));
                mention_all = Some(self.store.chat_name(&jid));
            } else {
                let user = target.split('@').next().unwrap_or("").to_string();
                body = body.replace(&token, &format!("@{user}"));
                if !mentions.contains(&target) {
                    mentions.push(target);
                }
            }
        }
        let message = crate::wa::build_text(&jid, body, &quote, mentions, mention_all);
        if self.enqueue_prepared(&jid, message, None) {
            self.store.drafts.remove(&jid);
            self.mentions.clear();
            self.clear_reply();
            self.ui.set_mention_open(false);
            self.ui.invoke_set_composer("".into(), 0);
            self.schedule_refresh_chats();
        }
    }

    // ---- mentions ----

    // Called on every composer edit with the cursor's byte offset: an
    // "@word" right before the cursor opens the member list.
    pub(super) fn composer_changed(&mut self, text: &str, cursor: usize) {
        self.save_draft(text, false);
        let Some(jid) = self.current_jid.clone() else {
            return;
        };
        if !is_group(&jid) {
            return;
        }
        let Some((at, cursor)) = crate::markup::mention_query(text, cursor) else {
            self.mention_at = None;
            self.ui.set_mention_open(false);
            return;
        };
        let query = crate::search::fold(&text[at + 1..cursor]);
        self.mention_at = Some((at, cursor));
        let members: Vec<Member> = self.store.members_of(&jid).to_vec();
        let mut rows: Vec<MemberItem> = Vec::new();
        let all_label = t("mention.all");
        if query.is_empty() || crate::search::fold(&all_label).starts_with(&query) {
            rows.push(MemberItem {
                jid: "all".into(),
                name: all_label.into(),
                number: t("mention.all_hint").into(),
                hasAvatar: false,
                avatar: empty_image(),
                initial: "@".into(),
                colorIdx: 0,
                checked: false,
                admin: false,
            });
        }
        for m in members {
            let canon = self.store.canon_owned(&m.jid);
            if self.store.self_jids.contains(&canon) {
                continue;
            }
            let name = self.store.chat_name(&canon);
            let number = if m.phone.is_empty() {
                format_number(&canon)
            } else {
                format_number(&m.phone)
            };
            if !query.is_empty()
                && !crate::search::fold(&name).contains(&query)
                && !number.replace(' ', "").contains(&query)
            {
                continue;
            }
            rows.push(self.to_member_row(&canon, &name, &number, m.admin, false));
            if rows.len() >= 8 {
                break;
            }
        }
        if rows.is_empty() {
            self.ui.set_mention_open(false);
            return;
        }
        for row in &rows {
            if row.jid != "all" {
                self.queue_avatar(row.jid.as_str());
            }
        }
        self.mention_model.set_vec(rows);
        self.ui.set_mention_open(true);
    }

    pub(super) fn mention_pick(&mut self, idx: usize) {
        let Some(row) = self.mention_model.row_data(idx) else {
            return;
        };
        let Some((at, end)) = self.mention_at.take() else {
            return;
        };
        self.ui.set_mention_open(false);
        let text = self.ui.get_composer_text().to_string();
        if at > text.len() || end > text.len() {
            return;
        }
        let label = if row.jid == "all" {
            t("mention.all")
        } else {
            row.name.to_string()
        };
        let label = clean_text(&label).replace('@', "");
        let insert = format!("@{label} ");
        let next = format!("{}{}{}", &text[..at], insert, &text[end..]);
        let cursor = (at + insert.len()) as i32;
        self.mentions.retain(|(l, _)| l != &label);
        self.mentions.push((label, row.jid.to_string()));
        self.save_draft(&next, false);
        self.ui.invoke_set_composer(next.into(), cursor);
    }

    pub(super) fn to_member_row(
        &self,
        jid: &str,
        name: &str,
        number: &str,
        admin: bool,
        checked: bool,
    ) -> MemberItem {
        let avatar = self.avatar_for(jid);
        MemberItem {
            jid: jid.into(),
            name: name.into(),
            number: number.into(),
            hasAvatar: avatar.is_some(),
            avatar: avatar.unwrap_or_else(empty_image),
            initial: initial_of(name).into(),
            colorIdx: color_idx_of(jid),
            checked,
            admin,
        }
    }

    pub(super) fn start_reply(&mut self, id: &str) {
        let Some(m) = self.find_message(id) else {
            return;
        };
        if m.raw.is_none() {
            return;
        }
        let name = if m.from_me {
            t("reactions.you")
        } else if m.sender.is_empty() {
            display_id(&m.jid)
        } else {
            m.sender.clone()
        };
        let preview = preview_body(m);
        self.reply_to = Some((m.jid.clone(), id.to_string()));
        self.ui.set_reply_open(true);
        self.ui.set_reply_name(name.into());
        self.ui.set_reply_text(preview.into());
        self.ui.invoke_focus_composer();
    }

    pub(super) fn clear_reply(&mut self) {
        self.reply_to = None;
        self.ui.set_reply_open(false);
        self.ui.set_reply_name("".into());
        self.ui.set_reply_text("".into());
    }
}
