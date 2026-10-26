// wa::Message -> StoredMessage normalization: the getContentType half of
// store.ts normalize() on master, shared by the live path (InboundMessage)
// and the history-sync path (WebMessageInfo).
use crate::store::{
    MessageKind, Store, StoredMessage, clean_text, display_id, format_duration,
    is_displayable_jid, normalize_jid, quoted_summary,
};
use crate::i18n::{t, ta};
use std::collections::HashMap;
use std::sync::Arc;
use whatsapp_rust::types::events::InboundMessage;
use whatsapp_rust::waproto::whatsapp as wa;

// Learns a LID<->PN pair when both identities of the same peer appear.
fn learn_pair(store: &mut Store, a: &str, b: &str) {
    let lid = if a.ends_with("@lid") {
        Some(a)
    } else if b.ends_with("@lid") {
        Some(b)
    } else {
        None
    };
    let pn = if a.ends_with("@s.whatsapp.net") {
        Some(a)
    } else if b.ends_with("@s.whatsapp.net") {
        Some(b)
    } else {
        None
    };
    if let (Some(lid), Some(pn)) = (lid, pn) {
        store.learn_alias(lid, pn);
    }
}

// The contextInfo of whichever content variant the message carries.
fn context_of(content: &wa::Message) -> Option<&wa::ContextInfo> {
    if let Some(ext) = content.extended_text_message.as_option() {
        return ext.context_info.as_option();
    }
    macro_rules! ctx {
        ($($field:ident),*) => {
            $(if let Some(inner) = content.$field.as_option()
                && let Some(ci) = inner.context_info.as_option() { return Some(ci); })*
        };
    }
    ctx!(image_message, video_message, audio_message, document_message, sticker_message);
    None
}

pub struct LiveMeta<'a> {
    pub chat: String,
    pub sender: String,
    pub sender_alt: Option<String>,
    pub recipient_alt: Option<String>,
    pub id: &'a str,
    pub from_me: bool,
    pub is_group: bool,
    pub push_name: &'a str,
    pub timestamp: i64,
}

pub fn from_live(store: &mut Store, inbound: &InboundMessage) -> Option<StoredMessage> {
    let info = &inbound.info;
    let src = &info.source;
    let meta = LiveMeta {
        chat: src.chat.to_non_ad_string(),
        sender: src.sender.to_non_ad_string(),
        sender_alt: src.sender_alt.as_ref().map(|j| j.to_non_ad_string()),
        recipient_alt: src.recipient_alt.as_ref().map(|j| j.to_non_ad_string()),
        id: &info.id,
        from_me: src.is_from_me,
        is_group: src.is_group,
        push_name: &info.push_name,
        timestamp: info.timestamp.timestamp(),
    };
    normalize(store, &meta, &inbound.message, &HashMap::new(), 0)
}

// The person a notice talks about: "You" for one of our own identities,
// otherwise the best name we have.
pub fn notice_name(store: &Store, jid: &str) -> String {
    let canon = store.canon_owned(&normalize_jid(jid));
    if store.self_jids.contains(&canon) || store.self_jids.contains(jid) {
        return t("reactions.you");
    }
    let name = store.chat_name(&canon);
    if name.is_empty() { display_id(&canon) } else { name }
}

fn join_names(names: &[String]) -> String {
    match names.len() {
        0 => String::new(),
        1 => names[0].clone(),
        n => format!("{} {} {}", names[..n - 1].join(", "), t("notice.and"), names[n - 1]),
    }
}

pub fn ephemeral_label(secs: u32) -> String {
    match secs {
        0 => t("ephemeral.off"),
        86_400 => t("ephemeral.day"),
        604_800 => t("ephemeral.week"),
        7_776_000 => t("ephemeral.quarter"),
        s if s % 86_400 == 0 => ta("ephemeral.days", &[&(s / 86_400).to_string()]),
        s if s % 3600 == 0 => ta("ephemeral.hours", &[&(s / 3600).to_string()]),
        s => ta("ephemeral.minutes", &[&(s / 60).to_string()]),
    }
}

