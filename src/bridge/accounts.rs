// Account switcher: add, switch, remove and the selftest.
use super::*;

impl Bridge {
    // ---- accounts ----

    pub(super) fn account_entry(&self) -> Option<crate::vault::Account> {
        self.vault
            .as_ref()?
            .accounts()
            .into_iter()
            .find(|a| a.id == self.account)
    }

    pub(super) fn update_account(&mut self, f: impl FnOnce(&mut crate::vault::Account)) {
        let Some(vault) = &self.vault else { return };
        let mut list = vault.accounts();
        if let Some(entry) = list.iter_mut().find(|a| a.id == self.account) {
            f(entry);
        }
        vault.set_accounts(&list);
    }

    // With the conversations gone nothing ties the slot to a number.
    pub(super) fn forget_account_identity(&mut self) {
        self.update_account(|a| {
            a.jid.clear();
            a.name.clear();
        });
    }

    pub(super) fn refresh_accounts(&mut self) {
        let Some(vault) = &self.vault else { return };
        let list = vault.accounts();
        let rows: Vec<AccountItem> = list
            .iter()
            .map(|a| {
                let number = if a.jid.is_empty() {
                    String::new()
                } else {
                    format_number(&a.jid)
                };
                let name = if !a.name.is_empty() {
                    a.name.clone()
                } else if !number.is_empty() {
                    number.clone()
                } else {
                    t("account.new")
                };
                let avatar = self.avatars.get(&a.jid).cloned().flatten();
                AccountItem {
                    id: a.id.as_str().into(),
                    initial: initial_of(&name).into(),
                    detail: if name == number {
                        "".into()
                    } else {
                        number.into()
                    },
                    name: name.into(),
                    active: a.id == self.account,
                    colorIdx: color_idx_of(&a.jid),
                    hasAvatar: avatar.is_some(),
                    avatar: avatar.unwrap_or_default(),
                }
            })
            .collect();
        if let Some(mine) = rows.iter().find(|row| row.active) {
            self.ui.set_my_account(mine.clone());
        }
        self.ui
            .set_can_add_account(list.len() < crate::vault::MAX_ACCOUNTS);
        self.ui.set_account_count(list.len() as i32);
        self.accounts_model.set_vec(rows);
        // The login screen offers the conversations a past session left.
        let kept = self.account_entry().map(|a| a.jid).unwrap_or_default();
        let has_kept = !self.registered && !self.store.chats.is_empty();
        self.ui.set_kept_conversations(has_kept);
        self.ui.set_kept_number(if has_kept && !kept.is_empty() {
            format_number(&kept).into()
        } else {
            "".into()
        });
    }

    pub fn switch_account(&mut self, id: &str) {
        self.ui.set_account_menu_open(false);
        let Some(vault) = &self.vault else { return };
        if self.switching
            || id == self.account
            || vault.locked()
            || !vault.accounts().iter().any(|a| a.id == id)
        {
            return;
        }
        // A call belongs to the account it runs on.
        if self.call.is_some() {
            self.ui.set_status_text(t("account.in_call").into());
            return;
        }
        self.reset_translation();
        self.switching = true;
        self.ui.set_account_switching(true);
        self.save_now();
        self.wa.send(Cmd::SwitchAccount(id.to_string()));
    }

    pub fn add_account(&mut self) {
        let Some(vault) = &self.vault else { return };
        let mut list = vault.accounts();
        if list.len() >= crate::vault::MAX_ACCOUNTS || self.switching {
            return;
        }
        let id = loop {
            let id = format!("{:08x}", rand::random::<u32>());
            if !list.iter().any(|a| a.id == id) {
                break id;
            }
        };
        list.push(crate::vault::Account {
            id: id.clone(),
            ..Default::default()
        });
        vault.set_accounts(&list);
        self.switch_account(&id);
    }

    pub(super) fn ask_remove_account(&mut self, id: &str) {
        self.ui.set_account_menu_open(false);
        let Some(vault) = &self.vault else { return };
        let Some(entry) = vault.accounts().into_iter().find(|a| a.id == id) else {
            return;
        };
        // The loaded account goes through the logout dialog instead: its
        // session has to be closed on the phone's side too.
        if id == self.account {
            return;
        }
        let label = if !entry.name.is_empty() {
            entry.name
        } else if !entry.jid.is_empty() {
            format_number(&entry.jid)
        } else {
            t("account.new")
        };
        self.ask_confirm(
            ta("account.remove_title", &[&label]),
            t("account.remove_body"),
            t("account.remove_action"),
            ConfirmAction::RemoveAccount(id.to_string()),
        );
    }

    // Only ever an account that is not loaded: its session database is
    // closed, so the files can simply go. The phone keeps listing the
    // device until it is removed there (or WhatsApp expires it).
    pub(super) fn remove_account(&mut self, id: &str) {
        let Some(vault) = &self.vault else { return };
        if id == self.account {
            return;
        }
        let mut list = vault.accounts();
        list.retain(|a| a.id != id);
        vault.set_accounts(&list);
        vault.wipe_account(id);
        crate::paths::remove_db(&crate::paths::wa_session_path(id));
        crate::paths::remove_db(&crate::paths::search_index_path(id));
        self.refresh_accounts();
    }

