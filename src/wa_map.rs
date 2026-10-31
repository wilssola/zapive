// wa::Message -> StoredMessage normalization: the getContentType half of
// store.ts normalize() on master, shared by the live path (InboundMessage)
// and the history-sync path (WebMessageInfo).
use crate::store::{
    MessageKind, Store, StoredMessage, clean_text, display_id, format_duration,
    is_displayable_jid, normalize_jid, quoted_summary,
};
use crate::i18n::{t, ta, tp};
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
    ctx!(image_message, video_message, audio_message, document_message, sticker_message,
        contact_message, contacts_array_message, location_message, live_location_message,
        poll_creation_message, poll_creation_message_v2, poll_creation_message_v3,
        poll_creation_message_v5, poll_creation_message_v6);
    None
}

fn display_content(message: &wa::Message) -> Option<&wa::Message> {
    let mut current = message;
    for _ in 0..32 {
        if current.view_once_message.is_set()
            || current.view_once_message_v2.is_set()
            || current.view_once_message_v2_extension.is_set()
            || current.image_message.as_option().and_then(|m| m.view_once) == Some(true)
            || current.video_message.as_option().and_then(|m| m.view_once) == Some(true)
            || current.ptv_message.as_option().and_then(|m| m.view_once) == Some(true)
            || current.audio_message.as_option().and_then(|m| m.view_once) == Some(true)
            || current.extended_text_message.as_option().and_then(|m| m.view_once) == Some(true)
        {
            return None;
        }
        let next = current.device_sent_message.as_option().and_then(|m| m.message.as_option())
            .or_else(|| current.ephemeral_message.as_option().and_then(|m| m.message.as_option()))
            .or_else(|| current.document_with_caption_message.as_option().and_then(|m| m.message.as_option()))
            .or_else(|| current.edited_message.as_option().and_then(|m| m.message.as_option()))
            .or_else(|| current.poll_creation_message_v4.as_option().and_then(|m| m.message.as_option()));
        match next {
            Some(inner) => current = inner,
            None => return Some(current),
        }
    }
    None
}

fn card_text(value: &str) -> String {
    clean_text(value).chars().filter(|c| !c.is_control()).take(2048).collect::<String>().trim().to_string()
}

fn contact_parts(contact: &wa::message::ContactMessage) -> (String, Vec<String>) {
    let mut name = card_text(contact.display_name.as_deref().unwrap_or(""));
    let mut phones = Vec::new();
    for line in contact.vcard.as_deref().unwrap_or("").lines().take(512) {
        let Some((key, value)) = line.split_once(':') else { continue };
        let property = key.split(';').next().unwrap_or("").rsplit('.').next().unwrap_or("");
        if property.eq_ignore_ascii_case("FN") && name.is_empty() {
            name = card_text(value);
        }
        if property.eq_ignore_ascii_case("TEL") {
            let value = value.trim();
            let value = value.strip_prefix("tel:").unwrap_or(value);
            if value.len() <= 64
                && value.chars().any(|c| c.is_ascii_digit())
                && value.chars().all(|c| c.is_ascii_digit() || matches!(c, '+' | ' ' | '-' | '(' | ')' | '.'))
                && !phones.iter().any(|p| p == value)
            {
                phones.push(value.to_string());
            }
        }
    }
    (name, phones)
}

fn contact_card(contact: &wa::message::ContactMessage) -> String {
    let (name, phones) = contact_parts(contact);
    let mut lines = vec![t("compat.contact")];
    if !name.is_empty() { lines.push(name); }
    lines.extend(phones);
    lines.join("\n")
}

fn location_card(lat: Option<f64>, lon: Option<f64>, live: bool, details: &[&str]) -> String {
    let mut lines = vec![t(if live { "compat.live_location" } else { "compat.location" })];
    lines.extend(details.iter().map(|s| card_text(s)).filter(|s| !s.is_empty()));
    match (lat, lon) {
        (Some(lat), Some(lon)) if lat.is_finite() && lon.is_finite()
            && (-90.0..=90.0).contains(&lat) && (-180.0..=180.0).contains(&lon) => {
                lines.push(format!("{lat:.6}, {lon:.6}"));
            }
        _ => lines.push(t("compat.invalid_location")),
    }
    lines.join("\n")
}

