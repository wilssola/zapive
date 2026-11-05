// The MCP server's settings and the UI-thread side of its tool calls.
use super::*;
use crate::mcp::Call;
use serde_json::{Value, json};

const TOKEN_KEY: &str = "settings:mcp_token";

impl Bridge {
    // Boot and account switch: the token lives in the (per-account,
    // encrypted) vault, the on/off switch and port in plain settings.
    pub(super) fn load_mcp(&mut self) {
        self.mcp = None;
        let enabled = self
            .vault
            .as_ref()
            .is_some_and(|v| v.setting_get("mcp_enabled").as_deref() == Some("1"));
        self.ui.set_mcp_enabled(enabled);
        if enabled {
            self.start_mcp();
        } else {
            self.show_mcp_stopped();
        }
    }

    pub(super) fn handle_mcp(&mut self, enabled: bool) {
        let Some(vault) = &self.vault else { return };
        vault.setting_set("mcp_enabled", if enabled { "1" } else { "0" });
        self.ui.set_mcp_enabled(enabled);
        self.mcp = None;
        if enabled {
            self.start_mcp();
        } else {
            self.show_mcp_stopped();
        }
    }

    pub(super) fn mcp_regenerate_token(&mut self) {
        let token = crate::mcp::new_token();
        if !self.store_mcp_token(&token) {
            return;
        }
        if let Some(server) = &self.mcp {
            server.set_token(&token);
        }
        self.ui.set_mcp_token(token.into());
    }

    pub(super) fn mcp_copy_config(&mut self) {
        let Some(server) = &self.mcp else { return };
        let config = crate::mcp::client_config(&server.url(), &self.ui.get_mcp_token());
        if let Err(e) = arboard::Clipboard::new().and_then(|mut c| c.set_text(config)) {
            eprintln!("[clipboard] copy failed: {e}");
        }
    }

    fn mcp_token(&mut self) -> Option<String> {
        let vault = self.vault.as_ref()?;
        if let Some(token) = vault.get(TOKEN_KEY).filter(|t| t.len() == 64) {
            return Some(token);
        }
        let token = crate::mcp::new_token();
        self.store_mcp_token(&token).then_some(token)
    }

    fn store_mcp_token(&mut self, token: &str) -> bool {
        let Some(vault) = &self.vault else {
            return false;
        };
        match vault.set_checked(TOKEN_KEY, token) {
            Ok(()) => true,
            Err(e) => {
                self.ui
                    .set_mcp_status(ta("settings.mcp.status_error", &[&e]).into());
                false
            }
        }
    }

    fn start_mcp(&mut self) {
        let Some(token) = self.mcp_token() else {
            return;
        };
        let Some(rt) = crate::RT.get() else { return };
        let port = self
            .vault
            .as_ref()
            .and_then(|v| v.setting_get("mcp_port"))
            .and_then(|p| p.parse::<u16>().ok())
            .filter(|p| *p != 0)
            .unwrap_or(crate::mcp::DEFAULT_PORT);
        self.ui.set_mcp_token(token.as_str().into());
        match crate::mcp::start(rt.handle(), port, &token) {
            Ok(server) => {
                let url = server.url();
                // Kept so a fallback port survives restarts and the
                // client config stays valid.
                if let (Some(vault), Some(bound)) = (&self.vault, url.rsplit(':').next()) {
                    vault.setting_set("mcp_port", bound.trim_end_matches("/mcp"));
                }
                self.ui.set_mcp_url(url.as_str().into());
                self.ui
                    .set_mcp_status(ta("settings.mcp.status_running", &[&url]).into());
                self.mcp = Some(server);
            }
            Err(e) => {
                self.ui.set_mcp_url("".into());
                self.ui
                    .set_mcp_status(ta("settings.mcp.status_error", &[&e]).into());
            }
        }
    }

    fn show_mcp_stopped(&mut self) {
        self.ui.set_mcp_url("".into());
        self.ui
            .set_mcp_status(t("settings.mcp.status_stopped").into());
    }

    // ---- tool calls (from crate::mcp, through ui_apply) ----

