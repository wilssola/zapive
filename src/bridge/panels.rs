// Starred and pinned message lists, keyboard-shortcut actions and the
// reaction sheet's tabs. All data lives in the store; these functions only
// shape it for the window.
use super::*;
use crate::store::{active_pins, adjacent_index, reaction_groups, shortcut_index};

const STARRED_LIMIT: usize = 500;
const SNIPPET_CHARS: usize = 160;

fn snippet(text: &str) -> String {
    let flat: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= SNIPPET_CHARS {
        return flat;
    }
    let cut: String = flat.chars().take(SNIPPET_CHARS).collect();
    format!("{}…", cut.trim_end())
}

impl Bridge {
    // ---- F2: starred messages ----

    // Every star lives in `starred_ids`; messages of cold chats are not in
    // memory, so each id is looked up in the vault before listing.
    fn hydrate_starred(&mut self, only_chat: Option<&str>) {
        let Some(vault) = &self.vault else { return };
        if let Some(chat) = only_chat {
            self.store.hydrate(vault, chat);
            return;
        }
        let jids: Vec<String> = self.store.chats.keys().cloned().collect();
        for jid in jids {
            if !self.store.hydrated.contains(&jid) {
                self.store.hydrate(vault, &jid);
            }
        }
    }

    pub(super) fn open_starred(&mut self, current_only: bool) {
        let only = if current_only { self.current_jid.clone() } else { None };
        if current_only && only.is_none() {
            return;
        }
        self.hydrate_starred(only.as_deref());
        let found: Vec<StoredMessage> = self
            .store
            .starred_messages(only.as_deref())
            .into_iter()
            .take(STARRED_LIMIT)
            .cloned()
            .collect();
        let mut faces = Vec::new();
        let rows: Vec<StarredItem> = found
            .iter()
            .map(|m| {
                let face = if m.from_me {
                    self.self_jid.clone()
                } else if m.sender_jid.is_empty() {
                    m.jid.clone()
                } else {
                    self.store.canon_owned(&m.sender_jid)
                };
                faces.push(face.clone());
                let sender = if m.from_me { t("reactions.you") } else { m.sender.clone() };
                let avatar = self.avatar_for(&face);
                StarredItem {
                    jid: m.jid.clone().into(),
                    msg_id: m.id.clone().into(),
                    chat_name: self.store.chat_name(&m.jid).into(),
                    sender: sender.into(),
                    text: self.store.named_mentions(&snippet(&crate::store::preview_body(m))).into(),
                    time: format_time(m.timestamp).into(),
                    avatar: avatar.unwrap_or_else(empty_image),
                }
            })
            .collect();
        for face in faces {
            self.queue_avatar(&face);
        }
        self.ui.set_starred_items(ModelRc::new(VecModel::from(rows)));
    }

    pub(super) fn open_starred_item(&mut self, jid: &str, id: &str) {
        if jid.is_empty() || id.is_empty() {
            return;
        }
        self.ui.set_starred_open(false);
        self.open_hit(jid, id);
    }

    // ---- F3: pinned messages ----

    // Rebuilds the banner list for the open chat. Called whenever the chat
    // opens or a pin changes.
    pub(super) fn refresh_pinned(&mut self) {
        let Some(jid) = self.current_jid.clone() else {
            self.ui.set_pinned_items(ModelRc::new(VecModel::from(Vec::<PinnedItem>::new())));
            return;
        };
        let items: Vec<PinnedItem> = active_pins(self.store.messages_for(&jid), now_secs())
            .into_iter()
            .map(|m| PinnedItem {
                msg_id: m.id.clone().into(),
                sender: if m.from_me { t("reactions.you") } else { m.sender.clone() }.into(),
                text: self.store.named_mentions(&snippet(&crate::store::preview_body(m))).into(),
            })
            .collect();
        let count = items.len() as i32;
        self.ui.set_pinned_items(ModelRc::new(VecModel::from(items)));
        if self.ui.get_pinned_index() >= count {
            self.ui.set_pinned_index(0);
        }
    }