#[derive(Default, Debug)]
pub struct SpecialCard<'a> {
    pub kind: &'static str,
    pub title: String,
    pub body: String,
    pub detail: String,
    pub options: Vec<String>,
    pub thumbnail: Option<&'a [u8]>,
}

fn titled_card(kind: &'static str, title: &str, body: &str) -> SpecialCard<'static> {
    SpecialCard { kind, title: t(title), body: t(body), ..Default::default() }
}

pub fn special_card(message: &StoredMessage) -> SpecialCard<'_> {
    if message.deleted || message.kind == MessageKind::System {
        return SpecialCard::default();
    }
    let Some(raw) = message.raw.as_deref() else {
        if crate::i18n::is_translation("compat.view_once", &message.text) {
            return titled_card("view-once", "card.view_once", "card.view_once_body");
        }
        if crate::i18n::is_translation("compat.unsupported", &message.text) {
            return titled_card("unsupported", "card.unsupported", "card.unsupported_body");
        }
        return SpecialCard::default();
    };
    let Some(content) = display_content(raw) else {
        return titled_card("view-once", "card.view_once", "card.view_once_body");
    };
    if let Some(contact) = content.contact_message.as_option() {
        let (name, phones) = contact_parts(contact);
        return SpecialCard {
            kind: "contact",
            title: if name.is_empty() { t("compat.contact") } else { name },
            body: phones.join("\n"),
            ..Default::default()
        };
    }
    if let Some(contacts) = content.contacts_array_message.as_option() {
        let body = contacts.contacts.iter().take(256).map(|contact| {
            let (name, phones) = contact_parts(contact);
            let mut lines = vec![if name.is_empty() { t("compat.contact") } else { name }];
            lines.extend(phones);
            lines.join("\n")
        }).collect::<Vec<_>>().join("\n\n");
        return SpecialCard { kind: "contact", title: t("card.contacts"), body, ..Default::default() };
    }
    if let Some(loc) = content.location_message.as_option() {
        let live = loc.is_live.unwrap_or(false);
        let name = card_text(loc.name.as_deref().unwrap_or(""));
        let body = [loc.address.as_deref().unwrap_or(""), loc.comment.as_deref().unwrap_or("")]
            .into_iter().map(card_text).filter(|s| !s.is_empty()).collect::<Vec<_>>().join("\n");
        return SpecialCard {
            kind: if live { "live-location" } else { "location" },
            title: if name.is_empty() { t(if live { "card.live_location" } else { "compat.location" }) } else { name },
            body,
            detail: coordinate_detail(loc.degrees_latitude, loc.degrees_longitude, live),
            thumbnail: loc.jpeg_thumbnail.as_deref(),
            ..Default::default()
        };
    }
    if let Some(loc) = content.live_location_message.as_option() {
        return SpecialCard {
            kind: "live-location", title: t("card.live_location"),
            body: card_text(loc.caption.as_deref().unwrap_or("")),
            detail: coordinate_detail(loc.degrees_latitude, loc.degrees_longitude, true),
            thumbnail: loc.jpeg_thumbnail.as_deref(),
            ..Default::default()
        };
    }
    let poll = content.poll_creation_message.as_option()
        .or_else(|| content.poll_creation_message_v2.as_option())
        .or_else(|| content.poll_creation_message_v3.as_option())
        .or_else(|| content.poll_creation_message_v5.as_option())
        .or_else(|| content.poll_creation_message_v6.as_option());
    if let Some(poll) = poll {
        let question = card_text(poll.name.as_deref().unwrap_or(""));
        return SpecialCard {
            kind: "poll", title: if question.is_empty() { t("compat.poll") } else { question },
            detail: t("compat.poll_phone"),
            options: poll.options.iter().take(256).map(|option| card_text(option.option_name.as_deref().unwrap_or("")))
                .filter(|name| !name.is_empty()).collect(),
            ..Default::default()
        };
    }
    if has_unsupported_content(content)
        && content.conversation.is_none()
        && !content.extended_text_message.is_set()
        && !content.image_message.is_set()
        && !content.video_message.is_set()
        && !content.audio_message.is_set()
        && !content.document_message.is_set()
        && !content.sticker_message.is_set()
    {
        return titled_card("unsupported", "card.unsupported", "card.unsupported_body");
    }
    SpecialCard::default()
}

