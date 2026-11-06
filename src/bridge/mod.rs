// The only module that touches the Slint window. Lives on the UI thread;
// the WhatsApp side reaches it through ui_apply, and user actions leave
// through WaService commands. Port of src/bridge.ts on master (phases 1-2:
// login, chat list, conversation, text messages, replies, backfill).
use crate::i18n::{t, ta};
use crate::markup::{MentionTarget, has_markup, render_plain, target_at};
use crate::qr::{empty_image, qr_image};
use crate::store::{
    Label, Member, MessageKind, Store, StoredMessage, clean_text, display_id, format_clock,
    format_day, format_number, format_stamp, format_time, is_channel, is_group, normalize_jid,
    now_secs, preview_body, reaction_summary, ticks_for,
};
use crate::vault::Vault;
use crate::wa::{
    AttachmentKind, Cmd, GroupChange, GroupSnapshot, HistoryChunk, MediaWant, QuoteRef, WaService,
};
use crate::{
    AccountItem, AppWindow, CallItem, CallWindow, ChatItem, DetailRow, LabelItem, LinkSpan,
    MemberItem, MessageItem, PdfPage, PinnedItem, ReactionItem, ReactionTab, ReleaseItem,
    SearchHit, StarredItem, StickerCell,
};
use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::time::{Duration, Instant, SystemTime};
use whatsapp_rust::proto_helpers::MessageExt as _;
use whatsapp_rust::types::events::MessageBatch;
use whatsapp_rust::waproto::whatsapp as wa;

mod accounts;
mod audio;
mod calls;
mod chat_info;
mod chat_row_menu;
mod chats;
mod composer;
mod conversation;
mod documents;
mod dropped;
mod helpers;
mod history;
mod incoming;
mod mcp;
mod media;
mod message_actions;
mod notifications;
mod outbox;
mod overlays;
mod panels;
mod polls;
mod selection;
mod session;
mod settings;
mod status;
mod stickers;
mod translation;
mod updates;
mod wiring;

use self::audio::*;
use self::calls::*;
pub use self::dropped::handle_file_drop_event;
use self::helpers::*;
use self::history::*;
use self::media::*;
use self::notifications::*;
use self::selection::*;
use self::stickers::*;
use self::translation::*;
pub use self::wiring::{install, replay_link_press};

// An event that arrived while the vault was locked; replayed on unlock.
type LockedEvent = Box<dyn FnOnce(&mut Bridge)>;

