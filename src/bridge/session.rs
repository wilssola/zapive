// Boot, unlock, login events, logout and the session lifecycle.
use super::*;

impl Bridge {
    // ---- boot ----

    // Called from main once the vault is open (or right away without PIN).
    pub fn boot(&mut self, vault: Vault, registered: bool) {
        crate::media::clean_tmp();
        crate::media::migrate_cache();
        // main opened this account's session and pointed the vault at it.
        self.account = vault.account();
        self.registered = registered;
        let known: Vec<String> = vault.accounts().into_iter().map(|a| a.id).collect();
        crate::paths::sweep_accounts(&known);
        self.store.load_from(&vault);
        // Known from the last run, so own rows have a face before the
        // connection comes up.
        if let Some(me) = vault
            .accounts()
            .into_iter()
            .find(|a| a.id == self.account)
            .map(|a| a.jid)
            .filter(|s| !s.is_empty())
        {
            self.store.set_self(&[&me]);
            self.self_jid = me;
        }
        self.media_key = vault.key_handle();
        if let Some(volume) = vault
            .setting_get("video_volume")
            .and_then(|v| v.parse::<f32>().ok())
        {
            self.video_volume = volume.clamp(0.0, 1.0);
        }
        self.wa.send(Cmd::MediaKey(vault.key_handle()));
        // On unless turned off, so existing installs keep their banners.
        self.notifications = vault.setting_get("notifications").as_deref() != Some("0");
        self.ui.set_notifications(self.notifications);
        self.notify_previews = vault.setting_get("notify_previews").as_deref() == Some("1");
        self.ui.set_notify_previews(self.notify_previews);
        self.ui.set_blur_names(vault.setting_get("privacy_blur_names").as_deref() == Some("1"));
        self.ui.set_blur_photos(vault.setting_get("privacy_blur_photos").as_deref() == Some("1"));
        self.ui.set_blur_messages(vault.setting_get("privacy_blur_messages").as_deref() == Some("1"));
        self.ui.set_blur_media(vault.setting_get("privacy_blur_media").as_deref() == Some("1"));
        self.vault = Some(vault);
        self.load_translation_settings();
        self.load_mcp();
        self.restore_outbox();
        self.expire_notification_tokens();
        println!(
            "[store] loaded chats={} msgChats={} total={}",
            self.store.chats.len(),
            self.store.messages.len(),
            self.store.total_messages()
        );
        self.ui
            .set_pin_set(self.vault.as_ref().is_some_and(|v| v.has_pin()));
        self.ui
            .set_screen(if registered { "main" } else { "login" }.into());
        self.refresh_accounts();
        self.refresh_labels();
        self.refresh_chats();
        self.refresh_statuses();
        self.refresh_calls();
        // Chats stored before the search index existed get indexed from
        // the vault in the background, a few at a time.
        self.once(4000, |b| b.index_backfill());
        // Before any call can start, so the first one already uses the
        // devices the user picked.
        self.load_devices();
        self.wa.send(Cmd::Start);
        // Events that arrived while the PIN screen was up.
        for f in std::mem::take(&mut self.locked_backlog) {
            f(self);
        }
    }

    pub(super) fn handle_unlock(&mut self, pin: &str) {
        // The vault was parked here until the PIN arrives.
        let Some(vault) = &mut self.vault else { return };
        if vault.unlock(pin) {
            self.ui.set_lock_error("".into());
            let vault = self.vault.take().expect("vault present");
            let registered = self.pending_registered;
            self.boot(vault, registered);
        } else {
            self.ui.set_lock_error(t("pin.wrong").into());
        }
    }

    // Vault handed over before unlock; boot happens in handle_unlock.
    pub fn park_locked_vault(&mut self, vault: Vault, registered: bool) {
        self.clear_notification_sensitive_state();
        self.vault = Some(vault);
        self.pending_registered = registered;
        self.ui.set_screen("locked".into());
    }

    pub(super) fn handle_pairing(&mut self, phone: &str) {
        let digits: String = phone.chars().filter(|c| c.is_ascii_digit()).collect();
        if digits.len() < 8 {
            self.ui.set_pairing_code(t("error.invalid_number").into());
            return;
        }
        self.ui.set_pairing_code(t("pairing.generating").into());
        self.wa.send(Cmd::PairWithCode(digits));
    }