fn coordinate_detail(lat: Option<f64>, lon: Option<f64>, live: bool) -> String {
    let coordinates = match (lat, lon) {
        (Some(lat), Some(lon)) if lat.is_finite() && lon.is_finite()
            && (-90.0..=90.0).contains(&lat) && (-180.0..=180.0).contains(&lon) => format!("{lat:.6}, {lon:.6}"),
        _ => t("compat.invalid_location"),
    };
    if live { format!("{coordinates}\n{}", t("card.live_location_detail")) } else { coordinates }
}

pub fn compatibility_card(content: &wa::Message) -> Option<String> {
    if let Some(contact) = content.contact_message.as_option() {
        return Some(contact_card(contact));
    }
    if let Some(contacts) = content.contacts_array_message.as_option() {
        let cards: Vec<_> = contacts.contacts.iter().take(256).map(contact_card).collect();
        return Some(if cards.is_empty() { t("compat.contact") } else { cards.join("\n\n") });
    }
    if let Some(loc) = content.location_message.as_option() {
        return Some(location_card(loc.degrees_latitude, loc.degrees_longitude, loc.is_live.unwrap_or(false),
            &[loc.name.as_deref().unwrap_or(""), loc.address.as_deref().unwrap_or(""), loc.comment.as_deref().unwrap_or("")]));
    }
    if let Some(loc) = content.live_location_message.as_option() {
        return Some(location_card(loc.degrees_latitude, loc.degrees_longitude, true, &[loc.caption.as_deref().unwrap_or("")]));
    }
    let poll = content.poll_creation_message.as_option()
        .or_else(|| content.poll_creation_message_v2.as_option())
        .or_else(|| content.poll_creation_message_v3.as_option())
        .or_else(|| content.poll_creation_message_v5.as_option())
        .or_else(|| content.poll_creation_message_v6.as_option());
    if let Some(poll) = poll {
        let mut lines = vec![t("compat.poll")];
        let question = card_text(poll.name.as_deref().unwrap_or(""));
        if !question.is_empty() { lines.push(question); }
        for (i, option) in poll.options.iter().take(256).enumerate() {
            let name = card_text(option.option_name.as_deref().unwrap_or(""));
            if !name.is_empty() { lines.push(format!("{}. {name}", i + 1)); }
        }
        lines.push(t("compat.poll_phone"));
        return Some(lines.join("\n"));
    }
    None
}

fn has_unsupported_content(content: &wa::Message) -> bool {
    use whatsapp_rust::waproto::buffa::Message as _;
    let mut remaining = content.clone();
    macro_rules! clear {
        ($($field:ident),* $(,)?) => { $(remaining.$field = Default::default();)* };
    }
    clear!(sender_key_distribution_message, fast_ratchet_key_sender_key_distribution_message,
        protocol_message, reaction_message, enc_reaction_message, poll_update_message,
        message_context_info, keep_in_chat_message, pin_in_chat_message, sticker_sync_rmr_message,
        call, chat, message_history_bundle, message_history_notice, status_notification_message,
        enc_event_response_message, enc_comment_message, poll_result_snapshot_message,
        poll_result_snapshot_message_v3, poll_add_option_message, group_root_key_share,
        root_secret_distribute_message, status_link_preview_metadata,
        ephemeral_message, device_sent_message, document_with_caption_message, edited_message);
    remaining.encoded_len() != 0
}

