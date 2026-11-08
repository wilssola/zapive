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

    // What the switcher calls the account in the bridge right now.
    pub(super) fn account_label(&self) -> String {
        let Some(entry) = self.account_entry() else {
            return t("account.new");
        };
        if !entry.name.is_empty() {
            entry.name
        } else if !entry.jid.is_empty() {
            format_number(&entry.jid)
        } else {
            t("account.new")
        }
    }

    // With the conversations gone nothing ties the slot to a number.
    pub(super) fn forget_account_identity(&mut self) {
        self.update_account(|a| {
            a.jid.clear();
            a.name.clear();
        });
    }

    pub(super) fn refresh_accounts(&mut self) {
        // A background account's view of "active" is the wrong one; the
        // switcher is redrawn for the account on screen right after.
        if self.headless {
            return;
        }
        let Some(vault) = &self.vault else { return };
        let list = vault.accounts();
        let mut others_unread = 0i32;
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
                let active = a.id == self.account;
                let unread = if active {
                    0
                } else {
                    self.background
                        .iter()
                        .find(|s| s.account == a.id)
                        .map_or(0, |s| s.store.sidebar_unread_counts().0)
                };
                others_unread = others_unread.saturating_add(unread);
                AccountItem {
                    id: a.id.as_str().into(),
                    initial: initial_of(&name).into(),
                    detail: if name == number {
                        "".into()
                    } else {
                        number.into()
                    },
                    name: name.into(),
                    active,
                    colorIdx: color_idx_of(&a.jid),
                    hasAvatar: avatar.is_some(),
                    avatar: avatar.unwrap_or_default(),
                    unread,
                    online: active || self.background.iter().any(|s| s.account == a.id),
                }
            })
            .collect();
        if let Some(mine) = rows.iter().find(|row| row.active) {
            self.ui.set_my_account(mine.clone());
        }
        self.ui
            .set_can_add_account(list.len() < crate::vault::MAX_ACCOUNTS);
        self.ui.set_account_count(list.len() as i32);
        self.ui.set_other_accounts_unread(others_unread);
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

    // Background traffic changes the badges in the switcher; redrawn at
    // most a few times a second, whatever the message rate.
    pub(super) fn queue_accounts_refresh(&mut self) {
        if self.headless || self.accounts_refresh_queued {
            return;
        }
        self.accounts_refresh_queued = true;
        self.once(300, |b| {
            b.accounts_refresh_queued = false;
            b.refresh_accounts();
        });
    }

    // Instant: every paired account keeps its own client connected, so a
    // switch only trades the bridge's per-account state for the stored
    // one. An account that is not running yet (never paired, or logged
    // out) gets its client built here.
    pub fn switch_account(&mut self, id: &str) {
        self.ui.set_account_menu_open(false);
        let Some(vault) = &self.vault else { return };
        if self.headless
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
        let mut next = match self.background.iter().position(|s| s.account == id) {
            Some(i) => self.background.remove(i),
            None => match self.open_account(id) {
                Some(state) => state,
                None => {
                    self.ui.set_status_text(t("status.connect_failed").into());
                    return;
                }
            },
        };
        let cold = !next.started;
        next.started = true;
        self.reset_translation();
        self.save_now();
        self.close_session_views();
        // What each account's own screen showed travels with it.
        let leaving = Carry::read(&self.ui);
        Carry::read(&next.views.ui).write(&self.ui);
        leaving.write(&next.views.ui);
        self.swap_account(&mut next);
        let previous = next;
        if let Some(vault) = &self.vault {
            vault.set_account(id);
            vault.set_active_account(id);
        }
        self.keep_or_drop(previous);
        self.load_translation_settings();
        self.load_mcp();
        if cold {
            self.restore_outbox();
            self.wa.send(Cmd::Start);
            self.once(4000, |b| b.index_backfill());
        }
        self.ui
            .set_screen(if self.registered { "main" } else { "login" }.into());
        self.refresh_accounts();
        self.refresh_labels();
        self.refresh_chats();
        self.refresh_statuses();
        self.refresh_calls();
    }

    // The account just switched away from: a paired one stays connected
    // in the background; an unpaired one is closed, and dropped from the
    // registry when nothing ties it to a number ("Add account" backed out
    // of before pairing).
    fn keep_or_drop(&mut self, previous: AccountState) {
        if previous.registered {
            self.background.push(previous);
            return;
        }
        previous.wa.send(Cmd::Shutdown);
        let abandoned = previous.store.chats.is_empty()
            && self.vault.as_ref().is_some_and(|v| {
                v.accounts()
                    .iter()
                    .any(|a| a.id == previous.account && a.jid.is_empty())
            });
        let id = previous.account.clone();
        drop(previous);
        if abandoned {
            self.remove_account(&id);
        }
    }

    // Builds an account's client and loads its conversations without
    // touching the bridge: the result is swapped in, or parked as a
    // background account.
    fn open_account(&mut self, id: &str) -> Option<AccountState> {
        let rt = crate::RT.get()?;
        let views = Views::new()?;
        let (wa, registered) = match WaService::start(rt, id) {
            Ok(pair) => pair,
            Err(e) => {
                eprintln!("[wa] cannot open the selected account: {e}");
                return None;
            }
        };
        let vault = self.vault.as_ref()?;
        let mut store = Store::default();
        vault.set_account(id);
        store.load_from(vault);
        vault.set_account(&self.account);
        let me = vault
            .accounts()
            .into_iter()
            .find(|a| a.id == id)
            .map(|a| a.jid)
            .unwrap_or_default();
        if !me.is_empty() {
            store.set_self(&[&me]);
        }
        wa.send(Cmd::MediaKey(vault.key_handle()));
        Some(AccountState::new(id, wa, registered, store, me, views))
    }

    // Boot: every other account that was paired connects too, so its
    // messages arrive (and notify) while another account is on screen.
    pub(super) fn start_background_sessions(&mut self) {
        let Some(vault) = &self.vault else { return };
        let others: Vec<String> = vault
            .accounts()
            .into_iter()
            .filter(|a| {
                a.id != self.account
                    && !a.jid.is_empty()
                    && !self.background.iter().any(|s| s.account == a.id)
            })
            .map(|a| a.id)
            .collect();
        for id in others {
            let Some(mut state) = self.open_account(&id) else {
                continue;
            };
            if !state.registered {
                // Logged out since: it waits for the user to open it.
                state.wa.send(Cmd::Shutdown);
                continue;
            }
            state.started = true;
            let epoch = state.epoch;
            self.background.push(state);
            self.run_headless(epoch, |b| {
                b.restore_outbox();
                b.wa.send(Cmd::Start);
                b.once(4000, |b| b.index_backfill());
            });
        }
        self.refresh_accounts();
    }

    // Runs `f` against a background account as if it were on screen:
    // its state is swapped into the bridge and a hidden window stands in
    // for the real one, so the regular event handlers store, notify and
    // count unread for it without anything showing on screen. Dropped
    // when the account is gone.
    pub(super) fn run_headless(&mut self, epoch: u64, f: impl FnOnce(&mut Bridge)) {
        if self.headless {
            return;
        }
        let Some(i) = self.background.iter().position(|s| s.epoch == epoch) else {
            return;
        };
        let mut state = self.background.remove(i);
        let foreground = self.account.clone();
        self.swap_account(&mut state);
        self.swap_views(&mut state.views);
        mirror_settings(&state.views.ui, &self.ui);
        self.ui
            .set_screen(if self.registered { "main" } else { "login" }.into());
        if let Some(vault) = &self.vault {
            vault.set_account(&self.account);
        }
        self.headless = true;
        f(self);
        self.headless = false;
        if let Some(vault) = &self.vault {
            vault.set_account(&foreground);
        }
        self.swap_views(&mut state.views);
        self.swap_account(&mut state);
        self.background.insert(i, state);
        self.queue_accounts_refresh();
    }

    // Work for the account behind `epoch`, wherever it is by now.
    pub(super) fn run_for_epoch(&mut self, epoch: u64, f: impl FnOnce(&mut Bridge)) {
        if self.epoch == epoch && !self.headless {
            f(self);
        } else {
            self.run_headless(epoch, f);
        }
    }

    pub(super) fn epoch_of(&self, account: &str) -> Option<u64> {
        if self.account == account {
            Some(self.epoch)
        } else {
            self.background
                .iter()
                .find(|s| s.account == account)
                .map(|s| s.epoch)
        }
    }

    // App exit: every account's pending changes reach the vault and every
    // client says goodbye.
    pub fn shutdown_sessions(&mut self) {
        self.save_now();
        let epochs: Vec<u64> = self.background.iter().map(|s| s.epoch).collect();
        for epoch in epochs {
            self.run_headless(epoch, |b| b.save_now());
        }
        self.wa.send(Cmd::Shutdown);
        for state in &self.background {
            state.wa.send(Cmd::Shutdown);
        }
    }

    pub fn add_account(&mut self) {
        let Some(vault) = &self.vault else { return };
        let mut list = vault.accounts();
        if list.len() >= crate::vault::MAX_ACCOUNTS || self.headless {
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

    // Only ever an account that is not on screen. A background client is
    // told to stop first; a session database it still holds open goes at
    // the next launch (paths::sweep_accounts). The phone keeps listing the
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
        if let Some(i) = self.background.iter().position(|s| s.account == id) {
            let state = self.background.remove(i);
            state.wa.send(Cmd::Shutdown);
        }
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
                println!("[selftest] add account: now on {:?}", self.account);
            }
            1 => {
                let list = ids(self);
                println!(
                    "[selftest] on the new account: {} account(s), {} chat(s), screen {}",
                    list.len(),
                    self.store.chats.len(),
                    self.ui.get_screen()
                );
                if self.account.is_empty() || list.len() != 2 {
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
                if ids(self).len() != crate::vault::MAX_ACCOUNTS {
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
}

// Everything the bridge holds for one account. The fields mirror Bridge
// fields of the same name and are traded with them by swap_account, so
// the code that reads self.store & co. works on whichever account is
// swapped in. `views` stays with the state: the hidden window and models
// a background account draws into while it runs headless.
pub(super) struct AccountState {
    pub(super) account: String,
    pub(super) epoch: u64,
    pub(super) registered: bool,
    // Its client was told to Start (cold accounts start when opened).
    pub(super) started: bool,
    pub(super) wa: WaService,
    pub(super) store: Store,
    self_jid: String,
    current_jid: Option<String>,
    unread_boundary: Option<String>,
    truncated: HashSet<String>,
    scroll_pos: HashMap<String, f32>,
    history_batches: u32,
    history_pending: bool,
    scroll_up_fetch: bool,
    last_scroll_load: Option<Instant>,
    save_queued: bool,
    groups_fetched: bool,
    wipe_on_logout: bool,
    peer_online: bool,
    chat_typing: HashMap<String, HashMap<String, (u64, String)>>,
    presence_subscribed: HashSet<String>,
    warm_order: std::collections::VecDeque<String>,
    sync_batches: u32,
    sync_messages: usize,
    initial_sync: history::InitialSync,
    group_meta_at: HashMap<String, Instant>,
    common_groups_cache: HashMap<String, (Instant, Vec<(String, String)>)>,
    common_groups_pending: HashMap<String, Instant>,
    notify_queue: Vec<(String, String, String, String)>,
    notify_queued: bool,
    preparation_states: HashMap<String, bool>,
    preparations_active: HashSet<String>,
    outbox: Vec<crate::outbox::Entry>,
    outbox_connected: bool,
    requested_channels: HashSet<String>,
    pub(super) views: Views,
}

impl AccountState {
    fn new(
        account: &str,
        wa: WaService,
        registered: bool,
        store: Store,
        self_jid: String,
        views: Views,
    ) -> Self {
        Self {
            account: account.to_string(),
            epoch: wa.epoch(),
            registered,
            started: false,
            wa,
            store,
            self_jid,
            current_jid: None,
            unread_boundary: None,
            truncated: HashSet::new(),
            scroll_pos: HashMap::new(),
            history_batches: 0,
            history_pending: false,
            scroll_up_fetch: false,
            last_scroll_load: None,
            save_queued: false,
            groups_fetched: false,
            wipe_on_logout: false,
            peer_online: false,
            chat_typing: HashMap::new(),
            presence_subscribed: HashSet::new(),
            warm_order: std::collections::VecDeque::new(),
            sync_batches: 0,
            sync_messages: 0,
            initial_sync: Default::default(),
            group_meta_at: HashMap::new(),
            common_groups_cache: HashMap::new(),
            common_groups_pending: HashMap::new(),
            notify_queue: Vec::new(),
            notify_queued: false,
            preparation_states: HashMap::new(),
            preparations_active: HashSet::new(),
            outbox: Vec::new(),
            outbox_connected: false,
            requested_channels: HashSet::new(),
            views,
        }
    }
}

// A window that is never shown, and models bound to nothing: what a
// background account's handlers draw into.
pub(super) struct Views {
    pub(super) ui: AppWindow,
    chats_model: Rc<VecModel<ChatItem>>,
    messages_model: Rc<VecModel<MessageItem>>,
    reactions_model: Rc<VecModel<ReactionItem>>,
    forward_model: Rc<VecModel<ChatItem>>,
    mention_model: Rc<VecModel<MemberItem>>,
    search_model: Rc<VecModel<SearchHit>>,
    chat_search_model: Rc<VecModel<SearchHit>>,
    member_model: Rc<VecModel<MemberItem>>,
    info_member_model: Rc<VecModel<MemberItem>>,
    label_model: Rc<VecModel<LabelItem>>,
    label_chip_model: Rc<VecModel<LabelItem>>,
    status_model: Rc<VecModel<ChatItem>>,
    calls_model: Rc<VecModel<CallItem>>,
    info_media_model: Rc<VecModel<ModelRc<StickerCell>>>,
    sticker_model: Rc<VecModel<ModelRc<StickerCell>>>,
    fav_model: Rc<VecModel<ModelRc<StickerCell>>>,
    gif_model: Rc<VecModel<ModelRc<StickerCell>>>,
}

impl Views {
    pub(super) fn new() -> Option<Self> {
        let ui = match AppWindow::new() {
            Ok(ui) => ui,
            Err(e) => {
                eprintln!("[accounts] cannot build a background view: {e}");
                return None;
            }
        };
        Some(Self {
            ui,
            chats_model: Default::default(),
            messages_model: Default::default(),
            reactions_model: Default::default(),
            forward_model: Default::default(),
            mention_model: Default::default(),
            search_model: Default::default(),
            chat_search_model: Default::default(),
            member_model: Default::default(),
            info_member_model: Default::default(),
            label_model: Default::default(),
            label_chip_model: Default::default(),
            status_model: Default::default(),
            calls_model: Default::default(),
            info_media_model: Default::default(),
            sticker_model: Default::default(),
            fav_model: Default::default(),
            gif_model: Default::default(),
        })
    }
}

// Per-account screen state that lives on the window: connection line,
// pairing, the fatal alert and the first-sync page.
struct Carry {
    status_text: SharedString,
    qr_image: slint::Image,
    pairing_mode: bool,
    pairing_code: SharedString,
    alert_open: bool,
    alert_title: SharedString,
    alert_text: SharedString,
    alert_action: SharedString,
    sync_banner: SharedString,
    sync_screen_open: bool,
    sync_screen_percent: i32,
    sync_screen_started: bool,
    sync_screen_catchup: bool,
    sync_screen_account: SharedString,
    sync_screen_messages: SharedString,
    sync_screen_chats: SharedString,
    sync_screen_media: SharedString,
    sync_screen_files: SharedString,
}

impl Carry {
    fn read(ui: &AppWindow) -> Self {
        Self {
            status_text: ui.get_status_text(),
            qr_image: ui.get_qr_image(),
            pairing_mode: ui.get_pairing_mode(),
            pairing_code: ui.get_pairing_code(),
            alert_open: ui.get_alert_open(),
            alert_title: ui.get_alert_title(),
            alert_text: ui.get_alert_text(),
            alert_action: ui.get_alert_action(),
            sync_banner: ui.get_sync_banner(),
            sync_screen_open: ui.get_sync_screen_open(),
            sync_screen_percent: ui.get_sync_screen_percent(),
            sync_screen_started: ui.get_sync_screen_started(),
            sync_screen_catchup: ui.get_sync_screen_catchup(),
            sync_screen_account: ui.get_sync_screen_account(),
            sync_screen_messages: ui.get_sync_screen_messages(),
            sync_screen_chats: ui.get_sync_screen_chats(),
            sync_screen_media: ui.get_sync_screen_media(),
            sync_screen_files: ui.get_sync_screen_files(),
        }
    }

    fn write(self, ui: &AppWindow) {
        ui.set_status_text(self.status_text);
        ui.set_qr_image(self.qr_image);
        ui.set_pairing_mode(self.pairing_mode);
        ui.set_pairing_code(self.pairing_code);
        ui.set_alert_open(self.alert_open);
        ui.set_alert_title(self.alert_title);
        ui.set_alert_text(self.alert_text);
        ui.set_alert_action(self.alert_action);
        ui.set_sync_banner(self.sync_banner);
        ui.set_sync_screen_open(self.sync_screen_open);
        ui.set_sync_screen_percent(self.sync_screen_percent);
        ui.set_sync_screen_started(self.sync_screen_started);
        ui.set_sync_screen_catchup(self.sync_screen_catchup);
        ui.set_sync_screen_account(self.sync_screen_account);
        ui.set_sync_screen_messages(self.sync_screen_messages);
        ui.set_sync_screen_chats(self.sync_screen_chats);
        ui.set_sync_screen_media(self.sync_screen_media);
        ui.set_sync_screen_files(self.sync_screen_files);
    }
}

// Settings the event handlers read off the window; a background run
// must see the user's choices, not the hidden window's defaults.
fn mirror_settings(from: &AppWindow, to: &AppWindow) {
    to.set_apply_deletions(from.get_apply_deletions());
    to.set_message_sound(from.get_message_sound());
    to.set_send_receipts(from.get_send_receipts());
    to.set_auto_download_photos(from.get_auto_download_photos());
    to.set_auto_download_audio(from.get_auto_download_audio());
    to.set_auto_download_video(from.get_auto_download_video());
    to.set_auto_download_documents(from.get_auto_download_documents());
    to.set_auto_download_stickers(from.get_auto_download_stickers());
    to.set_blur_photos(from.get_blur_photos());
    to.set_dark_theme(from.get_dark_theme());
}

impl Bridge {
    fn swap_account(&mut self, s: &mut AccountState) {
        use std::mem::swap;
        swap(&mut self.account, &mut s.account);
        swap(&mut self.epoch, &mut s.epoch);
        swap(&mut self.registered, &mut s.registered);
        swap(&mut self.wa, &mut s.wa);
        swap(&mut self.store, &mut s.store);
        swap(&mut self.self_jid, &mut s.self_jid);
        swap(&mut self.current_jid, &mut s.current_jid);
        swap(&mut self.unread_boundary, &mut s.unread_boundary);
        swap(&mut self.truncated, &mut s.truncated);
        swap(&mut self.scroll_pos, &mut s.scroll_pos);
        swap(&mut self.history_batches, &mut s.history_batches);
        swap(&mut self.history_pending, &mut s.history_pending);
        swap(&mut self.scroll_up_fetch, &mut s.scroll_up_fetch);
        swap(&mut self.last_scroll_load, &mut s.last_scroll_load);
        swap(&mut self.save_queued, &mut s.save_queued);
        swap(&mut self.groups_fetched, &mut s.groups_fetched);
        swap(&mut self.wipe_on_logout, &mut s.wipe_on_logout);
        swap(&mut self.peer_online, &mut s.peer_online);
        swap(&mut self.chat_typing, &mut s.chat_typing);
        swap(&mut self.presence_subscribed, &mut s.presence_subscribed);
        swap(&mut self.warm_order, &mut s.warm_order);
        swap(&mut self.sync_batches, &mut s.sync_batches);
        swap(&mut self.sync_messages, &mut s.sync_messages);
        swap(&mut self.initial_sync, &mut s.initial_sync);
        swap(&mut self.group_meta_at, &mut s.group_meta_at);
        swap(&mut self.common_groups_cache, &mut s.common_groups_cache);
        swap(
            &mut self.common_groups_pending,
            &mut s.common_groups_pending,
        );
        swap(&mut self.notify_queue, &mut s.notify_queue);
        swap(&mut self.notify_queued, &mut s.notify_queued);
        swap(&mut self.preparation_states, &mut s.preparation_states);
        swap(&mut self.preparations_active, &mut s.preparations_active);
        swap(&mut self.outbox, &mut s.outbox);
        swap(&mut self.outbox_connected, &mut s.outbox_connected);
        swap(&mut self.requested_channels, &mut s.requested_channels);
    }

    fn swap_views(&mut self, v: &mut Views) {
        use std::mem::swap;
        swap(&mut self.ui, &mut v.ui);
        swap(&mut self.chats_model, &mut v.chats_model);
        swap(&mut self.messages_model, &mut v.messages_model);
        swap(&mut self.reactions_model, &mut v.reactions_model);
        swap(&mut self.forward_model, &mut v.forward_model);
        swap(&mut self.mention_model, &mut v.mention_model);
        swap(&mut self.search_model, &mut v.search_model);
        swap(&mut self.chat_search_model, &mut v.chat_search_model);
        swap(&mut self.member_model, &mut v.member_model);
        swap(&mut self.info_member_model, &mut v.info_member_model);
        swap(&mut self.label_model, &mut v.label_model);
        swap(&mut self.label_chip_model, &mut v.label_chip_model);
        swap(&mut self.status_model, &mut v.status_model);
        swap(&mut self.calls_model, &mut v.calls_model);
        swap(&mut self.info_media_model, &mut v.info_media_model);
        swap(&mut self.sticker_model, &mut v.sticker_model);
        swap(&mut self.fav_model, &mut v.fav_model);
        swap(&mut self.gif_model, &mut v.gif_model);
    }
}

// A message for an account that is not on screen must reach that
// account's store and badge, never the visible chat list, and be there
// once the user switches to it.
#[cfg(test)]
mod background_tests {
    use super::*;
    use std::cell::Cell;
    use std::sync::Arc;
    use whatsapp_rust::Jid;
    use whatsapp_rust::types::events::{BatchOrigin, InboundMessage};
    use whatsapp_rust::types::message::{MessageInfo, MessageSource};

    const GROUP: &str = "120363000000000077@g.us";
    const OTHER: &str = "b2";
    const OTHER_EPOCH: u64 = 7;

    fn batch(id: &str, text: &str) -> MessageBatch {
        let info = MessageInfo {
            source: MessageSource {
                chat: GROUP.parse::<Jid>().expect("group jid"),
                sender: Jid::pn("5500000000001"),
                is_from_me: false,
                is_group: true,
                ..Default::default()
            },
            id: id.into(),
            push_name: "Fixture Sender".into(),
            timestamp: whatsapp_rust::chrono::Utc::now(),
            ..Default::default()
        };
        let message = wa::Message {
            conversation: Some(text.to_string()),
            ..Default::default()
        };
        let inbound = InboundMessage::builder()
            .message(Arc::new(message))
            .info(Arc::new(info))
            .build();
        MessageBatch::builder()
            .messages(Arc::from(vec![inbound]))
            .origin(BatchOrigin::Live)
            .build()
    }

    fn listed(b: &Bridge) -> bool {
        (0..b.chats_model.row_count())
            .filter_map(|i| b.chats_model.row_data(i))
            .any(|row| row.jid == GROUP)
    }

    fn other_row_unread(b: &Bridge) -> i32 {
        (0..b.accounts_model.row_count())
            .filter_map(|i| b.accounts_model.row_data(i))
            .find(|row| row.id == OTHER)
            .map_or(-1, |row| row.unread)
    }

    #[test]
    fn background_message_stays_with_its_account_until_switched_to() {
        let path = std::env::temp_dir().join(format!(
            "zapive-background-account-{}.db",
            std::process::id()
        ));
        crate::paths::remove_db(&path);
        let db = path.to_string_lossy().to_string();
        ui_thread::run(move |done| {
            let ui = AppWindow::new().expect("create Slint component");
            wiring::install(&ui, WaService::detached());
            apply_now(|b| {
                let mut vault = Vault::open_at(&db).expect("temp vault");
                vault.open().expect("open temp vault");
                vault.set_accounts(&[
                    crate::vault::Account {
                        id: String::new(),
                        jid: "5500000000002@s.whatsapp.net".into(),
                        name: "First".into(),
                    },
                    crate::vault::Account {
                        id: OTHER.into(),
                        jid: "5500000000003@s.whatsapp.net".into(),
                        name: "Second".into(),
                    },
                ]);
                b.vault = Some(vault);
                b.registered = true;
                b.notifications = false;
                b.ui.set_message_sound(false);
                let mut state = AccountState::new(
                    OTHER,
                    WaService::detached_at(OTHER_EPOCH),
                    true,
                    Store::default(),
                    "5500000000003@s.whatsapp.net".into(),
                    Views::new().expect("background views"),
                );
                state.started = true;
                b.background.push(state);
                b.refresh_accounts();
            });
            std::thread::spawn(|| {
                wa_apply_at(OTHER_EPOCH, |b| {
                    b.on_messages(&batch("BACKGROUND-1", "background-text"))
                })
            })
            .join()
            .expect("wa thread");
            // Past the headless save (2 s) and the switcher refresh.
            slint::Timer::single_shot(Duration::from_millis(2600), move || {
                let result = Cell::new(Ok(()));
                apply_now(|b| {
                    let fail = |what: String| result.set(Err(what));
                    if b.store.chats.contains_key(GROUP) || listed(b) {
                        return fail("the message leaked into the account on screen".into());
                    }
                    let unread = b.background[0]
                        .store
                        .chats
                        .get(GROUP)
                        .map_or(0, |chat| chat.unread);
                    if unread != 1 {
                        return fail(format!("background unread is {unread}"));
                    }
                    if other_row_unread(b) != 1 {
                        return fail(format!("switcher badge is {}", other_row_unread(b)));
                    }
                    b.switch_account(OTHER);
                    if b.account != OTHER || !listed(b) {
                        return fail("switching did not bring the chat on screen".into());
                    }
                    let persisted = b.vault.take().map(|vault| {
                        b.store.evict(GROUP);
                        b.store.hydrate(&vault, GROUP);
                        b.vault = Some(vault);
                        b.store.messages_for(GROUP).len()
                    });
                    if persisted != Some(1) {
                        return fail(format!("stored messages: {persisted:?}"));
                    }
                    b.switch_account("");
                    if !b.account.is_empty() || listed(b) {
                        return fail("switching back still shows the other account".into());
                    }
                    if b.background.len() != 1 || b.background[0].account != OTHER {
                        fail("the paired account left the background".into());
                    }
                });
                drop(ui);
                done(result.into_inner());
            });
        });
        crate::paths::remove_db(&path);
    }
}
