// The only module that touches the Slint window. Lives on the UI thread;
// the WhatsApp side reaches it through ui_apply, and user actions leave
// through WaService commands. Port of src/bridge.ts on master (phases 1-2:
// login, chat list, conversation, text messages, replies, backfill).
use crate::i18n::{t, ta};
use crate::markup::{MentionTarget, has_markup, to_markdown};
use crate::qr::{empty_image, qr_image};
use crate::store::{
    MessageKind, Store, StoredMessage, clean_text, compute_preview, display_id, format_day,
    format_number, format_time, is_channel, is_group, normalize_jid, preview_body,
    reaction_summary, ticks_for,
};
use crate::vault::Vault;
use crate::wa::{Cmd, HistoryChunk, QuoteRef, WaService};
use crate::{AppWindow, ChatItem, MessageItem};
use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::time::{Duration, Instant};
use whatsapp_rust::types::events::MessageBatch;
use whatsapp_rust::waproto::whatsapp as wa;

const MAX_HISTORY_BATCHES: u32 = 20;

pub struct Bridge {
    ui: AppWindow,
    wa: WaService,
    vault: Option<Vault>,
    store: Store,
    chats_model: Rc<VecModel<ChatItem>>,
    messages_model: Rc<VecModel<MessageItem>>,
    current_jid: Option<String>,
    self_jid: String,
    search_text: String,
    tab: String,
    view: String,
    groups_fetched: bool,
    // Message id being replied to.
    reply_to: Option<String>,
    // Chats whose list currently starts mid-conversation (after a jump).
    truncated: HashSet<String>,
    scroll_pos: HashMap<String, f32>,
    // History backfill state.
    history_batches: u32,
    history_pending: bool,
    scroll_up_fetch: bool,
    last_scroll_load: Option<Instant>,
    refresh_queued: bool,
    save_queued: bool,
    pending_registered: bool,
    // Keeps one-shot timers alive until they fire.
    oneshots: Vec<slint::Timer>,
}

thread_local! {
    static BRIDGE: RefCell<Option<Bridge>> = const { RefCell::new(None) };
}

// Runs a closure against the Bridge on the UI thread, from any thread.
// Dropped silently if the app is shutting down.
pub fn ui_apply(f: impl FnOnce(&mut Bridge) + Send + 'static) {
    let _ = slint::invoke_from_event_loop(move || apply_now(f));
}

fn apply_now(f: impl FnOnce(&mut Bridge)) {
    BRIDGE.with(|cell| {
        if let Some(bridge) = cell.borrow_mut().as_mut() {
            f(bridge);
        }
    });
}

// UI callbacks can fire synchronously from inside a Bridge method (e.g. a
// scroll write triggering load-older); deferring one tick avoids the
// re-entrant borrow.
fn defer(f: impl FnOnce(&mut Bridge) + 'static) {
    let cell = RefCell::new(Some(f));
    slint::Timer::single_shot(Duration::ZERO, move || {
        if let Some(f) = cell.borrow_mut().take() {
            apply_now(f);
        }
    });
}

pub fn install(ui: &AppWindow, wa: WaService) {
    let chats_model = Rc::new(VecModel::<ChatItem>::default());
    let messages_model = Rc::new(VecModel::<MessageItem>::default());
    ui.set_chats(ModelRc::from(chats_model.clone()));
    ui.set_messages(ModelRc::from(messages_model.clone()));
    wire_callbacks(ui);
    let bridge = Bridge {
        ui: ui.clone_strong(),
        wa,
        vault: None,
        store: Store::default(),
        chats_model,
        messages_model,
        current_jid: None,
        self_jid: String::new(),
        search_text: String::new(),
        tab: "all".into(),
        view: "chats".into(),
        groups_fetched: false,
        reply_to: None,
        truncated: HashSet::new(),
        scroll_pos: HashMap::new(),
        history_batches: 0,
        history_pending: false,
        scroll_up_fetch: false,
        last_scroll_load: None,
        refresh_queued: false,
        save_queued: false,
        pending_registered: false,
        oneshots: Vec::new(),
    };
    BRIDGE.with(|cell| *cell.borrow_mut() = Some(bridge));
}