    // ---- WhatsApp events (called through ui_apply) ----

    pub fn on_qr(&mut self, code: &str) {
        self.ui.set_qr_image(qr_image(code));
        self.ui.set_status_text(t("status.scan_qr").into());
    }

    pub fn on_pairing_code(&mut self, pretty: &str) {
        self.ui.set_pairing_code(pretty.into());
        self.ui.set_status_text(t("status.pairing_hint").into());
    }

    pub fn on_pairing_failed(&mut self) {
        self.ui.set_pairing_code(t("pairing.failed").into());
    }

    pub fn on_status(&mut self, text: &str) {
        self.ui.set_status_text(text.into());
        self.outbox_connected = false;
    }

    pub fn on_open(&mut self, pn: &str, lid: &str, name: &str) {
        self.ui.set_alert_open(false);
        self.ui.set_status_text(t("status.connected").into());
        if self.ui.get_screen() == "login" {
            self.ui.set_screen("main".into());
        }
        // Conversations kept from an earlier session are picked up again
        // only by the number they belong to. Anyone else pairing into
        // this slot starts clean, before the first history chunk lands.
        let me = normalize_jid(if pn.is_empty() { lid } else { pn });
        let kept = self.account_entry().map(|a| a.jid).unwrap_or_default();
        if !kept.is_empty() && kept != normalize_jid(pn) && kept != normalize_jid(lid) {
            println!("[store] a different number paired; dropping the kept conversations");
            self.wipe_conversations();
        }
        self.registered = true;
        self.store.set_self(&[pn, lid]);
        // A LID-primary session may carry no phone jid at all; our own
        // rows (voice notes, search hits) still need an identity.
        self.self_jid = me;
        let jid = self.self_jid.clone();
        let name = name.trim().to_string();
        self.update_account(|a| {
            a.jid = jid;
            if !name.is_empty() {
                a.name = name;
            }
        });
        self.refresh_accounts();
        self.queue_avatar(&self.self_jid.clone());
        if !self.groups_fetched {
            self.groups_fetched = true;
            self.wa.send(Cmd::FetchGroups);
        }
        // Backfill old conversations via on-demand history sync (the
        // pairing history payload arrives thin).
        self.once(8000, |b| b.pull_older_history());
        self.schedule_refresh_chats();
        self.outbox_connected = true;
        self.resume_preparations();
        self.dispatch_outbox();
    }

    // Errors the user must act on get a modal instead of a silent retry
    // loop in the status line.
    pub fn on_fatal(&mut self, kind: &str) {
        let conflict = kind == "conflict";
        self.ui.set_alert_title(
            t(if conflict {
                "alert.conflict_title"
            } else {
                "alert.offline_title"
            })
            .into(),
        );
        self.ui.set_alert_text(
            t(if conflict {
                "alert.conflict_body"
            } else {
                "alert.offline_body"
            })
            .into(),
        );
        self.ui.set_alert_action(
            t(if conflict {
                "alert.reconnect"
            } else {
                "alert.retry"
            })
            .into(),
        );
        self.ui.set_alert_open(true);
    }

    pub fn on_logged_out(&mut self) {
        self.registered = false;
        self.ui.set_pairing_mode(false);
        self.ui.set_pairing_code("".into());
        self.ui.set_qr_image(crate::qr::empty_image());
        self.ui.set_screen("login".into());
        self.close_session_views();
        // The session is gone either way; the conversations go with it
        // only when the user asked for that. Kept, they are what a new
        // pairing of the same number picks up again instead of waiting
        // for the phone to send everything once more.
        if std::mem::take(&mut self.wipe_on_logout) {
            self.wipe_conversations();
            self.forget_account_identity();
        } else {
            self.save_now();
        }
        self.refresh_accounts();
    }

    pub(super) fn logout(&mut self, wipe: bool) {
        self.wipe_on_logout = wipe;
        self.ui.set_status_text(t("status.logging_out").into());
        self.wa.send(Cmd::Logout);
    }