pub struct Bridge {
    ui: AppWindow,
    wa: WaService,
    vault: Option<Vault>,
    store: Store,
    translation: crate::translation::Worker,
    translation_gen: u64,
    translation_request: u64,
    translation_monitor: Option<u64>,
    translation_cache: HashMap<(String, String, String, String), TranslationEntry>,
    translation_settings: TranslationSettings,
    translation_pairs: TranslationPairs,
    chats_model: Rc<VecModel<ChatItem>>,
    messages_model: Rc<VecModel<MessageItem>>,
    current_jid: Option<String>,
    unread_boundary: Option<String>,
    self_jid: String,
    search_text: String,
    tab: String,
    view: String,
    groups_fetched: bool,
    // Message id being replied to.
    reply_to: Option<(String, String)>,
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
    // Accounts: which one is loaded, the wa.rs session epoch its events
    // carry, whether it is paired right now, and the switcher's rows.
    account: String,
    epoch: u64,
    registered: bool,
    switching: bool,
    // Set by the logout dialog: the conversations go with the session.
    // A logout that came from the phone leaves them alone.
    wipe_on_logout: bool,
    accounts_model: Rc<VecModel<AccountItem>>,
    // The open chat's contact is online; what the header falls back to
    // when a typing/recording line ends.
    peer_online: bool,
    // Bumped whenever the typing line changes hands, so an expiry timer
    // armed for an older "composing" does nothing.
    typing_gen: u64,
    chat_typing: HashMap<String, HashMap<String, (u64, String)>>,
    presence_subscribed: HashSet<String>,
    draft_sequence: u64,
    // WhatsApp events that arrived before the vault was open (the client
    // connects while the PIN screen is still up); replayed by boot.
    locked_backlog: Vec<LockedEvent>,
    // Media caches (pixels live UI-side; files live encrypted on disk).
    avatars: HashMap<String, Option<slint::Image>>,
    // Privacy-blur stand-ins of `avatars`, same keys.
    avatar_blurs: HashMap<String, slint::Image>,
    // Least recently stored first; bounds `avatars`.
    avatar_order: std::collections::VecDeque<String>,
    // The window is minimized or hidden: non-essential timers sleep.
    window_idle: bool,
    idle_since: Option<Instant>,
    requested_avatars: HashSet<String>,
    avatar_tries: HashMap<String, u32>,
    media_inflight: HashSet<String>,
    media_previews: HashMap<String, slint::Image>,
    preview_requested: HashSet<String>,
    // Media the server refused (expired, forbidden): asked once per run,
    // not again on every scroll.
    media_failed: HashSet<String>,
    media_path: HashMap<String, String>,
    decoded: HashMap<String, (slint::Image, i32, i32)>,
    decoded_order: std::collections::VecDeque<String>,
    // Pixel bytes held by `decoded`, tracked so eviction can bound them.
    decoded_bytes: usize,
    // Hydrated chats, most-recent last; cold ones live only in the vault.
    warm_order: std::collections::VecDeque<String>,
    hover_jid: String,
    animated: HashMap<String, Anim>,
    // Insertion order for `animated`, so eviction drops the oldest rather
    // than whichever key the map happens to yield first.
    anim_order: std::collections::VecDeque<String>,
    anim_bytes: usize,
    anim_timer: slint::Timer,
    // Stamps "last active" while the app runs.
    activity_timer: slint::Timer,
    stick_requeued: bool,
    media_key: crate::vault::KeyHandle,
    // Voice-note playback and recording.
    audio: Option<AudioState>,
    audio_rate_idx: usize,
    audio_buffers: HashMap<(String, usize), crate::audio::AudioBuffer>,
    audio_timer: slint::Timer,
    waves: HashMap<String, slint::Image>,
    recorder: Option<crate::audio::Recorder>,
    rec_timer: slint::Timer,
    // The paused recording being listened to, with its length in seconds.
    rec_preview: Option<(crate::audio::Player, f64)>,
    // Message actions and outgoing media.
    reactions_model: Rc<VecModel<ReactionItem>>,
    forward_model: Rc<VecModel<ChatItem>>,
    react_pick_for: Option<String>,
    // (chat, message id) pairs waiting for a destination.
    pending_forward: Vec<(String, String)>,
    // History sync progress, for the banner and its dialog.
    sync_batches: u32,
    sync_messages: usize,
    // The page shown while the first history is downloaded.
    initial_sync: history::InitialSync,
    // Multi-select mode over the open conversation.
    select_mode: bool,
    selected: Vec<String>,
    // The "@" being typed: byte range of "@query" in the composer.
    mention_at: Option<(usize, usize)>,
    // Labels inserted into the composer and the jids they stand for;
    // "all" is the whole group.
    mentions: Vec<(String, String)>,
    mention_model: Rc<VecModel<MemberItem>>,
    // Group rosters are refreshed at most every few minutes.
    group_meta_at: HashMap<String, Instant>,
    common_groups_cache: HashMap<String, (Instant, Vec<(String, String)>)>,
    common_groups_pending: HashMap<String, Instant>,
    // Search: hits in the sidebar (all chats) and in the open chat.
    search_model: Rc<VecModel<SearchHit>>,
    chat_search_model: Rc<VecModel<SearchHit>>,
    chat_search_query: String,
    chat_search_pos: usize,
    // Decoded sticker frames keyed by content hash, shared by every
    // message carrying the same sticker and by the picker.
    stickers: HashMap<String, Rc<Vec<slint::Image>>>,
    sticker_order: std::collections::VecDeque<String>,
    sticker_bytes: usize,
    // Member picker for "add member", and the info panel roster.
    member_model: Rc<VecModel<MemberItem>>,
    member_picked: HashSet<String>,
    info_member_model: Rc<VecModel<MemberItem>>,
    label_model: Rc<VecModel<LabelItem>>,
    label_chip_model: Rc<VecModel<LabelItem>>,
    // What the confirm dialog will do when accepted.
    confirm_action: Option<ConfirmAction>,
    // PDF viewer: the worker, the open document's page sizes (points),
    // and which pages are rendered at which width.
    pdf: crate::pdf::PdfWorker,
    pdf_gen: u64,
    pdf_id: String,
    pdf_path: Option<std::path::PathBuf>,
    pdf_sizes: Vec<(f32, f32)>,
    pdf_model: Rc<VecModel<PdfPage>>,
    pdf_zoom: f32,
    pdf_page_w: f32,
    pdf_rendered: HashMap<usize, u32>,
    pdf_pending: HashSet<(usize, u32)>,
    // Drag selection over the conversation: where the press started and
    // where every visible row currently sits (list coordinates).
    drag_anchor: Option<(String, f32)>,
    row_geom: HashMap<String, (f32, f32, u32)>,
    geom_epoch: u32,
    row_gc_queued: bool,
    // Who a pending "share group history" dialog is about.
    share_target: Option<(String, String)>,
    pending_image: Option<std::path::PathBuf>,
    original_image: Option<std::path::PathBuf>,
    preview_jid: Option<String>,
    preview_generation: u64,
    preview_busy: bool,
    // Images dropped together, waiting for their turn in the review.
    staged_files: Vec<std::path::PathBuf>,
    staged_total: usize,
    // A dropped non-image file currently shown in the review.
    staged_document: Option<std::path::PathBuf>,
    staged_position: usize,
    last_paste: Option<Instant>,
    // Toast coalescing: bursts flush as one summary after 1200ms.
    notify_queue: Vec<(String, String, String, String)>,
    notify_queued: bool,
    notifications: bool,
    notify_previews: bool,
    notify_tokens: HashMap<String, NotificationToken>,
    notification_thumbnails: HashMap<String, std::path::PathBuf>,
    // A check the user asked for from Settings: it also reports "nothing
    // new", where the periodic one stays quiet.
    preparation_states: HashMap<String, bool>,
    preparations_active: HashSet<String>,
    outbox: Vec<crate::outbox::Entry>,
    outbox_connected: bool,
    mcp: Option<crate::mcp::Server>,
    update_manual: bool,
    // Status/stories, calls, channels and the info panel.
    status_model: Rc<VecModel<ChatItem>>,
    calls_model: Rc<VecModel<CallItem>>,
    info_media_model: Rc<VecModel<ModelRc<StickerCell>>>,
    viewer: Option<(Vec<StoredMessage>, usize)>,
    // The call screen: its own window, created the first time one rings.
    call_ui: Option<CallWindow>,
    call: Option<ActiveCall>,
    call_timer: slint::Timer,
    // Call anti-spam, RAM only: caller jid -> when we last dismissed them.
    call_dismissed: HashMap<String, i64>,
    silenced_calls: Vec<SilencedCall>,
    call_silence_repeat: bool,
    call_silence_minutes: u32,
    ringer: Option<crate::call::Ringer>,
    requested_channels: HashSet<String>,
    // Sticker/GIF pickers and the video overlay.
    sticker_model: Rc<VecModel<ModelRc<StickerCell>>>,
    fav_model: Rc<VecModel<ModelRc<StickerCell>>>,
    gif_model: Rc<VecModel<ModelRc<StickerCell>>>,
    gif_url_by_id: HashMap<String, String>,
    zoom_frames: Vec<slint::Image>,
    zoom_idx: usize,
    zoom_timer: slint::Timer,
    video: Option<crate::video::Playback>,
    video_audio: Option<crate::audio::Player>,
    video_id: Option<String>,
    // Told apart from a player that was closed or replaced.
    video_gen: u64,
    video_playing: bool,
    video_pos: f64,
    video_duration: f64,
    video_volume: f32,
    video_muted: bool,
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

// WhatsApp events: the client connects (and receives) while the vault is
// still locked behind the PIN screen, but the store isn't loaded yet —
// processing an event then would build on empty state and a later save
// would clobber the vault. Park them and let boot replay in order.
pub fn wa_apply(f: impl FnOnce(&mut Bridge) + Send + 'static) {
    wa_apply_at(
        crate::wa::SESSION_EPOCH.load(std::sync::atomic::Ordering::SeqCst),
        f,
    );
}

// `epoch` is the session the work came from. The bridge holds one
// account's conversations at a time, so anything produced under another
// epoch than the one it has loaded is for a store that is not here.
pub fn wa_apply_at(epoch: u64, f: impl FnOnce(&mut Bridge) + Send + 'static) {
    let _ = slint::invoke_from_event_loop(move || {
        BRIDGE.with(|cell| {
            if let Some(bridge) = cell.borrow_mut().as_mut() {
                if bridge.epoch != epoch {
                    return;
                }
                if bridge.vault.as_ref().is_none_or(|v| v.locked()) {
                    // ~10k events covers hours on the lock screen; past
                    // that, keep the newest.
                    if bridge.locked_backlog.len() >= 10_000 {
                        drop(bridge.locked_backlog.remove(0));
                    }
                    bridge.locked_backlog.push(Box::new(f));
                } else {
                    f(bridge);
                }
            }
        });
    });
}

pub(super) fn apply_now(f: impl FnOnce(&mut Bridge)) {
    BRIDGE.with(|cell| {
        if let Some(bridge) = cell.borrow_mut().as_mut() {
            f(bridge);
        }
    });
}

// UI callbacks can fire synchronously from inside a Bridge method (e.g. a
// scroll write triggering load-older); deferring one tick avoids the
// re-entrant borrow.
pub(super) fn defer(f: impl FnOnce(&mut Bridge) + 'static) {
    defer_after(Duration::ZERO, f);
}

pub(super) fn defer_after(delay: Duration, f: impl FnOnce(&mut Bridge) + 'static) {
    let cell = RefCell::new(Some(f));
    slint::Timer::single_shot(delay, move || {
        if let Some(f) = cell.borrow_mut().take() {
            apply_now(f);
        }
    });
}

impl Bridge {
    // ---- helpers ----