fn wire_callbacks(ui: &AppWindow) {
    ui.set_status_text(t("status.connecting").into());

    ui.on_request_pairing_code(|phone| {
        let phone = phone.to_string();
        defer(move |b| b.handle_pairing(&phone));
    });
    ui.on_alert_retry(|| {
        defer(|b| {
            b.ui.set_alert_open(false);
            b.ui.set_status_text(t("status.connecting").into());
            b.wa.send(Cmd::Resume);
        });
    });
    ui.on_alert_dismiss(|| defer(|b| b.ui.set_alert_open(false)));
    ui.on_logout(|| {
        defer(|b| {
            b.ui.set_settings_open(false);
            b.ui.set_status_text(t("status.loggingOut").into());
            b.wa.send(Cmd::Logout);
        });
    });
    ui.on_unlock(|pin| {
        let pin = pin.to_string();
        defer(move |b| b.handle_unlock(&pin));
    });
    ui.on_theme_changed(|mode| {
        let mode = mode.to_string();
        defer(move |b| b.handle_theme(&mode));
    });
    ui.on_language_changed(|mode| {
        let mode = mode.to_string();
        defer(move |b| {
            if let Some(vault) = &b.vault {
                vault.setting_set("language", &mode);
            }
            b.ui.set_settings_status(t("lang.restart").into());
        });
    });

    ui.on_open_chat(|jid| {
        let jid = jid.to_string();
        defer(move |b| b.open_dm(&jid, None));
    });
    ui.on_open_dm(|target| {
        let target = target.to_string();
        defer(move |b| {
            // Styled-text links carry either a jid (mention) or a real URL.
            if target.starts_with("http://") || target.starts_with("https://") {
                crate::platform::open_path(&target);
            } else {
                b.open_dm(&target, None);
            }
        });
    });
    ui.on_search_changed(|text| {
        let text = text.to_string();
        defer(move |b| {
            b.search_text = text.trim().to_lowercase();
            b.refresh_chats();
        });
    });
    ui.on_tab_changed(|tab| {
        let tab = tab.to_string();
        defer(move |b| {
            b.tab = tab;
            b.refresh_chats();
        });
    });
    ui.on_view_changed(|view| {
        let view = view.to_string();
        defer(move |b| {
            b.view = view;
            b.refresh_chats();
        });
    });
    ui.on_send_message(|text| {
        let text = text.to_string();
        defer(move |b| b.handle_send_text(&text));
    });
    ui.on_load_older(|| defer(|b| b.handle_scroll_up_load()));
    ui.on_jump_latest(|| {
        defer(|b| {
            if let Some(jid) = &b.current_jid {
                let jid = jid.clone();
                b.scroll_pos.remove(&jid);
            }
            b.scroll_to_end();
        });
    });
    ui.on_request_reply(|id| {
        let id = id.to_string();
        defer(move |b| b.start_reply(&id));
    });
    ui.on_cancel_reply(|| defer(|b| b.clear_reply()));
    ui.on_open_quote(|id| {
        let id = id.to_string();
        defer(move |b| {
            if let Some(jid) = b.current_jid.clone()
                && !id.is_empty()
            {
                b.open_dm(&jid, Some(&id));
            }
        });
    });
    ui.on_copy_text(|id| {
        let id = id.to_string();
        defer(move |b| {
            if let Some(m) = b.find_message(&id) {
                let text = m.text.clone();
                let _ = arboard::Clipboard::new().and_then(|mut c| c.set_text(text));
            }
        });
    });
}

impl Bridge {
    // ---- boot ----

    // Called from main once the vault is open (or right away without PIN).
    pub fn boot(&mut self, vault: Vault, registered: bool) {
        self.store.load_from(&vault);
        self.vault = Some(vault);
        println!(
            "[store] loaded chats={} msgChats={} total={}",
            self.store.chats.len(),
            self.store.messages.len(),
            self.store.total_messages()
        );
        self.ui.set_pin_set(self.vault.as_ref().is_some_and(|v| v.has_pin()));
        self.ui.set_screen(if registered { "main" } else { "login" }.into());
        self.refresh_chats();
        self.wa.send(Cmd::Start);
    }

    fn handle_unlock(&mut self, pin: &str) {
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
        self.vault = Some(vault);
        self.pending_registered = registered;
        self.ui.set_screen("locked".into());
    }

    fn handle_theme(&mut self, mode: &str) {
        self.ui.set_theme_mode(mode.into());
        if let Some(vault) = &self.vault {
            vault.setting_set("theme", mode);
        }
        match mode {
            "dark" => self.ui.set_dark_theme(true),
            "light" => self.ui.set_dark_theme(false),
            _ => self.ui.set_dark_theme(crate::platform::system_dark()),
        }
    }

    fn handle_pairing(&mut self, phone: &str) {
        let digits: String = phone.chars().filter(|c| c.is_ascii_digit()).collect();
        if digits.len() < 8 {
            self.ui.set_pairing_code(t("error.invalidNumber").into());
            return;
        }
        self.ui.set_pairing_code(t("pairing.generating").into());
        self.wa.send(Cmd::PairWithCode(digits));
    }

    // ---- WhatsApp events (called through ui_apply) ----

    pub fn on_qr(&mut self, code: &str) {
        self.ui.set_qr_image(qr_image(code));
        self.ui.set_status_text(t("status.scanQr").into());
    }