// The sentence a group event shows in the conversation, in WhatsApp's
// wording. `actor` is who did it; `who` are the people it names.
pub fn group_notice(
    store: &Store,
    kind: &str,
    actor: Option<&str>,
    who: &[String],
    text: &str,
    number: u32,
) -> String {
    let me = t("reactions.you");
    let actor_name = actor.map(|a| notice_name(store, a)).unwrap_or_default();
    let actor_is_me = actor_name == me;
    let names: Vec<String> = who.iter().map(|w| notice_name(store, w)).collect();
    let names_me = names.len() == 1 && names[0] == me;
    let people = join_names(&names);
    let actor_or_someone = if actor_name.is_empty() { t("notice.someone") } else { actor_name.clone() };
    // Roster churn that does not involve us collapses into one line that
    // opens the group info, the way WhatsApp keeps busy groups readable.
    let self_leave = kind == "leave"
        || (kind == "remove" && actor.is_some() && who.len() == 1 && actor_name == names[0]);
    // Someone coming in through the link gets their own line: it is
    // also the handle for sharing the group's recent history with them.
    if kind == "invite" && !names_me {
        return ta("notice.joinedLinkShare", &[&people]);
    }
    if matches!(kind, "add" | "invite" | "remove" | "promote" | "demote")
        && !self_leave
        && !names_me
        && !actor_is_me
    {
        return t("notice.membersChanged");
    }
    match kind {
        "add" => {
            if names_me {
                ta("notice.addedYou", &[&actor_or_someone])
            } else if actor.is_some() && who.len() == 1 && actor_name == names[0] {
                ta("notice.joinedLink", &[&people])
            } else if actor_is_me {
                ta("notice.youAdded", &[&people])
            } else {
                ta("notice.added", &[&actor_or_someone, &people])
            }
        }
        "invite" => ta("notice.joinedLink", &[&people]),
        "remove" | "leave" => {
            if actor.is_some() && who.len() == 1 && actor_name == names[0] {
                if names_me { t("notice.youLeft") } else { ta("notice.left", &[&people]) }
            } else if names_me {
                ta("notice.removedYou", &[&actor_or_someone])
            } else if actor_is_me {
                ta("notice.youRemoved", &[&people])
            } else {
                ta("notice.removed", &[&actor_or_someone, &people])
            }
        }
        "promote" => {
            if names_me { t("notice.youAdmin") } else { ta("notice.promoted", &[&people]) }
        }
        "demote" => {
            if names_me { t("notice.youNotAdmin") } else { ta("notice.demoted", &[&people]) }
        }
        "subject" => ta("notice.subject", &[&actor_or_someone, text]),
        "description" => ta("notice.description", &[&actor_or_someone]),
        "icon" => ta("notice.icon", &[&actor_or_someone]),
        "create" => ta("notice.created", &[&actor_or_someone, text]),
        "announce" => ta("notice.announce", &[&actor_or_someone]),
        "not_announce" => ta("notice.notAnnounce", &[&actor_or_someone]),
        "locked" => ta("notice.locked", &[&actor_or_someone]),
        "unlocked" => ta("notice.unlocked", &[&actor_or_someone]),
        "ephemeral" => {
            if number == 0 {
                ta("notice.ephemeralOff", &[&actor_or_someone])
            } else {
                ta("notice.ephemeralOn", &[&actor_or_someone, &ephemeral_label(number)])
            }
        }
        "delete" => t("notice.deleted"),
        "missedVoice" => t("notice.missedVoice"),
        "missedVideo" => t("notice.missedVideo"),
        _ => String::new(),
    }
}