    pub(super) fn save_now(&mut self) {
        if let Some(vault) = self.vault.take() {
            if !vault.locked() {
                self.store.save_to(&vault);
            }
            self.vault = Some(vault);
        }
    }

    // Whatever is on screen belongs to a session that just ended.
    pub(super) fn close_session_views(&mut self) {
        self.chat_typing.clear();
        self.presence_subscribed.clear();
        self.ui.invoke_set_composer("".into(), 0);
        self.mentions.clear();
        self.ui.set_mention_open(false);
        self.ui.set_details_open(false);
        self.cancel_preview();
        self.current_jid = None;
        self.ui.set_typing_members(ModelRc::default());
        self.ui.set_typing_extra(0);
        self.unread_boundary = None;
        self.groups_fetched = false;
        self.ui.set_chat_open(false);
        self.ui.set_settings_open(false);
        self.ui.set_info_open(false);
        self.ui.set_account_menu_open(false);
        self.close_video();
        self.select_end();
    }

    // Drops the loaded account's conversations, in memory and on disk.
    pub(super) fn wipe_conversations(&mut self) {
        self.ui.set_details_open(false);
        self.cancel_preview();
        self.current_jid = None;
        self.ui.set_typing_members(ModelRc::default());
        self.ui.set_typing_extra(0);
        self.unread_boundary = None;
        self.ui.set_chat_open(false);
        self.clear_store_state();
        if let Some(vault) = &self.vault {
            vault.del_prefix("store:");
            // The on-disk data key can outlive the account (only the
            // vault's own `store:` keys were just wiped above), so a
            // fresh pairing must not inherit the previous account's
            // search index.
            self.store.search.reset(vault);
        }
        self.refresh_labels();
        self.refresh_chats();
        self.refresh_statuses();
        self.refresh_calls();
    }

    // Empties everything derived from one account's store. The store on
    // disk is untouched: this is also how the bridge lets go of an
    // account it is switching away from.
    pub(super) fn clear_store_state(&mut self) {
        self.reset_translation();
        self.clear_notification_sensitive_state();
        self.preparation_states.clear();
        self.preparations_active.clear();
        self.outbox.clear();
        self.outbox_connected = false;
        self.store = Store::default();
        self.chat_typing.clear();
        self.presence_subscribed.clear();
        self.typing_gen += 1;
        self.ui.invoke_set_composer("".into(), 0);
        self.self_jid.clear();
        self.chats_model.set_vec(Vec::new());
        self.messages_model.set_vec(Vec::new());
        self.status_model.set_vec(Vec::new());
        self.calls_model.set_vec(Vec::new());
        self.ui.set_chats_unread_count(0);
        self.ui.set_channels_unread_count(0);
        self.ui.set_communities_unread_count(0);
        self.ui.set_calls_unseen_count(0);
        self.ui.set_statuses_unseen(false);
        self.warm_order.clear();
        self.truncated.clear();
        self.scroll_pos.clear();
        self.history_batches = 0;
        self.history_pending = false;
        self.sync_batches = 0;
        self.sync_messages = 0;
        self.requested_channels.clear();
        self.pending_forward.clear();
        self.clear_notification_sensitive_state();
        self.audio_buffers.clear();
        self.waves.clear();
        self.avatars.clear();
        self.requested_avatars.clear();
        self.avatar_tries.clear();
        self.media_inflight.clear();
        self.media_previews.clear();
        self.preview_requested.clear();
        self.media_failed.clear();
        self.media_path.clear();
        self.decoded.clear();
        self.decoded_order.clear();
        self.decoded_bytes = 0;
        self.animated.clear();
        self.anim_order.clear();
        self.anim_bytes = 0;
        self.stickers.clear();
        self.sticker_order.clear();
        self.sticker_bytes = 0;
        self.group_meta_at.clear();
        self.common_groups_cache.clear();
        self.common_groups_pending.clear();
        self.search_model.set_vec(Vec::new());
        self.chat_search_model.set_vec(Vec::new());
        self.label_model.set_vec(Vec::new());
        self.label_chip_model.set_vec(Vec::new());
    }
}
