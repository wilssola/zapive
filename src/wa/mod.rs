// The WhatsApp side of the app. Owns the whatsapp-rust client on the
// tokio runtime; the UI talks to it through the Cmd channel, and events
// flow back through bridge::ui_apply. Mirrors src/whatsapp.ts on master.
// wa_apply parks events while the vault is still PIN-locked (the client
// connects before unlock) and replays them at boot.
use crate::bridge::wa_apply as ui_apply;
use crate::i18n::t;
use crate::paths::wa_session_path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use tokio::sync::mpsc;
use whatsapp_rust::async_channel;
use whatsapp_rust::bot::{Bot, BotHandle};
use whatsapp_rust::client::Client;
use whatsapp_rust::pair_code::PairCodeOptions;
use whatsapp_rust::store::SqliteStore;
use whatsapp_rust::types::call::{CallAction, ElsewhereOutcome, IncomingCall};
use whatsapp_rust::types::events::{Event, EventHandler};
use whatsapp_rust::types::presence::ReceiptType;
use whatsapp_rust::voip::{CallHandle, KeyframeUrgency, VideoFrame, VideoUpgradeToken};
use whatsapp_rust::waproto::whatsapp as wa;

mod attachments;
mod calls;
mod chats;
mod executor;
mod groups;
mod local;
mod media;
mod messaging;
mod polls;
mod profile;
mod pump;
mod session;
mod util;

use calls::*;
use executor::*;
use pump::*;
use session::*;
use util::*;

// After this many unclean drops in a row the retry loop stops and the
// user gets the "offline" modal instead.
const MAX_FAILURES: u32 = 5;

// Which session the UI is showing. Switching accounts swaps the client
// under a bridge that holds one account's conversations at a time, and
// an event of the account just left must never land in the store of the
// one just opened. Everything headed for the bridge is stamped with the
// epoch it was produced under and dropped if the epoch has moved on.
pub static SESSION_EPOCH: AtomicU64 = AtomicU64::new(0);

// What a reply quotes: enough to build the context info.
#[derive(Debug)]
pub struct QuoteRef {
    pub id: String,
    pub sender_jid: String,
    pub chat_jid: String,
    pub message: Arc<wa::Message>,
}

pub fn build_text(
    jid: &str,
    body: String,
    quote: &Option<QuoteRef>,
    mentions: Vec<String>,
    mention_all: Option<String>,
) -> wa::Message {
    use whatsapp_rust::proto_helpers::MessageBuilderExt as _;
    let mut ctx = quote.as_ref().and_then(|q| {
        let sender = parse_jid(&q.sender_jid)?;
        let source = parse_jid(&q.chat_jid)?;
        let to = parse_jid(jid)?;
        Some(whatsapp_rust::proto_helpers::build_quote_context_with_info(
            q.id.clone(),
            &sender,
            &source,
            &to,
            &q.message,
        ))
    });
    if !mentions.is_empty() || mention_all.is_some() {
        let mut info = ctx.take().unwrap_or_default();
        info.mentioned_jid = mentions;
        if let Some(subject) = mention_all {
            info.group_mentions = vec![wa::GroupMention {
                group_jid: Some(jid.into()),
                group_subject: Some(subject),
            }];
        }
        ctx = Some(info);
    }
    match ctx {
        Some(ctx) => wa::Message::text_with_context(body, ctx),
        None => wa::Message::text(body),
    }
}

pub(super) fn action_failed(epoch: u64, error: impl Into<String>) {
    let error = error.into();
    crate::bridge::wa_apply_at(epoch, move |b| b.message_action_failed(&error));
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
    // Just cache the file (audio, documents).
    File,
    // Cache the clip and decode its first picture for the bubble: the
    // thumbnail embedded in the message is about a hundred pixels wide.
    Video,
}

// What an outgoing file is sent as.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttachmentKind {
    Image,
    Document,
    Audio,
    Sticker,
}

impl AttachmentKind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Image => "image",
            Self::Document => "document",
            Self::Audio => "audio",
            Self::Sticker => "sticker",
        }
    }
}

