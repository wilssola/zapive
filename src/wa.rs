// The WhatsApp side of the app. Owns the whatsapp-rust client on the
// tokio runtime; the UI talks to it through the Cmd channel, and events
// flow back through bridge::ui_apply. Mirrors src/whatsapp.ts on master.
// wa_apply parks events while the vault is still PIN-locked (the client
// connects before unlock) and replays them at boot.
use crate::bridge::wa_apply as ui_apply;
use crate::i18n::t;
use crate::paths::wa_session_path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use tokio::sync::mpsc;
use whatsapp_rust::bot::{Bot, BotHandle};
use whatsapp_rust::client::Client;
use whatsapp_rust::pair_code::PairCodeOptions;
use whatsapp_rust::store::SqliteStore;
use whatsapp_rust::async_channel;
use whatsapp_rust::types::call::{CallAction, ElsewhereOutcome, IncomingCall};
use whatsapp_rust::types::events::{Event, EventHandler};
use whatsapp_rust::types::presence::ReceiptType;
use whatsapp_rust::voip::{CallHandle, VideoFrame, VideoUpgradeToken};
use whatsapp_rust::waproto::whatsapp as wa;

// After this many unclean drops in a row the retry loop stops and the
// user gets the "offline" modal instead.
const MAX_FAILURES: u32 = 5;

// What a reply quotes: enough to build the context info.
#[derive(Debug)]
pub struct QuoteRef {
    pub id: String,
    pub sender_jid: String,
    pub message: Arc<wa::Message>,
}

// What to do with a media file once cached.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaWant {
    // Decode the full image for the bubble.
    Image,
    // Decode (possibly animated) sticker frames.
    Sticker,
    // Extract looping GIF frames from the clip.
    Gif,
    // Just cache the file (audio, documents, video clips).
    File,
}

pub enum Cmd {
    // Spawns the connection loop; sent once the bridge is installed so no
    // early event is lost.
    Start,
    PairWithCode(String),
    // From the fatal modal's button: reconnect after conflict/offline.
    Resume,
    Logout,
    Shutdown,
    SendText { jid: String, body: String, quote: Option<QuoteRef> },
    FetchHistory { jid: String, oldest_id: String, from_me: bool, ts_ms: i64 },
    SubscribePresence(String),
    FetchGroups,
    // Media pipeline: the vault key arrives once the vault unlocks.
    MediaKey(crate::vault::KeyHandle),
    Media { id: String, mimetype: String, message: std::sync::Arc<wa::Message>, want: MediaWant },
    // Decodes an embedded thumbnail (link previews, video posters).
    DecodeThumb { id: String, bytes: Vec<u8>, link: bool },
    FetchAvatar(String),
    // Voice notes: full decode at a speed, waveform bars, send recording.
    AudioDecode { id: String, plain: std::path::PathBuf, rate_idx: usize, rate: f64 },
    Waveform { id: String, path: std::path::PathBuf },
    SendVoice { jid: String, samples: Vec<f32>, in_rate: u32, view_once: bool },
    // Message actions.
    React { jid: String, id: String, from_me: bool, participant: Option<String>, emoji: String },
    Revoke { jid: String, id: String },
    Star { jid: String, id: String, from_me: bool, participant: Option<String>, starred: bool },
    Forward { jid: String, message: std::sync::Arc<wa::Message> },
    // Outgoing media (paths are plain files picked by the user).
    SendImage { jid: String, path: std::path::PathBuf, caption: Option<String> },
    SendDocument { jid: String, path: std::path::PathBuf },
    SendAudioFile { jid: String, path: std::path::PathBuf },
    // Decode a local image for the send-preview overlay.
    PreviewImage { path: std::path::PathBuf },
    // Stickers, GIFs and the video player.
    SendSticker { jid: String, path: std::path::PathBuf },
    GifSearch(String),
    SendGifUrl { jid: String, url: String },
    ZoomFrames { id: String, path: std::path::PathBuf },
    PlayVideo { id: String, path: std::path::PathBuf },
    StopVideo,
    // Self-update.
    CheckUpdate,
    ApplyUpdate,
    // Calls, channels and the info panel.
    RejectCall { id: String, from: String },
    // Answers the parked offer with that id and drives the media plane.
    // `video` opens the camera as part of answering.
    AcceptCall { id: String, video: bool },
    // Places a 1:1 call; `video` starts it as a video call.
    StartCall { jid: String, video: bool },
    // Ends whichever call is live, telling the peer.
    HangupCall,
    SetCallMuted(bool),
    // Turns the camera on or off mid-call. Turning it on also answers a
    // pending upgrade request from the peer.
    SetCallVideo(bool),
    FetchChannel(String),
    FetchChatInfo { jid: String, group: bool },
    Archive { jid: String, archived: bool },
    // Internal: wipe the session store and restart with a fresh QR.
    ResetSession,
    // Internal: too many failures — stop retrying and tell the user.
    HaltRetries,
}

// One history-sync chunk, flattened into owned data for the UI thread.
pub struct HistChat {
    pub jid: String,
    pub name: Option<String>,
    pub timestamp: i64,
    pub unread: Option<u32>,
    pub pinned: Option<i64>,
    pub archived: Option<bool>,
}

pub struct HistoryChunk {
    pub on_demand: bool,
    pub chats: Vec<HistChat>,
    pub messages: Vec<wa::WebMessageInfo>,
    pub pushnames: Vec<(String, String)>,
}

#[derive(Clone)]
pub struct WaService {
    cmd_tx: mpsc::UnboundedSender<Cmd>,
}

impl WaService {
    // Builds the client (blocking briefly on the runtime to open the
    // store) and reports whether a paired session already exists.
    pub fn start(rt: &tokio::runtime::Runtime) -> Result<(Self, bool), String> {
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        let session = rt
            .block_on(build_session(cmd_tx.clone()))
            .map_err(|e| e.to_string())?;
        let registered = session.client.pn().is_some();
        rt.spawn(executor(session, cmd_rx, cmd_tx.clone()));
        Ok((Self { cmd_tx }, registered))
    }

    pub fn send(&self, cmd: Cmd) {
        let _ = self.cmd_tx.send(cmd);
    }
}

struct Session {
    // Keeps the bot's background workers (history sync intake) alive.
    _handle: BotHandle,
    client: Arc<Client>,
}

async fn build_session(
    cmd_tx: mpsc::UnboundedSender<Cmd>,
) -> Result<Session, Box<dyn std::error::Error + Send + Sync>> {
    let db_url = wa_session_path().to_string_lossy().into_owned();
    let store = SqliteStore::new(&db_url).await?;
    let bot = Bot::builder().with_backend(store).build().await?;
    let client = bot.client();
    client
        .subscribe_handler(Arc::new(Pump {
            client: client.clone(),
            cmd_tx,
            rt: tokio::runtime::Handle::current(),
            failures: AtomicU32::new(0),
            stopped: AtomicBool::new(false),
        }))
        .detach();
    // spawn() wires the sync worker and starts client.run(); the run loop
    // itself ends on disconnect, but the workers stay up, so later resumes
    // just spawn client.run() again.
    let handle = bot.spawn();
    Ok(Session { _handle: handle, client })
}

fn parse_jid(jid: &str) -> Option<whatsapp_rust::Jid> {
    jid.parse().ok()
}

// ---- call plumbing ----

// Offers waiting for the user to pick up. Answering needs the offer
// itself (it carries the encrypted callKey and the relay), and the event
// that delivered it is long gone by then. Only a handful can ring at
// once, so a small vector is plenty.
static PENDING_OFFERS: std::sync::LazyLock<std::sync::Mutex<Vec<(String, IncomingCall)>>> =
    std::sync::LazyLock::new(|| std::sync::Mutex::new(Vec::new()));