    pub fn on_pairing_code(&mut self, pretty: &str) {
        self.ui.set_pairing_code(pretty.into());
        self.ui.set_status_text(t("status.pairingHint").into());
    }

    pub fn on_pairing_failed(&mut self) {
        self.ui.set_pairing_code(t("pairing.failed").into());
    }

    pub fn on_status(&mut self, text: &str) {
        self.ui.set_status_text(text.into());
    }

    pub fn on_open(&mut self, pn: &str, lid: &str) {
        self.ui.set_alert_open(false);
        self.ui.set_status_text(t("status.connected").into());
        if self.ui.get_screen() == "login" {
            self.ui.set_screen("main".into());
        }
        self.store.set_self(&[pn, lid]);
        self.self_jid = normalize_jid(pn);
        if !self.groups_fetched {
            self.groups_fetched = true;
            self.wa.send(Cmd::FetchGroups);
        }
        // Backfill old conversations via on-demand history sync (the
        // pairing history payload arrives thin).
        self.once(8000, |b| b.pull_older_history());
        self.schedule_refresh_chats();
    }

    // Errors the user must act on get a modal instead of a silent retry
    // loop in the status line.
    pub fn on_fatal(&mut self, kind: &str) {
        let conflict = kind == "conflict";
        self.ui
            .set_alert_title(t(if conflict { "alert.conflictTitle" } else { "alert.offlineTitle" }).into());
        self.ui
            .set_alert_text(t(if conflict { "alert.conflictBody" } else { "alert.offlineBody" }).into());
        self.ui
            .set_alert_action(t(if conflict { "alert.reconnect" } else { "alert.retry" }).into());
        self.ui.set_alert_open(true);
    }

    pub fn on_logged_out(&mut self) {
        self.current_jid = None;
        self.groups_fetched = false;
        self.ui.set_pairing_mode(false);
        self.ui.set_pairing_code("".into());
        self.ui.set_chat_open(false);
        self.ui.set_screen("login".into());
        // Wipe local conversation data along with the session.
        self.store = Store::default();
        self.chats_model.set_vec(Vec::new());
        self.messages_model.set_vec(Vec::new());
        if let Some(vault) = &self.vault {
            vault.del_prefix("store:");
        }
    }

    pub fn on_groups(&mut self, groups: &[(String, String, bool, Option<String>)]) {
        for (jid, subject, is_community, parent) in groups {
            if subject.is_empty() {
                continue;
            }
            // Seed the sidebar with participating groups before any message.
            if !self.store.chats.contains_key(jid) {
                self.store.upsert_chat(jid, Some(subject), 0, None, None, None);
            }
            self.store.set_name(jid, subject);
            if let Some(entry) = self.store.chats.get_mut(jid) {
                entry.is_community = *is_community;
                entry.community = parent.clone().unwrap_or_default();
            }
        }
        self.schedule_refresh_chats();
    }