// A group event stored in the history payload as a stub (no content, a
// type and parameters), turned into a centered notice.
fn stub_system(store: &mut Store, web: &wa::WebMessageInfo) -> Option<StoredMessage> {
    use wa::web_message_info::StubType as S;
    let stub = web.message_stub_type?;
    let key = web.key.as_option()?;
    let chat = store.canon_owned(&normalize_jid(key.remote_jid.as_deref()?));
    if !is_displayable_jid(&chat) {
        return None;
    }
    let actor: Option<String> = if key.from_me.unwrap_or(false) {
        store.self_jids.iter().next().cloned()
    } else {
        key.participant.as_deref().map(normalize_jid).or_else(|| Some(chat.clone()))
    };
    let params = &web.message_stub_parameters;
    let jids: Vec<String> = params
        .iter()
        .filter(|p| p.contains('@'))
        .map(|p| normalize_jid(p))
        .collect();
    let first = params.first().map(String::as_str).unwrap_or("");
    let (kind, text, number) = match stub {
        S::GROUP_PARTICIPANT_ADD => ("add", "", 0),
        S::GROUP_PARTICIPANT_INVITE => ("invite", "", 0),
        S::GROUP_PARTICIPANT_REMOVE => ("remove", "", 0),
        S::GROUP_PARTICIPANT_LEAVE => ("leave", "", 0),
        S::GROUP_PARTICIPANT_PROMOTE => ("promote", "", 0),
        S::GROUP_PARTICIPANT_DEMOTE => ("demote", "", 0),
        S::GROUP_CHANGE_SUBJECT => ("subject", first, 0),
        S::GROUP_CHANGE_DESCRIPTION => ("description", "", 0),
        S::GROUP_CHANGE_ICON => ("icon", "", 0),
        S::GROUP_CREATE => ("create", first, 0),
        S::GROUP_CHANGE_ANNOUNCE => {
            if first == "on" { ("announce", "", 0) } else { ("not_announce", "", 0) }
        }
        S::GROUP_CHANGE_RESTRICT => {
            if first == "on" { ("locked", "", 0) } else { ("unlocked", "", 0) }
        }
        S::CHANGE_EPHEMERAL_SETTING => ("ephemeral", "", first.parse::<u32>().unwrap_or(0)),
        S::GROUP_DELETE => ("delete", "", 0),
        S::CALL_MISSED_VOICE | S::CALL_MISSED_GROUP_VOICE => ("missedVoice", "", 0),
        S::CALL_MISSED_VIDEO | S::CALL_MISSED_GROUP_VIDEO => ("missedVideo", "", 0),
        _ => return None,
    };
    // Leaving names oneself; the stub carries the actor as the leaver.
    let who: Vec<String> = if kind == "leave" && jids.is_empty() {
        actor.clone().into_iter().collect()
    } else {
        jids
    };
    let body = group_notice(store, kind, actor.as_deref(), &who, text, number);
    if body.is_empty() {
        return None;
    }
    Some(system_message(&chat, key.id.as_deref()?, web.message_timestamp.unwrap_or(0) as i64, body))
}

pub fn system_message(chat: &str, id: &str, timestamp: i64, text: String) -> StoredMessage {
    StoredMessage {
        id: id.to_string(),
        jid: chat.to_string(),
        kind: MessageKind::System,
        text,
        from_me: false,
        sender: String::new(),
        sender_jid: String::new(),
        forwarded: false,
        deleted: false,
        revoked: false,
        read_sent: false,
        gif: false,
        starred: false,
        sticker: false,
        mentions: Vec::new(),
        mentions_me: false,
        quote_id: String::new(),
        quote_author: String::new(),
        quote_text: String::new(),
        link_title: String::new(),
        link_desc: String::new(),
        link_url: String::new(),
        timestamp,
        mimetype: String::new(),
        media_w: 0,
        media_h: 0,
        duration_sec: 0,
        status: 0,
        reactions: HashMap::new(),
        raw: None,
    }
}

pub fn from_history(store: &mut Store, web: &wa::WebMessageInfo) -> Option<StoredMessage> {
    if web.message_stub_type.is_some_and(|s| s != wa::web_message_info::StubType::UNKNOWN)
        && web.message.as_option().is_none()
    {
        return stub_system(store, web);
    }
    let key = web.key.as_option()?;
    let chat = normalize_jid(key.remote_jid.as_deref()?);
    let from_me = key.from_me.unwrap_or(false);
    let participant = key.participant.as_deref().map(normalize_jid);
    let sender = participant.clone().unwrap_or_else(|| chat.clone());
    // History-synced messages carry their accumulated reactions inline.
    let mut reactions: HashMap<String, String> = HashMap::new();
    for r in &web.reactions {
        let Some(text) = r.text.as_deref().filter(|t| !t.is_empty()) else { continue };
        let reactor = r
            .key
            .as_option()
            .and_then(|k| k.participant.as_deref().or(k.remote_jid.as_deref()))
            .map(normalize_jid)
            .unwrap_or_else(|| "?".to_string());
        reactions.insert(reactor, clean_text(text));
    }
    let status = web.status.map(|s| s as u32).unwrap_or(0);
    let meta = LiveMeta {
        chat,
        sender,
        sender_alt: None,
        recipient_alt: None,
        id: key.id.as_deref()?,
        from_me,
        is_group: false, // only used for push-name learning on the live path
        push_name: web.push_name.as_deref().unwrap_or(""),
        timestamp: web.message_timestamp.unwrap_or(0) as i64,
    };
    normalize(store, &meta, web.message.as_option()?, &reactions, status)
}

