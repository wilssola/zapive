// On-demand history backfill and the sync dialog.
use super::*;

pub(super) const MAX_HISTORY_BATCHES: u32 = 20;

impl Bridge {
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
        if let Some(vault) = &self.vault {
            if self.store.load_older(vault, &jid) {
                self.prepend_older_rows(&jid);
                return;
            }
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
