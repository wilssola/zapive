// On-demand history backfill and the sync dialog.
use super::*;

pub(super) const MAX_HISTORY_BATCHES: u32 = 20;

// No first chunk at all for this long: stop waiting and show the app.
const INITIAL_SYNC_FIRST_CHUNK_MS: u64 = 120_000;
// Silence after the last chunk that counts as "the phone is done".
const INITIAL_SYNC_QUIET_MS: u64 = 25_000;

// A return after at least this long away shows the page too: the phone
// has queued messages for us.
const CATCHUP_AFTER_SECS: i64 = 2 * 60 * 60;
// Quiet time after the connection opens / after the last message batch
// before the catch-up page is considered done.
const CATCHUP_CONNECT_QUIET_MS: u64 = 8_000;
const CATCHUP_BATCH_QUIET_MS: u64 = 6_000;
const ACTIVITY_TICK_SECS: u64 = 60;

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

// What the first-sync page shows: only the first history of a fresh
// pairing counts, never on-demand backfill.
#[derive(Default)]
pub(super) struct InitialSync {
    active: bool,
    // Catching up after time away: no percentage, only counters.
    catchup: bool,
    started: bool,
    percent: u32,
    messages: usize,
    media: usize,
    files: usize,
    // Bumped on every chunk so stale timeouts do nothing.
    generation: u32,
}

// 12430 -> "12,430" / "12.430", following the app language.
fn grouped(n: usize) -> String {
    let sep = match crate::i18n::current_locale() {
        crate::i18n::Locale::Pt => '.',
        crate::i18n::Locale::En => ',',
    };
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(sep);
        }
        out.push(c);
    }
    out
}

impl Bridge {
    // ---- first-sync page ----

    pub(super) fn begin_initial_sync(&mut self) {
        if self.initial_sync.active {
            return;
        }
        self.initial_sync = InitialSync {
            active: true,
            ..Default::default()
        };
        self.render_initial_sync();
        self.ui.set_sync_screen_open(true);
        self.arm_initial_sync_timeout(INITIAL_SYNC_FIRST_CHUNK_MS);
    }

    // Seconds since the app last ran for this account; 0 when unknown.
    pub(super) fn away_secs(&self) -> i64 {
        let last = self
            .vault
            .as_ref()
            .and_then(|v| v.setting_get("last_active"))
            .and_then(|v| v.parse::<i64>().ok());
        last.map(|t| (now_secs() - t).max(0)).unwrap_or(0)
    }

    // Stamps "last active" now and every minute, so the next start knows
    // how long it was away (a crash loses at most a minute).
    pub(super) fn start_activity_tracking(&mut self) {
        self.stamp_activity();
        self.activity_timer.start(
            slint::TimerMode::Repeated,
            Duration::from_secs(ACTIVITY_TICK_SECS),
            || apply_now(|b| b.stamp_activity()),
        );
    }

    fn stamp_activity(&mut self) {
        if let Some(vault) = self.vault.as_ref()
            && !vault.locked()
            && self.registered
        {
            vault.setting_set("last_active", &now_secs().to_string());
        }
    }

    // Shows the page when the app comes back after a long absence.
    pub(super) fn begin_catchup_if_away(&mut self, away: i64) {
        if away < CATCHUP_AFTER_SECS {
            return;
        }
        self.begin_initial_sync();
        self.initial_sync.catchup = true;
        self.render_initial_sync();
    }

    // The connection opened: offline messages follow right away.
    pub(super) fn catchup_connected(&mut self) {
        if !self.initial_sync.active || !self.initial_sync.catchup {
            return;
        }
        self.initial_sync.started = true;
        self.initial_sync.generation += 1;
        self.render_initial_sync();
        self.arm_initial_sync_timeout(CATCHUP_CONNECT_QUIET_MS);
    }

    pub(super) fn note_catchup_messages(&mut self, batch: &MessageBatch) {
        if !self.initial_sync.active || !self.initial_sync.catchup {
            return;
        }
        let state = &mut self.initial_sync;
        for inbound in batch.iter() {
            if inbound.info.source.chat.to_non_ad_string() == "status@broadcast" {
                continue;
            }
            use whatsapp_rust::proto_helpers::MessageExt as _;
            let content = inbound.message.get_base_message();
            state.messages += 1;
            if content.image_message.is_set()
                || content.video_message.is_set()
                || content.audio_message.is_set()
            {
                state.media += 1;
            } else if content.document_message.is_set() {
                state.files += 1;
            }
        }
        state.started = true;
        state.generation += 1;
        self.render_initial_sync();
        self.arm_initial_sync_timeout(CATCHUP_BATCH_QUIET_MS);
    }

    pub(super) fn end_initial_sync(&mut self) {
        self.initial_sync.active = false;
        self.initial_sync.generation += 1;
        self.ui.set_sync_screen_open(false);
    }

    fn arm_initial_sync_timeout(&mut self, ms: u64) {
        let generation = self.initial_sync.generation;
        self.once(ms, move |b| {
            if b.initial_sync.active && b.initial_sync.generation == generation {
                println!("[history] first sync page closed after silence");
                b.end_initial_sync();
            }
        });
    }