// The call whose media plane is live, if any. Mute and hangup reach it
// from the UI thread through the command channel.
static LIVE_CALL: std::sync::LazyLock<std::sync::Mutex<Option<LiveCall>>> =
    std::sync::LazyLock::new(|| std::sync::Mutex::new(None));

struct LiveCall {
    id: String,
    handle: Arc<CallHandle>,
    // Mirrors the handle's mute so the capture side can send silence
    // instead of the room.
    muted: Arc<AtomicBool>,
    peer: whatsapp_rust::Jid,
    creator: whatsapp_rust::Jid,
    // Dropping this closes the camera and stops the encoder.
    camera: Option<crate::camera::CameraFeed>,
    // The peer asked to add video and is waiting on an answer; accepting
    // needs this exact token.
    upgrade: Option<VideoUpgradeToken>,
}

fn stash_offer(id: &str, offer: IncomingCall) {
    let Ok(mut offers) = PENDING_OFFERS.lock() else { return };
    offers.retain(|(known, _)| known != id);
    offers.push((id.to_string(), offer));
    if offers.len() > 8 {
        offers.remove(0);
    }
}

fn take_offer(id: &str) -> Option<IncomingCall> {
    let mut offers = PENDING_OFFERS.lock().ok()?;
    let at = offers.iter().position(|(known, _)| known == id)?;
    Some(offers.remove(at).1)
}

fn take_live() -> Option<LiveCall> {
    LIVE_CALL.lock().ok()?.take()
}

// Hanging up during setup: the handle only exists once accept/call has
// come back, so the request is parked here and the call tears itself
// down the moment it is connected.
static CANCEL_PENDING: AtomicBool = AtomicBool::new(false);

// Opens the camera and the decode side of a video call. The returned
// sender goes to the voip facade; the receiver is drained by a decode
// thread that pushes finished pictures at the call screen.
fn open_video() -> Option<(crate::camera::CameraFeed, async_channel::Sender<VideoFrame>)> {
    use crate::camera::{PREVIEW_BUSY, REMOTE_BUSY};
    let (sink_tx, sink_rx) = async_channel::bounded::<VideoFrame>(2);
    let camera = crate::camera::CameraFeed::start(Some(&crate::call::devices().camera), |pic| {
        // One frame in flight at a time. Without this a UI thread that
        // falls behind would queue megabytes of RGBA per second.
        if PREVIEW_BUSY.swap(true, Ordering::AcqRel) {
            return;
        }
        crate::bridge::ui_apply(move |b| b.on_call_preview(pic));
    })?;
    let spawned = std::thread::Builder::new()
        .name("zapive-video-decode".into())
        .spawn(move || {
            let Some(mut remote) = crate::camera::RemoteVideo::new() else {
                log::error!("[call] no H.264 decoder; peer video will not show");
                return;
            };
            // Every access unit must be decoded to keep the reference
            // frames intact; only the handoff to the UI is skippable.
            while let Ok(frame) = sink_rx.recv_blocking() {
                let Some(pic) = remote.decode(&frame) else { continue };
                if REMOTE_BUSY.swap(true, Ordering::AcqRel) {
                    continue;
                }
                crate::bridge::ui_apply(move |b| b.on_call_remote_video(pic));
            }
        });
    if let Err(e) = spawned {
        log::error!("[call] cannot start the video decoder: {e}");
        return None;
    }
    Some((camera, sink_tx))
}

// Owns a connected call until it ends: publishes the state the call
// screen renders, drains the engine's diagnostics, and tears the audio
// devices down on the way out.
async fn run_call(
    handle: CallHandle,
    audio: crate::call::CallAudio,
    camera: Option<crate::camera::CameraFeed>,
    outgoing: bool,
) {
    use whatsapp_rust::voip::CallEvent;
    let id = handle.call_id().to_string();
    let handle = Arc::new(handle);
    let has_video = camera.is_some();
    if let Ok(mut live) = LIVE_CALL.lock() {
        *live = Some(LiveCall {
            id: id.clone(),
            handle: handle.clone(),
            muted: audio.muted_flag(),
            peer: handle.peer_jid(),
            creator: handle.call_creator().clone(),
            camera,
            upgrade: None,
        });
    }
    if has_video {
        let id = id.clone();
        ui_apply(move |b| b.on_call_video(&id, true));
    }
    // The event stream is a single shared queue; drain it so the engine
    // never stalls on a full channel, and surface the terminal ones.
    // The user hung up while the handshake was still running.
    if CANCEL_PENDING.swap(false, Ordering::SeqCst) {
        handle.hangup_local().await;
        return;
    }
    let events = handle.events();
    let watched = id.clone();
    tokio::spawn(async move {
        while let Ok(event) = events.recv().await {
            match event {
                CallEvent::RelayAllocated => {
                    let id = watched.clone();
                    ui_apply(move |b| b.on_call_media_ready(&id));
                }
                CallEvent::RelayAllocateFailed(code) => {
                    log::warn!("[call] relay refused the allocate ({code})");
                }
                CallEvent::RelayAllocateTimedOut | CallEvent::RelayReconnectTimedOut => {
                    log::warn!("[call] relay went unresponsive");
                }
                CallEvent::AudioFormatMismatch { expected_rate, .. } => {
                    log::warn!("[call] peer picked a rate other than {expected_rate}");
                }
                // The peer wants to add video, or has answered our own
                // request. A request is parked until the user agrees: the
                // camera must never come on by itself.
                CallEvent::VideoStateChanged { state, upgrade_token, .. } => {
                    if state.is_upgrade_request() {
                        if let Ok(mut live) = LIVE_CALL.lock()
                            && let Some(call) = live.as_mut()
                            && call.id == watched
                        {
                            call.upgrade = upgrade_token;
                        }
                        let id = watched.clone();
                        ui_apply(move |b| b.on_call_video_requested(&id));
                    } else if state.is_inactive_for_call_mode() {
                        let id = watched.clone();
                        ui_apply(move |b| b.on_call_peer_video(&id, false));
                    } else {
                        let id = watched.clone();
                        ui_apply(move |b| b.on_call_peer_video(&id, true));
                    }
                }
                // A peer that lost the picture asks for a fresh IDR.
                CallEvent::RtcpReceived { packet_types, .. }
                    if packet_types.contains(&206) || packet_types.contains(&205) =>
                {
                    if let Ok(live) = LIVE_CALL.lock()
                        && let Some(camera) = live.as_ref().and_then(|c| c.camera.as_ref())
                    {
                        camera.request_keyframe();
                    }
                }
                _ => {}
            }
        }
    });
    // The callee is already talking once accept() returns; the caller
    // waits for the peer's <accept> stanza.
    if !outgoing {
        let id = id.clone();
        ui_apply(move |b| b.on_call_status(&id, "accept"));
    }
    handle.wait_ended().await;
    drop(audio);
    if let Ok(mut live) = LIVE_CALL.lock()
        && live.as_ref().is_some_and(|call| call.id == id)
    {
        *live = None;
    }
    let ended = id.clone();
    ui_apply(move |b| b.on_call_status(&ended, "terminate"));
}

// Both call paths fail the same way: tell the user, and make sure the
// call screen does not sit there pretending to connect.
fn fail_call(reason: &str) {
    let reason = reason.to_string();
    ui_apply(move |b| b.on_call_failed(&reason));
}

// Sync event tap on the library's dispatch thread: converts each event
// into UI work (via ui_apply) or a command back to the executor.
struct Pump {
    client: Arc<Client>,
    cmd_tx: mpsc::UnboundedSender<Cmd>,
    rt: tokio::runtime::Handle,
    failures: AtomicU32,
    stopped: AtomicBool,
}