    pub(super) fn once(&mut self, ms: u64, f: impl FnOnce(&mut Bridge) + 'static) {
        let timer = slint::Timer::default();
        let cell = RefCell::new(Some(f));
        timer.start(
            slint::TimerMode::SingleShot,
            Duration::from_millis(ms),
            move || {
                if let Some(f) = cell.borrow_mut().take() {
                    apply_now(f);
                }
            },
        );
        self.oneshots.retain(|timer| timer.running());
        self.oneshots.push(timer);
    }
}

// One process-wide Slint thread for every test that needs an AppWindow.
// Slint binds its platform to the first thread that touches it, so tests
// on separate libtest threads fail with "platform was initialized in
// another thread". `ui_thread::run` sends a closure to that thread, which
// runs the event loop for the whole process; the closure reports through
// the `done` callback it is given (so it can wait on timers). Calls are
// serialised by a mutex.
#[cfg(test)]
pub(crate) mod ui_thread {
    use std::sync::mpsc::{Sender, channel};
    use std::sync::{Mutex, OnceLock};

    type Job = Box<dyn FnOnce(Box<dyn FnOnce(Result<(), String>)>) + Send>;

    // The thread builds a window first (that is what initialises the
    // Slint platform and its event-loop proxy), reports ready, and only
    // then runs the loop; `run` waits for the ready signal, so
    // invoke_from_event_loop never races the platform setup.
    fn started() -> &'static Mutex<()> {
        static GATE: OnceLock<Mutex<()>> = OnceLock::new();
        GATE.get_or_init(|| {
            let (ready_tx, ready_rx) = channel::<()>();
            std::thread::Builder::new()
                .name("slint-test-ui".into())
                .spawn(move || {
                    let _anchor = crate::AppWindow::new().expect("anchor window");
                    ready_tx.send(()).expect("ready signal");
                    slint::run_event_loop_until_quit().expect("slint event loop");
                })
                .expect("spawn slint test thread");
            ready_rx
                .recv_timeout(std::time::Duration::from_secs(30))
                .expect("slint thread never became ready");
            Mutex::new(())
        })
    }