    pub fn mcp_call(&mut self, call: Call) -> Result<Value, String> {
        if self.vault.as_ref().is_none_or(|v| v.locked()) {
            return Err("Zapive is locked. Unlock it with the PIN first.".into());
        }
        if !self.registered {
            return Err("Zapive is not logged in to WhatsApp. Link a phone first.".into());
        }
        if self.switching {
            return Err("Zapive is switching accounts. Try again in a moment.".into());
        }
        match call {
            Call::ListChats {
                query,
                label,
                limit,
            } => self.mcp_list_chats(query.as_deref(), label.as_deref(), limit),
            Call::ListLabels => Ok(self.mcp_list_labels()),
            Call::CreateLabel { name } => Ok(self.mcp_create_label(&name)),
            Call::SetChatLabel {
                chat_id,
                label,
                labeled,
            } => self.mcp_set_chat_label(&chat_id, &label, labeled),
            Call::FindUnanswered {
                label,
                min_wait_minutes,
                max_age_days,
                include_groups,
                limit,
            } => self.mcp_unanswered(
                label.as_deref(),
                min_wait_minutes,
                max_age_days,
                include_groups,
                limit,
            ),
            Call::ReadMessages {
                chat_id,
                limit,
                before,
            } => self.mcp_read(&chat_id, limit, before.as_deref()),
            Call::SearchMessages {
                query,
                chat_id,
                limit,
            } => self.mcp_search(&query, chat_id.as_deref(), limit),
            Call::GetChatInfo { chat_id } => self.mcp_chat_info(&chat_id),
            Call::SendMessage {
                chat_id,
                text,
                reply_to,
            } => self.mcp_send(&chat_id, text, reply_to.as_deref()),
            Call::MarkAsRead { chat_id } => self.mcp_mark_read(&chat_id),
            Call::React {
                chat_id,
                message_id,
                emoji,
            } => self.mcp_react(&chat_id, &message_id, &emoji),
        }
    }

    fn mcp_chat(&self, chat_id: &str) -> Result<String, String> {
        let jid = self.store.canon_owned(&normalize_jid(chat_id));
        if self.store.chats.contains_key(&jid) {
            Ok(jid)
        } else {
            Err(format!(
                "Unknown chat_id '{chat_id}'. Use list_chats to find one."
            ))
        }
    }

    fn mcp_message(&mut self, jid: &str, id: &str) -> Result<StoredMessage, String> {
        if let Some(vault) = &self.vault {
            self.store.hydrate_id(vault, jid, id);
        }
        self.store
            .messages_for(jid)
            .iter()
            .find(|m| m.id == id)
            .cloned()
            .ok_or_else(|| format!("Unknown message_id '{id}' in chat '{jid}'."))
    }