impl EventHandler for Pump {
    fn handle_event(&self, event: Arc<Event>) {
        match &*event {
            Event::PairingQrCode(qr) => {
                let code = qr.code.clone();
                ui_apply(move |b| b.on_qr(&code));
            }
            Event::PairSuccess(_) => {
                ui_apply(|b| b.on_status(&t("status.connecting")));
            }
            Event::Connected(_) => {
                self.failures.store(0, Ordering::Relaxed);
                self.stopped.store(false, Ordering::Relaxed);
                let pn = self.client.pn().map(|j| j.to_non_ad_string()).unwrap_or_default();
                let lid = self.client.lid().map(|j| j.to_non_ad_string()).unwrap_or_default();
                ui_apply(move |b| b.on_open(&pn, &lid));
            }
            Event::StreamReplaced(_) => {
                // WhatsApp keeps one desktop session per account: the
                // library already stopped reconnecting; ask the user.
                self.stopped.store(true, Ordering::Relaxed);
                ui_apply(|b| b.on_fatal("conflict"));
            }
            Event::LoggedOut(_) => {
                self.stopped.store(true, Ordering::Relaxed);
                ui_apply(|b| b.on_status(&t("status.sessionEnded")));
                let _ = self.cmd_tx.send(Cmd::ResetSession);
            }
            Event::ConnectFailure(f) => {
                if f.reason.is_logged_out() {
                    self.stopped.store(true, Ordering::Relaxed);
                    let _ = self.cmd_tx.send(Cmd::ResetSession);
                } else {
                    self.count_failure();
                }
            }
            Event::Disconnected(d) => {
                if !d.reason.is_clean_shutdown() && !self.stopped.load(Ordering::Relaxed) {
                    self.count_failure();
                }
            }
            Event::Messages(_) => {
                let event = event.clone();
                ui_apply(move |b| {
                    if let Event::Messages(batch) = &*event {
                        b.on_messages(batch);
                    }
                });
            }
            Event::Receipt(receipt) => {
                let status = match receipt.r#type {
                    ReceiptType::Delivered => 3,
                    ReceiptType::Read | ReceiptType::ReadSelf => 4,
                    ReceiptType::Played | ReceiptType::PlayedSelf => 5,
                    _ => 0,
                };
                if status == 0 {
                    return;
                }
                let chat = receipt.source.chat.to_non_ad_string();
                let ids: Vec<String> = receipt.message_ids.iter().map(|i| i.to_string()).collect();
                ui_apply(move |b| b.on_receipt(&chat, &ids, status));
            }
            Event::HistorySync(_lazy) => {
                // Decoding inflates megabytes of protobuf: keep it off the
                // event bus thread and off the UI.
                let event = event.clone();
                self.rt.spawn(async move {
                    let Event::HistorySync(lazy) = &*event else { return };
                    let Some(hs) = lazy.get() else {
                        eprintln!("[history] chunk failed to decode");
                        return;
                    };
                    let on_demand = lazy.peer_data_request_session_id().is_some()
                        || hs.sync_type == wa::history_sync::HistorySyncType::OnDemand;
                    let mut chunk = HistoryChunk {
                        on_demand,
                        chats: Vec::new(),
                        messages: Vec::new(),
                        pushnames: Vec::new(),
                    };
                    for conv in &hs.conversations {
                        chunk.chats.push(HistChat {
                            jid: conv.id.clone(),
                            name: conv
                                .name
                                .clone()
                                .or_else(|| conv.display_name.clone())
                                .filter(|n| !n.is_empty()),
                            timestamp: conv.conversation_timestamp.unwrap_or(0) as i64,
                            unread: conv.unread_count,
                            pinned: conv.pinned.map(|p| p as i64),
                            archived: conv.archived,
                        });
                        for hm in &conv.messages {
                            if let Some(web) = hm.message.as_option() {
                                chunk.messages.push(web.clone());
                            }
                        }
                    }
                    for pn in &hs.pushnames {
                        if let (Some(id), Some(name)) = (pn.id.clone(), pn.pushname.clone()) {
                            chunk.pushnames.push((id, name));
                        }
                    }
                    ui_apply(move |b| b.on_history_chunk(chunk));
                });
            }
            // Every <call> stanza arrives as IncomingCall, not just the
            // offers: preaccept, accept, reject, terminate and the
            // transport chatter all land here. Only an <offer> may ring.
            Event::IncomingCall(call) => {
                let from = call.from.to_non_ad_string();
                let id = call.action.call_id().to_string();
                let ts = call.timestamp.timestamp();
                match &call.action {
                    CallAction::Offer { is_video, .. } => {
                        // An offer replayed from the offline queue is long
                        // dead. The library turns most of those into
                        // MissedCall already; this is the belt to that
                        // brace, since ringing for one is the worst
                        // possible outcome.
                        if call.offline {
                            ui_apply(move |b| b.on_call_missed(&id, &from, ts));
                            return;
                        }
                        let video = *is_video
                            || call.notify.as_deref().is_some_and(|n| n.contains("video"));
                        let group = call.group.is_some();
                        // Answering needs the offer itself (the encrypted
                        // callKey and the relay live on it), so park it
                        // until the user picks up or it goes stale.
                        stash_offer(&id, call.clone());
                        ui_apply(move |b| b.on_incoming_call(&id, &from, video, group, ts));
                    }
                    CallAction::Accept { .. } => {
                        ui_apply(move |b| b.on_call_status(&id, "accept"));
                    }
                    // A reason means one of the peer's devices bowed out
                    // (`busy`), not that the person declined: the rest of
                    // their devices keep ringing, so the call is still on.
                    CallAction::Reject { reason: None, .. } => {
                        ui_apply(move |b| b.on_call_status(&id, "reject"));
                    }
                    CallAction::Terminate { .. } => {
                        ui_apply(move |b| b.on_call_status(&id, "terminate"));
                    }
                    _ => {}
                }
            }
            Event::MissedCall(missed) => {
                let id = missed.call_id.clone();
                let from = missed.from.to_non_ad_string();
                let ts = missed.timestamp.timestamp();
                ui_apply(move |b| b.on_call_missed(&id, &from, ts));
            }
            // Another of our devices picked the call up (or turned it
            // down); either way this one stops ringing.
            Event::CallEndedElsewhere(elsewhere) => {
                let id = elsewhere.call_id.clone();
                let status = match elsewhere.outcome {
                    ElsewhereOutcome::Rejected => "reject",
                    // Accepted, and whatever outcome the server adds next.
                    _ => "accept",
                };
                ui_apply(move |b| b.on_call_status(&id, status));
            }
            Event::ChatPresence(update) => {
                let chat = update.source.chat.to_non_ad_string();
                let state = format!("{:?}", update.state);
                let media = format!("{:?}", update.media);
                ui_apply(move |b| b.on_chat_presence(&chat, &state, &media));
            }
            Event::Presence(update) => {
                let from = update.from.to_non_ad_string();
                let available = !update.unavailable;
                ui_apply(move |b| b.on_presence(&from, available));
            }
            Event::PinUpdate(u) => {
                let jid = u.jid.to_non_ad_string();
                let pinned = u.action.pinned.unwrap_or(false);
                let ts = u.timestamp.timestamp();
                ui_apply(move |b| b.on_chat_flag(&jid, Some(if pinned { ts } else { 0 }), None));
            }
            Event::ArchiveUpdate(u) => {
                let jid = u.jid.to_non_ad_string();
                let archived = u.action.archived.unwrap_or(false);
                ui_apply(move |b| b.on_chat_flag(&jid, None, Some(archived)));
            }
            Event::StarUpdate(u) => {
                let jid = u.chat_jid.to_non_ad_string();
                let id = u.message_id.clone();
                let starred = u.action.starred.unwrap_or(false);
                ui_apply(move |b| b.on_star(&jid, &id, starred));
            }
            Event::MarkChatAsReadUpdate(u) => {
                let jid = u.jid.to_non_ad_string();
                let read = u.action.read.unwrap_or(true);
                ui_apply(move |b| b.on_mark_read(&jid, read));
            }
            Event::ContactUpdate(u) => {
                let jid = u.jid.to_non_ad_string();
                let name = u
                    .action
                    .full_name
                    .clone()
                    .or_else(|| u.action.first_name.clone())
                    .unwrap_or_default();
                if !name.is_empty() {
                    ui_apply(move |b| b.on_contact(&jid, &name));
                }
            }
            _ => {}
        }
    }
}