    pub fn on_history_chunk(&mut self, chunk: HistoryChunk) {
        for (jid, name) in &chunk.pushnames {
            self.store.upsert_contact(jid, None, Some(name));
        }
        for c in &chunk.chats {
            self.store.upsert_chat(&c.jid, c.name.as_deref(), c.timestamp, c.unread, c.pinned, c.archived);
        }
        let mut added = 0usize;
        let mut added_to_current = false;
        for web in &chunk.messages {
            if let Some(stored) = crate::wa_map::from_history(&mut self.store, web) {
                let jid = stored.jid.clone();
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
            }
            // else: data is stored; the view catches up on next open.
        }
        self.scroll_up_fetch = false;
        self.schedule_refresh_chats();
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
                    self.schedule_save();
                }
                continue;
            }
            let content: &wa::Message = &inbound.message;
            // Deleted-for-everyone arrives as a protocol REVOKE.
            if let Some(pm) = content.protocol_message.as_option()
                && pm.r#type == Some(wa::message::protocol_message::Type::Revoke)
                && let Some(target) = pm.key.as_option().and_then(|k| k.id.clone())
            {
                let chat_jid = self.store.canon_owned(&normalize_jid(&chat));
                if self.store.mark_deleted(&chat_jid, &target)
                    && Some(&chat_jid) == self.current_jid.as_ref()
                {
                    self.patch_row(&target, |row| {
                        row.deleted = true;
                        row.kind = "text".into();
                        row.text = t("msg.deleted").into();
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
                self.apply_reaction(&chat, &target, &reactor, rm.text.as_deref().unwrap_or(""), info.source.is_from_me);
                continue;
            }
            let Some(stored) = crate::wa_map::from_live(&mut self.store, inbound) else { continue };
            let jid = stored.jid.clone();
            let from_me = stored.from_me;
            let mentions_me = stored.mentions_me;
            if !self.store.add_message(stored) {
                continue;
            }
            if Some(&jid) == self.current_jid.as_ref() {
                self.push_message_row(&jid);
            } else if !from_me
                && let Some(meta) = self.store.chats.get_mut(&jid)
            {
                meta.unread += 1;
                if mentions_me {
                    meta.mentioned = true;
                }
                // Toast notifications land in phase 7.
            }
        }
        self.schedule_refresh_chats();
    }

    pub fn on_receipt(&mut self, chat: &str, ids: &[String], status: u32) {
        let jid = self.store.canon_owned(&normalize_jid(chat));
        let mut changed = Vec::new();
        for id in ids {
            if self.store.set_status(&jid, id, status) {
                changed.push(id.clone());
            }
        }
        if Some(&jid) == self.current_jid.as_ref() {
            let patches: Vec<(String, &'static str, bool)> = changed
                .iter()
                .filter_map(|id| {
                    self.store.messages_for(&jid).iter().find(|m| &m.id == id).map(|m| {
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
        self.schedule_save();
    }

    pub fn on_chat_presence(&mut self, chat: &str, state: &str, media: &str) {
        let jid = self.store.canon_owned(&normalize_jid(chat));
        if Some(&jid) != self.current_jid.as_ref() {
            return;
        }
        let text = if state.contains("Composing") {
            if media.contains("Audio") { t("presence.recording") } else { t("presence.typing") }
        } else {
            String::new()
        };
        self.ui.set_current_status(text.into());
    }

    pub fn on_presence(&mut self, from: &str, available: bool) {
        let jid = self.store.canon_owned(&normalize_jid(from));
        if Some(&jid) != self.current_jid.as_ref() {
            return;
        }
        // Only overwrite the idle state; typing/recording wins.
        let current = self.ui.get_current_status();
        if current.is_empty() || current == t("presence.online").as_str() {
            self.ui
                .set_current_status(if available { t("presence.online").into() } else { "".into() });
        }
    }

    pub fn on_chat_flag(&mut self, jid: &str, pinned: Option<i64>, archived: Option<bool>) {
        let jid = self.store.canon_owned(&normalize_jid(jid));
        self.store.upsert_chat(&jid, None, 0, None, pinned, archived);
        self.schedule_refresh_chats();
    }

    pub fn on_star(&mut self, jid: &str, id: &str, starred: bool) {
        let jid = self.store.canon_owned(&normalize_jid(jid));
        if self.store.set_starred(&jid, id, starred) && Some(&jid) == self.current_jid.as_ref() {
            self.patch_row(id, |row| row.starred = starred);
        }
        self.schedule_save();
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
        self.store.upsert_contact(&normalize_jid(jid), Some(name), None);
        self.schedule_refresh_chats();
    }

    pub fn on_push_name(&mut self, jid: &str, name: &str) {
        self.store.upsert_contact(&normalize_jid(jid), None, Some(name));
        self.schedule_refresh_chats();
    }

    // The just-sent message flows through the normal pipeline; the later
    // echo from the server is deduplicated by id in the store.
    pub fn echo_sent(&mut self, jid: &str, id: &str, message: wa::Message) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let meta = crate::wa_map::LiveMeta {
            chat: jid.to_string(),
            sender: self.self_jid.clone(),
            sender_alt: None,
            recipient_alt: None,
            id,
            from_me: true,
            is_group: is_group(jid),
            push_name: "",
            timestamp: now,
        };
        let Some(stored) =
            crate::wa_map::normalize(&mut self.store, &meta, &message, &HashMap::new(), 0)
        else {
            return;
        };
        let jid = stored.jid.clone();
        if self.store.add_message(stored) && Some(&jid) == self.current_jid.as_ref() {
            self.push_message_row(&jid);
        }
        self.schedule_refresh_chats();
    }

    // ---- history backfill ----

    fn set_pending(&mut self, pending: bool) {
        self.history_pending = pending;
        self.ui.set_sync_banner(if pending { t("sync.older").into() } else { "".into() });
    }

    fn pull_older_history(&mut self) {
        if self.history_pending || self.history_batches >= MAX_HISTORY_BATCHES {
            return;
        }
        let Some(anchor) = self.store.oldest_message() else {
            // No message to anchor on yet (fresh pairing) — retry once
            // messages start flowing in.
            println!("[history] no anchor yet; retrying in 30s");
            self.once(30_000, |b| b.pull_older_history());
            return;
        };
        let (jid, id, from_me, ts) =
            (anchor.jid.clone(), anchor.id.clone(), anchor.from_me, anchor.timestamp);
        self.set_pending(true);
        self.history_batches += 1;
        println!(
            "[history] on-demand batch {} (before ts={ts}, total={})",
            self.history_batches,
            self.store.total_messages()
        );
        self.wa.send(Cmd::FetchHistory { jid, oldest_id: id, from_me, ts_ms: ts * 1000 });
    }

    fn handle_scroll_up_load(&mut self) {
        let Some(jid) = self.current_jid.clone() else { return };
        // After a jump the list starts mid-conversation; the rows above it
        // are already in the store.
        if self.store.messages_for(&jid).len() > self.messages_model.row_count() {
            self.prepend_older_rows(&jid);
            return;
        }
        if self.history_pending {
            return;
        }
        if let Some(last) = self.last_scroll_load
            && last.elapsed() < Duration::from_millis(2500)
        {
            return;
        }
        let list = self.store.messages_for(&jid);
        let Some(first) = list.first().filter(|m| m.raw.is_some()) else { return };
        if list.len() < 5 {
            return;
        }
        let (id, from_me, ts) = (first.id.clone(), first.from_me, first.timestamp);
        self.last_scroll_load = Some(Instant::now());
        self.scroll_up_fetch = true;
        self.set_pending(true);
        self.wa.send(Cmd::FetchHistory { jid, oldest_id: id, from_me, ts_ms: ts * 1000 });
    }

    // ---- reactions ----

    fn apply_reaction(&mut self, remote_jid: &str, target: &str, reactor: &str, emoji: &str, from_me: bool) {
        let chat_jid = self.store.canon_owned(&normalize_jid(remote_jid));
        let updated = self.store.apply_reaction(&chat_jid, target, reactor, emoji);
        if updated && Some(&chat_jid) == self.current_jid.as_ref() {
            let summary = self
                .store
                .messages_for(&chat_jid)
                .iter()
                .find(|m| m.id == target)
                .map(reaction_summary);
            if let Some(summary) = summary {
                self.patch_row(target, |row| row.reactions = summary.into());
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
                .map(|m| preview_body(m).chars().take(32).collect())
                .unwrap_or_default();
            let name = self.store.chat_name(&self.store.canon_owned(&normalize_jid(reactor)));
            let who = if from_me {
                "✓ ".to_string()
            } else if is_group(&chat_jid) && !name.is_empty() {
                format!("{}: ", name.split(' ').next().unwrap_or(""))
            } else {
                String::new()
            };
            if let Some(meta) = self.store.chats.get_mut(&chat_jid) {
                meta.preview =
                    format!("{who}{}", ta("preview.reacted", &[&clean_text(emoji), &snippet]));
                meta.timestamp = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs() as i64)
                    .unwrap_or(0);
            }
            self.schedule_refresh_chats();
        }
        self.schedule_save();
    }

    // ---- chat list ----

    fn to_chat_row(&self, jid: &str, preview: &str, timestamp: i64, unread: u32, mentioned: bool) -> ChatItem {
        let name = self.store.chat_name(jid);
        ChatItem {
            jid: jid.into(),
            name: name.clone().into(),
            preview: preview.into(),
            time: format_time(timestamp).into(),
            avatar: empty_image(),
            hasAvatar: false,
            initial: initial_of(&name).into(),
            colorIdx: color_idx_of(jid),
            unread: unread as i32,
            mentioned: mentioned && unread > 0,
            pinned: self.store.chats.get(jid).map(|c| c.pinned > 0).unwrap_or(false),
        }
    }

    fn visible_chats(&self) -> Vec<(String, String, i64, u32, bool)> {
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
                if self.tab == "archived" {
                    if !meta.archived {
                        return false;
                    }
                } else {
                    if meta.archived {
                        return false;
                    }
                    if self.tab == "unread" && meta.unread == 0 {
                        return false;
                    }
                }
                if !self.search_text.is_empty()
                    && !self.store.chat_name(&meta.jid).to_lowercase().contains(&self.search_text)
                {
                    return false;
                }
                true
            })
            .map(|meta| {
                (meta.jid.clone(), meta.preview.clone(), meta.timestamp, meta.unread, meta.mentioned)
            })
            .collect()
    }

    fn schedule_refresh_chats(&mut self) {
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
    fn refresh_chats(&mut self) {
        let rows: Vec<ChatItem> = self
            .visible_chats()
            .iter()
            .map(|(jid, preview, ts, unread, mentioned)| {
                self.to_chat_row(jid, preview, *ts, *unread, *mentioned)
            })
            .collect();
        let model = &self.chats_model;
        let common = model.row_count().min(rows.len());
        for (i, next) in rows.iter().take(common).enumerate() {
            let cur = model.row_data(i);
            let differs = cur
                .map(|cur| {
                    cur.jid != next.jid
                        || cur.name != next.name
                        || cur.preview != next.preview
                        || cur.time != next.time
                        || cur.unread != next.unread
                        || cur.mentioned != next.mentioned
                        || cur.pinned != next.pinned
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
        self.schedule_save();
    }

    fn schedule_save(&mut self) {
        if self.save_queued || self.vault.is_none() {
            return;
        }
        self.save_queued = true;
        self.once(2000, |b| {
            b.save_queued = false;
            if let Some(vault) = b.vault.take() {
                b.store.save_to(&vault);
                b.vault = Some(vault);
            }
        });
    }

    // ---- conversation ----

    fn apply_header(&mut self, jid: &str) {
        let name = self.store.chat_name(jid);
        self.ui.set_current_chat_name(name.clone().into());
        self.ui.set_current_avatar(empty_image());
        self.ui.set_current_avatar_has(false);
        self.ui.set_current_initial(initial_of(&name).into());
        self.ui.set_current_color_idx(color_idx_of(jid));
    }

    // Opens (or starts) the conversation with a jid; used by the chat list
    // and by clicking a sender name inside a group.
    pub fn open_dm(&mut self, jid_raw: &str, jump_to: Option<&str>) {
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

    fn open_jid(&mut self, jid: &str, jump_to: Option<&str>) {
        if let Some(current) = self.current_jid.clone()
            && current != jid
        {
            // Someone reading at the bottom expects the newest message next
            // time, not the offset that happened to be the end back then.
            if self.ui.get_stick_bottom() || self.truncated.contains(&current) {
                self.scroll_pos.remove(&current);
            } else {
                self.scroll_pos.insert(current.clone(), self.ui.get_conv_scroll());
            }
            self.truncated.remove(&current);
            self.clear_reply();
        }
        self.current_jid = Some(jid.to_string());
        self.ui.set_selected_jid(jid.into());
        self.ui.set_stick_bottom(true);
        if let Some(meta) = self.store.chats.get_mut(jid) {
            meta.unread = 0;
            meta.mentioned = false;
        }
        self.ui.set_current_status("".into());
        self.wa.send(Cmd::SubscribePresence(jid.to_string()));
        self.apply_header(jid);
        let list = self.store.messages_for(jid);
        // Jumping to a message means starting the list there: the row
        // lands at the top exactly, instead of chasing a pixel offset
        // inside a virtualized list that estimates unmeasured heights.
        let at = jump_to
            .and_then(|target| list.iter().position(|m| m.id == target))
            .filter(|&i| i > 0);
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
        self.ui.set_chat_open(true);
        self.ui.set_conv_ready(false);
        if at.is_some() {
            self.ui.set_stick_bottom(false);
            println!("[jump] {jid} at row {from}/{}", list.len());
        }
        let anchored_top = at.is_some();
        self.ui.set_scroll_armed(!anchored_top);
        let saved = if anchored_top { Some(0.0) } else { self.scroll_pos.get(jid).copied() };
        // Anchor across a few layout passes while hidden, then reveal.
        for delay in [0u64, 40, 90] {
            self.once(delay, move |b| match saved {
                Some(y) if y <= 0.0 => b.ui.invoke_set_conversation_scroll(y),
                _ => b.ui.invoke_scroll_conversation_end(),
            });
        }
        self.once(120, move |b| {
            b.ui.set_conv_ready(true);
            // Clear anything the transition re-armed while the list still
            // held the previous chat's offset.
            b.ui.set_scroll_armed(!anchored_top);
        });
        self.schedule_refresh_chats();
        // Thin conversation: ask the phone for this chat's older messages.
        let list = self.store.messages_for(jid);
        if list.len() < 20
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

    fn rebuild_conversation(&mut self, jid: &str) {
        let list = self.store.messages_for(jid);
        let rows: Vec<MessageItem> = list
            .iter()
            .enumerate()
            .map(|(i, m)| self.to_row(m, if i > 0 { list.get(i - 1) } else { None }))
            .collect();
        self.messages_model.set_vec(rows);
    }

    // Inserts freshly fetched history above the current rows and shifts
    // the viewport by exactly the height that was added, so the message
    // the user was reading stays put.
    fn prepend_older_rows(&mut self, jid: &str) {
        let list = self.store.messages_for(jid);
        let first_shown = self.messages_model.row_data(0).map(|r| r.id.to_string());
        let cut = first_shown.and_then(|id| list.iter().position(|m| m.id == id));
        let Some(cut) = cut.filter(|&c| c > 0) else { return };

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

        self.once(40, move |b| {
            let added = b.ui.get_conv_viewport_h() - before_h;
            b.ui.invoke_set_conversation_scroll(before_y - added);
            b.ui.set_conv_ready(true);
        });
    }

    fn push_message_row(&mut self, jid: &str) {
        let list = self.store.messages_for(jid);
        let Some(stored) = list.last() else { return };
        let prev = list.len().checked_sub(2).and_then(|i| list.get(i));
        let row = self.to_row(stored, prev);
        let from_me = stored.from_me;
        self.messages_model.push(row);
        // Don't yank the view down while the user is reading older rows.
        if from_me || self.ui.get_stick_bottom() {
            self.scroll_to_end();
        }
    }

    fn scroll_to_end(&mut self) {
        for delay in [0u64, 60, 220] {
            self.once(delay, |b| b.ui.invoke_scroll_conversation_end());
        }
    }

    // Formatted or mention-carrying messages render as styled text; plain
    // ones keep the selectable input.
    fn styled_for(&self, m: &StoredMessage) -> (slint::StyledText, bool) {
        let body = if m.deleted { "" } else { m.text.as_str() };
        let empty = || slint::StyledText::from_plain_text("");
        if body.is_empty() || !has_markup(body) {
            return (empty(), false);
        }
        let store = &self.store;
        let resolve = |num: &str| -> MentionTarget {
            // The id is either a phone number or a LID; try both before
            // falling back on the shape of the number itself.
            for form in [format!("{num}@s.whatsapp.net"), format!("{num}@lid")] {
                let canon = store.canon_owned(&form);
                let name = store
                    .contacts
                    .get(&canon)
                    .or_else(|| store.chats.get(&canon).filter(|c| !c.name.is_empty()).map(|c| &c.name))
                    .or_else(|| store.push_names.get(&canon));
                if let Some(name) = name {
                    return MentionTarget { name: Some(clean_text(name)), jid: canon };
                }
            }
            let guess = if num.len() > 13 { format!("{num}@lid") } else { format!("{num}@s.whatsapp.net") };
            MentionTarget { name: None, jid: store.canon_owned(&guess) }
        };
        match slint::StyledText::from_markdown(&to_markdown(body, &resolve)) {
            Ok(styled) => (styled, true),
            Err(_) => (empty(), false),
        }
    }

    fn to_row(&self, m: &StoredMessage, prev: Option<&StoredMessage>) -> MessageItem {
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
        let sender_jid =
            if m.sender_jid.is_empty() { m.jid.clone() } else { self.store.canon_owned(&m.sender_jid) };
        let group_indent = group && !m.from_me;
        let voice_jid = if m.kind == MessageKind::Audio {
            if m.from_me { self.self_jid.clone() } else { sender_jid.clone() }
        } else {
            String::new()
        };
        let (pic_w, pic_h) = media_box(m);
        let (ticks, ticks_blue) = ticks_for(m);
        let day = format_day(m.timestamp);
        let day_label = if prev.map(|p| format_day(p.timestamp) != day).unwrap_or(true) {
            day
        } else {
            String::new()
        };
        let (styled, has_styled) = self.styled_for(m);
        let link_host = host_of(&m.link_url);
        MessageItem {
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
                }
            })
            .into(),
            text: if m.deleted { t("msg.deleted").into() } else { m.text.clone().into() },
            fromMe: m.from_me,
            sender: m.sender.clone().into(),
            showSender: group && !m.from_me && first_of_run,
            // Saved contacts show their address-book name without the ~.
            senderLabel: if group && !sender_jid.is_empty() && !self.store.is_saved(&sender_jid) {
                format!("~ {}", self.store.chat_name(&sender_jid)).into()
            } else {
                self.store.chat_name(if sender_jid.is_empty() { &m.jid } else { &sender_jid }).into()
            },
            firstOfRun: first_of_run,
            time: format_time(m.timestamp).into(),
            picture: empty_image(),
            picW: pic_w,
            picH: pic_h,
            mediaPath: "".into(),
            mediaReady: false,
            reactions: reaction_summary(m).into(),
            dayLabel: day_label.into(),
            ticks: ticks.into(),
            ticksBlue: ticks_blue,
            senderAvatar: empty_image(),
            senderHasAvatar: false,
            senderInitial: initial_of(if m.sender.is_empty() { "?" } else { &m.sender }).into(),
            senderColorIdx: color_idx_of(if sender_jid.is_empty() { &m.jid } else { &sender_jid }),
            voiceJid: voice_jid.clone().into(),
            voiceAvatar: empty_image(),
            voiceHasAvatar: false,
            voiceInitial: initial_of(if m.from_me {
                let name = self.store.chat_name(&self.self_jid);
                if name.is_empty() { "?".to_string() } else { name }
            } else if m.sender.is_empty() {
                "?".to_string()
            } else {
                m.sender.clone()
            }
            .as_str())
            .into(),
            voiceColorIdx: color_idx_of(if voice_jid.is_empty() { &m.jid } else { &voice_jid }),
            groupIndent: group_indent,
            showAvatar: group_indent && first_of_run,
            // Unsaved senders show their number next to the push name.
            senderNumber: if group_indent && !sender_jid.is_empty() && !self.store.is_saved(&sender_jid)
            {
                format_number(&sender_jid).into()
            } else {
                "".into()
            },
            sticker: m.sticker,
            gif: m.gif,
            styled,
            hasStyled: has_styled,
            linkTitle: m.link_title.trim().into(),
            linkDesc: m.link_desc.trim().into(),
            linkHost: link_host.into(),
            linkUrl: m.link_url.clone().into(),
            hasLink: !m.link_title.is_empty() || !m.link_url.is_empty(),
            linkThumb: empty_image(),
            hasLinkThumb: false,
            linkThumbW: 0,
            linkThumbH: 0,
            wave: empty_image(),
            hasWave: false,
            playing: false,
            progress: 0.0,
            posLabel: "".into(),
            senderJid: sender_jid.into(),
            forwarded: m.forwarded,
            deleted: m.deleted,
            starred: m.starred,
            hasQuote: !m.quote_text.is_empty() || !m.quote_id.is_empty(),
            quoteName: if m.quote_author.is_empty() {
                t("reactions.you").into()
            } else {
                let name = self.store.chat_name(&m.quote_author);
                if name.is_empty() { display_id(&m.quote_author).into() } else { name.into() }
            },
            quoteText: self.store.named_mentions(&m.quote_text).into(),
            quoteId: m.quote_id.clone().into(),
        }
    }

    fn patch_row(&self, id: &str, patch: impl FnOnce(&mut MessageItem)) {
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

    fn find_message(&self, id: &str) -> Option<&StoredMessage> {
        let jid = self.current_jid.as_ref()?;
        self.store.messages_for(jid).iter().find(|m| m.id == id)
    }

    // ---- composing ----

    fn handle_send_text(&mut self, text: &str) {
        let body = text.trim().to_string();
        let Some(jid) = self.current_jid.clone() else { return };
        if body.is_empty() {
            return;
        }
        let quote = self.reply_to.take().and_then(|id| {
            let m = self.find_message(&id)?;
            let raw = m.raw.clone()?;
            Some(QuoteRef {
                id: m.id.clone(),
                sender_jid: if m.from_me { self.self_jid.clone() } else { m.sender_jid.clone() },
                message: raw,
            })
        });
        self.clear_reply();
        self.wa.send(Cmd::SendText { jid, body, quote });
    }

    fn start_reply(&mut self, id: &str) {
        let Some(m) = self.find_message(id) else { return };
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
        self.reply_to = Some(id.to_string());
        self.ui.set_reply_open(true);
        self.ui.set_reply_name(name.into());
        self.ui.set_reply_text(preview.into());
    }

    fn clear_reply(&mut self) {
        self.reply_to = None;
        self.ui.set_reply_open(false);
        self.ui.set_reply_name("".into());
        self.ui.set_reply_text("".into());
    }

    // ---- helpers ----

    fn once(&mut self, ms: u64, f: impl FnOnce(&mut Bridge) + 'static) {
        let timer = slint::Timer::default();
        let cell = RefCell::new(Some(f));
        timer.start(slint::TimerMode::SingleShot, Duration::from_millis(ms), move || {
            if let Some(f) = cell.borrow_mut().take() {
                apply_now(f);
            }
        });
        self.oneshots.retain(|timer| timer.running());
        self.oneshots.push(timer);
    }
}

// Scales media dimensions into the bubble's 330x380 box (stickers use a
// smaller square), falling back to a sane default when unknown.
fn media_box(m: &StoredMessage) -> (i32, i32) {
    if m.kind != MessageKind::Image && m.kind != MessageKind::Video {
        return (0, 0);
    }
    let (max_w, max_h) = if m.sticker { (180.0, 180.0) } else { (330.0, 380.0) };
    let w = if m.media_w > 0 { m.media_w as f64 } else { 260.0 };
    let h = if m.media_h > 0 { m.media_h as f64 } else { 200.0 };
    let scale = (max_w / w).min(max_h / h).min(1.0);
    (((w * scale).round() as i32).max(1), ((h * scale).round() as i32).max(1))
}

fn host_of(url: &str) -> String {
    if url.is_empty() {
        return String::new();
    }
    url::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(|h| h.trim_start_matches("www.").to_string()))
        .unwrap_or_default()
}

fn initial_of(name: &str) -> String {
    name.trim()
        .chars()
        .next()
        .map(|c| c.to_uppercase().to_string())
        .unwrap_or_else(|| "?".to_string())
}

fn color_idx_of(jid: &str) -> i32 {
    let mut h: i32 = 0;
    for b in jid.encode_utf16() {
        h = h.wrapping_add(b as i32);
    }
    h.abs() % 8
}