    pub(super) fn open_pinned(&mut self, id: &str) {
        let Some(jid) = self.current_jid.clone() else { return };
        if id.is_empty() {
            return;
        }
        self.open_hit(&jid, id);
    }

    pub(super) fn unpin_message(&mut self, id: &str) {
        let Some(m) = self.find_message(id).cloned() else { return };
        if m.pinned_until <= now_secs() {
            return;
        }
        let participant = self.key_participant(&m);
        self.wa.send(Cmd::PinMessage { jid: m.jid, id: m.id, from_me: m.from_me, participant, pin: false });
    }

    // ---- F4: keyboard shortcuts ----

    fn visible_jids(&self) -> Vec<String> {
        self.visible_chats().into_iter().map(|(jid, ..)| jid).collect()
    }

    pub(super) fn open_chat_at(&mut self, position: i32) {
        let list = self.visible_jids();
        if let Some(index) = shortcut_index(position, list.len()) {
            self.open_dm(&list[index], None);
        }
    }

    pub(super) fn select_adjacent_chat(&mut self, delta: i32) {
        let list = self.visible_jids();
        let current = self.current_jid.as_ref().and_then(|jid| list.iter().position(|candidate| candidate == jid));
        if let Some(index) = adjacent_index(current, list.len(), delta) {
            self.open_dm(&list[index], None);
        }
    }

    pub(super) fn toggle_archive_current(&mut self) {
        if self.current_jid.is_some() {
            self.toggle_archive();
        }
    }

    pub(super) fn toggle_mute_current(&mut self) {
        let Some(jid) = self.current_jid.clone() else { return };
        let muted = self.store.chats.get(&jid).is_some_and(|chat| chat.is_muted());
        self.mute_current(if muted { 0 } else { -1 });
    }

    // The newest message of mine that can still be edited.
    pub(super) fn edit_last_message(&mut self) {
        let Some(jid) = self.current_jid.clone() else { return };
        let id = self
            .store
            .messages_for(&jid)
            .iter()
            .rev()
            .find(|m| message_editable(m))
            .map(|m| m.id.clone());
        if let Some(id) = id {
            self.message_action(&id, "edit");
        }
    }

    // ---- F5: reaction tabs ----

    pub(super) fn refresh_reaction_tabs(&mut self, id: &str) {
        let tabs: Vec<ReactionTab> = self
            .find_message(id)
            .map(|m| {
                reaction_groups(&m.reactions)
                    .into_iter()
                    .map(|(emoji, who)| ReactionTab { emoji: emoji.into(), count: who.len().min(i32::MAX as usize) as i32 })
                    .collect()
            })
            .unwrap_or_default();
        self.ui.set_reaction_tabs(ModelRc::new(VecModel::from(tabs)));
    }

    // Removing my reaction is sending an empty one, exactly like tapping
    // the same emoji again.
    pub(super) fn remove_own_reaction(&mut self, id: &str) {
        let Some(m) = self.find_message(id).cloned() else { return };
        let mine = m.reactions.get("me").cloned().unwrap_or_default();
        if clean_text(&mine).is_empty() {
            return;
        }
        self.send_reaction(id, &mine);
        self.show_reactions(id);
        if self.find_message(id).is_none_or(|m| m.reactions.values().all(|emoji| clean_text(emoji).is_empty())) {
            self.ui.set_reactions_open(false);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::snippet;

    #[test]
    fn snippet_collapses_whitespace_and_truncates() {
        assert_eq!(snippet("a  b\n c"), "a b c");
        let long = "x".repeat(400);
        let out = snippet(&long);
        assert!(out.ends_with('…'));
        assert_eq!(out.chars().count(), super::SNIPPET_CHARS + 1);
    }
}