pub fn display_content_summary(message: &wa::Message) -> String {
    let Some(content) = display_content(message) else { return t("compat.view_once") };
    if let Some(card) = compatibility_card(content) {
        return card.chars().take(240).collect();
    }
    if has_unsupported_content(content)
        && content.conversation.is_none()
        && !content.extended_text_message.is_set()
        && !content.image_message.is_set()
        && !content.video_message.is_set()
        && !content.audio_message.is_set()
        && !content.document_message.is_set()
        && !content.sticker_message.is_set()
    {
        return t("compat.unsupported");
    }
    quoted_summary(content)
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
        s if s % 86_400 == 0 => tp("ephemeral.days", (s / 86_400) as i64, &[]),
        s if s % 3600 == 0 => tp("ephemeral.hours", (s / 3600) as i64, &[]),
        s => tp("ephemeral.minutes", (s / 60) as i64, &[]),
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
        return ta("notice.joined_link_share", &[&people]);
    }
    if matches!(kind, "add" | "invite" | "remove" | "promote" | "demote")
        && !self_leave
        && !names_me
        && !actor_is_me
    {
        return t("notice.members_changed");
    }
    match kind {
        "add" => {
            if names_me {
                ta("notice.added_you", &[&actor_or_someone])
            } else if actor.is_some() && who.len() == 1 && actor_name == names[0] {
                ta("notice.joined_link", &[&people])
            } else if actor_is_me {
                ta("notice.you_added", &[&people])
            } else {
                ta("notice.added", &[&actor_or_someone, &people])
            }
        }
        "invite" => ta("notice.joined_link", &[&people]),
        "remove" | "leave" => {
            if actor.is_some() && who.len() == 1 && actor_name == names[0] {
                if names_me { t("notice.you_left") } else { ta("notice.left", &[&people]) }
            } else if names_me {
                ta("notice.removed_you", &[&actor_or_someone])
            } else if actor_is_me {
                ta("notice.you_removed", &[&people])
            } else {
                ta("notice.removed", &[&actor_or_someone, &people])
            }
        }
        "promote" => {
            if names_me { t("notice.you_admin") } else { ta("notice.promoted", &[&people]) }
        }
        "demote" => {
            if names_me { t("notice.you_not_admin") } else { ta("notice.demoted", &[&people]) }
        }
        "subject" => ta("notice.subject", &[&actor_or_someone, text]),
        "description" => ta("notice.description", &[&actor_or_someone]),
        "icon" => ta("notice.icon", &[&actor_or_someone]),
        "create" => ta("notice.created", &[&actor_or_someone, text]),
        "announce" => ta("notice.announce", &[&actor_or_someone]),
        "not_announce" => ta("notice.not_announce", &[&actor_or_someone]),
        "locked" => ta("notice.locked", &[&actor_or_someone]),
        "unlocked" => ta("notice.unlocked", &[&actor_or_someone]),
        "ephemeral" => {
            if number == 0 {
                ta("notice.ephemeral_off", &[&actor_or_someone])
            } else {
                ta("notice.ephemeral_on", &[&actor_or_someone, &ephemeral_label(number)])
            }
        }
        "delete" => t("notice.deleted"),
        "missedVoice" => t("notice.missed_voice"),
        "missedVideo" => t("notice.missed_video"),
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
        pinned_until: 0,
        edited: false,
        edit_versions: Vec::new(),
        receipts: Default::default(),
        sticker: false,
        mentions: Vec::new(),
        mentions_me: false,
        quote_id: String::new(),
        quote_author: String::new(),
        quote_text: String::new(),
        quote_jid: String::new(),
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

    let displayable = display_content(message);
    let content = displayable.unwrap_or(message);

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

    let ctx = displayable.and_then(context_of);
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
            display_content_summary(quoted),
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
        pinned_until: 0,
        edited: false,
        edit_versions: Vec::new(),
        receipts: Default::default(),
        sticker: false,
        mentions: Vec::new(),
        mentions_me,
        quote_id,
        quote_jid: ctx.and_then(|c| c.remote_jid.as_deref())
            .map(normalize_jid)
            .filter(|remote| is_displayable_jid(remote))
            .map(|remote| store.canon_owned(&remote))
            .unwrap_or_default(),
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
        raw: displayable.map(|_| Arc::new(message.clone())),
    };

    if displayable.is_none() {
        stored.text = t("compat.view_once");
        return Some(stored);
    }
    if let Some(card) = compatibility_card(content) {
        stored.text = card;
        return Some(stored);
    }
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
        stored.text = clean_text(doc.file_name.as_deref().unwrap_or(&t("doc.fallback_name")));
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
    if has_unsupported_content(content) {
        stored.text = t("compat.unsupported");
        stored.raw = None;
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
    let displayable = display_content(&inbound.message);
    let content = displayable.unwrap_or(&inbound.message);
    let (kind, text) = if displayable.is_none() {
        (MessageKind::Text, t("compat.view_once"))
    } else if let Some(text) = &content.conversation {
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
        pinned_until: 0,
        edited: false,
        edit_versions: Vec::new(),
        receipts: Default::default(),
        sticker: false,
        mentions: Vec::new(),
        mentions_me: false,
        quote_id: String::new(),
        quote_author: String::new(),
        quote_text: String::new(),
        quote_jid: String::new(),
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
        raw: displayable.map(|_| inbound.message.clone()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i18n::{LOCALE_TEST_LOCK, Locale, set_locale};

    fn map(message: wa::Message) -> Option<StoredMessage> {
        normalize(&mut Store::default(), &LiveMeta {
            chat: "123@s.whatsapp.net".into(), sender: "123@s.whatsapp.net".into(),
            sender_alt: None, recipient_alt: None, id: "m1", from_me: false,
            is_group: false, push_name: "Alice", timestamp: 1234,
        }, &message, &HashMap::new(), 5)
    }

    #[test]
    fn text_metadata_and_empty_messages() {
        assert!(map(wa::Message::default()).is_none());
        let m = map(wa::Message { conversation: Some("hello".into()), ..Default::default() }).unwrap();
        assert_eq!(m.text, "hello");
        assert_eq!(m.jid, "123@s.whatsapp.net");
        assert_eq!(m.id, "m1");
        assert_eq!(m.timestamp, 1234);
        assert_eq!(m.status, 0);
        assert!(matches!(m.kind, MessageKind::Text));
        assert!(m.raw.is_some());
    }

    #[test]
    fn media_fields_and_defaults() {
        let image = map(wa::Message { image_message: Some(wa::message::ImageMessage {
            caption: Some("caption".into()), width: Some(640), height: Some(480),
            mimetype: Some("image/png".into()), ..Default::default()
        }).into(), ..Default::default() }).unwrap();
        assert!(matches!(image.kind, MessageKind::Image));
        assert_eq!((image.media_w, image.media_h), (640, 480));
        assert_eq!(image.text, "caption");
        assert_eq!(image.mimetype, "image/png");
        let audio = map(wa::Message { audio_message: Some(wa::message::AudioMessage {
            seconds: Some(65), ..Default::default()
        }).into(), ..Default::default() }).unwrap();
        assert!(matches!(audio.kind, MessageKind::Audio));
        assert_eq!(audio.duration_sec, 65);
        let video = map(wa::Message { video_message: Some(wa::message::VideoMessage {
            gif_playback: Some(true), seconds: Some(9), ..Default::default()
        }).into(), ..Default::default() }).unwrap();
        assert!(matches!(video.kind, MessageKind::Video));
        assert!(video.gif);
        assert_eq!(video.duration_sec, 9);
        let sticker = map(wa::Message { sticker_message: Some(wa::message::StickerMessage::default()).into(), ..Default::default() }).unwrap();
        assert!(sticker.sticker);
        assert!(matches!(sticker.kind, MessageKind::Image));
        let doc = map(wa::Message { document_message: Some(wa::message::DocumentMessage {
            file_name: Some("report.pdf".into()), title: Some("ignored".into()), ..Default::default()
        }).into(), ..Default::default() }).unwrap();
        assert!(matches!(doc.kind, MessageKind::Doc));
        assert_eq!(doc.text, "report.pdf");
    }

    #[test]
    fn extended_text_context_and_link_preview() {
        let m = map(wa::Message { extended_text_message: Some(wa::message::ExtendedTextMessage {
            text: Some("link".into()), title: Some("title".into()),
            description: Some("description".into()), matched_text: Some("https://example.com".into()),
            context_info: Some(wa::ContextInfo {
                is_forwarded: Some(true), stanza_id: Some("quoted".into()),
                mentioned_jid: vec!["456@s.whatsapp.net".into()],
                ..Default::default()
            }).into(), ..Default::default()
        }).into(), ..Default::default() }).unwrap();
        assert!(m.forwarded);
        assert_eq!(m.quote_id, "");
        assert_eq!(m.mentions, vec!["456@s.whatsapp.net"]);
        assert_eq!(m.link_title, "title");
        assert_eq!(m.link_desc, "description");
        assert_eq!(m.link_url, "https://example.com");
    }

    #[test]
    fn protocol_reaction_and_unknown_payloads_are_not_bubbles() {
        assert!(map(wa::Message { reaction_message: Some(wa::message::ReactionMessage::default()).into(), ..Default::default() }).is_none());
        assert!(map(wa::Message { protocol_message: Some(wa::message::ProtocolMessage::default()).into(), ..Default::default() }).is_none());
    }

    #[test]
    fn history_requires_key_id_and_message() {
        let mut store = Store::default();
        assert!(from_history(&mut store, &wa::WebMessageInfo::default()).is_none());
        let mut web = wa::WebMessageInfo {
            key: Some(wa::MessageKey { id: Some("history".into()), remote_jid: Some("123@s.whatsapp.net".into()), from_me: Some(true), ..Default::default() }).into(),
            message: Some(wa::Message { conversation: Some("old".into()), ..Default::default() }).into(),
            message_timestamp: Some(99), starred: Some(true), ..Default::default()
        };
        let m = from_history(&mut store, &web).unwrap();
        assert_eq!(m.timestamp, 99);
        assert_eq!(m.status, 2);
        assert!(m.from_me);
        assert!(!m.starred);
        web.key = Some(wa::MessageKey::default()).into();
        assert!(from_history(&mut store, &web).is_none());
    }

    #[test]
    fn history_normalizes_device_jids_and_defaults_timestamp() {
        let web = wa::WebMessageInfo {
            key: Some(wa::MessageKey {
                remote_jid: Some("123:7@s.whatsapp.net".into()),
                id: Some("device".into()), ..Default::default()
            }).into(),
            message: Some(wa::Message { conversation: Some("body".into()), ..Default::default() }).into(),
            ..Default::default()
        };
        let m = from_history(&mut Store::default(), &web).unwrap();
        assert_eq!(m.jid, "123@s.whatsapp.net");
        assert_eq!(m.timestamp, 0);
        assert_eq!(m.status, 0);
        let mut invalid = web.clone();
        invalid.key = Some(wa::MessageKey { remote_jid: Some("123@s.whatsapp.net".into()), id: Some(String::new()), ..Default::default() }).into();
        assert!(from_history(&mut Store::default(), &invalid).is_none());
    }

    #[test]
    fn system_defaults_and_duration_units() {
        let _lock = LOCALE_TEST_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let previous = crate::i18n::current_locale();
        struct Restore(Locale);
        impl Drop for Restore { fn drop(&mut self) { set_locale(self.0); } }
        let _restore = Restore(previous);
        set_locale(Locale::En);
        let m = system_message("chat", "notice", -1, "notice text".into());
        assert!(matches!(m.kind, MessageKind::System));
        assert_eq!(m.timestamp, -1);
        assert!(m.raw.is_none());
        assert!(!m.from_me);
        assert!(!m.deleted);
        assert_eq!(ephemeral_label(120), "2 minutes");
        assert_eq!(ephemeral_label(7200), "2 hours");
        assert_eq!(ephemeral_label(172800), "2 days");
        assert_eq!(ephemeral_label(61), "1 minute");
        assert_eq!(join_names(&[]), "");
        assert_eq!(join_names(&["A".into()]), "A");
        assert_eq!(join_names(&["A".into(), "B".into()]), "A and B");
    }
}