    // Folds one history chunk into the page; `kinds` are the kinds of the
    // messages that were actually added.
    pub(super) fn note_initial_chunk(
        &mut self,
        chunk: &HistoryChunk,
        added: usize,
        kinds: &[MessageKind],
    ) {
        if !self.initial_sync.active || chunk.on_demand {
            return;
        }
        let state = &mut self.initial_sync;
        state.started = true;
        state.generation += 1;
        state.messages += added;
        for kind in kinds {
            match kind {
                MessageKind::Image | MessageKind::Video | MessageKind::Audio => state.media += 1,
                MessageKind::Doc => state.files += 1,
                _ => {}
            }
        }
        if let Some(p) = chunk.progress {
            state.percent = state.percent.max(p.min(100));
        }
        let done = state.percent >= 100;
        self.render_initial_sync();
        if done {
            self.arm_initial_sync_timeout(1_500);
        } else {
            self.arm_initial_sync_timeout(INITIAL_SYNC_QUIET_MS);
        }
    }

    fn render_initial_sync(&mut self) {
        let s = &self.initial_sync;
        let name = self.account_entry().map(|a| a.name).unwrap_or_default();
        self.ui.set_sync_screen_account(name.into());
        self.ui.set_sync_screen_started(s.started);
        self.ui.set_sync_screen_catchup(s.catchup);
        self.ui.set_sync_screen_percent(s.percent as i32);
        self.ui.set_sync_screen_messages(grouped(s.messages).into());
        self.ui
            .set_sync_screen_chats(grouped(self.store.chats.len()).into());
        self.ui.set_sync_screen_media(grouped(s.media).into());
        self.ui.set_sync_screen_files(grouped(s.files).into());
    }

    // ---- history backfill ----

    pub(super) fn set_pending(&mut self, pending: bool) {
        self.history_pending = pending;
        self.ui.set_sync_banner(if pending {
            t("sync.older").into()
        } else {
            "".into()
        });
        if self.ui.get_sync_open() {
            self.ui.set_sync_detail(self.sync_summary().into());
        }
    }

    pub(super) fn sync_summary(&self) -> String {
        let progress = ta(
            "sync.progress",
            &[
                &self.sync_batches.to_string(),
                &self.sync_messages.to_string(),
            ],
        );
        if self.history_pending {
            format!("{}\n\n{progress}", t("sync.body"))
        } else if self.history_batches >= MAX_HISTORY_BATCHES {
            format!("{}\n\n{progress}", t("sync.complete"))
        } else {
            format!("{}\n\n{progress}", t("sync.idle"))
        }
    }

    pub(super) fn show_sync_dialog(&mut self) {
        self.ui.set_sync_detail(self.sync_summary().into());
        self.ui.set_sync_open(true);
    }

    pub(super) fn pull_older_history(&mut self) {
        if self.history_pending || self.history_batches >= MAX_HISTORY_BATCHES {
            return;
        }
        // With lazy loading the map may be empty at boot; sample chats that
        // actually have stored messages and anchor on the oldest of them.
        if self.store.total_messages() == 0
            && let Some(vault) = self.vault.as_ref()
        {
            for key in vault.keys("store:msgs:").into_iter().take(10) {
                let jid = key["store:msgs:".len()..].to_string();
                self.store.hydrate(vault, &jid);
            }
        }
        let Some(anchor) = self.store.oldest_message() else {
            // No message to anchor on yet (fresh pairing) — retry once
            // messages start flowing in.
            println!("[history] no anchor yet; retrying in 30s");
            self.once(30_000, |b| b.pull_older_history());
            return;
        };
        let (jid, id, from_me, ts) = (
            anchor.jid.clone(),
            anchor.id.clone(),
            anchor.from_me,
            anchor.timestamp,
        );
        self.set_pending(true);
        self.history_batches += 1;
        println!(
            "[history] on-demand batch {} (before ts={ts}, total={})",
            self.history_batches,
            self.store.total_messages()
        );
        self.wa.send(Cmd::FetchHistory {
            jid,
            oldest_id: id,
            from_me,
            ts_ms: ts * 1000,
        });
    }

    pub(super) fn handle_scroll_up_load(&mut self) {
        let Some(jid) = self.current_jid.clone() else {
            return;
        };
        if let Some(vault) = &self.vault
            && self.store.load_older(vault, &jid)
        {
            self.prepend_older_rows(&jid);
            return;
        }
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
        let Some(first) = list.first().filter(|m| m.raw.is_some()) else {
            return;
        };
        if list.len() < 5 {
            return;
        }
        let (id, from_me, ts) = (first.id.clone(), first.from_me, first.timestamp);
        self.last_scroll_load = Some(Instant::now());
        self.scroll_up_fetch = true;
        self.set_pending(true);
        self.wa.send(Cmd::FetchHistory {
            jid,
            oldest_id: id,
            from_me,
            ts_ms: ts * 1000,
        });
    }
}