pub enum Cmd {
    // Spawns the connection loop; sent once the bridge is installed so no
    // early event is lost.
    Start,
    PairWithCode(String),
    // From the fatal modal's button: reconnect after conflict/offline.
    Resume,
    Logout,
    // Closes the running session and opens another account's. Answered
    // with Bridge::on_account_ready once the new client exists; nothing
    // connects until the bridge has that account's store loaded and
    // sends Start.
    SwitchAccount(String),
    Shutdown,
    // `mentions` are the jids named as @user in the body; `mention_all`
    // carries the group subject when the body tags @all/@everyone.
    SendQueued {
        id: String,
        jid: String,
        message: Arc<wa::Message>,
    },
    PrepareAttachment(crate::outbox::Preparation),
    FetchHistory {
        jid: String,
        oldest_id: String,
        from_me: bool,
        ts_ms: i64,
    },
    // Group roster and settings, for the composer and the info panel.
    FetchGroupMeta(String),
    // Notifications: `until_ms` 0 mutes for good.
    Mute {
        jid: String,
        until_ms: i64,
    },
    Unmute(String),
    // Disappearing messages; `secs` 0 turns them off.
    SetEphemeral {
        jid: String,
        group: bool,
        secs: u32,
    },
    LeaveGroup(String),
    AddMembers {
        jid: String,
        members: Vec<String>,
    },
    // App-state chat actions mirrored to the phone.
    ClearChat(String),
    DeleteChat(String),
    DeleteForMe {
        jid: String,
        id: String,
        from_me: bool,
        participant: Option<String>,
        ts: i64,
    },
    LabelCreate {
        id: String,
        name: String,
        color: i32,
        order: i32,
    },
    LabelDelete(String),
    LabelChat {
        label_id: String,
        jid: String,
        on: bool,
    },
    // The whole favorites list, the way WhatsApp syncs it.
    Favorites(Vec<String>),
    SubscribePresence(String),
    FetchGroups,
    // Media pipeline: the vault key arrives once the vault unlocks.
    MediaKey(crate::vault::KeyHandle),
    Media {
        id: String,
        mimetype: String,
        message: std::sync::Arc<wa::Message>,
        want: MediaWant,
    },
    // Decodes an embedded thumbnail (link previews, video posters).
    DecodeThumb {
        id: String,
        bytes: Vec<u8>,
        link: bool,
    },
    DecodePreview {
        id: String,
        bytes: Vec<u8>,
    },
    LinkPreview {
        jid: String,
        id: String,
        text: String,
        url: String,
    },
    FetchAvatar(String),
    // The full-size picture for the info panel.
    AvatarLarge(String),
    // Downloads pdfium into the data directory for the PDF viewer.
    PdfEngineInstall,
    // First frame of a video status, for the viewer.
    StatusPoster {
        id: String,
        path: std::path::PathBuf,
    },
    // The full-resolution decode of a cached photo, for the lightbox.
    DecodeFull {
        id: String,
        path: std::path::PathBuf,
    },
    // Recent group messages, bundled for someone who just joined.
    ShareHistory {
        group: String,
        member: String,
        messages: Vec<wa::WebMessageInfo>,
    },
    // A link preview's real thumbnail from the CDN; `fallback` is the
    // inline jpeg used when the download fails.
    LinkThumb {
        id: String,
        direct_path: String,
        media_key: Vec<u8>,
        sha256: Vec<u8>,
        enc_sha256: Vec<u8>,
        fallback: Vec<u8>,
    },
    // Voice notes: full decode at a speed, waveform bars, send recording.
    AudioDecode {
        id: String,
        plain: std::path::PathBuf,
        rate_idx: usize,
        rate: f64,
    },
    Waveform {
        id: String,
        path: std::path::PathBuf,
    },
    SendVoice {
        jid: String,
        samples: Vec<f32>,
        in_rate: u32,
        view_once: bool,
    },
    // Message actions.
    React {
        jid: String,
        id: String,
        from_me: bool,
        participant: Option<String>,
        emoji: String,
    },
    Revoke {
        jid: String,
        id: String,
    },
    Edit {
        jid: String,
        id: String,
        text: String,
    },
    PinMessage {
        jid: String,
        id: String,
        from_me: bool,
        participant: Option<String>,
        pin: bool,
    },
    MarkUnread(String),
    // Polls. `selectable` is 0 for "any number" and 1 for single choice.
    // The secret that comes back is stored with the poll so votes open.
    CreatePoll {
        jid: String,
        question: String,
        options: Vec<String>,
        selectable: u32,
    },
    // A received vote, still encrypted. The bridge supplies the poll's
    // secret; decrypting happens here because it needs the LID/PN map.
    DecryptVote {
        jid: String,
        poll_id: String,
        creator: String,
        voter: String,
        secret: Vec<u8>,
        payload: Vec<u8>,
        iv: Vec<u8>,
        timestamp_ms: i64,
    },
    // Replaces my pick with exactly `options` (names; empty clears it).
    PollVote {
        jid: String,
        id: String,
        creator: String,
        secret: Vec<u8>,
        options: Vec<String>,
    },
    Star {
        jid: String,
        id: String,
        from_me: bool,
        participant: Option<String>,
        starred: bool,
    },
    // "Send message viewed" (opt-in, off by default): blue ticks and the
    // blue mic. `sender` is the group participant a receipt is addressed
    // to (None in a DM), matching Client::mark_as_read/mark_as_played.
    MarkRead {
        jid: String,
        sender: Option<String>,
        ids: Vec<String>,
    },
    MarkPlayed {
        jid: String,
        sender: Option<String>,
        id: String,
    },
    Forward {
        jid: String,
        message: std::sync::Arc<wa::Message>,
    },
    // Re-sends a sticker/GIF already on the CDN, reusing its media
    // reference to skip a re-upload -- but as a fresh message, not a
    // forward: unlike Cmd::Forward, this never sets `is_forwarded`, so
    // picking a sticker from the recents/favorites tray doesn't show a
    // "Forwarded" tag (WhatsApp itself never tags that as a forward).
    ResendMedia {
        jid: String,
        message: std::sync::Arc<wa::Message>,
    },
    // Outgoing media (paths are plain files picked by the user). `origin` is
    // that same file as shown in the row, which then reopens it instead of
    // downloading the copy that just went up.
    SendFile {
        jid: String,
        path: std::path::PathBuf,
        kind: AttachmentKind,
        origin: Option<String>,
        caption: Option<String>,
    },
    // Stickers and GIFs (the video player runs its own threads).
    GifSearch(String),
    SendGifUrl {
        jid: String,
        url: String,
    },
    ZoomFrames {
        id: String,
        path: std::path::PathBuf,
    },
    // Self-update.
    CheckUpdate,
    ApplyUpdate,
    // Calls, channels and the info panel.
    RejectCall {
        id: String,
        from: String,
    },
    // Answers the parked offer with that id and drives the media plane.
    // `video` opens the camera as part of answering.
    AcceptCall {
        id: String,
        video: bool,
    },
    // Places a 1:1 call; `video` starts it as a video call.
    StartCall {
        jid: String,
        video: bool,
    },
    // Ends whichever call is live, telling the peer.
    HangupCall,
    SetCallMuted(bool),
    // Turns the camera on or off mid-call. Turning it on also answers a
    // pending upgrade request from the peer.
    SetCallVideo(bool),
    FetchChannel(String),
    FetchChatInfo {
        jid: String,
        group: bool,
    },
    FetchCommonGroups {
        jid: String,
        identities: Vec<String>,
    },
    Archive {
        jid: String,
        archived: bool,
    },
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

// A group's roster and settings, flattened for the UI thread.
pub struct GroupSnapshot {
    pub jid: String,
    pub subject: String,
    pub desc: String,
    pub announce: bool,
    pub ephemeral: u32,
    pub members: Vec<crate::store::Member>,
}

// One action out of a group notification, already flattened.
pub struct GroupChange {
    pub jid: String,
    pub actor: Option<String>,
    pub actor_pn: Option<String>,
    pub ts: i64,
    // add | remove | promote | demote | subject | description | announce |
    // not_announce | ephemeral | locked | unlocked | delete | invite | leave
    pub kind: &'static str,
    // (jid, phone-number jid) of every participant the action names.
    pub who: Vec<(String, Option<String>)>,
    pub text: String,
    pub number: u32,
    // How members were added ("invite" = through the group's link).
    pub reason: String,
}

#[derive(Clone)]
pub struct WaService {
    cmd_tx: mpsc::UnboundedSender<Cmd>,
}

impl WaService {
    // Builds the client (blocking briefly on the runtime to open the
    // store) and reports whether a paired session already exists.
    pub fn start(rt: &tokio::runtime::Runtime, account: &str) -> Result<(Self, bool), String> {
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        let session = rt
            .block_on(build_session(cmd_tx.clone(), account))
            .map_err(|e| e.to_string())?;
        let registered = session.client.pn().is_some();
        rt.spawn(executor(session, cmd_rx, cmd_tx.clone()));
        Ok((Self { cmd_tx }, registered))
    }

    pub fn send(&self, cmd: Cmd) {
        let _ = self.cmd_tx.send(cmd);
    }
}

// ---- call plumbing ----