pub fn normalize(
    store: &mut Store,
    meta: &LiveMeta,
    message: &wa::Message,
    reactions: &HashMap<String, String>,
    status: u32,
) -> Option<StoredMessage> {
    use whatsapp_rust::proto_helpers::MessageExt as _;

    // LID addressing: the alt identity of the same peer collapses the
    // lid/pn versions of a chat into one.
    if let Some(alt) = &meta.sender_alt {
        learn_pair(store, &meta.sender, alt);
    }
    if let Some(alt) = &meta.recipient_alt {
        learn_pair(store, &meta.chat, alt);
    }

    let jid = store.canon_owned(&meta.chat);
    if !is_displayable_jid(&jid) || meta.id.is_empty() {
        return None;
    }

    // Disappearing/view-once wrappers hide the real content one level down.
    let content = message.get_base_message();

    let sender_jid = store.canon_owned(&meta.sender);
    if !meta.from_me && !meta.push_name.is_empty() {
        store.push_names.insert(sender_jid.clone(), meta.push_name.to_string());
    }
    let sender = if meta.from_me {
        String::new()
    } else {
        clean_text(
            store
                .contacts
                .get(&sender_jid)
                .or_else(|| store.chats.get(&sender_jid).filter(|c| !c.name.is_empty()).map(|c| &c.name))
                .or_else(|| store.push_names.get(&sender_jid))
                .cloned()
                .unwrap_or_else(|| display_id(&sender_jid))
                .as_str(),
        )
    };

    let ctx = context_of(content);
    let forwarded = ctx.and_then(|c| c.is_forwarded).unwrap_or(false);

    // "@number" mentions list us explicitly; "@all"/"@everyone" arrives as
    // a group mention that targets every participant.
    let mentions_me = !meta.from_me
        && ctx
            .map(|c| {
                !c.group_mentions.is_empty()
                    || c.mentioned_jid.iter().any(|j| {
                        let canon = store.canon_owned(&normalize_jid(j));
                        store.self_jids.contains(&canon)
                    })
            })
            .unwrap_or(false);

    // A reply carries a copy of what it answers; enough of it to draw the
    // quoted strip without looking the original up.
    let (quote_id, quote_author, quote_text) = match ctx.and_then(|c| c.quoted_message.as_option()) {
        Some(quoted) => (
            ctx.and_then(|c| c.stanza_id.clone()).unwrap_or_default(),
            ctx.and_then(|c| c.participant.as_deref())
                .map(|p| store.canon_owned(&normalize_jid(p)))
                .unwrap_or_default(),
            quoted_summary(quoted),
        ),
        None => Default::default(),
    };

    let mut stored = StoredMessage {
        id: meta.id.to_string(),
        jid,
        kind: MessageKind::Text,
        text: String::new(),
        from_me: meta.from_me,
        sender,
        sender_jid,
        forwarded,
        deleted: false,
        revoked: false,
        read_sent: false,
        gif: false,
        starred: false,
        sticker: false,
        mentions: Vec::new(),
        mentions_me,
        quote_id,
        quote_author,
        quote_text,
        link_title: String::new(),
        link_desc: String::new(),
        link_url: String::new(),
        timestamp: meta.timestamp,
        mimetype: String::new(),
        media_w: 0,
        media_h: 0,
        duration_sec: 0,
        status: if meta.from_me { status.max(2) } else { 0 },
        reactions: reactions.clone(),
        raw: Some(Arc::new(message.clone())),
    };

    if let Some(text) = &content.conversation {
        stored.text = clean_text(text);
        return Some(stored);
    }
    if let Some(ext) = content.extended_text_message.as_option() {
        stored.text = clean_text(ext.text.as_deref().unwrap_or(""));
        stored.mentions = ctx
            .map(|c| c.mentioned_jid.iter().map(|j| j.to_string()).collect())
            .unwrap_or_default();
        // Sites that expose no metadata (private pages, login walls) send
        // back just the URL; WhatsApp shows no card for those, and neither
        // do we.
        let url = ext.matched_text.as_deref().unwrap_or("");
        let link_title = clean_text(ext.title.as_deref().unwrap_or(""));
        let link_desc = clean_text(ext.description.as_deref().unwrap_or(""));
        let has_thumb = ext.jpeg_thumbnail.as_ref().is_some_and(|t| !t.is_empty());
        let host = url
            .trim_start_matches("https://")
            .trim_start_matches("http://")
            .split('/')
            .next()
            .unwrap_or("");
        let informative = has_thumb
            || (!link_title.is_empty() && link_title != host && !url.contains(&link_title))
            || (!link_desc.is_empty() && link_desc != url && link_desc != host);
        if informative && !url.is_empty() {
            stored.link_title = link_title;
            stored.link_desc = link_desc;
            stored.link_url = url.to_string();
        }
        return Some(stored);
    }
    if let Some(sticker) = content.sticker_message.as_option() {
        stored.kind = MessageKind::Image;
        stored.sticker = true;
        stored.media_w = sticker.width.unwrap_or(180);
        stored.media_h = sticker.height.unwrap_or(180);
        stored.mimetype = sticker.mimetype.clone().unwrap_or_else(|| "image/webp".into());
        return Some(stored);
    }
    if let Some(v) = content.video_message.as_option() {
        stored.kind = MessageKind::Video;
        stored.text = clean_text(v.caption.as_deref().unwrap_or(""));
        stored.media_w = v.width.unwrap_or(0);
        stored.media_h = v.height.unwrap_or(0);
        stored.mimetype = v.mimetype.clone().unwrap_or_else(|| "video/mp4".into());
        stored.duration_sec = v.seconds.unwrap_or(0);
        stored.gif = v.gif_playback.unwrap_or(false);
        return Some(stored);
    }
    if let Some(img) = content.image_message.as_option() {
        stored.kind = MessageKind::Image;
        stored.text = clean_text(img.caption.as_deref().unwrap_or(""));
        stored.media_w = img.width.unwrap_or(0);
        stored.media_h = img.height.unwrap_or(0);
        stored.mimetype = img.mimetype.clone().unwrap_or_else(|| "image/jpeg".into());
        return Some(stored);
    }
    if let Some(doc) = content.document_message.as_option() {
        stored.kind = MessageKind::Doc;
        stored.text = clean_text(doc.file_name.as_deref().unwrap_or(&t("doc.fallbackName")));
        stored.mimetype =
            doc.mimetype.clone().unwrap_or_else(|| "application/octet-stream".into());
        return Some(stored);
    }
    if let Some(audio) = content.audio_message.as_option() {
        let seconds = audio.seconds.unwrap_or(0);
        stored.kind = MessageKind::Audio;
        stored.text = format_duration(seconds);
        stored.mimetype = audio.mimetype.clone().unwrap_or_else(|| "audio/ogg".into());
        stored.duration_sec = seconds;
        return Some(stored);
    }
    None
}