impl Pump {
    fn count_failure(&self) {
        let n = self.failures.fetch_add(1, Ordering::Relaxed) + 1;
        if n >= MAX_FAILURES {
            self.stopped.store(true, Ordering::Relaxed);
            let _ = self.cmd_tx.send(Cmd::HaltRetries);
        } else {
            ui_apply(|b| b.on_status(&t("status.reconnecting")));
        }
    }
}

async fn executor(
    mut session: Session,
    mut cmd_rx: mpsc::UnboundedReceiver<Cmd>,
    cmd_tx: mpsc::UnboundedSender<Cmd>,
) {
    let mut media_key = crate::vault::KeyHandle::default();
    let media_sem = Arc::new(tokio::sync::Semaphore::new(3));
    let avatar_sem = Arc::new(tokio::sync::Semaphore::new(8));
    // Bumped on every play/stop; running frame loops exit when it moves.
    let video_gen = Arc::new(std::sync::atomic::AtomicU64::new(0));
    while let Some(cmd) = cmd_rx.recv().await {
        match cmd {
            Cmd::MediaKey(key) => {
                media_key = key;
            }
            Cmd::Media { id, mimetype, message, want } => {
                let client = session.client.clone();
                let key = media_key.clone();
                let sem = media_sem.clone();
                tokio::spawn(async move {
                    let _permit = sem.acquire_owned().await;
                    let Some(path) =
                        crate::media::ensure_cached(&client, &key, &id, &mimetype, &message).await
                    else {
                        ui_apply(move |b| b.on_media_missing(&id));
                        return;
                    };
                    match want {
                        MediaWant::File => ui_apply(move |b| b.on_media_file(&id, &path)),
                        MediaWant::Image => {
                            let decoded = crate::media::read_cached(&key, &path)
                                .and_then(|d| crate::media::decode_bytes(&d, 1280));
                            match decoded {
                                Some(img) => ui_apply(move |b| b.on_media_image(&id, &path, img)),
                                None => ui_apply(move |b| b.on_media_missing(&id)),
                            }
                        }
                        MediaWant::Sticker => {
                            let frames = crate::media::read_cached(&key, &path)
                                .map(|d| crate::media::sticker_frames(&d, 180, 24))
                                .unwrap_or_default();
                            ui_apply(move |b| b.on_media_sticker(&id, &path, frames));
                        }
                        MediaWant::Gif => {
                            let frames = tokio::task::spawn_blocking(move || {
                                crate::media::temp_plain(&key, &path)
                                    .map(|p| crate::video::frames(&p, 320, 15.0, 45))
                                    .map(|f| (path, f))
                            })
                            .await
                            .ok()
                            .flatten();
                            match frames {
                                Some((path, frames)) if !frames.is_empty() => {
                                    ui_apply(move |b| b.on_media_gif(&id, &path, frames));
                                }
                                Some((path, _)) => ui_apply(move |b| b.on_media_file(&id, &path)),
                                None => ui_apply(move |b| b.on_media_missing(&id)),
                            }
                        }
                    }
                });
            }
            Cmd::DecodeThumb { id, bytes, link } => {
                tokio::spawn(async move {
                    if let Some(img) =
                        tokio::task::spawn_blocking(move || crate::media::decode_bytes(&bytes, 1280))
                            .await
                            .ok()
                            .flatten()
                    {
                        ui_apply(move |b| b.on_thumb(&id, link, img));
                    }
                });
            }
            Cmd::React { jid, id, from_me, participant, emoji } => {
                let client = session.client.clone();
                tokio::spawn(async move {
                    let Some(chat) = parse_jid(&jid) else { return };
                    let key = whatsapp_rust::message_key(
                        id,
                        &chat,
                        from_me,
                        participant.and_then(|p| parse_jid(&p)).as_ref(),
                    );
                    if let Err(e) = client.send_reaction(chat, key, &emoji).await {
                        eprintln!("[wa] reaction failed: {e}");
                    }
                });
            }
            Cmd::Revoke { jid, id } => {
                let client = session.client.clone();
                tokio::spawn(async move {
                    let Some(chat) = parse_jid(&jid) else { return };
                    if let Err(e) = client
                        .revoke_message(chat, id, whatsapp_rust::send::RevokeType::Sender)
                        .await
                    {
                        eprintln!("[wa] revoke failed: {e}");
                    }
                });
            }
            Cmd::Star { jid, id, from_me, participant, starred } => {
                let client = session.client.clone();
                tokio::spawn(async move {
                    let Some(chat) = parse_jid(&jid) else { return };
                    let participant = participant.and_then(|p| parse_jid(&p));
                    let actions = client.chat_actions();
                    let result = if starred {
                        actions.star_message(&chat, participant.as_ref(), &id, from_me).await
                    } else {
                        actions.unstar_message(&chat, participant.as_ref(), &id, from_me).await
                    };
                    if let Err(e) = result {
                        eprintln!("[wa] star failed: {e}");
                    }
                });
            }
            Cmd::Forward { jid, message } => {
                let client = session.client.clone();
                tokio::spawn(async move {
                    let Some(to) = parse_jid(&jid) else { return };
                    match client.forward_message(to, &message).await {
                        Ok(sent) => {
                            use whatsapp_rust::proto_helpers::MessageExt as _;
                            let id = sent.message_id;
                            let echoed = message.prepare_for_forward();
                            ui_apply(move |b| b.echo_sent(&jid, &id, *echoed));
                        }
                        Err(e) => eprintln!("[wa] forward failed: {e}"),
                    }
                });
            }
            Cmd::SendImage { jid, path, caption } => {
                let client = session.client.clone();
                tokio::spawn(async move {
                    let Ok(bytes) = tokio::fs::read(&path).await else { return };
                    let thumb_src = bytes.clone();
                    let thumb = tokio::task::spawn_blocking(move || make_jpeg_thumb(&thumb_src))
                        .await
                        .ok()
                        .flatten();
                    let Some(to) = parse_jid(&jid) else { return };
                    let upload = match client
                        .upload(bytes, whatsapp_rust::wacore::download::MediaType::Image, Default::default())
                        .await
                    {
                        Ok(u) => u,
                        Err(e) => {
                            eprintln!("[wa] image upload failed: {e}");
                            return;
                        }
                    };
                    let message = whatsapp_rust::media::image_message(
                        upload,
                        whatsapp_rust::media::ImageOptions {
                            caption: caption.filter(|c| !c.is_empty()),
                            jpeg_thumbnail: thumb,
                            ..Default::default()
                        },
                    );
                    match client.send_message(to, message.clone()).await {
                        Ok(sent) => {
                            let id = sent.message_id;
                            ui_apply(move |b| b.echo_sent(&jid, &id, message));
                        }
                        Err(e) => eprintln!("[wa] image send failed: {e}"),
                    }
                });
            }
            Cmd::SendDocument { jid, path } => {
                let client = session.client.clone();
                tokio::spawn(async move {
                    let Ok(bytes) = tokio::fs::read(&path).await else { return };
                    let Some(to) = parse_jid(&jid) else { return };
                    let name = path
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_else(|| "file".into());
                    let upload = match client
                        .upload(bytes, whatsapp_rust::wacore::download::MediaType::Document, Default::default())
                        .await
                    {
                        Ok(u) => u,
                        Err(e) => {
                            eprintln!("[wa] document upload failed: {e}");
                            return;
                        }
                    };
                    let message = whatsapp_rust::media::document_message(
                        upload,
                        whatsapp_rust::media::DocumentOptions {
                            file_name: Some(name),
                            ..Default::default()
                        },
                    );
                    match client.send_message(to, message.clone()).await {
                        Ok(sent) => {
                            let id = sent.message_id;
                            ui_apply(move |b| b.echo_sent(&jid, &id, message));
                        }
                        Err(e) => eprintln!("[wa] document send failed: {e}"),
                    }
                });
            }
            Cmd::SendAudioFile { jid, path } => {
                let client = session.client.clone();
                tokio::spawn(async move {
                    let Ok(bytes) = tokio::fs::read(&path).await else { return };
                    let Some(to) = parse_jid(&jid) else { return };
                    let mimetype = match path.extension().and_then(|e| e.to_str()) {
                        Some("ogg") | Some("opus") => "audio/ogg; codecs=opus",
                        Some("mp3") => "audio/mpeg",
                        Some("m4a") | Some("aac") => "audio/mp4",
                        Some("wav") => "audio/wav",
                        _ => "audio/mpeg",
                    };
                    let upload = match client
                        .upload(bytes, whatsapp_rust::wacore::download::MediaType::Audio, Default::default())
                        .await
                    {
                        Ok(u) => u,
                        Err(e) => {
                            eprintln!("[wa] audio upload failed: {e}");
                            return;
                        }
                    };
                    let message = whatsapp_rust::media::audio_message(
                        upload,
                        whatsapp_rust::media::AudioOptions {
                            ptt: Some(true),
                            mimetype: Some(mimetype.to_string()),
                            ..Default::default()
                        },
                    );
                    match client.send_message(to, message.clone()).await {
                        Ok(sent) => {
                            let id = sent.message_id;
                            ui_apply(move |b| b.echo_sent(&jid, &id, message));
                        }
                        Err(e) => eprintln!("[wa] audio send failed: {e}"),
                    }
                });
            }
            Cmd::PreviewImage { path } => {
                tokio::spawn(async move {
                    let img = tokio::task::spawn_blocking(move || {
                        std::fs::read(&path)
                            .ok()
                            .and_then(|d| crate::media::decode_bytes(&d, 1280))
                            .map(|img| (path, img))
                    })
                    .await
                    .ok()
                    .flatten();
                    if let Some((path, img)) = img {
                        ui_apply(move |b| b.on_preview_ready(&path, img));
                    }
                });
            }
            Cmd::SendSticker { jid, path } => {
                let client = session.client.clone();
                tokio::spawn(async move {
                    let webp = tokio::task::spawn_blocking(move || {
                        let data = std::fs::read(&path).ok()?;
                        crate::media::to_webp_sticker(&data)
                    })
                    .await
                    .ok()
                    .flatten();
                    let Some(webp) = webp else {
                        eprintln!("[wa] sticker encode failed");
                        return;
                    };
                    let Some(to) = parse_jid(&jid) else { return };
                    let len = webp.len() as u64;
                    let upload = match client
                        .upload(webp, whatsapp_rust::wacore::download::MediaType::Sticker, Default::default())
                        .await
                    {
                        Ok(u) => u,
                        Err(e) => {
                            eprintln!("[wa] sticker upload failed: {e}");
                            return;
                        }
                    };
                    use whatsapp_rust::waproto::buffa::MessageField;
                    let mut sticker = wa::message::StickerMessage::default();
                    sticker.url = Some(upload.url);
                    sticker.direct_path = Some(upload.direct_path);
                    sticker.media_key = Some(upload.media_key.to_vec().into());
                    sticker.file_enc_sha256 = Some(upload.file_enc_sha256.to_vec().into());
                    sticker.file_sha256 = Some(upload.file_sha256.to_vec().into());
                    sticker.file_length = Some(len);
                    sticker.mimetype = Some("image/webp".into());
                    sticker.width = Some(512);
                    sticker.height = Some(512);
                    sticker.media_key_timestamp = Some(upload.media_key_timestamp);
                    let mut message = wa::Message::default();
                    message.sticker_message = MessageField::some(sticker);
                    match client.send_message(to, message.clone()).await {
                        Ok(sent) => {
                            let id = sent.message_id;
                            ui_apply(move |b| b.echo_sent(&jid, &id, message));
                        }
                        Err(e) => eprintln!("[wa] sticker send failed: {e}"),
                    }
                });
            }
            Cmd::GifSearch(query) => {
                tokio::spawn(async move {
                    let results = tokio::task::spawn_blocking(move || {
                        let q = if query.trim().is_empty() { "funny" } else { query.trim() };
                        let url = format!(
                            "https://api.openverse.org/v1/images/?extension=gif&page_size=20&q={}",
                            urlencode(q)
                        );
                        let mut res = ureq::get(&url).header("User-Agent", "Zapive").call().ok()?;
                        let text = res.body_mut().read_to_string().ok()?;
                        let json: serde_json::Value = serde_json::from_str(&text).ok()?;
                        let mut out = Vec::new();
                        for item in json.get("results")?.as_array()?.iter().take(12) {
                            let id = item.get("id")?.as_str()?.to_string();
                            let gif_url = item.get("url")?.as_str()?.to_string();
                            let thumb_url = item
                                .get("thumbnail")
                                .and_then(|t| t.as_str())
                                .unwrap_or(&gif_url)
                                .to_string();
                            let Some(mut thumb) =
                                ureq::get(&thumb_url).header("User-Agent", "Zapive").call().ok()
                            else {
                                continue;
                            };
                            let Some(bytes) = thumb.body_mut().read_to_vec().ok() else { continue };
                            if let Some(img) = crate::media::decode_cover(&bytes, 84) {
                                out.push((id, gif_url, img));
                            }
                        }
                        Some(out)
                    })
                    .await
                    .ok()
                    .flatten()
                    .unwrap_or_default();
                    ui_apply(move |b| b.on_gif_results(results));
                });
            }
            Cmd::SendGifUrl { jid, url } => {
                let client = session.client.clone();
                tokio::spawn(async move {
                    let mp4 = tokio::task::spawn_blocking(move || {
                        let mut res = ureq::get(&url).header("User-Agent", "Zapive").call().ok()?;
                        let bytes = res.body_mut().read_to_vec().ok()?;
                        let stamp = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .map(|d| d.as_millis())
                            .unwrap_or(0);
                        let gif = std::env::temp_dir().join(format!("zapive_gif_{stamp}.gif"));
                        let mp4 = std::env::temp_dir().join(format!("zapive_gif_{stamp}.mp4"));
                        std::fs::write(&gif, bytes).ok()?;
                        let result = crate::video::gif_to_mp4(&gif, &mp4);
                        let _ = std::fs::remove_file(&gif);
                        result.map(|_| mp4)
                    })
                    .await
                    .ok()
                    .flatten();
                    let Some(mp4) = mp4 else {
                        eprintln!("[wa] gif conversion failed");
                        return;
                    };
                    let Ok(bytes) = tokio::fs::read(&mp4).await else { return };
                    let _ = tokio::fs::remove_file(&mp4).await;
                    let Some(to) = parse_jid(&jid) else { return };
                    let upload = match client
                        .upload(bytes, whatsapp_rust::wacore::download::MediaType::Video, Default::default())
                        .await
                    {
                        Ok(u) => u,
                        Err(e) => {
                            eprintln!("[wa] gif upload failed: {e}");
                            return;
                        }
                    };
                    let message = whatsapp_rust::media::video_message(
                        upload,
                        whatsapp_rust::media::VideoOptions {
                            gif_playback: Some(true),
                            ..Default::default()
                        },
                    );
                    match client.send_message(to, message.clone()).await {
                        Ok(sent) => {
                            let id = sent.message_id;
                            ui_apply(move |b| b.echo_sent(&jid, &id, message));
                        }
                        Err(e) => eprintln!("[wa] gif send failed: {e}"),
                    }
                });
            }
            Cmd::ZoomFrames { id, path } => {
                let key = media_key.clone();
                tokio::spawn(async move {
                    let frames = tokio::task::spawn_blocking(move || {
                        crate::media::temp_plain(&key, &path)
                            .map(|p| crate::video::frames(&p, 720, 15.0, 90))
                    })
                    .await
                    .ok()
                    .flatten()
                    .unwrap_or_default();
                    if !frames.is_empty() {
                        ui_apply(move |b| b.on_zoom_frames(&id, frames));
                    }
                });
            }
            Cmd::PlayVideo { id, path } => {
                video_gen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let generation = video_gen.load(std::sync::atomic::Ordering::SeqCst);
                let key = media_key.clone();
                let gen_handle = video_gen.clone();
                tokio::spawn(async move {
                    let Some(plain) = ({
                        let key = key.clone();
                        let path = path.clone();
                        tokio::task::spawn_blocking(move || crate::media::temp_plain(&key, &path))
                            .await
                            .ok()
                            .flatten()
                    }) else {
                        return;
                    };
                    // Soundtrack decoded whole; playback starts UI-side in
                    // sync with the first frame.
                    let audio_plain = plain.clone();
                    let audio = tokio::task::spawn_blocking(move || {
                        crate::audio::decode_with_tempo(&audio_plain, 1.0)
                    })
                    .await
                    .ok()
                    .flatten();
                    let id_for_audio = id.clone();
                    if let Some(buffer) = audio {
                        ui_apply(move |b| b.on_video_audio(&id_for_audio, buffer));
                    }
                    // Paced frame loop on a blocking thread; a newer
                    // generation (or StopVideo) ends it.
                    tokio::task::spawn_blocking(move || {
                        let frames = crate::video::frames(&plain, 560, 15.0, 900);
                        let started = std::time::Instant::now();
                        for (i, frame) in frames.into_iter().enumerate() {
                            if gen_handle.load(std::sync::atomic::Ordering::SeqCst) != generation {
                                return;
                            }
                            let due = std::time::Duration::from_millis((i as u64) * 1000 / 15);
                            if let Some(wait) = due.checked_sub(started.elapsed()) {
                                std::thread::sleep(wait);
                            }
                            let id = id.clone();
                            ui_apply(move |b| b.on_video_frame(&id, frame));
                        }
                        let id_done = id.clone();
                        ui_apply(move |b| b.on_video_ended(&id_done));
                    });
                });
            }
            Cmd::StopVideo => {
                video_gen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }
            Cmd::CheckUpdate => {
                tokio::spawn(async move {
                    let found = tokio::task::spawn_blocking(crate::update::check)
                        .await
                        .ok()
                        .flatten();
                    if let Some(version) = found {
                        // Direct apply: the banner must show on the lock
                        // and login screens, not wait in the backlog.
                        crate::bridge::ui_apply(move |b| b.on_update_available(&version));
                    }
                });
            }
            Cmd::ApplyUpdate => {
                tokio::spawn(async move {
                    let result = tokio::task::spawn_blocking(crate::update::apply)
                        .await
                        .unwrap_or_else(|e| Err(e.to_string()));
                    crate::bridge::ui_apply(move |b| b.on_update_applied(result));
                });
            }
            Cmd::RejectCall { id, from } => {
                let client = session.client.clone();
                tokio::spawn(async move {
                    // The parked offer carries the creator and the ringing
                    // generation, which a bare id cannot reconstruct.
                    if let Some(offer) = take_offer(&id) {
                        if let Err(e) = client.voip().reject(&offer).await {
                            log::warn!("[wa] call reject failed: {e}");
                        }
                        return;
                    }
                    let Some(peer) = parse_jid(&from) else { return };
                    if let Err(e) = client.voip().reject_call(&id, &peer, &peer).await {
                        log::warn!("[wa] call reject failed: {e}");
                    }
                });
            }
            Cmd::AcceptCall { id, video } => {
                let client = session.client.clone();
                CANCEL_PENDING.store(false, Ordering::SeqCst);
                tokio::spawn(async move {
                    let Some(offer) = take_offer(&id) else {
                        fail_call(&t("call.gone"));
                        return;
                    };
                    let Some(audio) = crate::call::CallAudio::start() else {
                        // No devices means no call; decline rather than
                        // leave the caller listening to nothing.
                        if let Err(e) = client.voip().reject(&offer).await {
                            log::warn!("[wa] call reject failed: {e}");
                        }
                        fail_call(&t("call.noDevices"));
                        return;
                    };
                    // Answering an audio-only offer with video is not on
                    // the table: the peer never offered to receive it.
                    let vision = video.then(open_video).flatten();
                    let voip = client.voip();
                    let mut builder = voip.accept(&offer).audio(audio.mic(), audio.speaker());
                    if let Some((camera, sink)) = &vision {
                        builder = builder.video(camera.source(), sink.clone());
                    }
                    // preaccept -> callKey decrypt -> accept -> relay.
                    match builder.start().await {
                        Ok(handle) => {
                            run_call(handle, audio, vision.map(|(c, _)| c), false).await;
                        }
                        Err(e) => {
                            log::error!("[wa] call accept failed: {e}");
                            fail_call(&t("call.failed"));
                        }
                    }
                });
            }
            Cmd::StartCall { jid, video } => {
                let client = session.client.clone();
                CANCEL_PENDING.store(false, Ordering::SeqCst);
                tokio::spawn(async move {
                    let Some(peer) = parse_jid(&jid) else {
                        fail_call(&t("call.failed"));
                        return;
                    };
                    let Some(audio) = crate::call::CallAudio::start() else {
                        fail_call(&t("call.noDevices"));
                        return;
                    };
                    let vision = video.then(open_video).flatten();
                    if video && vision.is_none() {
                        fail_call(&t("call.noCamera"));
                        return;
                    }
                    let voip = client.voip();
                    let mut builder = voip.call(&peer).audio(audio.mic(), audio.speaker());
                    if let Some((camera, sink)) = &vision {
                        builder = builder.video(camera.source(), sink.clone());
                    }
                    log::info!(
                        "[wa] placing a {} call to {peer}",
                        if vision.is_some() { "video" } else { "voice" }
                    );
                    match builder.start().await {
                        Ok(handle) => {
                            // The call id is minted here, so the screen
                            // only learns it now.
                            let id = handle.call_id().to_string();
                            log::info!("[wa] offer sent for call {id}; waiting for the relay");
                            ui_apply(move |b| b.on_outgoing_call(&id));
                            run_call(handle, audio, vision.map(|(c, _)| c), true).await;
                        }
                        Err(e) => {
                            log::error!("[wa] outgoing call failed: {e}");
                            fail_call(&t("call.failed"));
                        }
                    }
                });
            }
            Cmd::HangupCall => {
                let client = session.client.clone();
                tokio::spawn(async move {
                    let Some(live) = take_live() else {
                        // Nothing connected yet: leave the note for the
                        // setup task to find.
                        CANCEL_PENDING.store(true, Ordering::SeqCst);
                        return;
                    };
                    // terminate() tears the local media down as well, but
                    // hangup() is what guarantees it when the stanza
                    // cannot be sent.
                    if let Err(e) =
                        client.voip().terminate(&live.id, &live.peer, &live.creator).await
                    {
                        log::warn!("[wa] call terminate failed: {e}");
                    }
                    live.handle.hangup_local().await;
                });
            }
            Cmd::SetCallVideo(on) => {
                let handle = LIVE_CALL
                    .lock()
                    .ok()
                    .and_then(|live| live.as_ref().map(|call| (call.handle.clone(), call.id.clone())));
                let Some((handle, id)) = handle else { continue };
                tokio::spawn(async move {
                    if !on {
                        if let Err(e) = handle.stop_video().await {
                            log::warn!("[wa] stopping video failed: {e}");
                        }
                        if let Ok(mut live) = LIVE_CALL.lock()
                            && let Some(call) = live.as_mut()
                        {
                            // Dropping the feed is what releases the camera.
                            call.camera = None;
                            call.upgrade = None;
                        }
                        ui_apply(move |b| b.on_call_video(&id, false));
                        return;
                    }
                    let Some((camera, sink)) = open_video() else {
                        fail_call(&t("call.noCamera"));
                        return;
                    };
                    // A parked request from the peer has to be answered with
                    // its own token; anything else is a fresh upgrade.
                    let token = LIVE_CALL
                        .lock()
                        .ok()
                        .and_then(|mut live| live.as_mut().and_then(|call| call.upgrade.take()));
                    let started = match token {
                        Some(token) => {
                            handle.accept_video(token, camera.source(), sink.clone()).await
                        }
                        None => handle.start_video(camera.source(), sink.clone()).await,
                    };
                    if let Err(e) = started {
                        log::error!("[wa] starting video failed: {e}");
                        fail_call(&t("call.noCamera"));
                        return;
                    }
                    if let Ok(mut live) = LIVE_CALL.lock()
                        && let Some(call) = live.as_mut()
                    {
                        call.camera = Some(camera);
                    }
                    ui_apply(move |b| b.on_call_video(&id, true));
                });
            }
            Cmd::SetCallMuted(muted) => {
                // set_muted announces <mute_v2> to the peer, so it has to be
                // awaited; the capture side reads the flag either way.
                let live = LIVE_CALL
                    .lock()
                    .ok()
                    .and_then(|live| live.as_ref().map(|c| (c.handle.clone(), c.muted.clone())));
                let Some((handle, flag)) = live else { continue };
                flag.store(muted, Ordering::SeqCst);
                tokio::spawn(async move {
                    if let Err(e) = handle.set_muted(muted).await {
                        log::warn!("[wa] the peer was not told about the mute: {e}");
                    }
                });
            }
            Cmd::FetchChannel(jid) => {
                let client = session.client.clone();
                tokio::spawn(async move {
                    let Some(target) = parse_jid(&jid) else { return };
                    match client.newsletter().get_metadata(&target).await {
                        Ok(meta) => {
                            let name = meta.name.clone();
                            let url = meta.picture_url.clone().or(meta.preview_url.clone());
                            let avatar = match url {
                                Some(url) => {
                                    tokio::task::spawn_blocking(move || {
                                        let mut res = ureq::get(&url).call().ok()?;
                                        let bytes = res.body_mut().read_to_vec().ok()?;
                                        crate::media::decode_cover(&bytes, 64)
                                    })
                                    .await
                                    .ok()
                                    .flatten()
                                }
                                None => None,
                            };
                            ui_apply(move |b| b.on_channel_meta(&jid, &name, avatar));
                        }
                        Err(e) => eprintln!("[wa] channel metadata failed for {jid}: {e}"),
                    }
                });
            }
            Cmd::FetchChatInfo { jid, group } => {
                let client = session.client.clone();
                tokio::spawn(async move {
                    let Some(target) = parse_jid(&jid) else { return };
                    if group {
                        match client.groups().get_metadata(&target).await {
                            Ok(meta) => {
                                let desc = meta.description.clone().unwrap_or_default();
                                let members = meta
                                    .size
                                    .map(|s| s as usize)
                                    .unwrap_or(meta.participants.len());
                                ui_apply(move |b| b.on_chat_info(&jid, "", &desc, members));
                            }
                            Err(e) => eprintln!("[wa] group info failed: {e}"),
                        }
                    } else {
                        match client.contacts().get_user_info(&[target.clone()]).await {
                            Ok(map) => {
                                let about = map
                                    .get(&target)
                                    .and_then(|info| info.status.clone())
                                    .unwrap_or_default();
                                ui_apply(move |b| b.on_chat_info(&jid, &about, "", 0));
                            }
                            Err(e) => eprintln!("[wa] user info failed: {e}"),
                        }
                    }
                });
            }
            Cmd::Archive { jid, archived } => {
                let client = session.client.clone();
                tokio::spawn(async move {
                    let Some(target) = parse_jid(&jid) else { return };
                    let actions = client.chat_actions();
                    let result = if archived {
                        actions.archive_chat(&target, None).await
                    } else {
                        actions.unarchive_chat(&target, None).await
                    };
                    if let Err(e) = result {
                        eprintln!("[wa] archive failed: {e}");
                    }
                });
            }
            Cmd::AudioDecode { id, plain, rate_idx, rate } => {
                tokio::spawn(async move {
                    let buffer = tokio::task::spawn_blocking(move || {
                        crate::audio::decode_with_tempo(&plain, rate)
                    })
                    .await
                    .ok()
                    .flatten();
                    match buffer {
                        Some(buffer) => {
                            ui_apply(move |b| b.on_audio_ready(&id, rate_idx, buffer))
                        }
                        None => eprintln!("[audio] decode failed for {id}"),
                    }
                });
            }
            Cmd::Waveform { id, path } => {
                let key = media_key.clone();
                tokio::spawn(async move {
                    let wave = tokio::task::spawn_blocking(move || {
                        crate::media::temp_plain(&key, &path)
                            .and_then(|p| crate::audio::waveform(&p))
                    })
                    .await
                    .ok()
                    .flatten();
                    if let Some(img) = wave {
                        ui_apply(move |b| b.on_wave(&id, img));
                    }
                });
            }
            Cmd::SendVoice { jid, samples, in_rate, view_once } => {
                let client = session.client.clone();
                tokio::spawn(async move {
                    let wave = crate::audio::message_waveform(&samples);
                    let tmp = std::env::temp_dir().join(format!(
                        "zapive_voice_{}.ogg",
                        std::process::id()
                    ));
                    let encode_path = tmp.clone();
                    let seconds = tokio::task::spawn_blocking(move || {
                        crate::audio::encode_voice_ogg(&samples, in_rate, &encode_path)
                    })
                    .await
                    .ok()
                    .flatten();
                    let Some(seconds) = seconds else {
                        eprintln!("[audio] voice encode failed");
                        return;
                    };
                    let Ok(bytes) = tokio::fs::read(&tmp).await else { return };
                    let _ = tokio::fs::remove_file(&tmp).await;
                    let Some(to) = parse_jid(&jid) else { return };
                    let upload = match client
                        .upload(bytes, whatsapp_rust::wacore::download::MediaType::Audio, Default::default())
                        .await
                    {
                        Ok(u) => u,
                        Err(e) => {
                            eprintln!("[audio] voice upload failed: {e}");
                            return;
                        }
                    };
                    let mut message = whatsapp_rust::media::audio_message(
                        upload,
                        whatsapp_rust::media::AudioOptions {
                            ptt: Some(true),
                            duration_seconds: Some(seconds),
                            waveform: Some(wave),
                            ..Default::default()
                        },
                    );
                    if view_once {
                        message = wrap_view_once(message);
                    }
                    match client.send_message(to, message.clone()).await {
                        Ok(sent) => {
                            let id = sent.message_id;
                            ui_apply(move |b| b.echo_sent(&jid, &id, message));
                        }
                        Err(e) => eprintln!("[audio] voice send failed: {e}"),
                    }
                });
            }
            Cmd::FetchAvatar(jid) => {
                let client = session.client.clone();
                let sem = avatar_sem.clone();
                let key = media_key.clone();
                tokio::spawn(async move {
                    let path = crate::media::avatar_cache_path(&jid);
                    // Disk first: the sealed cache fills the list instantly;
                    // the network only refreshes stale entries.
                    let hit = {
                        let key = key.clone();
                        let path = path.clone();
                        tokio::task::spawn_blocking(move || {
                            let meta = std::fs::metadata(&path).ok()?;
                            let fresh = meta
                                .modified()
                                .ok()
                                .and_then(|m| m.elapsed().ok())
                                .map(|age| age.as_secs() < 7 * 24 * 3600)
                                .unwrap_or(false);
                            if meta.len() == 0 {
                                return Some((None, fresh)); // remembered "no picture"
                            }
                            let img = crate::media::read_cached(&key, &path)
                                .and_then(|b| crate::media::decode_cover(&b, 64))?;
                            Some((Some(img), fresh))
                        })
                        .await
                        .ok()
                        .flatten()
                    };
                    if let Some((img, fresh)) = hit {
                        let jid2 = jid.clone();
                        ui_apply(move |b| b.on_avatar(&jid2, img, true));
                        if fresh {
                            return;
                        }
                    }
                    let _permit = sem.acquire_owned().await;
                    let Some(target) = parse_jid(&jid) else { return };
                    let result = client.contacts().get_profile_picture(&target, true).await;
                    match result {
                        Ok(Some(pic)) => {
                            let url = pic.url.clone();
                            let img = tokio::task::spawn_blocking(move || {
                                let mut res = ureq::get(&url).call().ok()?;
                                let bytes = res.body_mut().read_to_vec().ok()?;
                                let img = crate::media::decode_cover(&bytes, 64)?;
                                let _ = std::fs::write(&path, key.encrypt_bytes(&bytes));
                                Some(img)
                            })
                            .await
                            .ok()
                            .flatten();
                            match img {
                                Some(img) => {
                                    ui_apply(move |b| b.on_avatar(&jid, Some(img), true))
                                }
                                // The URL fetch failed: transient, retryable.
                                None => ui_apply(move |b| b.on_avatar(&jid, None, false)),
                            }
                        }
                        // Confirmed: this jid has no picture (or hides it).
                        Ok(None) => {
                            let _ = std::fs::write(&path, []);
                            ui_apply(move |b| b.on_avatar(&jid, None, true));
                        }
                        Err(_) => ui_apply(move |b| b.on_avatar(&jid, None, false)),
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(25)).await;
                });
            }
            Cmd::Start | Cmd::Resume => {
                let client = session.client.clone();
                tokio::spawn(async move { client.run().await });
            }
            Cmd::PairWithCode(digits) => {
                let client = session.client.clone();
                tokio::spawn(async move {
                    let options = PairCodeOptions { phone_number: digits, ..Default::default() };
                    match client.pair_with_code(options).await {
                        Ok(code) => {
                            let pretty = format_pair_code(&code);
                            ui_apply(move |b| b.on_pairing_code(&pretty));
                        }
                        Err(e) => {
                            eprintln!("[wa] pair code request failed: {e}");
                            ui_apply(|b| b.on_pairing_failed());
                        }
                    }
                });
            }
            Cmd::SendText { jid, body, quote } => {
                let client = session.client.clone();
                tokio::spawn(async move {
                    use whatsapp_rust::proto_helpers::MessageBuilderExt as _;
                    let Some(to) = parse_jid(&jid) else { return };
                    let message = match &quote {
                        Some(q) => {
                            let Some(sender) = parse_jid(&q.sender_jid) else { return };
                            let ctx = whatsapp_rust::proto_helpers::build_quote_context_with_info(
                                q.id.clone(),
                                &sender,
                                &to,
                                &to,
                                &q.message,
                            );
                            wa::Message::text_with_context(body.clone(), ctx)
                        }
                        None => wa::Message::text(body.clone()),
                    };
                    match client.send_message(to, message.clone()).await {
                        Ok(sent) => {
                            let id = sent.message_id;
                            ui_apply(move |b| b.echo_sent(&jid, &id, message));
                        }
                        Err(e) => eprintln!("[wa] sendText failed: {e}"),
                    }
                });
            }
            Cmd::FetchHistory { jid, oldest_id, from_me, ts_ms } => {
                let client = session.client.clone();
                tokio::spawn(async move {
                    let Some(chat) = parse_jid(&jid) else { return };
                    let ok = client
                        .fetch_message_history(&chat, &oldest_id, from_me, ts_ms, 50)
                        .await
                        .is_ok();
                    ui_apply(move |b| b.on_history_requested(ok));
                });
            }
            Cmd::SubscribePresence(jid) => {
                let client = session.client.clone();
                tokio::spawn(async move {
                    if let Some(target) = parse_jid(&jid) {
                        let _ = client.presence().subscribe(target).await;
                    }
                });
            }
            Cmd::FetchGroups => {
                let client = session.client.clone();
                tokio::spawn(async move {
                    match client.groups().get_participating().await {
                        Ok(groups) => {
                            let list: Vec<(String, String, bool, Option<String>)> = groups
                                .iter()
                                .map(|(jid, meta)| {
                                    (
                                        jid.to_non_ad_string(),
                                        meta.subject.clone(),
                                        meta.is_parent_group,
                                        meta.parent_group_jid
                                            .as_ref()
                                            .map(|j| j.to_non_ad_string()),
                                    )
                                })
                                .collect();
                            ui_apply(move |b| b.on_groups(&list));
                        }
                        Err(e) => eprintln!("[wa] fetch groups failed: {e}"),
                    }
                });
            }
            Cmd::Logout => {
                // Dispatches Event::LoggedOut, which routes back here as
                // ResetSession.
                session.client.logout().await;
            }
            Cmd::HaltRetries => {
                session.client.disconnect().await;
                ui_apply(|b| b.on_fatal("offline"));
            }
            Cmd::ResetSession => {
                session.client.disconnect().await;
                drop(session);
                let path = wa_session_path();
                for suffix in ["", "-wal", "-shm"] {
                    let mut p = path.as_os_str().to_owned();
                    p.push(suffix);
                    let _ = std::fs::remove_file(std::path::PathBuf::from(p));
                }
                match build_session(cmd_tx.clone()).await {
                    Ok(next) => {
                        session = next;
                        ui_apply(|b| b.on_logged_out());
                        let client = session.client.clone();
                        tokio::spawn(async move { client.run().await });
                    }
                    Err(e) => {
                        eprintln!("[wa] session rebuild failed: {e}");
                        let msg = t("status.connectFailed");
                        ui_apply(move |b| b.on_status(&msg));
                        return;
                    }
                }
            }
            Cmd::Shutdown => {
                // Leaving a call ringing on the peer's phone after the app
                // is gone would be the worst kind of goodbye.
                if let Some(live) = take_live() {
                    let _ =
                        session.client.voip().terminate(&live.id, &live.peer, &live.creator).await;
                    live.handle.hangup_local().await;
                }
                session.client.disconnect().await;
                return;
            }
        }
    }
}

fn urlencode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

// Small jpeg preview embedded in outgoing image messages.
fn make_jpeg_thumb(data: &[u8]) -> Option<Vec<u8>> {
    let img = image::load_from_memory(data).ok()?;
    let small = img.resize(72, 72, image::imageops::FilterType::Triangle).to_rgb8();
    let mut out = Vec::new();
    let mut cursor = std::io::Cursor::new(&mut out);
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut cursor, 70)
        .encode_image(&small)
        .ok()?;
    Some(out)
}

// A view-once wrapper around any content message.
fn wrap_view_once(inner: wa::Message) -> wa::Message {
    use whatsapp_rust::waproto::buffa::MessageField;
    let mut wrapper = wa::message::FutureProofMessage::default();
    wrapper.message = MessageField::some(inner);
    let mut out = wa::Message::default();
    out.view_once_message_v2 = MessageField::some(wrapper);
    out
}

fn format_pair_code(code: &str) -> String {
    if code.len() == 8 { format!("{}-{}", &code[..4], &code[4..]) } else { code.to_string() }
}
