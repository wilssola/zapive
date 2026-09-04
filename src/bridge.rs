// The only module that touches the Slint window. Lives on the UI thread;
// the WhatsApp side reaches it through ui_apply, and user actions leave
// through WaService commands. Port of src/bridge.ts on master (phases 1-2:
// login, chat list, conversation, text messages, replies, backfill).
use crate::i18n::{t, ta};
use crate::markup::{MentionTarget, has_markup, to_markdown};
use crate::qr::{empty_image, qr_image};
use crate::store::{
    MessageKind, Store, StoredMessage, clean_text, display_id, format_day,
    format_number, format_time, is_channel, is_group, normalize_jid, now_secs,
    preview_body, reaction_summary, ticks_for,
};
use crate::vault::Vault;
use crate::wa::{Cmd, HistoryChunk, MediaWant, QuoteRef, WaService};
use crate::{AppWindow, CallItem, CallWindow, ChatItem, MessageItem, ReactionItem, StickerCell};
use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::time::{Duration, Instant};
use whatsapp_rust::types::events::MessageBatch;
use whatsapp_rust::waproto::whatsapp as wa;

const MAX_HISTORY_BATCHES: u32 = 20;

// An offer older than this never rings. The server replays queued call
// stanzas on reconnect and the locked-vault backlog replays its own on
// unlock, so without this the app announces calls that ended hours ago.
const STALE_CALL_SECS: i64 = 75;
// How long an unanswered offer keeps ringing before it counts as missed.
const RING_TIMEOUT_MS: u64 = 45_000;
// How long the call screen stays up after the call ends.
const CALL_LINGER_MS: u64 = 2_000;

// Curated emoji palette for the picker (variation selectors stripped so
// every glyph renders in Slint).
const EMOJIS: &str = "😀 😃 😄 😁 😆 😅 🤣 😂 🙂 😉 😊 😇 🥰 😍 🤩 😘 😜 🤪 🤑 🤗 🤭 🤫 🤔 🤐 🤨 😐 😑 😶 😏 😒 🙄 😬 🤥 😌 😔 😪 🤤 😴 😷 🤒 🤕 🤢 🤮 🤧 🥵 🥶 🥴 😵 🤯 🤠 🥳 😎 🤓 🧐 😕 😟 🙁 😮 😯 😲 😳 🥺 😦 😧 😨 😰 😥 😢 😭 😱 😖 😣 😞 😓 😩 😫 🥱 😤 😡 😠 🤬 😈 👿 💀 💩 🤡 👹 👻 👽 🤖 😺 😸 😹 😻 😼 😽 🙌 👏 🤝 👍 👎 👊 ✊ 🤛 🤜 🤞 ✌ 🤟 🤘 👌 🤏 👈 👉 👆 👇 ☝ ✋ 🤚 🖐 🖖 👋 🤙 💪 🙏 ❤ 🧡 💛 💚 💙 💜 🖤 🤍 💔 💕 💞 💓 💯 💥 💫 🔥 ⭐ 🌟 ⚡ 🎉 🎈 🎁 🏆 ⚽ 🎮 🎵 ☕ 🍻 🍕 🍔 🎂 🍫 🚀 ✈ 🚗 💰";

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
    // WhatsApp events that arrived before the vault was open (the client
    // connects while the PIN screen is still up); replayed by boot.
    locked_backlog: Vec<Box<dyn FnOnce(&mut Bridge)>>,
    // Media caches (pixels live UI-side; files live encrypted on disk).
    avatars: HashMap<String, Option<slint::Image>>,
    requested_avatars: HashSet<String>,
    avatar_tries: HashMap<String, u32>,
    media_inflight: HashSet<String>,
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
    rec_started: Option<Instant>,
    // Message actions and outgoing media.
    reactions_model: Rc<VecModel<ReactionItem>>,
    forward_model: Rc<VecModel<ChatItem>>,
    react_pick_for: Option<String>,
    pending_forward: Option<(String, String)>,
    pending_image: Option<std::path::PathBuf>,
    last_paste: Option<Instant>,
    // Toast coalescing: bursts flush as one summary after 1200ms.
    notify_queue: Vec<(String, String, String)>,
    notify_queued: bool,
    // Status/stories, calls, channels and the info panel.
    status_model: Rc<VecModel<ChatItem>>,
    calls_model: Rc<VecModel<CallItem>>,
    info_media_model: Rc<VecModel<ModelRc<StickerCell>>>,
    viewer: Option<(Vec<StoredMessage>, usize)>,
    // The call screen: its own window, created the first time one rings.
    call_ui: Option<CallWindow>,
    call: Option<ActiveCall>,
    call_timer: slint::Timer,
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
    video_audio: Option<crate::audio::Player>,
    video_id: Option<String>,
    // Keeps one-shot timers alive until they fire.
    oneshots: Vec<slint::Timer>,
}

struct Anim {
    frames: Vec<slint::Image>,
    idx: usize,
    looping: bool,
}

// Whatever the call screen is currently showing. One at a time: a second
// offer arriving mid-call is turned down rather than queued.
struct ActiveCall {
    // Empty until the server hands back the id of an outgoing call.
    id: String,
    // Canonical peer jid, for the name and the avatar.
    jid: String,
    // The sender jid exactly as it arrived; a reject stanza needs it.
    from: String,
    outgoing: bool,
    // The call was offered (or placed) as a video call.
    video: bool,
    // Our camera is on and sending.
    sending_video: bool,
    // The peer's camera is on, or they have asked us to turn ours on.
    peer_video: bool,
    peer_wants_video: bool,
    // "incoming" | "outgoing" | "connecting" | "active" | "ended"
    state: &'static str,
    started: Option<Instant>,
}

// The note being played (or paused); the mini player takes over when the
// user leaves its conversation.
struct AudioState {
    id: String,
    jid: String,
    duration: f64,
    paused: bool,
    // Waiting for the decoded buffer; holds where to start, in source secs.
    pending_offset: Option<f64>,
    player: Option<crate::audio::Player>,
}

const RATES: [f64; 5] = [1.0, 1.5, 2.0, 2.5, 3.0];
const RATE_LABELS: [&str; 5] = ["1x", "1.5x", "2x", "2.5x", "3x"];

const DECODE_CACHE_MAX: usize = 50;
// The count cap alone is not a memory bound: photos are decoded at up to
// 1280px, so one entry can be 6 MB of RGBA and fifty of them 300 MB. These
// budgets are what actually keeps the caches honest; the counts above stay
// as a ceiling on bookkeeping.
const DECODE_CACHE_BYTES: usize = 64 * 1024 * 1024;
const ANIM_CACHE_BYTES: usize = 24 * 1024 * 1024;
const MAX_ANIMATIONS: usize = 6;
// How many chats keep their message lists in RAM at once.
const WARM_MAX: usize = 8;

fn image_of(d: &crate::media::Decoded) -> slint::Image {
    let mut buf = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::new(d.w, d.h);
    buf.make_mut_bytes().copy_from_slice(&d.rgba);
    slint::Image::from_rgba8(buf)
}

// What an image costs in memory: Slint keeps decoded RGBA, so it is the
// pixel count times four regardless of the size it is drawn at.
fn image_bytes(image: &slint::Image) -> usize {
    let size = image.size();
    size.width as usize * size.height as usize * 4
}