    fn chat_kind(meta: &crate::store::ChatMeta) -> &'static str {
        if meta.is_community {
            "community"
        } else if is_channel(&meta.jid) {
            "channel"
        } else if is_group(&meta.jid) {
            "group"
        } else {
            "direct"
        }
    }

    fn mcp_list_chats(
        &self,
        query: Option<&str>,
        label: Option<&str>,
        limit: usize,
    ) -> Result<Value, String> {
        let query = query.map(crate::search::fold).unwrap_or_default();
        let in_label = self.mcp_label_members(label)?;
        let chats: Vec<Value> = self
            .store
            .sorted_chats()
            .into_iter()
            .filter(|meta| crate::store::is_displayable_jid(&meta.jid))
            .filter(|meta| in_label.as_ref().is_none_or(|set| set.contains(&meta.jid)))
            .filter(|meta| {
                query.is_empty()
                    || meta.jid.contains(&query)
                    || crate::search::fold(&self.store.chat_name(&meta.jid)).contains(&query)
            })
            .take(limit)
            .map(|meta| {
                json!({
                    "chat_id": meta.jid,
                    "name": self.store.chat_name(&meta.jid),
                    "type": Self::chat_kind(meta),
                    "unread": meta.unread,
                    "last_activity": format_stamp(meta.timestamp),
                    "timestamp": meta.timestamp,
                    "preview": meta.preview,
                    "pinned": meta.pinned > 0,
                    "archived": meta.archived,
                    "muted": meta.is_muted(),
                    "labels": self.mcp_chat_labels(&meta.jid),
                })
            })
            .collect();
        Ok(json!({ "chats": chats }))
    }

    // ---- labels ----

    // The labels a user made (WhatsApp's built-in filters stay hidden,
    // as in the chat list).
    fn mcp_user_labels(&self) -> Vec<&crate::store::Label> {
        self.store
            .sorted_labels()
            .into_iter()
            .filter(|l| !l.is_builtin_filter())
            .collect()
    }

    // Resolves a label_id or an exact (accent- and case-insensitive) name.
    fn mcp_label(&self, reference: &str) -> Result<String, String> {
        let reference = reference.trim();
        let labels = self.mcp_user_labels();
        if let Some(label) = labels.iter().find(|l| l.id == reference) {
            return Ok(label.id.clone());
        }
        let wanted = crate::search::fold(reference);
        let named: Vec<_> = labels
            .iter()
            .filter(|l| crate::search::fold(&l.name) == wanted)
            .collect();
        match named.as_slice() {
            [one] => Ok(one.id.clone()),
            [] => Err(format!(
                "Unknown label '{reference}'. Use list_labels to see the available labels."
            )),
            _ => Err(format!(
                "Several labels are named '{reference}'. Use its label_id from list_labels."
            )),
        }
    }

    // The chats under `label`, or None when no label filter was asked for.
    fn mcp_label_members(&self, label: Option<&str>) -> Result<Option<HashSet<String>>, String> {
        let Some(label) = label else { return Ok(None) };
        let id = self.mcp_label(label)?;
        Ok(Some(
            self.store.label_chats.get(&id).cloned().unwrap_or_default(),
        ))
    }

    fn mcp_chat_labels(&self, jid: &str) -> Vec<Value> {
        self.mcp_user_labels()
            .into_iter()
            .filter(|l| {
                self.store
                    .label_chats
                    .get(&l.id)
                    .is_some_and(|set| set.contains(jid))
            })
            .map(|l| json!({ "label_id": l.id, "name": l.name }))
            .collect()
    }

    fn mcp_list_labels(&self) -> Value {
        let labels: Vec<Value> = self
            .mcp_user_labels()
            .into_iter()
            .map(|l| {
                json!({
                    "label_id": l.id,
                    "name": l.name,
                    "chat_count": self.store.label_chats.get(&l.id).map_or(0, |set| set.len()),
                })
            })
            .collect();
        json!({ "labels": labels })
    }

    fn mcp_create_label(&mut self, name: &str) -> Value {
        let folded = crate::search::fold(name);
        if let Some(label) = self
            .mcp_user_labels()
            .into_iter()
            .find(|l| crate::search::fold(&l.name) == folded)
        {
            return json!({ "label_id": label.id, "name": label.name, "created": false });
        }
        let id = self.store.next_label_id();
        let order = self.store.labels.len() as i32;
        self.store.upsert_label(crate::store::Label {
            id: id.clone(),
            name: name.to_string(),
            color: order % 20,
            order,
            list_type: Some(5),
        });
        self.wa.send(Cmd::LabelCreate {
            id: id.clone(),
            name: name.to_string(),
            color: order % 20,
            order,
        });
        self.refresh_labels();
        self.schedule_refresh_chats();
        self.schedule_save();
        json!({ "label_id": id, "name": name, "created": true })
    }

    fn mcp_set_chat_label(
        &mut self,
        chat_id: &str,
        label: &str,
        labeled: bool,
    ) -> Result<Value, String> {
        let jid = self.mcp_chat(chat_id)?;
        let id = self.mcp_label(label)?;
        if self.store.set_label_chat(&id, &jid, labeled) {
            self.wa.send(Cmd::LabelChat {
                label_id: id.clone(),
                jid: jid.clone(),
                on: labeled,
            });
        }
        self.refresh_labels();
        self.schedule_refresh_chats();
        self.schedule_save();
        Ok(json!({
            "chat_id": jid,
            "label_id": id,
            "labeled": labeled,
            "labels": self.mcp_chat_labels(&jid),
        }))
    }

    // ---- unanswered chats ----

    // The newest messages of a chat, oldest first: stored history plus the
    // live rows the vault has not received yet.
    fn mcp_recent_messages(&self, jid: &str, count: usize) -> Result<Vec<StoredMessage>, String> {
        let vault = self.vault.as_ref().ok_or("Zapive is locked.")?;
        let mut by_id: HashMap<String, StoredMessage> = vault
            .history_page(jid, None, count)?
            .into_iter()
            .map(|m| (m.id.clone(), m))
            .collect();
        for m in self.store.messages_for(jid) {
            by_id.insert(m.id.clone(), m.clone());
        }
        let mut list: Vec<StoredMessage> = by_id.into_values().collect();
        list.sort_by(|a, b| (a.timestamp, &a.id).cmp(&(b.timestamp, &b.id)));
        let skip = list.len().saturating_sub(count);
        Ok(list.split_off(skip))
    }

    fn mcp_unanswered(
        &self,
        label: Option<&str>,
        min_wait_minutes: u32,
        max_age_days: u32,
        include_groups: bool,
        limit: usize,
    ) -> Result<Value, String> {
        const WINDOW: usize = 30;
        const MAX_SCAN: usize = 500;
        const SNIPPET_CHARS: usize = 300;
        let in_label = self.mcp_label_members(label)?;
        let now = crate::store::now_secs();
        let oldest_activity = now - i64::from(max_age_days) * 86_400;
        let waited_enough = now - i64::from(min_wait_minutes) * 60;
        let mut scanned = 0;
        let mut truncated = false;
        let mut found: Vec<(i64, Value)> = Vec::new();
        for meta in self.store.sorted_chats() {
            let kind = Self::chat_kind(meta);
            let wanted_kind = kind == "direct" || (include_groups && kind == "group");
            if !wanted_kind
                || !crate::store::is_displayable_jid(&meta.jid)
                || meta.left
                || meta.timestamp < oldest_activity
                || in_label.as_ref().is_some_and(|set| !set.contains(&meta.jid))
            {
                continue;
            }
            if scanned >= MAX_SCAN {
                truncated = true;
                break;
            }
            scanned += 1;
            let messages: Vec<StoredMessage> = self
                .mcp_recent_messages(&meta.jid, WINDOW)?
                .into_iter()
                .filter(|m| m.kind != MessageKind::System && !m.deleted && !m.revoked)
                .collect();
            // The run of messages after the user's last reply.
            let pending = messages.iter().rev().take_while(|m| !m.from_me).count();
            if pending == 0 {
                continue;
            }
            let run = &messages[messages.len() - pending..];
            let since = run[0].timestamp;
            if since > waited_enough {
                continue;
            }
            let mut last = self.mcp_message_json(&run[pending - 1]);
            let clipped = last["text"]
                .as_str()
                .filter(|text| text.chars().count() > SNIPPET_CHARS)
                .map(|text| format!("{}…", text.chars().take(SNIPPET_CHARS).collect::<String>()));
            if let Some(clipped) = clipped {
                last["text"] = json!(clipped);
            }
            found.push((
                since,
                json!({
                    "chat_id": meta.jid,
                    "name": self.store.chat_name(&meta.jid),
                    "type": kind,
                    "labels": self.mcp_chat_labels(&meta.jid),
                    "waiting_since": format_stamp(since),
                    "timestamp": since,
                    "waiting_minutes": (now - since) / 60,
                    "pending_messages": pending,
                    "last_message": last,
                    "unread": meta.unread,
                    "archived": meta.archived,
                    "muted": meta.is_muted(),
                }),
            ));
        }
        found.sort_by_key(|(since, _)| *since);
        let total = found.len();
        let chats: Vec<Value> = found.into_iter().take(limit).map(|(_, v)| v).collect();
        let mut result = json!({
            "total": total,
            "chats": chats,
            "chats_checked": scanned,
        });
        if truncated {
            result["note"] = json!(format!(
                "Only the {MAX_SCAN} most recent matching chats were checked. Narrow it with `label` or `max_age_days`."
            ));
        }
        Ok(result)
    }

    fn mcp_message_json(&self, m: &StoredMessage) -> Value {
        let sender = if m.from_me {
            t("reactions.you")
        } else if !m.sender.is_empty() {
            m.sender.clone()
        } else {
            self.store.chat_name(&m.jid)
        };
        let text = if m.deleted || m.revoked {
            String::new()
        } else if m.text.is_empty() || m.kind != MessageKind::Text {
            if m.text.is_empty() {
                preview_body(m)
            } else {
                format!("[{}] {}", preview_body(m), m.text)
            }
        } else {
            m.text.clone()
        };
        let mut value = json!({
            "message_id": m.id,
            "from_me": m.from_me,
            "sender": sender,
            "sender_id": if m.from_me { self.self_jid.clone() } else { m.sender_jid.clone() },
            "time": format_stamp(m.timestamp),
            "timestamp": m.timestamp,
            "text": text,
        });
        if m.deleted || m.revoked {
            value["deleted"] = json!(true);
        }
        if !m.quote_id.is_empty() {
            value["reply_to"] = json!({ "message_id": m.quote_id, "text": m.quote_text });
        }
        if !m.reactions.is_empty() {
            value["reactions"] = json!(m.reactions.values().collect::<Vec<_>>());
        }
        value
    }

    fn mcp_read(
        &mut self,
        chat_id: &str,
        limit: usize,
        before: Option<&str>,
    ) -> Result<Value, String> {
        let jid = self.mcp_chat(chat_id)?;
        let cursor = match before {
            Some(id) => Some((self.mcp_message(&jid, id)?.timestamp, id.to_string())),
            None => None,
        };
        let vault = self.vault.as_ref().ok_or("Zapive is locked.")?;
        let disk = vault.history_page(
            &jid,
            cursor.as_ref().map(|(ts, id)| (*ts, id.as_str())),
            limit,
        )?;
        let mut by_id: HashMap<String, StoredMessage> =
            disk.into_iter().map(|m| (m.id.clone(), m)).collect();
        // Live rows not flushed to the vault yet win over their stored copy.
        for m in self.store.messages_for(&jid) {
            by_id.insert(m.id.clone(), m.clone());
        }
        let mut list: Vec<StoredMessage> = by_id
            .into_values()
            .filter(|m| m.kind != MessageKind::System || !m.text.is_empty())
            .filter(|m| {
                cursor
                    .as_ref()
                    .is_none_or(|(ts, id)| (m.timestamp, m.id.as_str()) < (*ts, id.as_str()))
            })
            .collect();
        list.sort_by(|a, b| (a.timestamp, &a.id).cmp(&(b.timestamp, &b.id)));
        let skip = list.len().saturating_sub(limit);
        let messages: Vec<Value> = list[skip..]
            .iter()
            .map(|m| self.mcp_message_json(m))
            .collect();
        Ok(json!({
            "chat_id": jid,
            "name": self.store.chat_name(&jid),
            "messages": messages,
            "has_more": skip > 0 || messages.len() == limit,
        }))
    }

    fn mcp_search(
        &mut self,
        query: &str,
        chat_id: Option<&str>,
        limit: usize,
    ) -> Result<Value, String> {
        let jid = chat_id.map(|id| self.mcp_chat(id)).transpose()?;
        let hits: Vec<Value> = self
            .store
            .search
            .search(query, jid.as_deref(), limit)
            .iter()
            .map(|h| {
                json!({
                    "chat_id": h.jid,
                    "chat_name": self.store.chat_name(&h.jid),
                    "message_id": h.id,
                    "from_me": h.from_me,
                    "sender": if h.from_me { t("reactions.you") } else { h.sender.clone() },
                    "time": format_stamp(h.ts),
                    "timestamp": h.ts,
                    "text": h.snippet,
                })
            })
            .collect();
        Ok(json!({ "hits": hits }))
    }

    fn mcp_chat_info(&self, chat_id: &str) -> Result<Value, String> {
        let jid = self.mcp_chat(chat_id)?;
        let meta = &self.store.chats[&jid];
        let mut value = json!({
            "chat_id": jid,
            "name": self.store.chat_name(&jid),
            "type": Self::chat_kind(meta),
            "unread": meta.unread,
            "last_activity": format_stamp(meta.timestamp),
            "timestamp": meta.timestamp,
            "pinned": meta.pinned > 0,
            "archived": meta.archived,
            "muted": meta.is_muted(),
            "favorite": meta.favorite,
            "can_send": meta.can_send(),
            "disappearing_seconds": meta.ephemeral,
            "labels": self.mcp_chat_labels(&jid),
        });
        if !meta.community.is_empty() {
            value["community"] =
                json!({ "chat_id": meta.community, "name": self.store.chat_name(&meta.community) });
        }
        if is_group(&jid) {
            let members: Vec<Value> = self
                .store
                .members_of(&jid)
                .iter()
                .take(1024)
                .map(|m| {
                    let name = if m.name.is_empty() {
                        self.store.chat_name(&self.store.canon_owned(&m.jid))
                    } else {
                        m.name.clone()
                    };
                    json!({ "id": m.jid, "name": name, "admin": m.admin })
                })
                .collect();
            value["member_count"] = json!(members.len());
            value["members"] = json!(members);
            value["admin"] = json!(meta.admin);
        }
        Ok(value)
    }

    fn mcp_send(
        &mut self,
        chat_id: &str,
        text: String,
        reply_to: Option<&str>,
    ) -> Result<Value, String> {
        let jid = self.mcp_chat(chat_id)?;
        if !self.store.chats[&jid].can_send() {
            return Err(
                "Messages cannot be sent to this chat (left the group, or only admins can send)."
                    .into(),
            );
        }
        let quote = match reply_to {
            Some(id) => {
                let m = self.mcp_message(&jid, id)?;
                let raw = m.raw.clone().ok_or("This message cannot be replied to.")?;
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
            }
            None => None,
        };
        let known: HashSet<String> = self
            .store
            .messages_for(&jid)
            .iter()
            .map(|m| m.id.clone())
            .collect();
        let message = crate::wa::build_text(&jid, text, &quote, Vec::new(), None);
        if !self.enqueue_prepared(&jid, message, None) {
            return Err("The message could not be queued (storage failed).".into());
        }
        self.schedule_refresh_chats();
        let id = self
            .store
            .messages_for(&jid)
            .iter()
            .rev()
            .find(|m| m.from_me && !known.contains(&m.id))
            .map(|m| m.id.clone());
        Ok(json!({
            "chat_id": jid,
            "message_id": id,
            "status": if self.outbox_connected { "sending" } else { "queued until Zapive reconnects" },
        }))
    }

    fn mcp_mark_read(&mut self, chat_id: &str) -> Result<Value, String> {
        let jid = self.mcp_chat(chat_id)?;
        if let Some(meta) = self.store.chats.get_mut(&jid) {
            meta.unread = 0;
            meta.mentioned = false;
        }
        if let Some(vault) = &self.vault {
            self.store.hydrate(vault, &jid);
        }
        self.send_view_receipts(&jid);
        self.schedule_refresh_chats();
        self.schedule_save();
        Ok(json!({ "chat_id": jid, "unread": 0 }))
    }

    fn mcp_react(&mut self, chat_id: &str, message_id: &str, emoji: &str) -> Result<Value, String> {
        let jid = self.mcp_chat(chat_id)?;
        let m = self.mcp_message(&jid, message_id)?;
        if m.kind == MessageKind::System || m.deleted || m.revoked {
            return Err("This message cannot be reacted to.".into());
        }
        self.wa.send(Cmd::React {
            jid: m.jid.clone(),
            id: m.id.clone(),
            from_me: m.from_me,
            participant: self.key_participant(&m),
            emoji: emoji.to_string(),
        });
        if self.store.apply_reaction(&m.jid, &m.id, "me", emoji) {
            let summary = self
                .store
                .messages_for(&m.jid)
                .iter()
                .find(|x| x.id == m.id)
                .map(reaction_summary)
                .unwrap_or_default();
            self.patch_row(&m.id, |row| {
                row.reactions = summary.0.into();
                row.reactionCount = summary.1.into();
            });
        }
        self.schedule_save();
        Ok(json!({ "chat_id": jid, "message_id": m.id, "emoji": emoji }))
    }
}