    // Runs `job` on the Slint thread and blocks until it calls `done`.
    pub(crate) fn run(job: impl FnOnce(Box<dyn FnOnce(Result<(), String>)>) + Send + 'static) {
        let _gate = started().lock().unwrap_or_else(|p| p.into_inner());
        let (tx, rx) = channel::<Result<(), String>>();
        let job: Job = Box::new(job);
        let report: Sender<_> = tx;
        slint::invoke_from_event_loop(move || {
            job(Box::new(move |result| {
                let _ = report.send(result);
            }))
        })
        .expect("slint thread alive");
        match rx.recv_timeout(std::time::Duration::from_secs(30)) {
            Ok(Ok(())) => {}
            Ok(Err(message)) => panic!("{message}"),
            Err(e) => panic!("ui job never finished: {e}"),
        }
    }
}

// RED fixtures for BUG-006: a group message delivered through the same
// path wa.rs uses (wa_apply_at -> on_messages) while the user is not
// navigating must reach the store and the visible chat list on its own.
#[cfg(test)]
mod idle_event_tests {
    use super::*;
    use std::cell::Cell;
    use std::sync::Arc;
    use whatsapp_rust::Jid;
    use whatsapp_rust::types::events::{BatchOrigin, InboundMessage};
    use whatsapp_rust::types::message::{MessageInfo, MessageSource};
    use whatsapp_rust::waproto::whatsapp as wa;