// A status update (status@broadcast) as a StoredMessage keyed by author.
pub fn status_entry(store: &mut Store, inbound: &InboundMessage) -> Option<StoredMessage> {
    let info = &inbound.info;
    let author = store.canon_owned(&info.source.sender.to_non_ad_string());
    if author.is_empty() {
        return None;
    }
    if !info.push_name.is_empty() {
        store.contacts.insert(author.clone(), info.push_name.to_string());
    }
    let content = {
        use whatsapp_rust::proto_helpers::MessageExt as _;
        inbound.message.get_base_message()
    };
    let (kind, text) = if let Some(text) = &content.conversation {
        (MessageKind::Text, clean_text(text))
    } else if let Some(ext) = content.extended_text_message.as_option() {
        (MessageKind::Text, clean_text(ext.text.as_deref().unwrap_or("")))
    } else if let Some(img) = content.image_message.as_option() {
        (MessageKind::Image, clean_text(img.caption.as_deref().unwrap_or("")))
    } else if let Some(v) = content.video_message.as_option() {
        // Rendered from the embedded jpeg thumbnail.
        (MessageKind::Image, clean_text(v.caption.as_deref().unwrap_or("")))
    } else {
        return None;
    };
    Some(StoredMessage {
        id: info.id.to_string(),
        jid: author.clone(),
        kind,
        text,
        from_me: info.source.is_from_me,
        sender: store.contacts.get(&author).cloned().unwrap_or_else(|| display_id(&author)),
        sender_jid: author,
        forwarded: false,
        deleted: false,
        revoked: false,
        read_sent: false,
        gif: false,
        starred: false,
        sticker: false,
        mentions: Vec::new(),
        mentions_me: false,
        quote_id: String::new(),
        quote_author: String::new(),
        quote_text: String::new(),
        link_title: String::new(),
        link_desc: String::new(),
        link_url: String::new(),
        timestamp: info.timestamp.timestamp(),
        mimetype: String::new(),
        media_w: 0,
        media_h: 0,
        duration_sec: 0,
        status: 0,
        reactions: HashMap::new(),
        raw: Some(inbound.message.clone()),
    })
}