    // Developer probe (--accounts-selftest): walks the account switcher on
    // the real session plumbing -- add an account, leave a trace in the
    // first one's store, switch back and forth -- and reports after each
    // step. Refuses to touch a data directory that holds a paired session:
    // point APPDATA/XDG_DATA_HOME at an empty one.
    pub fn accounts_selftest(&mut self, step: u32) {
        let ids = |b: &Bridge| -> Vec<String> {
            b.vault
                .as_ref()
                .map(|v| v.accounts().into_iter().map(|a| a.id).collect())
                .unwrap_or_default()
        };
        let fail = |what: &str| println!("[selftest] FAIL: {what}");
        match step {
            0 => {
                if self.registered || ids(self).len() != 1 {
                    println!("[selftest] refusing: this data directory is in use");
                    slint::quit_event_loop().ok();
                    return;
                }
                // Something that must survive a round trip, and must not
                // show up in the other account.
                self.store.upsert_chat(
                    "5511999990000@s.whatsapp.net",
                    Some("Probe"),
                    1,
                    None,
                    None,
                    None,
                );
                self.add_account();
                println!("[selftest] add account: switching = {}", self.switching);
            }
            1 => {
                let list = ids(self);
                println!(
                    "[selftest] on the new account: {} account(s), {} chat(s), screen {}",
                    list.len(),
                    self.store.chats.len(),
                    self.ui.get_screen()
                );
                if self.switching || self.account.is_empty() || list.len() != 2 {
                    fail("adding an account should land on a second, new one");
                }
                if !self.store.chats.is_empty() {
                    fail("the first account's conversations leaked into the new one");
                }
                if self.ui.get_screen() != "login" {
                    fail("an unpaired account should be on the login screen");
                }
                self.switch_account("");
            }
            2 => {
                let list = ids(self);
                println!(
                    "[selftest] back on the first: {} account(s), {} chat(s)",
                    list.len(),
                    self.store.chats.len()
                );
                if !self.account.is_empty() {
                    fail("switching back did not happen");
                }
                if self.store.chats.len() != 1 {
                    fail("the first account's conversations did not come back");
                }
                // Never paired, nothing in it: the slot should be gone.
                if list.len() != 1 {
                    fail("the abandoned empty account was kept");
                }
                // The limit.
                for _ in 0..8 {
                    let Some(vault) = &self.vault else { break };
                    let mut list = vault.accounts();
                    if list.len() >= crate::vault::MAX_ACCOUNTS {
                        break;
                    }
                    list.push(crate::vault::Account {
                        id: format!("probe{}", list.len()),
                        jid: "5511888880000@s.whatsapp.net".into(),
                        ..Default::default()
                    });
                    vault.set_accounts(&list);
                }
                self.add_account();
                if self.switching || ids(self).len() != crate::vault::MAX_ACCOUNTS {
                    fail("a sixth account was accepted");
                }
                self.refresh_accounts();
                println!(
                    "[selftest] at the limit: {} accounts, can add = {}",
                    ids(self).len(),
                    self.ui.get_can_add_account()
                );
                println!("[selftest] accounts done");
                slint::quit_event_loop().ok();
            }
            _ => {}
        }
    }

    // wa.rs has the other account's client built (not yet connected).
    pub fn on_account_ready(&mut self, account: &str, registered: bool, epoch: u64) {
        // Whatever the old session delivered since the switch was asked.
        self.save_now();
        let previous = std::mem::replace(&mut self.account, account.to_string());
        let abandoned = previous != account && !self.registered && self.store.chats.is_empty();
        self.epoch = epoch;
        self.registered = registered;
        self.switching = false;
        self.close_session_views();
        self.clear_store_state();
        if let Some(vault) = self.vault.take() {
            vault.set_account(account);
            vault.set_active_account(account);
            self.store.load_from(&vault);
            self.vault = Some(vault);
        }
        self.load_translation_settings();
        self.load_mcp();
        self.restore_outbox();
        // "Add account" backed out of before pairing: the empty slot
        // would otherwise sit in the switcher forever.
        if abandoned {
            self.remove_account(&previous);
        }
        if let Some(me) = self
            .account_entry()
            .map(|a| a.jid)
            .filter(|s| !s.is_empty())
        {
            self.store.set_self(&[&me]);
            self.self_jid = me;
        }
        self.ui.set_qr_image(crate::qr::empty_image());
        self.ui.set_pairing_mode(false);
        self.ui.set_pairing_code("".into());
        self.ui.set_status_text(t("status.connecting").into());
        self.ui.set_alert_open(false);
        self.ui
            .set_screen(if registered { "main" } else { "login" }.into());
        self.ui.set_account_switching(false);
        self.refresh_accounts();
        self.refresh_labels();
        self.refresh_chats();
        self.refresh_statuses();
        self.refresh_calls();
        self.once(4000, |b| b.index_backfill());
        self.wa.send(Cmd::Start);
    }
}
