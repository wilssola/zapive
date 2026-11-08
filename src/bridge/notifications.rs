// Desktop notifications and their reply tokens.
use super::*;

pub(super) const NOTIFICATION_TOKEN_TTL: Duration = Duration::from_secs(60);

#[cfg_attr(not(windows), allow(dead_code))] // only the Windows toast has a reply box
pub(super) const NOTIFICATION_REPLY_MAX_BYTES: usize = 4096;

#[derive(Clone)]
pub(super) struct NotificationToken {
    // The linked account the chat belongs to.
    pub(super) account: String,
    pub(super) jid: String,
    pub(super) msg_id: String,
    pub(super) expires: SystemTime,
}

// A token only opens something while it is fresh and still points at a
// chat and a message this session knows about.
pub(super) fn token_usable(
    expires: SystemTime,
    now: SystemTime,
    chat_known: bool,
    message_known: bool,
) -> bool {
    expires > now && chat_known && message_known
}

// Every media kind gets a translated one-liner in notifications.
pub(super) fn notification_body(m: &StoredMessage) -> String {
    if m.sticker {
        return t("preview.sticker");
    }
    match m.kind {
        MessageKind::Image => t("preview.photo"),
        MessageKind::Audio => t("preview.audio"),
        MessageKind::Doc => ta("preview.document", &[&m.text]),
        MessageKind::Video => {
            if m.gif {
                t("preview.gif")
            } else {
                t("preview.video")
            }
        }
        MessageKind::Text | MessageKind::System => m.text.clone(),
    }
}

impl Bridge {
    // ---- notifications ----

    pub(super) fn push_notification(
        &mut self,
        title: String,
        text: String,
        jid: String,
        msg_id: String,
    ) {
        if !self.notifications {
            return;
        }
        self.notify_queue.push((title, text, jid, msg_id));
        if !self.notify_queued {
            self.notify_queued = true;
            self.once(1200, |b| b.flush_notifications());
        }
    }