    const IDLE_GROUP: &str = "120363000000000001@g.us";
    const ACTIVE_GROUP: &str = "120363000000000002@g.us";

    #[derive(Debug, Default)]
    struct Snapshot {
        // Messages of the chat once the store has been written to the vault
        // and hydrated back: cold chats are dropped from RAM after a flush
        // by design, so RAM alone is not what "arrived" means.
        persisted: usize,
        rows: Vec<(String, String, i32)>,
        refresh_queued: bool,
        current_jid: Option<String>,
    }

    fn group_batch(group: &str, id: &str, text: &str) -> MessageBatch {
        let info = MessageInfo {
            source: MessageSource {
                chat: group.parse::<Jid>().expect("group jid"),
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

    // Delivers from a foreign thread, exactly like Pump::handle_event.
    fn deliver(batch: MessageBatch) {
        std::thread::spawn(move || wa_apply_at(0, move |b| b.on_messages(&batch)))
            .join()
            .expect("wa thread");
    }

    fn snapshot(group: &str) -> Snapshot {
        let seen = Cell::new(Snapshot::default());
        apply_now(|b| {
            let persisted = match b.vault.take() {
                Some(vault) => {
                    let saved = b.store.save_checked(&vault);
                    b.store.evict(group);
                    b.store.hydrate(&vault, group);
                    b.vault = Some(vault);
                    saved
                        .map(|_| b.store.messages_for(group).len())
                        .unwrap_or(0)
                }
                None => 0,
            };
            seen.set(Snapshot {
                persisted,
                rows: (0..b.chats_model.row_count())
                    .filter_map(|i| b.chats_model.row_data(i))
                    .map(|r| (r.jid.to_string(), r.preview.to_string(), r.unread))
                    .collect(),
                refresh_queued: b.refresh_queued,
                current_jid: b.current_jid.clone(),
            })
        });
        seen.into_inner()
    }

    fn check(s: &Snapshot, group: &str, text: &str) -> Result<(), String> {
        if s.current_jid.is_some() {
            return Err(format!("fixture navigated: {s:?}"));
        }
        if s.persisted != 1 {
            return Err(format!("message not stored: {s:?}"));
        }
        if s.refresh_queued {
            return Err(format!("chat-list refresh never drained: {s:?}"));
        }
        let Some(row) = s.rows.iter().find(|(jid, _, _)| jid == group) else {
            return Err(format!("group missing from chat list: {s:?}"));
        };
        if !row.1.contains(text) {
            return Err(format!("stale preview in chat list: {s:?}"));
        }
        if row.2 != 1 {
            return Err(format!("unread not updated: {s:?}"));
        }
        Ok(())
    }

    #[test]
    fn group_message_updates_chat_list_without_navigation_idle_and_active() {
        let path =
            std::env::temp_dir().join(format!("zapive-red-bug006-{}.db", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let db = path.to_string_lossy().to_string();
        ui_thread::run(move |done| {
            let ui = AppWindow::new().expect("create Slint component");
            wiring::install(&ui, WaService::detached());
            apply_now(|b| {
                let mut vault = Vault::open_at(&db).expect("temp vault");
                vault.open().expect("open temp vault");
                b.vault = Some(vault);
                b.epoch = 0;
                b.notifications = false;
                b.ui.set_message_sound(false);
                b.set_window_idle(true);
            });
            slint::Timer::single_shot(Duration::from_millis(100), || {
                deliver(group_batch(IDLE_GROUP, "RED-BUG006-IDLE", "red-idle-text"));
            });
            slint::Timer::single_shot(Duration::from_millis(600), || {
                apply_now(|b| b.set_window_idle(false));
                deliver(group_batch(
                    ACTIVE_GROUP,
                    "RED-BUG006-ACTIVE",
                    "red-active-text",
                ));
            });
            slint::Timer::single_shot(Duration::from_millis(1300), move || {
                let idle = check(&snapshot(IDLE_GROUP), IDLE_GROUP, "red-idle-text");
                let active = check(&snapshot(ACTIVE_GROUP), ACTIVE_GROUP, "red-active-text");
                drop(ui);
                done(if idle.is_ok() && active.is_ok() {
                    Ok(())
                } else {
                    Err(format!("idle: {idle:?}; active: {active:?}"))
                });
            });
        });
        let _ = std::fs::remove_file(&path);
    }
}