// WhatsApp-style thumbnail box: fit within 330x380, never upscale.
fn bubble_fit(w: i32, h: i32) -> (i32, i32) {
    let scale = (330.0 / w as f64).min(380.0 / h as f64).min(1.0);
    (((w as f64 * scale).round() as i32).max(1), ((h as f64 * scale).round() as i32).max(1))
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
    let _ = slint::invoke_from_event_loop(move || {
        BRIDGE.with(|cell| {
            if let Some(bridge) = cell.borrow_mut().as_mut() {
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
    let reactions_model = Rc::new(VecModel::<ReactionItem>::default());
    let forward_model = Rc::new(VecModel::<ChatItem>::default());
    let status_model = Rc::new(VecModel::<ChatItem>::default());
    let calls_model = Rc::new(VecModel::<CallItem>::default());
    let info_media_model = Rc::new(VecModel::<ModelRc<StickerCell>>::default());
    let sticker_model = Rc::new(VecModel::<ModelRc<StickerCell>>::default());
    let fav_model = Rc::new(VecModel::<ModelRc<StickerCell>>::default());
    let gif_model = Rc::new(VecModel::<ModelRc<StickerCell>>::default());
    ui.set_sticker_rows(ModelRc::from(sticker_model.clone()));
    ui.set_fav_rows(ModelRc::from(fav_model.clone()));
    ui.set_gif_rows(ModelRc::from(gif_model.clone()));
    ui.set_chats(ModelRc::from(chats_model.clone()));
    ui.set_messages(ModelRc::from(messages_model.clone()));
    ui.set_reaction_rows(ModelRc::from(reactions_model.clone()));
    ui.set_forward_rows(ModelRc::from(forward_model.clone()));
    ui.set_statuses(ModelRc::from(status_model.clone()));
    ui.set_calls(ModelRc::from(calls_model.clone()));
    ui.set_info_media(ModelRc::from(info_media_model.clone()));
    let emoji_rows: Vec<ModelRc<SharedString>> = EMOJIS
        .split(' ')
        .collect::<Vec<_>>()
        .chunks(8)
        .map(|chunk| {
            let row: Vec<SharedString> = chunk.iter().map(|e| SharedString::from(*e)).collect();
            ModelRc::from(Rc::new(VecModel::from(row)))
        })
        .collect();
    ui.set_emoji_rows(ModelRc::from(Rc::new(VecModel::from(emoji_rows))));
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
        locked_backlog: Vec::new(),
        avatars: HashMap::new(),
        requested_avatars: HashSet::new(),
        avatar_tries: HashMap::new(),
        media_inflight: HashSet::new(),
        media_path: HashMap::new(),
        decoded: HashMap::new(),
        decoded_order: std::collections::VecDeque::new(),
        decoded_bytes: 0,
        warm_order: std::collections::VecDeque::new(),
        hover_jid: String::new(),
        animated: HashMap::new(),
        anim_order: std::collections::VecDeque::new(),
        anim_bytes: 0,
        anim_timer: slint::Timer::default(),
        stick_requeued: false,
        media_key: crate::vault::KeyHandle::default(),
        audio: None,
        audio_rate_idx: 0,
        audio_buffers: HashMap::new(),
        audio_timer: slint::Timer::default(),
        waves: HashMap::new(),
        recorder: None,
        rec_timer: slint::Timer::default(),
        rec_started: None,
        reactions_model,
        forward_model,
        react_pick_for: None,
        pending_forward: None,
        pending_image: None,
        last_paste: None,
        notify_queue: Vec::new(),
        notify_queued: false,
        status_model,
        calls_model,
        info_media_model,
        viewer: None,
        call_ui: None,
        call: None,
        call_timer: slint::Timer::default(),
        ringer: None,
        requested_channels: HashSet::new(),
        sticker_model,
        fav_model,
        gif_model,
        gif_url_by_id: HashMap::new(),
        zoom_frames: Vec::new(),
        zoom_idx: 0,
        zoom_timer: slint::Timer::default(),
        video_audio: None,
        video_id: None,
        oneshots: Vec::new(),
    };
    BRIDGE.with(|cell| *cell.borrow_mut() = Some(bridge));
    // Update checks start here, not at boot: the banner must also reach
    // the lock and login screens.
    apply_now(|b| b.once(10_000, |b| b.update_tick()));
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
    ui.on_autostart_changed(|on| {
        defer(move |b| b.handle_autostart(on));
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
    ui.on_chat_hover(|jid| {
        let jid = jid.to_string();
        defer(move |b| b.chat_hover(&jid));
    });
    ui.on_update_apply(|| {
        defer(|b| {
            b.ui.set_update_state(2);
            b.wa.send(Cmd::ApplyUpdate);
        });
    });
    ui.on_update_restart(|| {
        defer(|_| {
            if let Ok(exe) = std::env::current_exe() {
                let _ = std::process::Command::new(exe).spawn();
            }
            let _ = slint::quit_event_loop();
        });
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
    ui.on_react_to(|id, emoji| {
        let (id, emoji) = (id.to_string(), emoji.to_string());
        defer(move |b| {
            b.ui.set_react_bar_id("".into());
            b.send_reaction(&id, &emoji);
        });
    });
    ui.on_react_pick(|id| {
        let id = id.to_string();
        defer(move |b| {
            // The full picker doubles as the reaction picker.
            b.react_pick_for = Some(id.clone());
            b.ui.set_picker_open(true);
        });
    });
    ui.on_emoji_pick(|emoji| {
        let emoji = emoji.to_string();
        defer(move |b| {
            if let Some(target) = b.react_pick_for.take() {
                b.ui.set_picker_open(false);
                b.send_reaction(&target, &emoji);
            } else {
                b.ui.invoke_append_composer(emoji.into());
            }
        });
    });
    ui.on_show_reactions(|id| {
        let id = id.to_string();
        defer(move |b| b.show_reactions(&id));
    });
    ui.on_toggle_star(|id| {
        let id = id.to_string();
        defer(move |b| b.toggle_star(&id));
    });
    ui.on_delete_message(|id| {
        let id = id.to_string();
        defer(move |b| b.delete_message(&id));
    });
    ui.on_request_forward(|id| {
        let id = id.to_string();
        defer(move |b| b.request_forward(&id));
    });
    ui.on_forward_search(|query| {
        let query = query.to_string();
        defer(move |b| b.fill_forward_rows(&query));
    });
    ui.on_forward_to(|jid| {
        let jid = jid.to_string();
        defer(move |b| b.handle_forward_to(&jid));
    });
    ui.on_attach_image(|| defer(|b| b.handle_attach("image")));
    ui.on_attach_audio(|| defer(|b| b.handle_attach("audio")));
    ui.on_attach_doc(|| defer(|b| b.handle_attach("doc")));
    ui.on_confirm_send_image(|caption| {
        let caption = caption.to_string();
        defer(move |b| b.confirm_send_image(&caption));
    });
    ui.on_cancel_send_image(|| {
        defer(|b| {
            b.pending_image = None;
            b.ui.set_preview_open(false);
        });
    });
    ui.on_paste_clipboard(|| defer(|b| b.handle_paste()));
    ui.on_picker_opened(|| defer(|b| b.load_sticker_panel()));
    ui.on_picker_closed(|| defer(|_| {}));
    ui.on_sticker_send(|id| {
        let id = id.to_string();
        defer(move |b| b.send_sticker_by_id(&id));
    });
    ui.on_attach_sticker(|| {
        defer(|b| {
            if b.current_jid.is_none() {
                return;
            }
            std::thread::spawn(|| {
                let picked = rfd::FileDialog::new()
                    .add_filter(t("picker.images"), &["jpg", "jpeg", "png", "webp"])
                    .pick_file();
                if let Some(path) = picked {
                    ui_apply(move |b| {
                        if let Some(jid) = b.current_jid.clone() {
                            b.wa.send(Cmd::SendSticker { jid, path });
                        }
                    });
                }
            });
        });
    });
    ui.on_gif_search(|query| {
        let query = query.to_string();
        defer(move |b| {
            b.ui.set_gif_hint("".into());
            b.wa.send(Cmd::GifSearch(query));
        });
    });
    ui.on_gif_send(|id| {
        let id = id.to_string();
        defer(move |b| b.send_gif_by_id(&id));
    });
    ui.on_open_video(|id| {
        let id = id.to_string();
        defer(move |b| b.open_video(&id));
    });
    ui.on_close_video(|| defer(|b| b.close_video()));
    ui.on_status_open(|jid| {
        let jid = jid.to_string();
        defer(move |b| b.open_status_viewer(&jid));
    });
    ui.on_status_next(|| defer(|b| b.step_status(1)));
    ui.on_status_prev(|| defer(|b| b.step_status(-1)));
    ui.on_status_close(|| {
        defer(|b| {
            b.viewer = None;
            b.ui.set_sv_open(false);
        });
    });
    ui.on_start_call(|jid| {
        let jid = jid.to_string();
        defer(move |b| b.start_call(&jid));
    });
    // Enumerating devices touches the audio and camera stacks, so it waits
    // until the user actually opens Settings.
    ui.on_settings_opened(|| defer(|b| b.refresh_devices()));
    ui.on_start_video_call(|jid| {
        let jid = jid.to_string();
        defer(move |b| b.start_video_call(&jid));
    });
    ui.on_device_changed(|kind, name| {
        let (kind, name) = (kind.to_string(), name.to_string());
        defer(move |b| b.set_device(&kind, &name));
    });
    ui.on_open_info(|| defer(|b| b.open_contact_info()));
    ui.on_close_info(|| defer(|b| b.ui.set_info_open(false)));
    ui.on_toggle_archive(|| defer(|b| b.toggle_archive()));
    ui.on_clear_chat(|| defer(|b| b.clear_current_chat()));
    ui.on_save_pin(|current, next| {
        let (current, next) = (current.to_string(), next.to_string());
        defer(move |b| b.handle_save_pin(&current, &next));
    });
    ui.on_remove_pin(|current| {
        let current = current.to_string();
        defer(move |b| b.handle_remove_pin(&current));
    });
    ui.set_audio_rate_label("1x".into());
    ui.on_audio_toggle(|id| {
        let id = id.to_string();
        defer(move |b| b.toggle_audio(&id));
    });
    ui.on_audio_seek(|id, frac| {
        let id = id.to_string();
        defer(move |b| b.seek_audio(&id, frac));
    });
    ui.on_audio_cycle_rate(|| defer(|b| b.cycle_audio_rate()));
    ui.on_mini_audio_toggle(|| {
        defer(|b| {
            if let Some(id) = b.audio.as_ref().map(|a| a.id.clone()) {
                b.toggle_audio(&id);
            }
        });
    });
    ui.on_mini_audio_open(|| {
        defer(|b| {
            if let Some((jid, id)) = b.audio.as_ref().map(|a| (a.jid.clone(), a.id.clone())) {
                b.open_dm(&jid, Some(&id));
            }
        });
    });
    ui.on_mini_audio_close(|| defer(|b| b.stop_audio()));
    ui.on_rec_start(|| defer(|b| b.start_recording()));
    ui.on_rec_stop(|| defer(|b| b.stop_recording(true)));
    ui.on_rec_cancel(|| defer(|b| b.stop_recording(false)));
    ui.on_play_audio(|path| {
        // Documents (and, until phase 4, audio files) open with whatever
        // the OS associates; the handler needs a decrypted copy.
        let path = path.to_string();
        defer(move |b| {
            let cached = std::path::PathBuf::from(&path);
            if let Some(plain) = crate::media::temp_plain(&b.media_key, &cached) {
                crate::platform::open_path(&plain.to_string_lossy());
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
        crate::media::clean_tmp();
        self.store.load_from(&vault);
        self.media_key = vault.key_handle();
        self.wa.send(Cmd::MediaKey(vault.key_handle()));
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
        self.refresh_statuses();
        self.refresh_calls();
        // Before any call can start, so the first one already uses the
        // devices the user picked.
        self.load_devices();
        self.wa.send(Cmd::Start);
        // Events that arrived while the PIN screen was up.
        for f in std::mem::take(&mut self.locked_backlog) {
            f(self);
        }
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

    // The OS is the only source of truth here (a Run entry, an autostart
    // .desktop, a LaunchAgent), so the chip is refreshed from it rather
    // than from what was asked for: a rejected write shows as unchanged.
    fn handle_autostart(&mut self, on: bool) {
        match crate::platform::autostart_set(on) {
            Ok(()) => self.ui.set_settings_status("".into()),
            Err(e) => self.ui.set_settings_status(ta("autostart.failed", &[&e]).into()),
        }
        self.ui.set_autostart(crate::platform::autostart_enabled());
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
        self.avatars.clear();
        self.requested_avatars.clear();
        self.avatar_tries.clear();
        self.media_inflight.clear();
        self.media_path.clear();
        self.decoded.clear();
        self.decoded_order.clear();
        self.decoded_bytes = 0;
        self.animated.clear();
        self.anim_order.clear();
        self.anim_bytes = 0;
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
        self.resolve_channel_names();
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
                self.load_media_for_chat(&jid);
                self.queue_row_avatars(&jid);
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
                    self.refresh_statuses();
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
            } else if !from_me {
                if let Some(meta) = self.store.chats.get_mut(&jid) {
                    meta.unread += 1;
                    if mentions_me {
                        meta.mentioned = true;
                    }
                }
                if let Some(m) = self.store.messages_for(&jid).last().cloned() {
                    let body = self.store.named_mentions(&notification_body(&m));
                    let title = self.store.chat_name(&jid);
                    let text = if is_group(&jid) && !m.sender.is_empty() {
                        format!("{}: {body}", m.sender)
                    } else {
                        body
                    };
                    self.push_notification(title, text, jid.clone());
                }
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
        let avatar = self.avatar_for(jid);
        ChatItem {
            jid: jid.into(),
            name: name.clone().into(),
            preview: preview.into(),
            time: format_time(timestamp).into(),
            hasAvatar: avatar.is_some(),
            avatar: avatar.unwrap_or_else(empty_image),
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
        self.ensure_avatars();
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
                // Never write through a locked vault: the store isn't
                // loaded yet and every set() would be dropped anyway.
                if !vault.locked() {
                    b.store.save_to(&vault);
                }
                b.vault = Some(vault);
            }
        });
    }

    // ---- conversation ----

    fn apply_header(&mut self, jid: &str) {
        let name = self.store.chat_name(jid);
        let avatar = self.avatar_for(jid);
        self.ui.set_current_jid(jid.into());
        // Calls are one-to-one: groups, channels and status cannot ring.
        self.ui.set_current_callable(
            !is_group(jid) && !is_channel(jid) && jid != "status@broadcast",
        );
        self.ui.set_current_chat_name(name.clone().into());
        self.ui.set_current_avatar_has(avatar.is_some());
        self.ui.set_current_avatar(avatar.unwrap_or_else(empty_image));
        self.ui.set_current_initial(initial_of(&name).into());
        self.ui.set_current_color_idx(color_idx_of(jid));
    }

    // Opens (or starts) the conversation with a jid; used by the chat list
    // and by clicking a sender name inside a group.
    // Hydrates a chat and rotates the warm set, evicting the coldest.
    fn warm_chat(&mut self, jid: &str) {
        let Some(vault) = self.vault.as_ref() else { return };
        let jid = self.store.canon_owned(jid);
        self.store.hydrate(vault, &jid);
        self.warm_order.retain(|j| j != &jid);
        self.warm_order.push_back(jid);
        while self.warm_order.len() > WARM_MAX {
            let evictable = self.warm_order.iter().position(|j| {
                Some(j.as_str()) != self.current_jid.as_deref()
                    && !self.store.dirty_jids.contains(j)
            });
            let Some(idx) = evictable else { break };
            if let Some(old) = self.warm_order.remove(idx) {
                self.store.evict(&old);
            }
        }
    }

    // Mouse resting on a chat row usually precedes a click; preload the
    // tail so the open feels instant.
    pub fn chat_hover(&mut self, jid: &str) {
        if self.hover_jid == jid || self.store.hydrated.contains(self.store.canon(jid)) {
            return;
        }
        self.hover_jid = jid.to_string();
        let jid = jid.to_string();
        self.once(120, move |b| {
            if b.hover_jid == jid {
                b.warm_chat(&jid);
            }
        });
    }

    pub fn open_dm(&mut self, jid_raw: &str, jump_to: Option<&str>) {
        self.warm_chat(jid_raw);
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
        self.load_media_for_chat(jid);
        self.queue_row_avatars(jid);
        self.queue_avatar(jid);
        // Playback follows the user: the mini bar takes over elsewhere.
        self.sync_mini_player();
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

        let jid_owned = jid.to_string();
        self.once(40, move |b| {
            let added = b.ui.get_conv_viewport_h() - before_h;
            b.ui.invoke_set_conversation_scroll(before_y - added);
            b.ui.set_conv_ready(true);
            b.load_media_for_chat(&jid_owned);
            b.queue_row_avatars(&jid_owned);
        });
    }

    fn push_message_row(&mut self, jid: &str) {
        let list = self.store.messages_for(jid);
        let Some(stored) = list.last() else { return };
        let prev = list.len().checked_sub(2).and_then(|i| list.get(i));
        let row = self.to_row(stored, prev);
        let from_me = stored.from_me;
        let needs_media =
            stored.kind != MessageKind::Text || !stored.link_title.is_empty() || !stored.link_url.is_empty();
        let stored = stored.clone();
        self.messages_model.push(row);
        // Don't yank the view down while the user is reading older rows.
        if from_me || self.ui.get_stick_bottom() {
            self.scroll_to_end();
        }
        if needs_media {
            self.request_media(&stored);
        }
        self.queue_row_avatars(jid);
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
            // Rebuilt rows keep already-decoded media so scrollback and
            // re-opens draw instantly.
            picture: self
                .decoded
                .get(&m.id)
                .map(|(img, _, _)| img.clone())
                .unwrap_or_else(empty_image),
            picW: self.decoded.get(&m.id).map(|&(_, w, _)| w).unwrap_or(pic_w),
            picH: self.decoded.get(&m.id).map(|&(_, _, h)| h).unwrap_or(pic_h),
            mediaPath: self.media_path.get(&m.id).cloned().unwrap_or_default().into(),
            mediaReady: self.media_path.contains_key(&m.id),
            reactions: reaction_summary(m).into(),
            dayLabel: day_label.into(),
            ticks: ticks.into(),
            ticksBlue: ticks_blue,
            senderHasAvatar: self.avatar_for(&sender_jid).is_some(),
            senderAvatar: self.avatar_for(&sender_jid).unwrap_or_else(empty_image),
            senderInitial: initial_of(if m.sender.is_empty() { "?" } else { &m.sender }).into(),
            senderColorIdx: color_idx_of(if sender_jid.is_empty() { &m.jid } else { &sender_jid }),
            voiceJid: voice_jid.clone().into(),
            voiceHasAvatar: self.avatar_for(&voice_jid).is_some(),
            voiceAvatar: self.avatar_for(&voice_jid).unwrap_or_else(empty_image),
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
            hasWave: self.waves.contains_key(&m.id),
            wave: self.waves.get(&m.id).cloned().unwrap_or_else(empty_image),
            playing: self
                .audio
                .as_ref()
                .map(|a| a.id == m.id && !a.paused && a.player.is_some())
                .unwrap_or(false),
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

    // ---- media (results arrive from the tokio side) ----

    fn avatar_for(&self, jid: &str) -> Option<slint::Image> {
        self.avatars.get(jid).cloned().flatten()
    }

    fn queue_avatar(&mut self, jid: &str) {
        if jid.is_empty() || self.requested_avatars.contains(jid) {
            return;
        }
        self.requested_avatars.insert(jid.to_string());
        self.wa.send(Cmd::FetchAvatar(jid.to_string()));
    }

    fn ensure_avatars(&mut self) {
        let jids: Vec<String> = self
            .store
            .sorted_chats()
            .into_iter()
            .map(|meta| meta.jid.clone())
            .filter(|jid| !self.requested_avatars.contains(jid))
            .collect();
        for jid in jids {
            self.queue_avatar(&jid);
        }
    }

    pub fn on_avatar(&mut self, jid: &str, img: Option<crate::media::Decoded>, resolved: bool) {
        match (img, resolved) {
            (Some(decoded), _) => {
                let image = image_of(&decoded);
                self.avatars.insert(jid.to_string(), Some(image.clone()));
                self.patch_avatar_everywhere(jid, &image);
            }
            (None, true) => {
                // Confirmed: no picture. The initial stays.
                self.avatars.insert(jid.to_string(), None);
            }
            (None, false) => {
                // Transient failure — retry a few times, spaced out.
                let tries = self.avatar_tries.entry(jid.to_string()).or_insert(0);
                *tries += 1;
                if *tries < 3 {
                    self.requested_avatars.remove(jid);
                    let jid = jid.to_string();
                    self.once(1500, move |b| b.queue_avatar(&jid));
                }
            }
        }
    }

    fn patch_avatar_everywhere(&mut self, jid: &str, image: &slint::Image) {
        // Chat list row.
        for i in 0..self.chats_model.row_count() {
            if let Some(mut row) = self.chats_model.row_data(i)
                && row.jid == jid
            {
                row.avatar = image.clone();
                row.hasAvatar = true;
                self.chats_model.set_row_data(i, row);
                break;
            }
        }
        // Conversation header.
        if Some(&jid.to_string()) == self.current_jid.as_ref() {
            self.ui.set_current_avatar(image.clone());
            self.ui.set_current_avatar_has(true);
        }
        // Sender and voice-note avatars inside message rows.
        for i in 0..self.messages_model.row_count() {
            let Some(mut row) = self.messages_model.row_data(i) else { continue };
            let mut changed = false;
            if row.senderJid == jid && !row.senderHasAvatar {
                row.senderAvatar = image.clone();
                row.senderHasAvatar = true;
                changed = true;
            }
            if row.voiceJid == jid && !row.voiceHasAvatar {
                row.voiceAvatar = image.clone();
                row.voiceHasAvatar = true;
                changed = true;
            }
            if changed {
                self.messages_model.set_row_data(i, row);
            }
        }
    }

    // Requests download/decoding for every media-bearing message of a chat,
    // newest first.
    fn load_media_for_chat(&mut self, jid: &str) {
        let pending: Vec<StoredMessage> = self
            .store
            .messages_for(jid)
            .iter()
            .rev()
            .filter(|m| {
                m.kind != MessageKind::Text || !m.link_title.is_empty() || !m.link_url.is_empty()
            })
            .cloned()
            .collect();
        for m in pending {
            self.request_media(&m);
        }
    }

    fn request_media(&mut self, m: &StoredMessage) {
        if m.deleted || self.media_inflight.contains(&m.id) {
            return;
        }
        let Some(raw) = m.raw.clone() else { return };
        use whatsapp_rust::proto_helpers::MessageExt as _;
        let inner = raw.get_base_message();
        match m.kind {
            MessageKind::Text => {
                // Link preview thumbnail travels inside the message itself.
                if let Some(thumb) = inner
                    .extended_text_message
                    .as_option()
                    .and_then(|e| e.jpeg_thumbnail.as_ref())
                    .filter(|t| !t.is_empty())
                {
                    self.media_inflight.insert(m.id.clone());
                    self.wa.send(Cmd::DecodeThumb {
                        id: m.id.clone(),
                        bytes: thumb.to_vec(),
                        link: true,
                    });
                }
                return;
            }
            MessageKind::Image => {
                self.media_inflight.insert(m.id.clone());
                self.wa.send(Cmd::Media {
                    id: m.id.clone(),
                    mimetype: m.mimetype.clone(),
                    message: raw,
                    want: if m.sticker { MediaWant::Sticker } else { MediaWant::Image },
                });
            }
            MessageKind::Video => {
                self.media_inflight.insert(m.id.clone());
                if m.gif {
                    self.wa.send(Cmd::Media {
                        id: m.id.clone(),
                        mimetype: m.mimetype.clone(),
                        message: raw,
                        want: MediaWant::Gif,
                    });
                    return;
                }
                // Poster from the embedded thumbnail; the clip itself is
                // cached for the click-to-play path.
                if let Some(thumb) = inner
                    .video_message
                    .as_option()
                    .and_then(|v| v.jpeg_thumbnail.as_ref())
                    .filter(|t| !t.is_empty())
                {
                    self.wa.send(Cmd::DecodeThumb {
                        id: m.id.clone(),
                        bytes: thumb.to_vec(),
                        link: false,
                    });
                }
                self.wa.send(Cmd::Media {
                    id: m.id.clone(),
                    mimetype: m.mimetype.clone(),
                    message: raw,
                    want: MediaWant::File,
                });
            }
            MessageKind::Audio | MessageKind::Doc => {
                self.media_inflight.insert(m.id.clone());
                self.wa.send(Cmd::Media {
                    id: m.id.clone(),
                    mimetype: m.mimetype.clone(),
                    message: raw,
                    want: MediaWant::File,
                });
            }
        }
    }

    fn remember_decoded(&mut self, id: &str, image: slint::Image, w: i32, h: i32) {
        // Re-decoding the same message replaces its entry, so drop the old
        // one from the running total first.
        if let Some((old, _, _)) = self.decoded.remove(id) {
            self.decoded_bytes = self.decoded_bytes.saturating_sub(image_bytes(&old));
            self.decoded_order.retain(|known| known != id);
        }
        self.decoded_bytes += image_bytes(&image);
        self.decoded_order.push_back(id.to_string());
        self.decoded.insert(id.to_string(), (image, w, h));
        // Evicting is cheap: the rows already on screen hold their own
        // reference to the image, so this only costs a re-decode the next
        // time the conversation is rebuilt.
        while self.decoded_order.len() > DECODE_CACHE_MAX
            || (self.decoded_bytes > DECODE_CACHE_BYTES && self.decoded_order.len() > 1)
        {
            let Some(evicted) = self.decoded_order.pop_front() else { break };
            if let Some((old, _, _)) = self.decoded.remove(&evicted) {
                self.decoded_bytes = self.decoded_bytes.saturating_sub(image_bytes(&old));
            }
        }
    }

    // A late-loading image must not push the newest message out of view;
    // debounced so a burst of images re-anchors only once.
    fn restick(&mut self) {
        if !self.ui.get_stick_bottom() || self.stick_requeued {
            return;
        }
        self.stick_requeued = true;
        self.once(120, |b| {
            b.stick_requeued = false;
            if b.ui.get_stick_bottom() {
                b.ui.invoke_scroll_conversation_end();
            }
        });
    }

    pub fn on_media_missing(&mut self, id: &str) {
        self.media_inflight.remove(id);
        self.patch_row(id, |row| {
            row.kind = "text".into();
            row.text = t("media.unavailable").into();
        });
    }

    pub fn on_media_file(&mut self, id: &str, path: &std::path::Path) {
        self.media_inflight.remove(id);
        let path_str = path.to_string_lossy().into_owned();
        self.media_path.insert(id.to_string(), path_str.clone());
        self.patch_row(id, |row| {
            row.mediaPath = path_str.into();
            row.mediaReady = true;
        });
        // Voice notes draw their amplitude bars once the file is here.
        let is_audio = self
            .current_jid
            .as_ref()
            .and_then(|jid| self.store.messages_for(jid).iter().find(|m| m.id == id))
            .map(|m| m.kind == MessageKind::Audio)
            .unwrap_or(false);
        if is_audio && !self.waves.contains_key(id) {
            self.wa.send(Cmd::Waveform { id: id.to_string(), path: path.to_path_buf() });
        }
        self.restick();
    }

    pub fn on_media_image(&mut self, id: &str, path: &std::path::Path, img: crate::media::Decoded) {
        self.media_inflight.remove(id);
        let image = image_of(&img);
        let (pic_w, pic_h) = bubble_fit(img.w as i32, img.h as i32);
        self.remember_decoded(id, image.clone(), pic_w, pic_h);
        let path_str = path.to_string_lossy().into_owned();
        self.media_path.insert(id.to_string(), path_str.clone());
        self.patch_row(id, |row| {
            row.picture = image.clone();
            row.picW = pic_w;
            row.picH = pic_h;
            row.mediaPath = path_str.into();
            row.mediaReady = true;
        });
        self.feed_viewer(id, &image);
        self.restick();
    }

    pub fn on_media_sticker(&mut self, id: &str, path: &std::path::Path, frames: Vec<crate::media::Decoded>) {
        self.media_inflight.remove(id);
        if frames.is_empty() {
            self.on_media_missing(id);
            return;
        }
        let images: Vec<slint::Image> = frames.iter().map(image_of).collect();
        let (w, h) = (frames[0].w as i32, frames[0].h as i32);
        self.remember_decoded(id, images[0].clone(), w, h);
        let path_str = path.to_string_lossy().into_owned();
        self.media_path.insert(id.to_string(), path_str.clone());
        let first = images[0].clone();
        self.patch_row(id, |row| {
            row.picture = first;
            row.picW = w;
            row.picH = h;
            row.mediaPath = path_str.into();
            row.mediaReady = true;
        });
        if images.len() > 1 {
            self.start_animation(id, images, true);
        }
        self.restick();
    }

    pub fn on_media_gif(&mut self, id: &str, path: &std::path::Path, frames: Vec<crate::media::Decoded>) {
        self.media_inflight.remove(id);
        if frames.is_empty() {
            self.on_media_missing(id);
            return;
        }
        let images: Vec<slint::Image> = frames.iter().map(image_of).collect();
        let (pic_w, pic_h) = bubble_fit(frames[0].w as i32, frames[0].h as i32);
        self.remember_decoded(id, images[0].clone(), pic_w, pic_h);
        let path_str = path.to_string_lossy().into_owned();
        self.media_path.insert(id.to_string(), path_str.clone());
        let first = images[0].clone();
        self.patch_row(id, |row| {
            row.picture = first;
            row.picW = pic_w;
            row.picH = pic_h;
            row.mediaPath = path_str.into();
            row.mediaReady = true;
        });
        if images.len() > 1 {
            // Like WhatsApp, a GIF plays once and rests on its first frame.
            self.start_animation(id, images, false);
        }
        self.restick();
    }

    pub fn on_thumb(&mut self, id: &str, link: bool, img: crate::media::Decoded) {
        let image = image_of(&img);
        let (w, h) = (img.w as i32, img.h as i32);
        if link {
            self.media_inflight.remove(id);
            self.patch_row(id, |row| {
                row.linkThumb = image;
                row.hasLinkThumb = true;
                row.linkThumbW = w;
                row.linkThumbH = h;
            });
        } else {
            // Video poster: the bubble keeps its box; ready comes with the
            // clip download.
            let (pic_w, pic_h) = bubble_fit(w, h);
            self.patch_row(id, |row| {
                row.picture = image.clone();
                row.picW = pic_w;
                row.picH = pic_h;
            });
            self.feed_viewer(id, &image);
        }
        self.restick();
    }

    fn start_animation(&mut self, id: &str, frames: Vec<slint::Image>, looping: bool) {
        self.drop_animation(id);
        self.anim_bytes += frames.iter().map(image_bytes).sum::<usize>();
        self.anim_order.push_back(id.to_string());
        self.animated.insert(id.to_string(), Anim { frames, idx: 0, looping });
        // A 320px GIF can be 45 frames of RGBA, so the count cap needs a
        // byte budget beside it.
        while self.anim_order.len() > MAX_ANIMATIONS
            || (self.anim_bytes > ANIM_CACHE_BYTES && self.anim_order.len() > 1)
        {
            let Some(oldest) = self.anim_order.pop_front() else { break };
            self.forget_animation(&oldest);
        }
        if !self.anim_timer.running() {
            self.anim_timer.start(
                slint::TimerMode::Repeated,
                Duration::from_millis(66),
                || apply_now(|b| b.tick_animations()),
            );
        }
    }

    // Removes an animation and its bytes, keeping the order queue in step.
    fn drop_animation(&mut self, id: &str) {
        if self.forget_animation(id) {
            self.anim_order.retain(|known| known != id);
        }
    }

    fn forget_animation(&mut self, id: &str) -> bool {
        let Some(anim) = self.animated.remove(id) else { return false };
        self.anim_bytes = self
            .anim_bytes
            .saturating_sub(anim.frames.iter().map(image_bytes).sum::<usize>());
        true
    }

    fn tick_animations(&mut self) {
        if self.animated.is_empty() {
            self.anim_timer.stop();
            return;
        }
        let mut finished = Vec::new();
        let mut patches: Vec<(String, slint::Image)> = Vec::new();
        for (id, anim) in self.animated.iter_mut() {
            if anim.looping {
                anim.idx = (anim.idx + 1) % anim.frames.len();
            } else if anim.idx + 1 < anim.frames.len() {
                anim.idx += 1;
            } else {
                finished.push(id.clone());
                patches.push((id.clone(), anim.frames[0].clone()));
                continue;
            }
            patches.push((id.clone(), anim.frames[anim.idx].clone()));
        }
        for id in finished {
            self.drop_animation(&id);
        }
        for (id, frame) in patches {
            self.patch_row(&id, |row| row.picture = frame);
        }
    }

    // Group senders and voice notes want their pictures; collected after
    // the rows are built (to_row itself is read-only).
    fn queue_row_avatars(&mut self, jid: &str) {
        let mut wanted: Vec<String> = Vec::new();
        for m in self.store.messages_for(jid) {
            if is_group(jid) && !m.from_me && !m.sender_jid.is_empty() {
                wanted.push(self.store.canon_owned(&m.sender_jid));
            }
            if m.kind == MessageKind::Audio {
                wanted.push(if m.from_me {
                    self.self_jid.clone()
                } else if m.sender_jid.is_empty() {
                    jid.to_string()
                } else {
                    self.store.canon_owned(&m.sender_jid)
                });
            }
        }
        wanted.sort();
        wanted.dedup();
        for jid in wanted {
            self.queue_avatar(&jid);
        }
    }

    // ---- status/stories ----

    fn refresh_statuses(&mut self) {
        let authors = self.store.status_authors();
        let rows: Vec<ChatItem> = authors
            .iter()
            .map(|(jid, latest, _count)| {
                self.to_chat_row(jid, "", latest.timestamp, 0, false)
            })
            .collect();
        self.status_model.set_vec(rows);
    }

    fn open_status_viewer(&mut self, jid: &str) {
        let Some(items) = self.store.statuses.get(jid).cloned() else { return };
        if items.is_empty() {
            return;
        }
        self.viewer = Some((items, 0));
        self.show_status();
    }

    fn step_status(&mut self, delta: i32) {
        let Some((items, idx)) = &mut self.viewer else { return };
        let next = *idx as i32 + delta;
        if next < 0 || next >= items.len() as i32 {
            self.viewer = None;
            self.ui.set_sv_open(false);
            return;
        }
        *idx = next as usize;
        self.show_status();
    }

    fn show_status(&mut self) {
        let Some((items, idx)) = &self.viewer else { return };
        let (count, idx_now) = (items.len(), *idx);
        let item = items[idx_now].clone();
        self.ui.set_sv_name(self.store.chat_name(&item.jid).into());
        self.ui.set_sv_time(format_time(item.timestamp).into());
        self.ui.set_sv_text(item.text.clone().into());
        self.ui.set_sv_index(idx_now as i32 + 1);
        self.ui.set_sv_count(count as i32);
        let has_image = item.kind == MessageKind::Image;
        self.ui.set_sv_has_image(false);
        if has_image {
            if let Some((img, _, _)) = self.decoded.get(&item.id) {
                self.ui.set_sv_image(img.clone());
                self.ui.set_sv_has_image(true);
            } else if let Some(raw) = item.raw.clone() {
                // Instant thumbnail while the full image downloads.
                use whatsapp_rust::proto_helpers::MessageExt as _;
                let inner = raw.get_base_message();
                let thumb = inner
                    .image_message
                    .as_option()
                    .and_then(|m| m.jpeg_thumbnail.as_ref())
                    .or_else(|| {
                        inner.video_message.as_option().and_then(|m| m.jpeg_thumbnail.as_ref())
                    })
                    .filter(|t| !t.is_empty());
                if let Some(thumb) = thumb {
                    self.wa.send(Cmd::DecodeThumb {
                        id: item.id.clone(),
                        bytes: thumb.to_vec(),
                        link: false,
                    });
                }
                if inner.image_message.is_set() && !self.media_inflight.contains(&item.id) {
                    self.media_inflight.insert(item.id.clone());
                    self.wa.send(Cmd::Media {
                        id: item.id.clone(),
                        mimetype: item.mimetype.clone(),
                        message: raw,
                        want: MediaWant::Image,
                    });
                }
            }
        }
        self.ui.set_sv_open(true);
    }

    // Puts a freshly decoded bitmap on the open viewer when it matches.
    fn feed_viewer(&mut self, id: &str, image: &slint::Image) {
        if let Some((items, idx)) = &self.viewer
            && items[*idx].id == id
        {
            self.ui.set_sv_image(image.clone());
            self.ui.set_sv_has_image(true);
        }
    }

    // ---- calls ----

    fn refresh_calls(&mut self) {
        let rows: Vec<CallItem> = self
            .store
            .calls
            .iter()
            .take(60)
            .map(|call| {
                let name = self.store.chat_name(&call.jid);
                let avatar = self.avatar_for(&call.jid);
                let label = t(match call.status.as_str() {
                    "accept" => "call.answered",
                    "reject" => "call.declined",
                    "timeout" => {
                        if call.outgoing { "call.noAnswer" } else { "call.missed" }
                    }
                    "offer" => "call.ringing",
                    "outgoing" => "call.calling",
                    _ => "call.ended",
                });
                let kind = t(if call.video { "call.video" } else { "call.voice" });
                let arrow = t(if call.outgoing { "call.out" } else { "call.in" });
                CallItem {
                    id: call.id.clone().into(),
                    from: call.jid.clone().into(),
                    name: name.clone().into(),
                    detail: format!("{arrow} · {kind} · {label}").into(),
                    time: format_time(call.timestamp).into(),
                    hasAvatar: avatar.is_some(),
                    avatar: avatar.unwrap_or_else(empty_image),
                    initial: initial_of(&name).into(),
                    colorIdx: color_idx_of(&call.jid),
                    video: call.video,
                    group: call.group,
                }
            })
            .collect();
        self.calls_model.set_vec(rows);
    }

    // ---- the call screen ----

    // Created the first time a call needs it, then reused: building a
    // window is expensive and the callbacks only get wired once.
    fn call_window(&mut self) -> Option<CallWindow> {
        if let Some(win) = &self.call_ui {
            return Some(win.clone_strong());
        }
        let win = match CallWindow::new() {
            Ok(win) => win,
            Err(e) => {
                eprintln!("[call] cannot open the call window: {e}");
                return None;
            }
        };
        win.on_accept(|| defer(|b| b.accept_call()));
        win.on_decline(|| defer(|b| b.decline_call()));
        win.on_hangup(|| defer(|b| b.hangup_call()));
        win.on_toggle_mute(|| defer(|b| b.toggle_call_mute()));
        win.on_toggle_video(|| defer(|b| b.toggle_call_video()));
        // Closing the window means the same as decline (or hang up, once
        // the call is up), matching WhatsApp's own call window.
        win.window().on_close_requested(|| {
            defer(|b| b.decline_call());
            slint::CloseRequestResponse::HideWindow
        });
        self.call_ui = Some(win.clone_strong());
        Some(win)
    }

    // Pushes the current call onto the screen. Safe to call repeatedly;
    // every state change goes through here.
    fn paint_call(&mut self) {
        let Some(call) = self.call.as_ref() else { return };
        let (jid, state, video) = (call.jid.clone(), call.state, call.video);
        let name = self.store.chat_name(&jid);
        let avatar = self.avatar_for(&jid);
        let detail = t(match state {
            "incoming" => {
                if video {
                    "call.incomingVideo"
                } else {
                    "call.incomingVoice"
                }
            }
            "outgoing" => "call.calling",
            "connecting" => "call.connecting",
            "active" => "call.inCall",
            _ => "call.ended",
        });
        self.queue_avatar(&jid);
        let dark = self.ui.get_dark_theme();
        let Some(win) = self.call_window() else { return };
        // The call window carries its own copy of the theme global.
        win.set_dark_theme(dark);
        win.set_state(state.into());
        win.set_peer_name(name.clone().into());
        win.set_detail(detail.into());
        win.set_has_avatar(avatar.is_some());
        win.set_avatar(avatar.unwrap_or_else(empty_image));
        win.set_initial(initial_of(&name).into());
        win.set_color_idx(color_idx_of(&jid));
        let Some(call) = self.call.as_ref() else { return };
        // A voice call is a portrait card; video needs room for a
        // landscape picture, the way WhatsApp resizes its call window.
        let video = call.video || call.sending_video || call.peer_video;
        let wanted = if video {
            slint::LogicalSize::new(760.0, 560.0)
        } else {
            slint::LogicalSize::new(360.0, 540.0)
        };
        if win.window().size().to_logical(win.window().scale_factor()) != wanted {
            win.window().set_size(wanted);
        }
        win.set_video_call(video);
        win.set_video_on(call.sending_video);
        win.set_peer_video(call.peer_video);
        win.set_video_wanted(call.peer_wants_video);
    }

    // ---- video ----

    fn toggle_call_video(&mut self) {
        let Some(call) = self.call.as_mut() else { return };
        if call.state != "active" {
            return;
        }
        let wanted = !call.sending_video;
        // The camera is only really on once the call side says so; this
        // keeps the button from flapping if it fails to open.
        call.peer_wants_video = false;
        self.wa.send(Cmd::SetCallVideo(wanted));
    }

    // Our own camera came on or went off.
    pub fn on_call_video(&mut self, id: &str, on: bool) {
        let Some(call) = self.call.as_mut() else { return };
        if call.id != id {
            return;
        }
        call.sending_video = on;
        if !on {
            self.clear_local_preview();
        }
        self.paint_call();
    }

    // The peer's camera came on or went off.
    pub fn on_call_peer_video(&mut self, id: &str, on: bool) {
        let Some(call) = self.call.as_mut() else { return };
        if call.id != id {
            return;
        }
        call.peer_video = on;
        if !on {
            self.clear_remote_video();
        }
        self.paint_call();
    }

    // The peer asked to add video. The camera stays off until the user
    // agrees by pressing the button.
    pub fn on_call_video_requested(&mut self, id: &str) {
        let Some(call) = self.call.as_mut() else { return };
        if call.id != id {
            return;
        }
        call.peer_wants_video = true;
        self.paint_call();
    }

    // One frame of the local self-view.
    pub fn on_call_preview(&mut self, picture: crate::media::Decoded) {
        if let Some(win) = self.call_ui.as_ref()
            && self.call.as_ref().is_some_and(|call| call.sending_video)
        {
            win.set_local_video(image_of(&picture));
            win.set_has_local_video(true);
        }
        // The capture thread waits on this before sending the next one.
        crate::camera::PREVIEW_BUSY.store(false, std::sync::atomic::Ordering::Release);
    }

    // One decoded frame from the peer.
    pub fn on_call_remote_video(&mut self, picture: crate::media::Decoded) {
        if let Some(win) = self.call_ui.as_ref()
            && self.call.is_some()
        {
            win.set_remote_video(image_of(&picture));
            win.set_has_remote_video(true);
        }
        crate::camera::REMOTE_BUSY.store(false, std::sync::atomic::Ordering::Release);
    }

    fn clear_local_preview(&mut self) {
        // A frame dropped mid-flight would otherwise leave the gate shut
        // and the next call's self-view blank.
        crate::camera::PREVIEW_BUSY.store(false, std::sync::atomic::Ordering::Release);
        if let Some(win) = self.call_ui.as_ref() {
            win.set_has_local_video(false);
            win.set_local_video(empty_image());
        }
    }

    fn clear_remote_video(&mut self) {
        crate::camera::REMOTE_BUSY.store(false, std::sync::atomic::Ordering::Release);
        if let Some(win) = self.call_ui.as_ref() {
            win.set_has_remote_video(false);
            win.set_remote_video(empty_image());
        }
    }

    fn show_call_window(&mut self) {
        let Some(win) = self.call_window() else { return };
        win.set_muted(false);
        win.set_timer("".into());
        if let Err(e) = win.show() {
            eprintln!("[call] cannot show the call window: {e}");
            return;
        }
        crate::platform::focus_call_window();
    }

    // Dropping the ringer stops the stream; None means silence.
    fn set_ring(&mut self, kind: Option<crate::call::Ring>) {
        self.ringer = None;
        if let Some(kind) = kind {
            self.ringer = crate::call::Ringer::start(kind);
        }
    }

    fn tick_call(&mut self) {
        let Some(started) = self.call.as_ref().and_then(|call| call.started) else {
            self.call_timer.stop();
            return;
        };
        let secs = started.elapsed().as_secs();
        let text = if secs >= 3600 {
            format!("{}:{:02}:{:02}", secs / 3600, (secs / 60) % 60, secs % 60)
        } else {
            format!("{}:{:02}", secs / 60, secs % 60)
        };
        if let Some(win) = self.call_ui.as_ref() {
            win.set_timer(text.into());
        }
    }

    // The peer picked up (or we did): start counting.
    fn mark_call_active(&mut self) {
        self.set_ring(None);
        let Some(call) = self.call.as_mut() else { return };
        if call.state == "active" {
            return;
        }
        call.state = "active";
        call.started = Some(Instant::now());
        self.paint_call();
        self.tick_call();
        self.call_timer.start(slint::TimerMode::Repeated, Duration::from_secs(1), || {
            apply_now(|b| b.tick_call())
        });
    }

    // Leaves the reason on screen for a moment, the way the phone does,
    // then puts the window away.
    fn end_call_ui(&mut self, reason: &str) {
        self.set_ring(None);
        self.call_timer.stop();
        let reason = t(reason);
        let Some(call) = self.call.as_mut() else { return };
        call.state = "ended";
        call.started = None;
        if let Some(win) = self.call_ui.as_ref() {
            win.set_state("ended".into());
            win.set_timer("".into());
            win.set_detail(reason.into());
            win.set_muted(false);
        }
        self.once(CALL_LINGER_MS, |b| b.close_call_window());
    }

    fn close_call_window(&mut self) {
        // A new call may have started while the ended screen lingered.
        if self.call.as_ref().is_some_and(|call| call.state != "ended") {
            return;
        }
        self.call = None;
        self.set_ring(None);
        self.call_timer.stop();
        // Video frames are megabytes each; do not leave one parked in a
        // hidden window for the rest of the session.
        self.clear_local_preview();
        self.clear_remote_video();
        if let Some(win) = self.call_ui.as_ref() {
            let _ = win.hide();
        }
    }

    // ---- devices ----

    // Fills the pickers and applies the saved choices. Enumeration talks
    // to the audio and camera stacks, so it happens when Settings opens
    // rather than on every launch.
    pub fn refresh_devices(&mut self) {
        let system = t("device.system");
        let listed = |mut names: Vec<String>| {
            names.insert(0, system.clone());
            let rows: Vec<SharedString> = names.into_iter().map(SharedString::from).collect();
            ModelRc::from(Rc::new(VecModel::from(rows)))
        };
        self.ui.set_mic_devices(listed(crate::call::input_names()));
        self.ui.set_speaker_devices(listed(crate::call::output_names()));
        self.ui
            .set_camera_devices(listed(crate::camera::cameras().into_iter().map(|c| c.name).collect()));
        let chosen = crate::call::devices();
        let shown = |name: &str| {
            if name.is_empty() { system.clone().into() } else { SharedString::from(name) }
        };
        self.ui.set_mic_device(shown(&chosen.mic));
        self.ui.set_speaker_device(shown(&chosen.speaker));
        self.ui.set_camera_device(shown(&chosen.camera));
    }

    pub fn set_device(&mut self, kind: &str, name: &str) {
        // The first entry in every picker means "let the system decide",
        // which is stored as an empty name.
        let name = if name == t("device.system") { "" } else { name };
        let mut chosen = crate::call::devices();
        match kind {
            "mic" => chosen.mic = name.to_string(),
            "speaker" => chosen.speaker = name.to_string(),
            "camera" => chosen.camera = name.to_string(),
            _ => return,
        }
        if let Some(vault) = &self.vault {
            vault.setting_set(&format!("device.{kind}"), name);
        }
        crate::call::set_devices(chosen);
    }

    // Restores the saved devices at boot, before any call can start.
    fn load_devices(&mut self) {
        let Some(vault) = &self.vault else { return };
        let get = |key: &str| vault.setting_get(key).unwrap_or_default();
        crate::call::set_devices(crate::call::Devices {
            mic: get("device.mic"),
            speaker: get("device.speaker"),
            camera: get("device.camera"),
        });
    }

    // ---- call actions ----

    pub fn start_call(&mut self, jid: &str) {
        self.place_call(jid, false);
    }

    pub fn start_video_call(&mut self, jid: &str) {
        self.place_call(jid, true);
    }

    fn place_call(&mut self, jid: &str, video: bool) {
        let jid = self.store.canon_owned(&normalize_jid(jid));
        if is_group(&jid) || is_channel(&jid) || jid == "status@broadcast" {
            return;
        }
        if self.call.as_ref().is_some_and(|call| call.state != "ended") {
            return;
        }
        self.call = Some(ActiveCall {
            id: String::new(),
            jid: jid.clone(),
            from: jid.clone(),
            outgoing: true,
            video,
            sending_video: video,
            peer_video: false,
            peer_wants_video: false,
            state: "outgoing",
            started: None,
        });
        self.paint_call();
        self.show_call_window();
        self.set_ring(Some(crate::call::Ring::Back));
        self.wa.send(Cmd::StartCall { jid, video });
    }

    fn accept_call(&mut self) {
        let Some(call) = self.call.as_mut() else { return };
        if call.state != "incoming" {
            return;
        }
        call.state = "connecting";
        // A video offer is answered with the camera on, the way the phone
        // does it; the button in the call window turns it back off.
        let video = call.video;
        call.sending_video = video;
        let id = call.id.clone();
        self.set_ring(None);
        self.paint_call();
        self.wa.send(Cmd::AcceptCall { id, video });
    }

    // Doubles as the hang-up for a call already up, so the window's close
    // button always does the right thing.
    fn decline_call(&mut self) {
        let Some(call) = self.call.as_ref() else {
            self.close_call_window();
            return;
        };
        match call.state {
            "incoming" => {
                let (id, from) = (call.id.clone(), call.from.clone());
                self.wa.send(Cmd::RejectCall { id: id.clone(), from: from.clone() });
                self.record_call(&id, &from, "reject", false);
                self.end_call_ui("call.declined");
            }
            "ended" => self.close_call_window(),
            _ => self.hangup_call(),
        }
    }

    fn hangup_call(&mut self) {
        if self.call.is_none() {
            return;
        }
        self.wa.send(Cmd::HangupCall);
        self.end_call_ui("call.ended");
    }

    fn toggle_call_mute(&mut self) {
        let Some(win) = self.call_ui.as_ref() else { return };
        let muted = !win.get_muted();
        win.set_muted(muted);
        self.wa.send(Cmd::SetCallMuted(muted));
    }

    // ---- call events ----

    fn record_call(&mut self, id: &str, from: &str, status: &str, outgoing: bool) {
        if id.is_empty() {
            return;
        }
        let known = self.store.calls.iter().find(|c| c.id == id).cloned();
        let (video, group, outgoing) = match &known {
            Some(entry) => (entry.video, entry.group, entry.outgoing),
            None => (false, false, outgoing),
        };
        // A finished call stays "answered" in the log; the terminate that
        // follows it is the hang-up, not a separate outcome.
        if known.as_ref().is_some_and(|entry| entry.status == "accept") && status == "terminate" {
            return;
        }
        let from = known.map(|entry| entry.jid).unwrap_or_else(|| from.to_string());
        if from.is_empty() {
            return;
        }
        if self.store.upsert_call(id, &from, status, video, group, 0, outgoing).is_some() {
            self.refresh_calls();
            self.schedule_save();
        }
    }

    // Files a call the log has not seen before, with everything the
    // stanza carried. Later outcomes go through record_call, which keeps
    // what is already known.
    fn record_call_new(
        &mut self,
        id: &str,
        from: &str,
        status: &str,
        video: bool,
        group: bool,
        ts: i64,
    ) {
        if self.store.upsert_call(id, from, status, video, group, ts, false).is_some() {
            self.refresh_calls();
            self.schedule_save();
        }
    }

    pub fn on_incoming_call(&mut self, id: &str, from: &str, video: bool, group: bool, ts: i64) {
        // Offers replayed from the offline queue, and offers that waited
        // in the locked-vault backlog while the PIN screen was up, arrive
        // long after they stopped ringing. Log them, never announce them.
        let stale = ts > 0 && now_secs() - ts > STALE_CALL_SECS;
        if stale {
            // A replay of a call already in the log tells us nothing new;
            // recording it again would only overwrite its real outcome.
            if !self.store.calls.iter().any(|c| c.id == id) {
                self.record_call_new(id, from, "timeout", video, group, ts);
            }
            return;
        }
        self.record_call_new(id, from, "offer", video, group, ts);
        // Already on a call: turn this one down rather than take the
        // screen away from the call in progress.
        if self.call.as_ref().is_some_and(|call| call.state != "ended") {
            self.wa.send(Cmd::RejectCall { id: id.to_string(), from: from.to_string() });
            self.record_call(id, from, "reject", false);
            return;
        }
        let jid = self.store.canon_owned(&normalize_jid(from));
        self.call = Some(ActiveCall {
            id: id.to_string(),
            jid: jid.clone(),
            from: from.to_string(),
            outgoing: false,
            video,
            sending_video: false,
            peer_video: video,
            peer_wants_video: false,
            state: "incoming",
            started: None,
        });
        self.paint_call();
        self.show_call_window();
        self.set_ring(Some(crate::call::Ring::In));
        let name = self.store.chat_name(&jid);
        let detail = t(if video { "call.incomingVideo" } else { "call.incomingVoice" });
        crate::platform::toast(&name, &detail, None);
        // Nobody home: stop ringing and file it as missed.
        let id = id.to_string();
        self.once(RING_TIMEOUT_MS, move |b| b.ring_timeout(&id));
    }

    fn ring_timeout(&mut self, id: &str) {
        if !self.call.as_ref().is_some_and(|call| call.state == "incoming" && call.id == id) {
            return;
        }
        self.record_call(id, "", "timeout", false);
        self.end_call_ui("call.missed");
    }

    // A call that must not ring: an offline-queue replay, or one the peer
    // gave up on before we ever saw the offer.
    pub fn on_call_missed(&mut self, id: &str, from: &str, ts: i64) {
        match self.store.calls.iter().find(|c| c.id == id) {
            // Only a call still shown as ringing becomes a missed one; an
            // outcome already recorded stands.
            Some(entry) if entry.status != "offer" => {}
            Some(_) => self.record_call(id, from, "timeout", false),
            None => self.record_call_new(id, from, "timeout", false, false, ts),
        }
        if self.call.as_ref().is_some_and(|call| call.id == id) {
            self.end_call_ui("call.missed");
        }
    }

    pub fn on_call_status(&mut self, id: &str, status: &str) {
        self.record_call(id, "", status, false);
        // An outgoing call has no id until the server mints one, so an
        // empty id matches whatever is on screen.
        let Some(call) = self.call.as_ref() else { return };
        if !call.id.is_empty() && call.id != id {
            return;
        }
        // The screen is already showing the outcome; nothing left to do.
        if call.state == "ended" {
            return;
        }
        match (status, call.state) {
            // Still ringing here, so the accept came from another of our
            // devices: this one stops.
            ("accept", "incoming") => self.end_call_ui("call.elsewhere"),
            ("accept", _) => self.mark_call_active(),
            // Peer-device "busy" rejects never get here, so a reject is
            // always someone turning the call down.
            ("reject", _) => self.end_call_ui("call.declined"),
            (_, "incoming") => self.end_call_ui("call.missed"),
            _ => self.end_call_ui("call.ended"),
        }
    }

    // The outgoing call reached the server; from here on it has an id.
    pub fn on_outgoing_call(&mut self, id: &str) {
        let jid = match self.call.as_mut() {
            Some(call) if call.outgoing && call.id.is_empty() => {
                call.id = id.to_string();
                call.jid.clone()
            }
            _ => return,
        };
        if self.store.upsert_call(id, &jid, "outgoing", false, false, 0, true).is_some() {
            self.refresh_calls();
            self.schedule_save();
        }
    }

    // The relay is up. For the callee that is the whole handshake done;
    // the caller still waits for the peer to pick up.
    pub fn on_call_media_ready(&mut self, id: &str) {
        if self.call.as_ref().is_some_and(|call| call.id == id && call.state == "connecting") {
            self.mark_call_active();
        }
    }

    pub fn on_call_failed(&mut self, reason: &str) {
        self.set_ring(None);
        self.call_timer.stop();
        let Some(call) = self.call.as_mut() else { return };
        call.state = "ended";
        call.started = None;
        if let Some(win) = self.call_ui.as_ref() {
            win.set_state("ended".into());
            win.set_timer("".into());
            win.set_detail(reason.into());
        }
        self.once(CALL_LINGER_MS * 2, |b| b.close_call_window());
    }

    // ---- channels & info panel ----

    // Channels arrive as jids only; ask the server for their names.
    fn resolve_channel_names(&mut self) {
        let wanted: Vec<String> = self
            .store
            .sorted_chats()
            .into_iter()
            .filter(|meta| is_channel(&meta.jid) && meta.name.is_empty())
            .map(|meta| meta.jid.clone())
            .filter(|jid| !self.requested_channels.contains(jid))
            .collect();
        for jid in wanted {
            self.requested_channels.insert(jid.clone());
            self.wa.send(Cmd::FetchChannel(jid));
        }
    }

    pub fn on_channel_meta(&mut self, jid: &str, name: &str, avatar: Option<crate::media::Decoded>) {
        if !name.is_empty() {
            self.store.set_name(jid, name);
        }
        if let Some(decoded) = avatar {
            let image = image_of(&decoded);
            self.avatars.insert(jid.to_string(), Some(image.clone()));
            self.patch_avatar_everywhere(jid, &image);
        }
        self.schedule_refresh_chats();
    }

    fn open_contact_info(&mut self) {
        let Some(jid) = self.current_jid.clone() else { return };
        let group = is_group(&jid);
        let name = self.store.chat_name(&jid);
        let avatar = self.avatar_for(&jid);
        let archived = self.store.chats.get(&jid).map(|c| c.archived).unwrap_or(false);
        self.ui.set_info_name(name.clone().into());
        self.ui.set_info_id(display_id(&jid).into());
        self.ui.set_info_about("".into());
        self.ui.set_info_desc("".into());
        self.ui.set_info_is_group(group);
        self.ui.set_info_members("".into());
        self.ui.set_info_archived(archived);
        self.ui.set_info_has_avatar(avatar.is_some());
        self.ui.set_info_avatar(avatar.unwrap_or_else(empty_image));
        self.ui.set_info_initial(initial_of(&name).into());
        self.ui.set_info_color_idx(color_idx_of(&jid));
        // Shared media: the last 12 pictures already decoded.
        let cells: Vec<StickerCell> = self
            .store
            .messages_for(&jid)
            .iter()
            .rev()
            .filter(|m| m.kind == MessageKind::Image && !m.sticker)
            .filter_map(|m| {
                self.decoded.get(&m.id).map(|(img, _, _)| StickerCell {
                    id: m.id.clone().into(),
                    pic: img.clone(),
                    ready: true,
                })
            })
            .take(12)
            .collect();
        let grid: Vec<ModelRc<StickerCell>> = cells
            .chunks(4)
            .map(|chunk| ModelRc::from(Rc::new(VecModel::from(chunk.to_vec()))))
            .collect();
        self.info_media_model.set_vec(grid);
        self.ui.set_info_open(true);
        self.wa.send(Cmd::FetchChatInfo { jid, group });
    }

    pub fn on_chat_info(&mut self, jid: &str, about: &str, desc: &str, members: usize) {
        if Some(&jid.to_string()) != self.current_jid.as_ref() || !self.ui.get_info_open() {
            return;
        }
        self.ui.set_info_about(about.into());
        self.ui.set_info_desc(desc.into());
        if members > 0 {
            self.ui.set_info_members(ta("info.members", &[&members.to_string()]).into());
        }
    }

    fn toggle_archive(&mut self) {
        let Some(jid) = self.current_jid.clone() else { return };
        let next = !self.store.chats.get(&jid).map(|c| c.archived).unwrap_or(false);
        if let Some(meta) = self.store.chats.get_mut(&jid) {
            meta.archived = next;
        }
        self.ui.set_info_archived(next);
        self.wa.send(Cmd::Archive { jid, archived: next });
        self.schedule_refresh_chats();
    }

    fn clear_current_chat(&mut self) {
        let Some(jid) = self.current_jid.clone() else { return };
        self.store.messages.remove(&jid);
        if let Some(meta) = self.store.chats.get_mut(&jid) {
            meta.preview = String::new();
        }
        if let Some(vault) = &self.vault {
            vault.del(&format!("store:msgs:{jid}"));
        }
        self.messages_model.set_vec(Vec::new());
        self.ui.set_info_open(false);
        self.schedule_refresh_chats();
    }

    // ---- sticker & GIF pickers ----

    fn sticker_grid(&self, items: Vec<&StoredMessage>) -> Vec<ModelRc<StickerCell>> {
        let cells: Vec<StickerCell> = items
            .iter()
            .map(|m| {
                let ready = self.decoded.contains_key(&m.id);
                StickerCell {
                    id: m.id.clone().into(),
                    pic: self
                        .decoded
                        .get(&m.id)
                        .map(|(img, _, _)| img.clone())
                        .unwrap_or_else(empty_image),
                    ready,
                }
            })
            .collect();
        cells.chunks(4).map(|c| ModelRc::from(Rc::new(VecModel::from(c.to_vec())))).collect()
    }

    // Fills the sticker tab with recent/starred stickers (lazy decode).
    fn load_sticker_panel(&mut self) {
        // Sticker/GIF history scans hydrated chats only; wake the most
        // recent ones without disturbing the warm LRU.
        if let Some(vault) = self.vault.as_ref() {
            let recent: Vec<String> = {
                let sorted = self.store.sorted_chats();
                sorted.iter().take(15).map(|m| m.jid.clone()).collect()
            };
            for jid in recent {
                self.store.hydrate(vault, &jid);
            }
        }
        self.wa.send(Cmd::GifSearch(String::new()));
        let favs = self.store.starred_stickers(32);
        self.ui
            .set_fav_hint(if favs.is_empty() { t("fav.empty").into() } else { "".into() });
        let fav_pending: Vec<StoredMessage> =
            favs.iter().filter(|m| !self.decoded.contains_key(&m.id)).map(|m| (*m).clone()).collect();
        let fav_grid = self.sticker_grid(favs);
        self.fav_model.set_vec(fav_grid);
        let recents = self.store.recent_stickers(32);
        let recent_pending: Vec<StoredMessage> = recents
            .iter()
            .filter(|m| !self.decoded.contains_key(&m.id))
            .map(|m| (*m).clone())
            .collect();
        let recent_grid = self.sticker_grid(recents);
        self.sticker_model.set_vec(recent_grid);
        for m in fav_pending.into_iter().chain(recent_pending) {
            self.request_media(&m);
        }
    }

    fn send_sticker_by_id(&mut self, id: &str) {
        let Some(jid) = self.current_jid.clone() else { return };
        // Panel stickers are past messages: forwarding reuses their CDN copy.
        let raw = self
            .store
            .messages
            .values()
            .flatten()
            .find(|m| m.id == id && m.sticker)
            .and_then(|m| m.raw.clone());
        if let Some(message) = raw {
            self.ui.set_picker_open(false);
            self.wa.send(Cmd::Forward { jid, message });
        }
    }

    pub fn on_gif_results(&mut self, results: Vec<(String, String, crate::media::Decoded)>) {
        self.gif_url_by_id.clear();
        if results.is_empty() {
            self.ui.set_gif_hint(t("gif.noResults").into());
            self.gif_model.set_vec(Vec::new());
            return;
        }
        let cells: Vec<StickerCell> = results
            .iter()
            .map(|(id, url, img)| {
                self.gif_url_by_id.insert(id.clone(), url.clone());
                StickerCell { id: id.clone().into(), pic: image_of(img), ready: true }
            })
            .collect();
        let grid: Vec<ModelRc<StickerCell>> =
            cells.chunks(4).map(|c| ModelRc::from(Rc::new(VecModel::from(c.to_vec())))).collect();
        self.gif_model.set_vec(grid);
        self.ui.set_gif_hint("".into());
    }

    fn send_gif_by_id(&mut self, id: &str) {
        let Some(jid) = self.current_jid.clone() else { return };
        self.ui.set_picker_open(false);
        if let Some(url) = self.gif_url_by_id.get(id).cloned() {
            self.wa.send(Cmd::SendGifUrl { jid, url });
            return;
        }
        // A history GIF: forward the original message.
        let raw = self
            .store
            .messages
            .values()
            .flatten()
            .find(|m| m.id == id && m.gif)
            .and_then(|m| m.raw.clone());
        if let Some(message) = raw {
            self.wa.send(Cmd::Forward { jid, message });
        }
    }

    // ---- video overlay (GIF zoom and click-to-play) ----

    fn open_video(&mut self, id: &str) {
        let Some(m) = self.find_message(id).cloned() else { return };
        let Some(path) = self.media_path.get(id).map(std::path::PathBuf::from) else { return };
        self.close_video();
        self.video_id = Some(id.to_string());
        // The bubble's frames show instantly while the sharper set loads.
        if m.gif {
            if let Some(anim) = self.animated.get(id) {
                self.zoom_frames = anim.frames.clone();
            } else if let Some((img, _, _)) = self.decoded.get(id) {
                self.zoom_frames = vec![img.clone()];
            }
            self.start_zoom_loop();
            self.ui.set_video_w(m.media_w.max(320) as i32);
            self.ui.set_video_h(m.media_h.max(240) as i32);
            self.ui.set_video_open(true);
            self.wa.send(Cmd::ZoomFrames { id: id.to_string(), path });
            return;
        }
        self.ui.set_video_w(m.media_w.max(320) as i32);
        self.ui.set_video_h(m.media_h.max(240) as i32);
        self.ui.set_video_open(true);
        self.wa.send(Cmd::PlayVideo { id: id.to_string(), path });
    }

    fn close_video(&mut self) {
        self.wa.send(Cmd::StopVideo);
        self.zoom_timer.stop();
        self.zoom_frames.clear();
        self.zoom_idx = 0;
        self.video_audio = None;
        self.video_id = None;
        self.ui.set_video_open(false);
    }

    fn start_zoom_loop(&mut self) {
        if let Some(first) = self.zoom_frames.first() {
            self.ui.set_video_frame(first.clone());
        }
        if self.zoom_frames.len() < 2 {
            return;
        }
        self.zoom_timer.start(slint::TimerMode::Repeated, Duration::from_millis(66), || {
            apply_now(|b| {
                if b.zoom_frames.is_empty() {
                    b.zoom_timer.stop();
                    return;
                }
                b.zoom_idx = (b.zoom_idx + 1) % b.zoom_frames.len();
                b.ui.set_video_frame(b.zoom_frames[b.zoom_idx].clone());
            });
        });
    }

    pub fn on_zoom_frames(&mut self, id: &str, frames: Vec<crate::media::Decoded>) {
        if self.video_id.as_deref() != Some(id) || frames.is_empty() {
            return;
        }
        let (w, h) = (frames[0].w as i32, frames[0].h as i32);
        self.zoom_frames = frames.iter().map(image_of).collect();
        self.zoom_idx = 0;
        self.ui.set_video_w(w);
        self.ui.set_video_h(h);
        self.start_zoom_loop();
    }

    pub fn on_video_audio(&mut self, id: &str, buffer: crate::audio::AudioBuffer) {
        if self.video_id.as_deref() == Some(id) {
            self.video_audio = crate::audio::Player::start(&buffer, 0.0);
        }
    }

    pub fn on_video_frame(&mut self, id: &str, frame: crate::media::Decoded) {
        if self.video_id.as_deref() != Some(id) {
            return;
        }
        self.ui.set_video_w(frame.w as i32);
        self.ui.set_video_h(frame.h as i32);
        self.ui.set_video_frame(image_of(&frame));
    }

    pub fn on_video_ended(&mut self, id: &str) {
        if self.video_id.as_deref() == Some(id) {
            self.close_video();
        }
    }

    // ---- self-update ----

    fn update_tick(&mut self) {
        self.wa.send(Cmd::CheckUpdate);
        // Check again every 6 hours while the app stays open.
        self.once(21_600_000, |b| b.update_tick());
    }

    pub fn on_update_available(&mut self, version: &str) {
        self.ui.set_update_version(version.into());
        if self.ui.get_update_state() == 0 {
            self.ui.set_update_state(1);
        }
    }

    pub fn on_update_applied(&mut self, result: Result<String, String>) {
        match result {
            Ok(version) => {
                self.ui.set_update_version(version.into());
                self.ui.set_update_state(3);
            }
            Err(e) => {
                eprintln!("[update] failed: {e}");
                self.ui.set_update_state(1);
            }
        }
    }

    // ---- notifications ----

    fn push_notification(&mut self, title: String, text: String, jid: String) {
        self.notify_queue.push((title, text, jid));
        if !self.notify_queued {
            self.notify_queued = true;
            self.once(1200, |b| b.flush_notifications());
        }
    }

    fn flush_notifications(&mut self) {
        self.notify_queued = false;
        let queue = std::mem::take(&mut self.notify_queue);
        match queue.len() {
            0 => {}
            1 => {
                let (title, text, jid) = queue.into_iter().next().expect("one entry");
                crate::platform::toast(&title, &text, Some(jid));
            }
            n => {
                // A burst becomes one summary, targeting the latest chat.
                let mut names: Vec<&str> = Vec::new();
                for (title, _, _) in &queue {
                    if !names.contains(&title.as_str()) {
                        names.push(title);
                        if names.len() == 3 {
                            break;
                        }
                    }
                }
                let text = if names.len() == 1 {
                    ta("notify.fromOne", &[&n.to_string(), names[0]])
                } else {
                    ta("notify.newMessages", &[&n.to_string(), &names.join(", ")])
                };
                let jid = queue.last().map(|(_, _, jid)| jid.clone());
                crate::platform::toast(&t("notify.appName"), &text, jid);
            }
        }
    }

    pub fn on_notification_activated(&mut self, jid: &str) {
        let _ = self.ui.show();
        crate::platform::focus_window();
        // Locked: just surface the PIN screen; opening a chat would touch
        // the (still empty) store.
        if self.vault.as_ref().is_none_or(|v| v.locked()) {
            return;
        }
        self.open_dm(jid, None);
    }

    // ---- PIN ----

    fn handle_save_pin(&mut self, current: &str, next: &str) {
        let Some(vault) = &mut self.vault else { return };
        let status = match vault.change_pin(current, Some(next)) {
            Ok(()) => {
                self.ui.set_pin_set(true);
                t("pin.saved")
            }
            Err(crate::vault::PinError::WrongPin) => t("pin.wrongCurrent"),
            Err(crate::vault::PinError::BadFormat) => t("pin.format"),
        };
        self.ui.set_settings_status(status.into());
    }

    fn handle_remove_pin(&mut self, current: &str) {
        let Some(vault) = &mut self.vault else { return };
        let status = match vault.change_pin(current, None) {
            Ok(()) => {
                self.ui.set_pin_set(false);
                t("pin.removed")
            }
            Err(_) => t("pin.wrongCurrent"),
        };
        self.ui.set_settings_status(status.into());
    }

    // ---- message actions ----

    // The participant that goes into a message key inside a group.
    fn key_participant(&self, m: &StoredMessage) -> Option<String> {
        if !is_group(&m.jid) {
            return None;
        }
        Some(if m.from_me { self.self_jid.clone() } else { m.sender_jid.clone() })
            .filter(|p| !p.is_empty())
    }

    fn send_reaction(&mut self, id: &str, emoji: &str) {
        let Some(m) = self.find_message(id).cloned() else { return };
        // Reacting again with the same emoji removes it, like WhatsApp.
        let mine = m.reactions.get("me").cloned().unwrap_or_default();
        let next = if clean_text(&mine) == clean_text(emoji) { "" } else { emoji };
        self.wa.send(Cmd::React {
            jid: m.jid.clone(),
            id: id.to_string(),
            from_me: m.from_me,
            participant: self.key_participant(&m),
            emoji: next.to_string(),
        });
        if self.store.apply_reaction(&m.jid, id, "me", next) {
            let summary = self
                .store
                .messages_for(&m.jid)
                .iter()
                .find(|x| x.id == id)
                .map(reaction_summary)
                .unwrap_or_default();
            self.patch_row(id, |row| row.reactions = summary.into());
        }
        self.schedule_save();
    }

    // Lists who reacted to a message, like WhatsApp's reaction sheet.
    fn show_reactions(&mut self, id: &str) {
        let Some(m) = self.find_message(id) else { return };
        let entries: Vec<(String, String)> = m
            .reactions
            .iter()
            .filter(|(_, e)| !e.is_empty())
            .map(|(r, e)| (r.clone(), e.clone()))
            .collect();
        if entries.is_empty() {
            return;
        }
        let rows: Vec<ReactionItem> = entries
            .iter()
            .map(|(reactor, emoji)| {
                let who = if reactor == "me" {
                    self.self_jid.clone()
                } else {
                    self.store.canon_owned(&normalize_jid(reactor))
                };
                let name =
                    if reactor == "me" { t("reactions.you") } else { self.store.chat_name(&who) };
                let name = if name.is_empty() { display_id(&who) } else { name };
                let avatar = self.avatar_for(&who);
                ReactionItem {
                    jid: who.clone().into(),
                    name: name.clone().into(),
                    emoji: clean_text(emoji).into(),
                    hasAvatar: avatar.is_some(),
                    avatar: avatar.unwrap_or_else(empty_image),
                    initial: initial_of(&name).into(),
                    colorIdx: color_idx_of(&who),
                }
            })
            .collect();
        for (reactor, _) in &entries {
            if reactor != "me" {
                let who = self.store.canon_owned(&normalize_jid(reactor));
                self.queue_avatar(&who);
            }
        }
        let count = rows.len();
        self.reactions_model.set_vec(rows);
        self.ui.set_reactions_title(ta("reactions.count", &[&count.to_string()]).into());
        self.ui.set_reactions_open(true);
    }

    fn toggle_star(&mut self, id: &str) {
        let Some(m) = self.find_message(id).cloned() else { return };
        let next = !m.starred;
        self.wa.send(Cmd::Star {
            jid: m.jid.clone(),
            id: id.to_string(),
            from_me: m.from_me,
            participant: self.key_participant(&m),
            starred: next,
        });
        if self.store.set_starred(&m.jid, id, next) {
            self.patch_row(id, |row| row.starred = next);
        }
    }

    fn delete_message(&mut self, id: &str) {
        let Some(m) = self.find_message(id).cloned() else { return };
        self.wa.send(Cmd::Revoke { jid: m.jid.clone(), id: id.to_string() });
        if self.store.mark_deleted(&m.jid, id) {
            self.patch_row(id, |row| {
                row.deleted = true;
                row.kind = "text".into();
                row.text = t("msg.deleted").into();
            });
            self.schedule_refresh_chats();
        }
    }

    fn request_forward(&mut self, id: &str) {
        let Some(jid) = self.current_jid.clone() else { return };
        let Some(m) = self.find_message(id) else { return };
        let preview = preview_body(m);
        let image = self.decoded.get(id).map(|(img, _, _)| img.clone());
        self.pending_forward = Some((jid, id.to_string()));
        self.fill_forward_rows("");
        // Show what is being forwarded, like WhatsApp's bottom bar.
        self.ui.set_forward_preview_text(preview.into());
        self.ui.set_forward_preview_has_image(image.is_some());
        self.ui.set_forward_preview_image(image.unwrap_or_else(empty_image));
        self.ui.set_forward_open(true);
    }

    // Chats offered when forwarding, narrowed by the search box.
    fn fill_forward_rows(&mut self, query: &str) {
        let q = query.trim().to_lowercase();
        let digits: String = q.chars().filter(|c| c.is_ascii_digit()).collect();
        let rows: Vec<ChatItem> = self
            .store
            .sorted_chats()
            .into_iter()
            .filter(|meta| !is_channel(&meta.jid))
            .filter(|meta| {
                if q.is_empty() {
                    return true;
                }
                let name = self.store.chat_name(&meta.jid).to_lowercase();
                name.contains(&q) || (!digits.is_empty() && meta.jid.contains(&digits))
            })
            .take(200)
            .map(|meta| self.to_chat_row(&meta.jid, &meta.preview, meta.timestamp, 0, false))
            .collect();
        self.forward_model.set_vec(rows);
    }

    fn handle_forward_to(&mut self, target: &str) {
        let pending = self.pending_forward.take();
        self.ui.set_forward_open(false);
        let (Some((source, id)), false) = (pending, target.is_empty()) else { return };
        self.warm_chat(&source);
        let raw = self.store.messages_for(&source).iter().find(|m| m.id == id).and_then(|m| m.raw.clone());
        if let Some(message) = raw {
            self.wa.send(Cmd::Forward { jid: target.to_string(), message });
        }
    }

    // ---- outgoing media ----

    fn handle_attach(&mut self, kind: &'static str) {
        if self.current_jid.is_none() {
            return;
        }
        std::thread::spawn(move || {
            let dialog = rfd::FileDialog::new();
            let dialog = match kind {
                "image" => dialog
                    .add_filter(t("picker.images"), &["jpg", "jpeg", "png", "webp", "bmp"]),
                "audio" => dialog
                    .add_filter(t("picker.audio"), &["mp3", "ogg", "opus", "m4a", "aac", "wav"]),
                _ => dialog.add_filter(t("picker.all"), &["*"]),
            };
            if let Some(path) = dialog.pick_file() {
                ui_apply(move |b| b.on_picked(kind, path));
            }
        });
    }

    fn on_picked(&mut self, kind: &str, path: std::path::PathBuf) {
        let Some(jid) = self.current_jid.clone() else { return };
        match kind {
            // WhatsApp-style confirmation: caption field before sending.
            "image" => self.wa.send(Cmd::PreviewImage { path }),
            "audio" => self.wa.send(Cmd::SendAudioFile { jid, path }),
            _ => self.wa.send(Cmd::SendDocument { jid, path }),
        }
    }

    pub fn on_preview_ready(&mut self, path: &std::path::Path, img: crate::media::Decoded) {
        self.pending_image = Some(path.to_path_buf());
        self.ui.set_preview_image(image_of(&img));
        self.ui.set_preview_open(true);
    }

    fn confirm_send_image(&mut self, caption: &str) {
        let path = self.pending_image.take();
        self.ui.set_preview_open(false);
        let (Some(jid), Some(path)) = (self.current_jid.clone(), path) else { return };
        let caption = caption.trim();
        self.wa.send(Cmd::SendImage {
            jid,
            path,
            caption: (!caption.is_empty()).then(|| caption.to_string()),
        });
    }

    // Fired on any Ctrl+V while a chat is open; only acts when the
    // clipboard holds an image (text pastes normally into the composer).
    fn handle_paste(&mut self) {
        if self.current_jid.is_none() || self.ui.get_preview_open() {
            return;
        }
        if let Some(last) = self.last_paste
            && last.elapsed() < Duration::from_millis(400)
        {
            return;
        }
        self.last_paste = Some(Instant::now());
        let Ok(mut clipboard) = arboard::Clipboard::new() else { return };
        let Ok(img) = clipboard.get_image() else { return };
        let (w, h) = (img.width as u32, img.height as u32);
        let Some(buffer) = image::RgbaImage::from_raw(w, h, img.bytes.into_owned()) else {
            return;
        };
        let path = std::env::temp_dir().join(format!(
            "zapive_paste_{}.png",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis())
                .unwrap_or(0)
        ));
        if image::DynamicImage::ImageRgba8(buffer).save(&path).is_ok() {
            self.wa.send(Cmd::PreviewImage { path });
        }
    }

    // ---- audio (player, mini player, recording) ----

    fn plain_audio_path(&self, id: &str) -> Option<std::path::PathBuf> {
        let cached = std::path::PathBuf::from(self.media_path.get(id)?);
        crate::media::temp_plain(&self.media_key, &cached)
    }

    fn toggle_audio(&mut self, id: &str) {
        if id.is_empty() {
            return;
        }
        if let Some(a) = &mut self.audio
            && a.id == id
        {
            if let Some(player) = &a.player {
                if a.paused {
                    player.resume();
                    a.paused = false;
                } else {
                    player.pause();
                    a.paused = true;
                }
                let (aid, ajid, paused) = (a.id.clone(), a.jid.clone(), a.paused);
                if Some(&ajid) == self.current_jid.as_ref() {
                    self.patch_row(&aid, |row| row.playing = !paused);
                }
                self.ui.set_mini_audio_playing(!paused);
            }
            return;
        }
        self.start_audio(id, 0.0);
    }

    fn start_audio(&mut self, id: &str, offset_secs: f64) {
        let Some(jid) = self.current_jid.clone() else { return };
        let Some(m) = self.store.messages_for(&jid).iter().find(|m| m.id == id) else { return };
        let duration = (m.duration_sec.max(1)) as f64;
        self.stop_audio();
        self.audio = Some(AudioState {
            id: id.to_string(),
            jid,
            duration,
            paused: false,
            pending_offset: Some(offset_secs),
            player: None,
        });
        self.spin_audio_at(offset_secs);
    }

    // Starts (or requests the decode for) the current note at a source
    // position, honoring the selected speed.
    fn spin_audio_at(&mut self, offset_secs: f64) {
        let rate_idx = self.audio_rate_idx;
        let rate = RATES[rate_idx];
        let Some(a) = &mut self.audio else { return };
        let id = a.id.clone();
        if let Some(buffer) = self.audio_buffers.get(&(id.clone(), rate_idx)) {
            let player = crate::audio::Player::start(buffer, offset_secs / rate);
            if let Some(a) = &mut self.audio {
                a.player = player;
                a.paused = false;
                a.pending_offset = None;
            }
            self.ensure_audio_timer();
            let jid = self.audio.as_ref().map(|a| a.jid.clone()).unwrap_or_default();
            if Some(&jid) == self.current_jid.as_ref() {
                self.patch_row(&id, |row| row.playing = true);
            }
            self.ui.set_mini_audio_playing(true);
            return;
        }
        a.pending_offset = Some(offset_secs);
        let Some(plain) = self.plain_audio_path(&id) else {
            self.audio = None;
            return;
        };
        self.wa.send(Cmd::AudioDecode { id, plain, rate_idx, rate });
    }

    pub fn on_audio_ready(&mut self, id: &str, rate_idx: usize, buffer: crate::audio::AudioBuffer) {
        if self.audio_buffers.len() > 8 {
            self.audio_buffers.clear();
        }
        self.audio_buffers.insert((id.to_string(), rate_idx), buffer);
        if let Some(a) = &self.audio
            && a.id == id
            && rate_idx == self.audio_rate_idx
            && let Some(offset) = a.pending_offset
        {
            self.spin_audio_at(offset);
        }
    }

    fn seek_audio(&mut self, id: &str, frac: f32) {
        let matches = self.audio.as_ref().map(|a| a.id == id).unwrap_or(false);
        if matches {
            let duration = self.audio.as_ref().map(|a| a.duration).unwrap_or(1.0);
            self.spin_audio_at((frac as f64).clamp(0.0, 1.0) * duration);
        } else {
            let duration = self
                .current_jid
                .as_ref()
                .and_then(|jid| self.store.messages_for(jid).iter().find(|m| m.id == id))
                .map(|m| m.duration_sec.max(1) as f64)
                .unwrap_or(0.0);
            self.start_audio(id, (frac as f64).clamp(0.0, 1.0) * duration);
        }
    }

    fn cycle_audio_rate(&mut self) {
        // The old player ran at the previous rate: its source position is
        // output position times that rate; capture it before restarting.
        let prev_rate = RATES[self.audio_rate_idx];
        self.audio_rate_idx = (self.audio_rate_idx + 1) % RATES.len();
        self.ui.set_audio_rate_label(RATE_LABELS[self.audio_rate_idx].into());
        let offset = self.audio.as_mut().map(|a| {
            let pos = a.player.as_ref().map(|p| p.position_secs() * prev_rate).unwrap_or(0.0);
            a.player = None;
            pos.min(a.duration)
        });
        if let Some(offset) = offset {
            // The running note restarts at its position, new speed.
            self.spin_audio_at(offset);
        }
    }

    fn ensure_audio_timer(&mut self) {
        if self.audio_timer.running() {
            return;
        }
        self.audio_timer.start(slint::TimerMode::Repeated, Duration::from_millis(250), || {
            apply_now(|b| b.tick_audio());
        });
    }

    fn tick_audio(&mut self) {
        let rate = RATES[self.audio_rate_idx];
        let Some(a) = &self.audio else {
            self.audio_timer.stop();
            return;
        };
        let Some(player) = &a.player else { return };
        if player.finished() {
            self.stop_audio();
            return;
        }
        let pos = (player.position_secs() * rate).min(a.duration);
        let progress = (pos / a.duration).min(1.0) as f32;
        let label = crate::store::format_duration(pos as u32);
        let (id, jid, paused) = (a.id.clone(), a.jid.clone(), a.paused);
        if Some(&jid) == self.current_jid.as_ref() {
            self.patch_row(&id, |row| {
                row.playing = !paused;
                row.progress = progress;
                row.posLabel = label.into();
            });
        }
        self.ui.set_mini_audio_progress(progress);
    }

    fn stop_audio(&mut self) {
        if let Some(a) = self.audio.take() {
            if Some(&a.jid) == self.current_jid.as_ref() {
                self.patch_row(&a.id, |row| {
                    row.playing = false;
                    row.progress = 0.0;
                    row.posLabel = "".into();
                });
            }
        }
        self.ui.set_mini_audio(false);
        self.ui.set_mini_audio_playing(false);
        self.audio_timer.stop();
    }

    // Shows or hides the mini bar depending on where the note lives.
    fn sync_mini_player(&mut self) {
        let show = match (&self.audio, &self.current_jid) {
            (Some(a), Some(current)) => &a.jid != current,
            (Some(_), None) => true,
            _ => false,
        };
        if !show {
            self.ui.set_mini_audio(false);
            return;
        }
        let Some(a) = &self.audio else { return };
        let name = self.store.chat_name(&a.jid);
        let avatar = self.avatar_for(&a.jid);
        self.ui.set_mini_audio_name(name.clone().into());
        self.ui.set_mini_audio_avatar_has(avatar.is_some());
        self.ui.set_mini_audio_avatar(avatar.unwrap_or_else(empty_image));
        self.ui.set_mini_audio_initial(initial_of(&name).into());
        self.ui.set_mini_audio_color_idx(color_idx_of(&a.jid));
        self.ui.set_mini_audio_playing(!a.paused);
        self.ui.set_mini_audio(true);
    }

    pub fn on_wave(&mut self, id: &str, img: crate::media::Decoded) {
        let image = image_of(&img);
        self.waves.insert(id.to_string(), image.clone());
        self.patch_row(id, |row| {
            row.wave = image;
            row.hasWave = true;
        });
    }

    fn start_recording(&mut self) {
        if self.recorder.is_some() || self.current_jid.is_none() {
            return;
        }
        match crate::audio::Recorder::start() {
            Some(recorder) => {
                self.recorder = Some(recorder);
                self.rec_started = Some(Instant::now());
                self.ui.set_rec_elapsed("0:00".into());
                self.ui.set_rec_active(true);
                self.rec_timer.start(
                    slint::TimerMode::Repeated,
                    Duration::from_millis(500),
                    || {
                        apply_now(|b| {
                            if let Some(started) = b.rec_started {
                                let secs = started.elapsed().as_secs() as u32;
                                b.ui.set_rec_elapsed(
                                    crate::store::format_duration(secs).into(),
                                );
                            }
                        });
                    },
                );
            }
            None => self.ui.set_status_text(t("rec.noMic").into()),
        }
    }

    fn stop_recording(&mut self, send: bool) {
        self.rec_timer.stop();
        self.rec_started = None;
        self.ui.set_rec_active(false);
        let view_once = self.ui.get_rec_view_once();
        self.ui.set_rec_view_once(false);
        let Some(recorder) = self.recorder.take() else { return };
        if !send {
            return;
        }
        let (samples, in_rate) = recorder.stop();
        // Anything shorter than half a second is a misclick.
        if (samples.len() as f64) < in_rate as f64 / 2.0 {
            return;
        }
        let Some(jid) = self.current_jid.clone() else { return };
        self.wa.send(Cmd::SendVoice { jid, samples, in_rate, view_once });
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

// Every media kind gets a translated one-liner in notifications.
fn notification_body(m: &StoredMessage) -> String {
    if m.sticker {
        return t("preview.sticker");
    }
    match m.kind {
        MessageKind::Image => t("preview.photo"),
        MessageKind::Audio => t("preview.audio"),
        MessageKind::Doc => ta("preview.document", &[&m.text]),
        MessageKind::Video => {
            if m.gif { t("preview.gif") } else { t("preview.video") }
        }
        MessageKind::Text => m.text.clone(),
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