    // Chime for a live incoming message, like WhatsApp Web: independent of
    // the desktop-notification switch, quiet for muted chats, offline
    // backlog and the conversation being read right now.
    pub(super) fn play_message_sound(
        &self,
        jid: &str,
        mentions_me: bool,
        timestamp: i64,
        system_message: bool,
    ) {
        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_secs() as i64);
        let ctx = crate::sound::ChimeContext {
            setting_on: self.ui.get_message_sound(),
            from_me: false,
            system_message,
            vault_locked: self.vault.as_ref().is_none_or(|vault| vault.locked()),
            chat_muted: self
                .store
                .chats
                .get(jid)
                .is_some_and(|chat| chat.is_muted()),
            chat_archived: self.store.chats.get(jid).is_some_and(|chat| chat.archived),
            mentions_me,
            age_secs: (timestamp > 0).then_some(now - timestamp),
            chat_open: self.current_jid.as_deref() == Some(jid),
            window_focused: self.ui.get_window_active(),
            since_last: crate::sound::since_last_chime(),
        };
        if crate::sound::should_chime(&ctx) {
            crate::sound::play_message();
        }
    }

    pub(super) fn flush_notifications(&mut self) {
        self.notify_queued = false;
        let queue = std::mem::take(&mut self.notify_queue);
        match queue.len() {
            0 => {}
            1 => {
                let (title, text, jid, msg_id) = queue.into_iter().next().expect("one entry");
                if self.vault.as_ref().is_none_or(|vault| vault.locked()) {
                    self.clear_notification_sensitive_state();
                    return;
                }
                if let Some(vault) = &self.vault {
                    self.store.hydrate(vault, &jid);
                }
                let Some(chat) = self.store.chats.get(&jid) else {
                    return;
                };
                if chat.is_muted() || chat.archived {
                    return;
                }
                let message = self
                    .store
                    .messages_for(&jid)
                    .iter()
                    .find(|message| message.id == msg_id)
                    .cloned();
                let Some(message) = message else { return };
                if self.vault.as_ref().is_none_or(|vault| vault.locked()) {
                    self.clear_notification_sensitive_state();
                    return;
                }
                let Some(token) = self.register_notification_token(&jid, &message.id) else {
                    return;
                };
                let toast_title = self.with_account_label(if self.notify_previews {
                    title.clone()
                } else {
                    t("notify.app_name")
                });
                let mut payload = crate::platform::ToastPayload {
                    token: Some(token.clone()),
                    reply: true,
                    ..Default::default()
                };
                let toast_body = if self.notify_previews {
                    if let Some(raw) = message.raw.as_ref() {
                        let inner = raw.get_base_message();
                        let view_once = raw.is_view_once();
                        let thumbnail = if view_once {
                            None
                        } else {
                            inner
                                .image_message
                                .as_option()
                                .and_then(|v| v.jpeg_thumbnail.as_deref())
                                .or_else(|| {
                                    inner
                                        .video_message
                                        .as_option()
                                        .and_then(|v| v.jpeg_thumbnail.as_deref())
                                })
                                .or_else(|| {
                                    inner.sticker_message.as_option().and_then(|v| {
                                        v.context_info
                                            .quoted_message
                                            .image_message
                                            .as_option()
                                            .and_then(|image| image.jpeg_thumbnail.as_deref())
                                    })
                                })
                        };
                        payload.thumbnail =
                            thumbnail.and_then(crate::media::notification_thumbnail);
                        payload.play = !view_once
                            && inner.audio_message.as_option().is_some()
                            && self.media_path.contains_key(&message.id);
                    }
                    text
                } else {
                    t("notify.new_message")
                };
                if let Some(path) = &payload.thumbnail {
                    self.notification_thumbnails
                        .insert(token.clone(), path.clone());
                }
                crate::platform::toast(&toast_title, &toast_body, payload);
                let expired_token = token.clone();
                self.once(60_000, move |bridge| {
                    bridge.notify_tokens.remove(&expired_token);
                    if let Some(path) = bridge.notification_thumbnails.remove(&expired_token) {
                        let _ = std::fs::remove_file(path);
                    }
                });
            }
            _ => {
                if self.vault.as_ref().is_none_or(|vault| vault.locked()) {
                    return;
                }
                let mut names: Vec<&str> = Vec::new();
                if self.notify_previews {
                    for (title, _, jid, _) in &queue {
                        if self
                            .store
                            .chats
                            .get(jid)
                            .is_some_and(|chat| !(chat.is_muted() || chat.archived))
                            && !names.contains(&title.as_str())
                        {
                            names.push(title);
                            if names.len() == 3 {
                                break;
                            }
                        }
                    }
                }
                let count = queue
                    .iter()
                    .filter(|(_, _, jid, _)| {
                        self.store
                            .chats
                            .get(jid)
                            .is_some_and(|chat| !(chat.is_muted() || chat.archived))
                    })
                    .count();
                if count == 0 || (self.notify_previews && names.is_empty()) {
                    return;
                }
                let text = if !self.notify_previews {
                    ta("notify.new_messages", &[&count.to_string(), ""])
                } else if names.len() == 1 {
                    ta("notify.from_one", &[&count.to_string(), names[0]])
                } else {
                    ta(
                        "notify.new_messages",
                        &[&count.to_string(), &names.join(", ")],
                    )
                };
                // When every banner in the group belongs to one chat, a
                // click opens that chat; otherwise it only raises the app.
                let live: Vec<&(String, String, String, String)> = queue
                    .iter()
                    .filter(|(_, _, jid, _)| {
                        self.store
                            .chats
                            .get(jid)
                            .is_some_and(|chat| !(chat.is_muted() || chat.archived))
                    })
                    .collect();
                let single_chat = live
                    .first()
                    .map(|entry| entry.2.clone())
                    .filter(|jid| live.iter().all(|entry| &entry.2 == jid));
                let mut payload = crate::platform::ToastPayload::default();
                if let Some(jid) = single_chat
                    && let Some(last) = live.last().map(|entry| entry.3.clone())
                {
                    if let Some(vault) = &self.vault {
                        self.store.hydrate(vault, &jid);
                    }
                    payload.token = self.register_notification_token(&jid, &last);
                }
                let title = self.with_account_label(t("notify.app_name"));
                crate::platform::toast(&title, &text, payload);
            }
        }
    }

    // With more than one account linked, a banner says whose it is.
    fn with_account_label(&self, title: String) -> String {
        let several = self.vault.as_ref().is_some_and(|v| v.accounts().len() > 1);
        if several {
            ta("notify.account_title", &[&title, &self.account_label()])
        } else {
            title
        }
    }

    // A toast of a background account brings that account on screen
    // before its chat is looked up.
    fn follow_token_account(&mut self, token: &str) {
        let Some(account) = self.notify_tokens.get(token).map(|t| t.account.clone()) else {
            return;
        };
        if account != self.account {
            self.switch_account(&account);
        }
    }

    pub(super) fn clear_notification_sensitive_state(&mut self) {
        self.notify_queue.clear();
        self.notify_queued = false;
        self.notify_tokens.clear();
        for (_, path) in self.notification_thumbnails.drain() {
            let _ = std::fs::remove_file(path);
        }
        crate::media::clean_notification_thumbnails();
    }

    pub(super) fn register_notification_token(
        &mut self,
        jid: &str,
        msg_id: &str,
    ) -> Option<String> {
        use rand::TryRngCore as _;
        let mut bytes = [0u8; 16];
        rand::rngs::OsRng.try_fill_bytes(&mut bytes).ok()?;
        let token = bytes
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        self.expire_notification_tokens();
        self.notify_tokens.insert(
            token.clone(),
            NotificationToken {
                account: self.account.clone(),
                jid: jid.to_string(),
                msg_id: msg_id.to_string(),
                expires: SystemTime::now() + NOTIFICATION_TOKEN_TTL,
            },
        );
        Some(token)
    }

    pub(super) fn expire_notification_tokens(&mut self) {
        let now = SystemTime::now();
        self.notify_tokens.retain(|_, token| token.expires > now);
        let expired: Vec<String> = self
            .notification_thumbnails
            .keys()
            .filter(|token| !self.notify_tokens.contains_key(*token))
            .cloned()
            .collect();
        for token in expired {
            if let Some(path) = self.notification_thumbnails.remove(&token) {
                let _ = std::fs::remove_file(path);
            }
        }
    }

    pub(super) fn take_notification_token(&mut self, token: &str) -> Option<NotificationToken> {
        if self.vault.as_ref().is_none_or(|vault| vault.locked()) {
            self.clear_notification_sensitive_state();
            return None;
        }
        self.expire_notification_tokens();
        let item = self.notify_tokens.remove(token)?;
        if let Some(path) = self.notification_thumbnails.remove(token) {
            let _ = std::fs::remove_file(path);
        }
        let chat_known = self.store.chats.contains_key(&item.jid);
        let message_known = self
            .store
            .messages_for(&item.jid)
            .iter()
            .any(|message| message.id == item.msg_id);
        if !token_usable(item.expires, SystemTime::now(), chat_known, message_known) {
            return None;
        }
        Some(item)
    }

    // Bring the window back from the tray or the taskbar. Showing goes
    // through Slint, which tracks visibility itself; the platform call then
    // wins the foreground the click entitles us to.
    pub(super) fn raise_main_window(&mut self) {
        let _ = self.ui.show();
        crate::platform::focus_window();
    }

    // A click on a toast that has no conversation attached (grouped
    // summaries, call banners): just bring the app forward. Only the
    // Windows toast reports a token-less body click.
    #[cfg_attr(not(windows), allow(dead_code))]
    pub fn on_notification_open_app(&mut self) {
        self.raise_main_window();
    }

    pub fn on_notification_activated(&mut self, token: &str) {
        self.raise_main_window();
        self.follow_token_account(token);
        let Some(item) = self.take_notification_token(token) else {
            return;
        };
        self.open_dm(&item.jid, None);
        self.raise_main_window();
    }

    pub fn on_notification_play(&mut self, token: &str) {
        self.follow_token_account(token);
        let Some(item) = self.take_notification_token(token) else {
            return;
        };
        let valid_audio = self
            .store
            .messages_for(&item.jid)
            .iter()
            .find(|message| message.id == item.msg_id)
            .and_then(|message| message.raw.as_ref())
            .is_some_and(|raw| {
                !raw.is_view_once() && raw.get_base_message().audio_message.as_option().is_some()
            });
        if valid_audio && self.media_path.contains_key(&item.msg_id) {
            self.toggle_audio_background(&item.jid, &item.msg_id);
        }
    }

    #[cfg_attr(not(windows), allow(dead_code))] // only the Windows toast has a reply box
    pub fn on_notification_reply(&mut self, token: &str, text: &str) {
        // Answered from the banner: sent from that account's own client,
        // without taking the screen away from the one being read.
        if let Some(account) = self.notify_tokens.get(token).map(|t| t.account.clone())
            && account != self.account
        {
            if let Some(epoch) = self.epoch_of(&account) {
                let (token, text) = (token.to_string(), text.to_string());
                self.run_headless(epoch, move |b| b.on_notification_reply(&token, &text));
            }
            return;
        }
        let Some(item) = self.take_notification_token(token) else {
            return;
        };
        let mut body: String = text
            .chars()
            .filter(|ch| !ch.is_control() || *ch == '\n')
            .collect();
        body = body
            .trim_matches(|ch: char| ch != '\n' && ch.is_whitespace())
            .to_string();
        if body.is_empty() {
            return;
        }
        if body.len() > NOTIFICATION_REPLY_MAX_BYTES {
            let mut end = NOTIFICATION_REPLY_MAX_BYTES;
            while !body.is_char_boundary(end) {
                end -= 1;
            }
            body.truncate(end);
        }
        if body.is_empty()
            || !self.store.chats.contains_key(&item.jid)
            || !self
                .store
                .chats
                .get(&item.jid)
                .is_some_and(|chat| chat.can_send())
            || !self
                .store
                .messages_for(&item.jid)
                .iter()
                .any(|message| message.id == item.msg_id)
        {
            return;
        }
        let message = crate::wa::build_text(&item.jid, body, &None, Vec::new(), None);
        if !self.enqueue_prepared(&item.jid, message, None) {
            return;
        }
        if self.ui.get_send_receipts() {
            let message = self
                .store
                .messages_for(&item.jid)
                .iter()
                .find(|m| m.id == item.msg_id)
                .cloned();
            if let Some(message) = message {
                self.wa.send(Cmd::MarkRead {
                    jid: item.jid.clone(),
                    sender: self.key_participant(&message),
                    ids: vec![item.msg_id.clone()],
                });
                self.store
                    .mark_read_sent(&item.jid, std::slice::from_ref(&item.msg_id));
            }
        }
        self.schedule_save();
    }

    pub(super) fn toggle_audio_background(&mut self, jid: &str, id: &str) {
        if self.vault.as_ref().is_none_or(|vault| vault.locked())
            || !self.media_path.contains_key(id)
            || !self
                .store
                .messages_for(jid)
                .iter()
                .any(|message| message.id == id)
        {
            return;
        }
        if self.audio.as_ref().is_some_and(|audio| audio.id == id) {
            self.toggle_audio(id);
        } else {
            self.start_audio_for_notification(jid, id);
        }
    }

    pub(super) fn start_audio_for_notification(&mut self, jid: &str, id: &str) {
        let Some(message) = self
            .store
            .messages_for(jid)
            .iter()
            .find(|message| message.id == id)
        else {
            return;
        };
        let duration = message.duration_sec.max(1) as f64;
        self.stop_audio();
        self.audio = Some(AudioState {
            id: id.to_string(),
            jid: jid.to_string(),
            duration,
            paused: false,
            pending_offset: Some(0.0),
            player: None,
        });
        self.spin_audio_at(0.0);
        self.sync_mini_player();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_is_usable_only_when_fresh_and_resolvable() {
        let now = SystemTime::now();
        let later = now + Duration::from_secs(1);
        let earlier = now - Duration::from_secs(1);
        assert!(token_usable(later, now, true, true));
        assert!(!token_usable(now, now, true, true));
        assert!(!token_usable(earlier, now, true, true));
        assert!(!token_usable(later, now, false, true));
        assert!(!token_usable(later, now, true, false));
        assert!(!token_usable(later, now, false, false));
    }

    #[test]
    fn default_ttl_leaves_the_toast_clickable_for_a_minute() {
        let now = SystemTime::now();
        assert!(token_usable(
            now + NOTIFICATION_TOKEN_TTL,
            now + Duration::from_secs(59),
            true,
            true
        ));
        assert!(!token_usable(
            now + NOTIFICATION_TOKEN_TTL,
            now + Duration::from_secs(61),
            true,
            true
        ));
    }
}
