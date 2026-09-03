// The WhatsApp side of the app. Owns the whatsapp-rust client on the
// tokio runtime; the UI talks to it through the Cmd channel, and events
// flow back through bridge::ui_apply. Mirrors src/whatsapp.ts on master.
use crate::bridge::ui_apply;
use crate::i18n::t;
use crate::paths::wa_session_path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use tokio::sync::mpsc;
use whatsapp_rust::bot::{Bot, BotHandle};
use whatsapp_rust::client::Client;
use whatsapp_rust::pair_code::PairCodeOptions;
use whatsapp_rust::store::SqliteStore;
use whatsapp_rust::types::events::{Event, EventHandler};
use whatsapp_rust::types::presence::ReceiptType;
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
            Event::HistorySync(lazy) => {
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
            Event::PushNameUpdate(u) => {
                let jid = u.jid.to_non_ad_string();
                let name = u.new_push_name.clone();
                ui_apply(move |b| b.on_push_name(&jid, &name));
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
    let avatar_sem = Arc::new(tokio::sync::Semaphore::new(3));
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
                tokio::spawn(async move {
                    let _permit = sem.acquire_owned().await;
                    let Some(target) = parse_jid(&jid) else { return };
                    let result = client.contacts().get_profile_picture(&target, true).await;
                    match result {
                        Ok(Some(pic)) => {
                            let url = pic.url.clone();
                            let img = tokio::task::spawn_blocking(move || {
                                let mut res = ureq::get(&url).call().ok()?;
                                let bytes = res.body_mut().read_to_vec().ok()?;
                                crate::media::decode_cover(&bytes, 96)
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
                        Ok(None) => ui_apply(move |b| b.on_avatar(&jid, None, true)),
                        Err(_) => ui_apply(move |b| b.on_avatar(&jid, None, false)),
                    }
                    // Pace the workers like the Node build's 60ms gap.
                    tokio::time::sleep(std::time::Duration::from_millis(60)).await;
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
                session.client.disconnect().await;
                return;
            }
        }
    }
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
