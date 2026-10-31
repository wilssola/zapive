// In-memory chat/message store with vault persistence. Port of
// src/store.ts on master; jids are canonical strings (device suffix
// stripped, LID resolved through the alias table).
use crate::i18n::{t, ta};
use crate::vault::Vault;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use whatsapp_rust::waproto::whatsapp as wa;

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MessageKind {
    Text,
    Image,
    Audio,
    Doc,
    Video,
    // A centered notice about the chat itself (members joined, subject
    // changed, disappearing messages toggled); `text` is the sentence.
    System,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct StoredMessage {
    pub id: String,
    pub jid: String,
    pub kind: MessageKind,
    pub text: String,
    pub from_me: bool,
    pub sender: String,
    #[serde(default)]
    pub sender_jid: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub forwarded: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub deleted: bool,
    // The sender took this message back. `deleted` additionally means the
    // body was dropped; with the "apply deletions" setting off, text and
    // media survive and only this flag is set, so the reader knows what
    // happened without losing the message (see Store::mark_revoked).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub revoked: bool,
    // A read (or played) receipt for this message was already sent, so
    // "send message viewed" (opt-in) doesn't ack it twice.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub read_sent: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub gif: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub starred: bool,
    #[serde(default)]
    pub pinned_until: i64,
    #[serde(default)]
    pub edited: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub edit_versions: Vec<EditVersion>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub receipts: HashMap<String, ParticipantReceipt>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub sticker: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mentions: Vec<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub mentions_me: bool,
    // The message this one replies to, as WhatsApp sends it inline.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub quote_id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub quote_jid: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub quote_author: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub quote_text: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub link_title: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub link_desc: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub link_url: String,
    pub timestamp: i64,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub mimetype: String,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub media_w: u32,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub media_h: u32,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub duration_sec: u32,
    // WhatsApp delivery status for own messages (2 ack, 3 delivered, 4 read).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub status: u32,
    // reactor jid -> emoji
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub reactions: HashMap<String, String>,
    // The full protobuf, kept for media download / forward / quote; stored
    // as prost bytes in base64.
    #[serde(default, with = "raw_proto", skip_serializing_if = "Option::is_none")]
    pub raw: Option<Arc<wa::Message>>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct EditVersion {
    pub text: String,
    pub timestamp: Option<i64>,
}

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct ParticipantReceipt {
    #[serde(default)]
    pub level: u32,
    pub delivered: Option<i64>,
    pub read: Option<i64>,
    pub played: Option<i64>,
}

impl ParticipantReceipt {
    pub fn promote(&mut self, status: u32, timestamp: i64) {
        self.level = self.level.max(status);
        let time = (timestamp > 0).then_some(timestamp);
        for (enabled, slot) in [
            (status == 3, &mut self.delivered),
            (status == 4, &mut self.read),
            (status == 5, &mut self.played),
        ] {
            if enabled && let Some(time) = time {
                *slot = Some(slot.map_or(time, |old| old.min(time)));
            }
        }
    }

    pub fn status(&self) -> u32 {
        self.level.max(if self.played.is_some() { 5 } else if self.read.is_some() { 4 }
        else if self.delivered.is_some() { 3 } else { 0 })
    }
}

impl StoredMessage {
    pub fn record_edit(&mut self, text: String, timestamp: i64) -> bool {
        if self.deleted || self.text == text { return false; }
        let time = (timestamp > 0).then_some(timestamp);
        if self.edit_versions.iter().any(|v| v.text == text && v.timestamp == time) { return false; }
        if self.edit_versions.is_empty() {
            self.edit_versions.push(EditVersion {
                text: self.text.clone(), timestamp: (!self.edited).then_some(self.timestamp),
            });
        }
        self.edit_versions.push(EditVersion { text, timestamp: time });
        self.edit_versions.sort_by_key(|v| v.timestamp.unwrap_or(0));
        self.text = self.edit_versions.last().unwrap().text.clone();
        self.edited = true;
        true
    }

    pub fn is_voice_message(&self) -> bool {
        use whatsapp_rust::proto_helpers::MessageExt as _;
        self.kind == MessageKind::Audio && self.raw.as_ref()
            .and_then(|raw| raw.get_base_message().audio_message.as_option())
            .and_then(|audio| audio.ptt)
            .unwrap_or(false)
    }

    pub fn audio_filename(&self, source: Option<&str>) -> String {
        use whatsapp_rust::proto_helpers::MessageExt as _;
        let original = source.and_then(|path| path.rsplit(['/', '\\']).next())
            .filter(|name| !name.trim().is_empty());
        let document = self.raw.as_ref()
            .and_then(|raw| raw.get_base_message().document_message.as_option())
            .filter(|doc| doc.mimetype.as_deref().unwrap_or(&self.mimetype).starts_with("audio/"))
            .and_then(|doc| doc.file_name.as_deref())
            .filter(|name| !name.trim().is_empty());
        original.or(document).map(clean_text).unwrap_or_else(|| t("audio.fallback_name"))
    }
}

fn is_zero(n: &u32) -> bool {
    *n == 0
}

mod raw_proto {
    use super::*;
    use base64::Engine as _;
    use base64::engine::general_purpose::STANDARD as B64;
    use whatsapp_rust::waproto::buffa::Message as _;

    pub fn serialize<S: serde::Serializer>(
        value: &Option<Arc<wa::Message>>,
        ser: S,
    ) -> Result<S::Ok, S::Error> {
        match value {
            Some(msg) => ser.serialize_some(&B64.encode(msg.encode_to_vec())),
            None => ser.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: serde::Deserializer<'de>>(
        de: D,
    ) -> Result<Option<Arc<wa::Message>>, D::Error> {
        let b64: Option<String> = Option::deserialize(de)?;
        Ok(b64
            .and_then(|s| B64.decode(s).ok())
            .and_then(|bytes| wa::Message::decode(&mut bytes.as_slice()).ok())
            .map(Arc::new))
    }
}

pub fn ticks_for(m: &StoredMessage) -> (&'static str, bool) {
    if !m.from_me {
        return ("", false);
    }
    match m.status {
        s if s >= 4 => ("✓✓", true),
        s if s >= 3 => ("✓✓", false),
        _ => ("✓", false),
    }
}

// The reaction chip under a bubble: up to three distinct emoji, and how
// many reacted when more than one did. Kept apart so the count is drawn in
// the text font rather than the emoji one.
pub fn reaction_summary(m: &StoredMessage) -> (String, String) {
    let values: Vec<&String> = m.reactions.values().filter(|v| !v.is_empty()).collect();
    if values.is_empty() {
        return (String::new(), String::new());
    }
    let mut unique: Vec<&str> = Vec::new();
    for v in &values {
        if !unique.contains(&v.as_str()) {
            unique.push(v);
            if unique.len() == 3 {
                break;
            }
        }
    }
    let count = if values.len() > 1 { values.len().to_string() } else { String::new() };
    (unique.concat(), count)
}

#[derive(Clone, Serialize, Deserialize)]
pub struct CallEntry {
    pub id: String,
    pub jid: String, // caller (or group) jid
    pub video: bool,
    pub group: bool,
    pub status: String, // offer | outgoing | accept | reject | timeout | terminate
    pub timestamp: i64,
    // Calls placed from here. Defaulted so vaults written before calls
    // could be placed still load.
    #[serde(default)]
    pub outgoing: bool,
    #[serde(default)]
    pub seen: bool,
}

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct ChatMeta {
    pub jid: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    pub preview: String,
    pub timestamp: i64,
    #[serde(default)]
    pub unread: u32,
    // An unread message mentions us.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub mentioned: bool,
    // Pin timestamp; 0 = not pinned.
    #[serde(default, skip_serializing_if = "is_zero_i64")]
    pub pinned: i64,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub archived: bool,
    // Parent community jid, when this group belongs to one.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub community: String,
    // This jid is the community itself.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_community: bool,
    // Notifications: 0 = on, -1 = muted always, otherwise muted until
    // this unix time (seconds).
    #[serde(default, skip_serializing_if = "is_zero_i64")]
    pub muted: i64,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub favorite: bool,
    // Disappearing-message timer in seconds; 0 = off.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub ephemeral: u32,
    // Group settings that decide whether we may write here.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub announce: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub admin: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub left: bool,
}

impl ChatMeta {
    pub fn is_muted(&self) -> bool {
        self.muted == -1 || (self.muted > 0 && self.muted > now_secs())
    }

    // Announcement groups only take messages from admins.
    pub fn can_send(&self) -> bool {
        !self.left && (!self.announce || self.admin)
    }
}

// One group participant, as the group roster lists them.
#[derive(Clone, Serialize, Deserialize)]
pub struct Member {
    pub jid: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub phone: String,
    // The name the server shows for this participant, when it sends one.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub admin: bool,
}

// A WhatsApp list (label) chats can be filed under.
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Label {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub color: i32,
    #[serde(default)]
    pub order: i32,
    #[serde(default)]
    pub list_type: Option<i32>,
}

impl Label {
    pub fn is_builtin_filter(&self) -> bool {
        matches!(self.list_type, Some(1..=3))
    }
}

fn is_zero_i64(n: &i64) -> bool {
    *n == 0
}

pub fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn fresh(timestamp: i64) -> bool {
    timestamp > now_secs() - 24 * 60 * 60
}

pub fn is_channel(jid: &str) -> bool {
    jid.ends_with("@newsletter")
}

pub fn is_displayable_jid(jid: &str) -> bool {
    if jid.is_empty() || jid == "status@broadcast" || jid.ends_with("@broadcast") {
        return false;
    }
    jid.ends_with("@s.whatsapp.net")
        || jid.ends_with("@g.us")
        || jid.ends_with("@lid")
        || is_channel(jid)
}

pub fn is_group(jid: &str) -> bool {
    jid.ends_with("@g.us")
}

fn local_date(timestamp: i64) -> chrono::DateTime<chrono::Local> {
    use chrono::TimeZone as _;
    chrono::Local.timestamp_opt(timestamp, 0).single().unwrap_or_else(chrono::Local::now)
}

pub fn format_time(timestamp: i64) -> String {
    if timestamp == 0 {
        return String::new();
    }
    format_time_at(local_date(timestamp).naive_local(), chrono::Local::now().date_naive())
}

fn format_time_at(d: chrono::NaiveDateTime, today: chrono::NaiveDate) -> String {
    use chrono::Datelike as _;
    match today.signed_duration_since(d.date()).num_days() {
        0 => d.format("%H:%M").to_string(),
        1 => t("day.yesterday"),
        2..=6 => t(match d.weekday() {
            chrono::Weekday::Mon => "day.monday",
            chrono::Weekday::Tue => "day.tuesday",
            chrono::Weekday::Wed => "day.wednesday",
            chrono::Weekday::Thu => "day.thursday",
            chrono::Weekday::Fri => "day.friday",
            chrono::Weekday::Sat => "day.saturday",
            chrono::Weekday::Sun => "day.sunday",
        }),
        _ => d.format("%d/%m").to_string(),
    }
}

// The wall-clock time of a message, whatever day it is from: bubbles
// always carry the hour; the day comes from the separator above them.
pub fn format_clock(timestamp: i64) -> String {
    if timestamp == 0 {
        return String::new();
    }
    local_date(timestamp).format("%H:%M").to_string()
}

// Day and time together, for search results and exports.
pub fn format_stamp(timestamp: i64) -> String {
    if timestamp == 0 {
        return String::new();
    }
    local_date(timestamp).format("%d/%m/%Y %H:%M").to_string()
}

pub fn format_day(timestamp: i64) -> String {
    if timestamp == 0 {
        return String::new();
    }
    let d = local_date(timestamp).date_naive();
    let today = chrono::Local::now().date_naive();
    if d == today {
        return t("day.today");
    }
    if today.pred_opt() == Some(d) {
        return t("day.yesterday");
    }
    d.format("%d/%m/%Y").to_string()
}

// Slint's text renderer shows tofu for emoji followed by a variation
// selector (e.g. "❤️" = U+2764 U+FE0F); stripping the selector keeps the
// colored glyph.
pub fn clean_text(s: &str) -> String {
    s.chars().filter(|c| !('\u{FE00}'..='\u{FE0F}').contains(c)).collect()
}

// A readable identity for a jid without a known name.
pub fn display_id(jid: &str) -> String {
    let user = jid.split('@').next().unwrap_or(jid);
    if jid.ends_with("@s.whatsapp.net")
        && (10..=15).contains(&user.len())
        && user.bytes().all(|b| b.is_ascii_digit())
    {
        return format!("+{user}");
    }
    user.to_string()
}

// +5538920006927 -> +55 38 92000-6927 (falls back to a plain +digits).
pub fn format_number(jid: &str) -> String {
    if jid.ends_with("@lid") {
        return String::new(); // LIDs are not phone numbers
    }
    let digits: String = jid
        .split('@')
        .next()
        .unwrap_or("")
        .chars()
        .filter(|c| c.is_ascii_digit())
        .collect();
    if digits.is_empty() {
        return String::new();
    }
    if digits.starts_with("55") && (digits.len() == 12 || digits.len() == 13) {
        let area = &digits[2..4];
        let rest = &digits[4..];
        let split = rest.len() - 4;
        return format!("+55 {area} {}-{}", &rest[..split], &rest[split..]);
    }
    format!("+{digits}")
}

pub fn format_duration(seconds: u32) -> String {
    format!("{}:{:02}", seconds / 60, seconds % 60)
}

#[derive(Default, Clone, Serialize, Deserialize)]
pub struct Draft {
    pub text: String,
    pub mentions: Vec<(String, String)>,
    pub promoted: u64,
}

#[derive(Default)]
pub struct Store {
    pub chats: HashMap<String, ChatMeta>,
    pub drafts: HashMap<String, Draft>,
    // Our own identities (phone jid and lid).
    pub self_jids: HashSet<String>,
    pub messages: HashMap<String, Vec<StoredMessage>>,
    // Address-book names.
    pub contacts: HashMap<String, String>,
    // Names announced in messages.
    pub push_names: HashMap<String, String>,
    // @lid jid -> phone-number jid.
    pub aliases: HashMap<String, String>,
    // Stars can arrive before the message does.
    pub starred_ids: HashSet<String>,
    // Jids known from the address book.
    pub saved_contacts: HashSet<String>,
    pub deleted_jids: HashSet<String>,
    pub calls: Vec<CallEntry>,
    // Author jid -> updates (status@broadcast, 24h lifetime).
    pub statuses: HashMap<String, Vec<StoredMessage>>,
    pub dirty_jids: HashSet<String>,
    // Chats whose message list has been read from the vault this run.
    // Cold chats keep only their ChatMeta in RAM; bodies load on demand.
    pub hydrated: HashSet<String>,
    pub calls_dirty: bool,
    pub starred_dirty: bool,
    pub statuses_dirty: bool,
    // Group jid -> roster, from the last metadata fetch.
    pub members: HashMap<String, Vec<Member>>,
    pub members_dirty: bool,
    // WhatsApp lists: label id -> label, and label id -> chats filed there.
    pub labels: HashMap<String, Label>,
    pub label_chats: HashMap<String, HashSet<String>>,
    pub labels_dirty: bool,
    pub search: crate::search::SearchIndex,
    pending_removals: HashSet<(String, String)>,
    pending_merges: Vec<(String, String)>,
    older_cursors: HashMap<String, (i64, String)>,
    older_exhausted: HashSet<String>,
}

impl Store {
    // Resolves a jid through the LID->PN alias table.
    pub fn canon<'a>(&'a self, jid: &'a str) -> &'a str {
        self.aliases.get(jid).map(String::as_str).unwrap_or(jid)
    }

    pub fn canon_owned(&self, jid: &str) -> String {
        self.canon(jid).to_string()
    }

    // Learns that `lid` and `pn` are the same contact; merges duplicates.
    pub fn learn_alias(&mut self, lid: &str, pn: &str) -> bool {
        if lid == pn || self.aliases.get(lid).map(String::as_str) == Some(pn) {
            return false;
        }
        self.aliases.insert(lid.to_string(), pn.to_string());
        self.merge_jid(lid, pn);
        true
    }

    fn merge_jid(&mut self, from: &str, into: &str) {
        if from == into { return; }
        self.pending_merges.push((from.to_string(), into.to_string()));
        self.older_cursors.remove(into);
        self.older_exhausted.remove(into);
        if let Some(src) = self.drafts.remove(from) {
            if self.drafts.get(into).is_none_or(|dst| {
                (!src.text.trim().is_empty(), src.promoted) > (!dst.text.trim().is_empty(), dst.promoted)
            }) {
                self.drafts.insert(into.to_string(), src);
            }
        }
        if let Some(src) = self.chats.remove(from) {
            let dst = self.chats.get(into);
            let merged = ChatMeta {
                jid: into.to_string(),
                name: dst
                    .filter(|d| !d.name.is_empty())
                    .map(|d| d.name.clone())
                    .unwrap_or_else(|| src.name.clone()),
                preview: if dst.map(|d| d.timestamp).unwrap_or(0) >= src.timestamp {
                    dst.map(|d| d.preview.clone()).unwrap_or_else(|| src.preview.clone())
                } else {
                    src.preview.clone()
                },
                timestamp: src.timestamp.max(dst.map(|d| d.timestamp).unwrap_or(0)),
                unread: src.unread + dst.map(|d| d.unread).unwrap_or(0),
                mentioned: src.mentioned || dst.map(|d| d.mentioned).unwrap_or(false),
                pinned: src.pinned.max(dst.map(|d| d.pinned).unwrap_or(0)),
                archived: src.archived || dst.map(|d| d.archived).unwrap_or(false),
                community: dst.map(|d| d.community.clone()).unwrap_or_else(|| src.community.clone()),
                is_community: src.is_community || dst.map(|d| d.is_community).unwrap_or(false),
                muted: dst.map(|d| d.muted).filter(|m| *m != 0).unwrap_or(src.muted),
                favorite: src.favorite || dst.map(|d| d.favorite).unwrap_or(false),
                ephemeral: dst.map(|d| d.ephemeral).filter(|e| *e != 0).unwrap_or(src.ephemeral),
                announce: src.announce || dst.map(|d| d.announce).unwrap_or(false),
                admin: src.admin || dst.map(|d| d.admin).unwrap_or(false),
                left: src.left && dst.map(|d| d.left).unwrap_or(true),
            };
            self.chats.insert(into.to_string(), merged);
        }
        for set in self.label_chats.values_mut() {
            if set.remove(from) {
                set.insert(into.to_string());
                self.labels_dirty = true;
            }
        }
        self.search.remove_chat(from);
        self.pending_removals = self.pending_removals.drain().map(|(jid, id)| {
            (if jid == from { into.to_string() } else { jid }, id)
        }).collect();
        if let Some(mut src_msgs) = self.messages.remove(from) {
            for m in &mut src_msgs {
                m.jid = into.to_string();
            }
            self.merge_page(into, src_msgs);
            self.dirty_jids.insert(into.to_string());
            self.dirty_jids.remove(from);
        }
        if let Some(name) = self.contacts.get(from).cloned()
            && !self.contacts.contains_key(into)
        {
            self.contacts.insert(into.to_string(), name);
        }
        if let Some(push) = self.push_names.get(from).cloned()
            && !self.push_names.contains_key(into)
        {
            self.push_names.insert(into.to_string(), push);
        }
        if self.saved_contacts.remove(from) {
            self.saved_contacts.insert(into.to_string());
        }
    }

    pub fn chat_name(&self, jid: &str) -> String {
        if let Some(contact) = self.contacts.get(jid) {
            return clean_text(contact);
        }
        if let Some(meta) = self.chats.get(jid)
            && !meta.name.is_empty()
        {
            return clean_text(&meta.name);
        }
        if let Some(push) = self.push_names.get(jid) {
            return clean_text(push);
        }
        display_id(jid)
    }

    // Turns "@5511999999999" into "@Name" wherever we know who it is, so
    // previews and notifications read like the conversation does.
    pub fn named_mentions(&self, text: &str) -> String {
        if !text.contains('@') {
            return text.to_string();
        }
        let mut out = String::with_capacity(text.len());
        let bytes = text.as_bytes();
        let mut i = 0;
        while i < text.len() {
            if bytes[i] == b'@' {
                let digits_end = text[i + 1..]
                    .find(|c: char| !c.is_ascii_digit())
                    .map(|off| i + 1 + off)
                    .unwrap_or(text.len());
                let num = &text[i + 1..digits_end];
                if (5..=20).contains(&num.len()) {
                    let mut replaced = false;
                    for form in [format!("{num}@s.whatsapp.net"), format!("{num}@lid")] {
                        let jid = self.canon(&form);
                        let name = self
                            .contacts
                            .get(jid)
                            .or_else(|| self.chats.get(jid).filter(|c| !c.name.is_empty()).map(|c| &c.name))
                            .or_else(|| self.push_names.get(jid));
                        if let Some(name) = name {
                            out.push('@');
                            out.push_str(&clean_text(name));
                            replaced = true;
                            break;
                        }
                    }
                    if !replaced {
                        out.push_str(&text[i..digits_end]);
                    }
                    i = digits_end;
                    continue;
                }
            }
            let ch = text[i..].chars().next().unwrap();
            out.push(ch);
            i += ch.len_utf8();
        }
        out
    }

    // Records our own jids so mentions can be matched against them.
    pub fn set_self(&mut self, ids: &[&str]) {
        for id in ids {
            if id.is_empty() {
                continue;
            }
            let jid = normalize_jid(id);
            let canon = self.canon_owned(&jid);
            self.self_jids.insert(jid);
            self.self_jids.insert(canon);
        }
    }

    pub fn sorted_chats(&self) -> Vec<&ChatMeta> {
        let mut list: Vec<&ChatMeta> = self.chats.values().collect();
        list.sort_by(|a, b| {
            let (pa, pb) = (a.pinned, b.pinned);
            if (pa > 0) != (pb > 0) {
                return if pa > 0 { std::cmp::Ordering::Less } else { std::cmp::Ordering::Greater };
            }
            if pa > 0 {
                return pb.cmp(&pa);
            }
            let promotion = |chat: &ChatMeta| {
                self.drafts.get(&chat.jid)
                    .filter(|draft| !draft.text.trim().is_empty())
                    .map(|draft| draft.promoted)
                    .unwrap_or(0)
            };
            promotion(b).cmp(&promotion(a))
                .then_with(|| b.timestamp.cmp(&a.timestamp))
        });
        list
    }

    pub fn upsert_contact(&mut self, jid: &str, name: Option<&str>, notify: Option<&str>) {
        let jid = normalize_jid(jid);
        if jid.is_empty() {
            return;
        }
        if let Some(name) = name.filter(|n| !n.is_empty()) {
            self.contacts.insert(jid.clone(), name.to_string());
            self.saved_contacts.insert(jid);
        } else if let Some(notify) = notify.filter(|n| !n.is_empty()) {
            self.push_names.insert(jid, notify.to_string());
        }
    }

    // A jid counts as saved when the address book knows it, or when the
    // phone gave its DM a name (which it only does for saved contacts).
    pub fn is_saved(&self, jid: &str) -> bool {
        if self.saved_contacts.contains(jid) {
            return true;
        }
        self.chats
            .get(jid)
            .map(|c| !c.name.is_empty() && jid.ends_with("@s.whatsapp.net"))
            .unwrap_or(false)
    }

    // Chat metadata from history sync / typed updates. `pinned` and
    // `archived` as None mean "unchanged".
    pub fn upsert_chat(
        &mut self,
        jid: &str,
        name: Option<&str>,
        timestamp: i64,
        unread: Option<u32>,
        pinned: Option<i64>,
        archived: Option<bool>,
    ) {
        let jid = self.canon_owned(&normalize_jid(jid));
        if !is_displayable_jid(&jid) {
            return;
        }
        let mut meta = self.chats.get(&jid).cloned().unwrap_or_default();
        meta.jid = jid.clone();
        if let Some(name) = name.filter(|n| !n.is_empty()) {
            meta.name = name.to_string();
        }
        meta.timestamp = timestamp.max(meta.timestamp);
        if let Some(unread) = unread {
            meta.unread = unread;
        }
        if let Some(pinned) = pinned {
            meta.pinned = pinned;
        }
        if let Some(archived) = archived {
            meta.archived = archived;
        }
        self.chats.insert(jid, meta);
    }

    // Group subjects / contact names discovered outside the chat events.
    pub fn set_name(&mut self, jid: &str, name: &str) {
        if let Some(existing) = self.chats.get_mut(jid) {
            existing.name = name.to_string();
        } else {
            self.contacts.insert(jid.to_string(), name.to_string());
        }
    }

    // Returns true if the message was newly added (false = duplicate).
    pub fn add_message(&mut self, mut stored: StoredMessage) -> bool {
        if self.pending_removals.contains(&(self.canon_owned(&stored.jid), stored.id.clone())) {
            return false;
        }
        let list = self.messages.entry(stored.jid.clone()).or_default();
        if list.iter().any(|m| m.id == stored.id) {
            return false;
        }
        // A star may have synced before this message was backfilled.
        if self.starred_ids.contains(&stored.id) {
            stored.starred = true;
        }
        let jid = stored.jid.clone();
        let timestamp = stored.timestamp;
        let preview_msg = stored.clone();
        list.push(stored);
        list.sort_by_key(|m| m.timestamp);
        self.dirty_jids.insert(jid.clone());
        self.search.add(&jid, &preview_msg);

        let existing = self.chats.get(&jid);
        if existing.map(|e| timestamp >= e.timestamp).unwrap_or(true) {
            let preview = compute_preview(&preview_msg, self);
            let mut meta = self.chats.get(&jid).cloned().unwrap_or_default();
            meta.jid = jid.clone();
            meta.preview = preview;
            meta.timestamp = timestamp.max(meta.timestamp);
            self.chats.insert(jid.clone(), meta);
        }
        if preview_msg.kind != MessageKind::System
            && !preview_msg.from_me
            && !preview_msg.sender.is_empty()
            && (jid.ends_with("@s.whatsapp.net") || jid.ends_with("@lid"))
        {
            self.push_names.insert(jid, preview_msg.sender);
        }
        true
    }

    // Drops a chat's messages from RAM, the index and (via deleted_jids)
    // the vault, keeping the chat itself in the list.
    pub fn clear_messages(&mut self, jid: &str) {
        let canonical = self.canon_owned(jid);
        let jid = canonical.as_str();
        self.older_cursors.remove(jid);
        self.older_exhausted.remove(jid);
        self.pending_removals.retain(|(chat, _)| chat != jid);
        self.messages.remove(jid);
        self.hydrated.remove(jid);
        self.dirty_jids.remove(jid);
        self.deleted_jids.insert(jid.to_string());
        for (from, into) in &self.pending_merges {
            if into == jid { self.deleted_jids.insert(from.clone()); }
        }
        self.search.remove_chat(jid);
        if let Some(meta) = self.chats.get_mut(jid) {
            meta.preview = String::new();
        }
    }

    // Removes a chat entirely (WhatsApp's "delete chat").
    pub fn delete_chat(&mut self, jid: &str) {
        let canonical = self.canon_owned(jid);
        let jid = canonical.as_str();
        self.clear_messages(jid);
        self.chats.remove(jid);
        self.drafts.remove(jid);
        for set in self.label_chats.values_mut() {
            set.remove(jid);
        }
    }

    pub fn sorted_labels(&self) -> Vec<&Label> {
        let mut out: Vec<&Label> = self.labels.values().collect();
        out.sort_by(|a, b| a.order.cmp(&b.order).then(a.name.cmp(&b.name)));
        out
    }

    pub fn set_label_chat(&mut self, label_id: &str, jid: &str, on: bool) -> bool {
        let set = self.label_chats.entry(label_id.to_string()).or_default();
        let changed = if on { set.insert(jid.to_string()) } else { set.remove(jid) };
        if changed {
            self.labels_dirty = true;
        }
        changed
    }

    pub fn upsert_label(&mut self, label: Label) {
        self.labels.insert(label.id.clone(), label);
        self.labels_dirty = true;
    }

    pub fn remove_label(&mut self, id: &str) {
        self.labels.remove(id);
        self.label_chats.remove(id);
        self.labels_dirty = true;
    }

    // A fresh label id: WhatsApp numbers them, skipping the predefined
    // ones (below 5) it keeps for itself.
    pub fn next_label_id(&self) -> String {
        let max = self.labels.keys().filter_map(|k| k.parse::<i64>().ok()).max().unwrap_or(4);
        (max.max(4) + 1).to_string()
    }

    pub fn set_members(&mut self, jid: &str, members: Vec<Member>) {
        self.members.insert(jid.to_string(), members);
        self.members_dirty = true;
    }

    pub fn members_of(&self, jid: &str) -> &[Member] {
        self.members.get(jid).map(Vec::as_slice).unwrap_or(&[])
    }

    // Applies a roster change from a group notification. Returns whether
    // the roster is known at all (a fresh fetch fixes an unknown one).
    pub fn patch_members(&mut self, jid: &str, jids: &[String], action: &str) -> bool {
        let Some(list) = self.members.get_mut(jid) else { return false };
        for who in jids {
            match action {
                "add" => {
                    if !list.iter().any(|m| m.jid == *who) {
                        list.push(Member {
                            jid: who.clone(),
                            phone: String::new(),
                            name: String::new(),
                            admin: false,
                        });
                    }
                }
                "remove" => list.retain(|m| m.jid != *who),
                "promote" | "demote" => {
                    if let Some(m) = list.iter_mut().find(|m| m.jid == *who) {
                        m.admin = action == "promote";
                    }
                }
                _ => {}
            }
        }
        self.members_dirty = true;
        true
    }

    // Whether one of our identities is an admin of this group.
    pub fn self_is_admin(&self, jid: &str) -> bool {
        self.members_of(jid).iter().any(|m| {
            m.admin
                && (self.self_jids.contains(&m.jid)
                    || self.self_jids.contains(self.canon(&m.jid))
                    || (!m.phone.is_empty() && self.self_jids.contains(&m.phone)))
        })
    }

    pub fn messages_for(&self, jid: &str) -> &[StoredMessage] {
        self.messages.get(jid).map(Vec::as_slice).unwrap_or(&[])
    }

    // Loads a chat's message list from the vault, merging with whatever
    // arrived over the socket while the chat was cold.
    pub fn hydrate(&mut self, vault: &Vault, jid: &str) {
        let jid = self.canon_owned(jid);
        if self.hydrated.contains(&jid) { return; }
        if let Err(e) = self.flush_history(vault) { eprintln!("[store] hydrate {jid} failed: {e}"); return; }
        let disk = match vault.history_page(&jid, None, 100) {
            Ok(disk) => disk,
            Err(e) => { eprintln!("[store] hydrate {jid} failed: {e}"); return; }
        };
        if let Some(first) = disk.first() {
            self.older_cursors.insert(jid.clone(), (first.timestamp, first.id.clone()));
        }
        if disk.len() < 100 { self.older_exhausted.insert(jid.clone()); }
        self.messages.remove(&jid);
        self.merge_page(&jid, disk);
        self.hydrated.insert(jid.clone());
        self.refresh_audio_preview(&jid);
    }

    fn merge_page(&mut self, jid: &str, disk: Vec<StoredMessage>) -> bool {
        let live = self.messages.entry(jid.to_string()).or_default();
        let removals = &self.pending_removals;
        live.retain(|m| !removals.contains(&(jid.to_string(), m.id.clone())));
        let known: HashSet<String> = live.iter().map(|m| m.id.clone()).collect();
        let before = live.len();
        live.extend(disk.into_iter().filter(|m| !known.contains(&m.id)
            && !removals.contains(&(jid.to_string(), m.id.clone()))));
        live.sort_by(|a, b| (a.timestamp, &a.id).cmp(&(b.timestamp, &b.id)));
        live.len() > before
    }

    pub fn load_older(&mut self, vault: &Vault, jid: &str) -> bool {
        let jid = self.canon_owned(jid);
        self.hydrate(vault, &jid);
        if !self.hydrated.contains(&jid) || self.older_exhausted.contains(&jid) { return false; }
        if let Err(e) = self.flush_history(vault) { eprintln!("[store] load_older {jid} failed: {e}"); return false; }
        let cursor = self.older_cursors.get(&jid).map(|(ts, id)| (*ts, id.as_str()));
        let page = match vault.history_page(&jid, cursor, 100) { Ok(page) => page, Err(e) => { eprintln!("[store] load_older {jid} failed: {e}"); return false; } };
        if let Some(first) = page.first() {
            self.older_cursors.insert(jid.clone(), (first.timestamp, first.id.clone()));
        }
        if page.len() < 100 { self.older_exhausted.insert(jid.clone()); }
        let found = !page.is_empty();
        self.merge_page(&jid, page);
        found
    }

    pub fn hydrate_id(&mut self, vault: &Vault, jid: &str, id: &str) -> bool {
        let jid = self.canon_owned(jid);
        if self.pending_removals.contains(&(jid.clone(), id.to_string())) { return false; }
        if self.messages_for(&jid).iter().any(|m| m.id == id) { return true; }
        if self.flush_history(vault).is_err() { return false; }
        let Ok(Some(message)) = vault.history_id(&jid, id) else { return false };
        self.merge_page(&jid, vec![message]);
        true
    }

    pub fn load_around(&mut self, vault: &Vault, jid: &str, id: &str) -> bool {
        let jid = self.canon_owned(jid);
        if let Err(e) = self.flush_history(vault) { eprintln!("[store] load_around {jid} failed: {e}"); return false; }
        self.hydrate(vault, &jid);
        if !self.hydrate_id(vault, &jid, id) { return false; }
        let Some(target) = self.messages_for(&jid).iter().find(|m| m.id == id) else { return false };
        let mut page = match vault.history_page(&jid, Some((target.timestamp, &target.id)), 50) { Ok(page) => page, Err(e) => { eprintln!("[store] load_around {jid} failed: {e}"); return false; } };
        let after = match vault.history_after(&jid, target.timestamp, &target.id, 50) { Ok(after) => after, Err(e) => { eprintln!("[store] load_around {jid} failed: {e}"); return false; } };
        page.extend(after);
        self.merge_page(&jid, page);
        true
    }

    // Drops a hydrated chat back to metadata-only. Dirty chats stay: their
    // unsaved messages would be lost.
    pub fn evict(&mut self, jid: &str) {
        let jid = self.canon_owned(jid);
        if self.dirty_jids.contains(&jid) || !self.pending_merges.is_empty() {
            return;
        }
        self.messages.remove(&jid);
        self.hydrated.remove(&jid);
        self.older_cursors.remove(&jid);
        self.older_exhausted.remove(&jid);
    }

    // Records a call event; returns true when it is new or changed.
    pub fn upsert_call(
        &mut self,
        id: &str,
        from: &str,
        status: &str,
        video: bool,
        group: bool,
        timestamp: i64,
        outgoing: bool,
    ) -> Option<CallEntry> {
        let jid = self.canon_owned(&normalize_jid(from));
        if let Some(existing) = self.calls.iter_mut().find(|c| c.id == id) {
            if existing.status == status {
                return None;
            }
            existing.status = status.to_string();
            self.calls_dirty = true;
            return Some(existing.clone());
        }
        let entry = CallEntry {
            id: id.to_string(),
            jid,
            video,
            group,
            status: status.to_string(),
            timestamp: if timestamp > 0 { timestamp } else { now_secs() },
            outgoing,
            seen: outgoing,
        };
        self.calls.insert(0, entry.clone());
        self.calls.truncate(100);
        self.calls_dirty = true;
        Some(entry)
    }

    pub fn add_status(&mut self, mut entry: StoredMessage) -> bool {
        if !fresh(entry.timestamp) {
            return false;
        }
        let author = entry.jid.clone();
        entry.sender_jid = author.clone();
        let list = self.statuses.entry(author).or_default();
        if list.iter().any(|m| m.id == entry.id) {
            return false;
        }
        list.push(entry);
        list.sort_by_key(|m| m.timestamp);
        self.statuses_dirty = true;
        true
    }

    pub fn prune_statuses(&mut self) {
        self.statuses.retain(|_, list| {
            list.retain(|m| fresh(m.timestamp));
            !list.is_empty()
        });
    }

    pub fn sidebar_unread_counts(&self) -> (i32, i32, i32) {
        let (mut chats, mut channels, mut communities) = (0i32, 0i32, 0i32);
        for chat in self.chats.values() {
            let unread = i32::from(chat.unread > 0);
            if is_channel(&chat.jid) {
                channels = channels.saturating_add(unread);
            } else {
                if !chat.is_community {
                    chats = chats.saturating_add(unread);
                }
                if chat.is_community || !chat.community.is_empty() {
                    communities = communities.saturating_add(unread);
                }
            }
        }
        (chats, channels, communities)
    }

    pub fn unseen_calls(&self) -> i32 {
        self.calls.iter().filter(|call| !call.outgoing && !call.seen && call.status == "timeout")
            .count().min(i32::MAX as usize) as i32
    }

    pub fn mark_calls_seen(&mut self) {
        for call in &mut self.calls {
            if !call.seen && call.status == "timeout" {
                call.seen = true;
                self.calls_dirty = true;
            }
        }
    }

    pub fn status_authors(&mut self) -> Vec<(String, StoredMessage, usize)> {
        self.prune_statuses();
        let mut out: Vec<(String, StoredMessage, usize)> = self
            .statuses
            .iter()
            .filter_map(|(jid, list)| {
                list.last().map(|latest| (jid.clone(), latest.clone(), list.len()))
            })
            .collect();
        out.sort_by(|a, b| b.1.timestamp.cmp(&a.1.timestamp));
        out
    }

    // The oldest stored message that still has its raw protobuf (needed as
    // the anchor for on-demand history fetches).
    pub fn oldest_message(&self) -> Option<&StoredMessage> {
        self.messages
            .values()
            .flatten()
            .filter(|m| m.timestamp > 0 && m.raw.is_some())
            .min_by_key(|m| m.timestamp)
    }

    // The oldest message of one chat (per-chat backfill anchor).
    pub fn oldest_in_chat(&self, jid: &str) -> Option<&StoredMessage> {
        self.messages
            .get(jid)?
            .iter()
            .find(|m| m.timestamp > 0 && m.raw.is_some())
    }

    // Most recent received GIFs (gif-playback videos) for the picker.
    pub fn recent_gifs(&self, limit: usize) -> Vec<&StoredMessage> {
        let mut out: Vec<&StoredMessage> =
            self.messages.values().flatten().filter(|m| m.gif).collect();
        out.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
        out.truncate(limit);
        out
    }

    // Stickers the user actually sent, newest first, deduped by content.
    pub fn recent_stickers(&self, limit: usize) -> Vec<&StoredMessage> {
        self.dedupe_stickers(|m| m.from_me, limit)
    }

    // Stickers starred on the phone; starring syncs through app state.
    pub fn starred_stickers(&self, limit: usize) -> Vec<&StoredMessage> {
        self.dedupe_stickers(|m| m.starred, limit)
    }

    fn dedupe_stickers(
        &self,
        keep: impl Fn(&StoredMessage) -> bool,
        limit: usize,
    ) -> Vec<&StoredMessage> {
        let mut items: Vec<&StoredMessage> = self
            .messages
            .values()
            .flatten()
            .filter(|m| m.sticker && keep(m))
            .collect();
        items.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        for m in items {
            let key = m
                .raw
                .as_ref()
                .and_then(|r| r.sticker_message.as_option())
                .and_then(|s| s.file_sha256.clone())
                .map(|sha| {
                    use base64::Engine as _;
                    base64::engine::general_purpose::STANDARD.encode(sha)
                })
                .unwrap_or_else(|| m.id.clone());
            if seen.insert(key) {
                out.push(m);
                if out.len() >= limit {
                    break;
                }
            }
        }
        out
    }

    pub fn set_starred(&mut self, jid: &str, id: &str, starred: bool) -> bool {
        if starred {
            self.starred_ids.insert(id.to_string());
        } else {
            self.starred_ids.remove(id);
        }
        self.starred_dirty = true;
        let Some(m) = self.messages.get_mut(jid).and_then(|l| l.iter_mut().find(|x| x.id == id))
        else {
            return false;
        };
        if m.starred == starred {
            return false;
        }
        m.starred = starred;
        self.dirty_jids.insert(jid.to_string());
        true
    }

    pub fn total_messages(&self) -> usize {
        self.messages.values().map(Vec::len).sum()
    }

    // Marks a message as deleted-for-everyone (protocol REVOKE), wiping
    // its body: the destructive path, used when "apply deletions" is on,
    // and always for a revoke performed on this account's own devices.
    pub fn mark_deleted(&mut self, jid: &str, id: &str) -> bool {
        let Some(m) = self.messages.get_mut(jid).and_then(|l| l.iter_mut().find(|x| x.id == id))
        else {
            return false;
        };
        if m.deleted {
            return false;
        }
        m.deleted = true;
        m.revoked = true;
        m.kind = MessageKind::Text;
        m.text = String::new();
        m.edit_versions.clear();
        m.raw = None;
        self.dirty_jids.insert(jid.to_string());
        self.search.remove(jid, id);
        true
    }

    // Notes that someone else revoked this message, without touching its
    // body: the default path while "apply deletions" is off. Text and
    // media survive -- the message stays exactly as rendered, just with
    // a "deleted by sender" indicator -- and it stays in the search
    // index, since its content is still there.
    pub fn mark_revoked(&mut self, jid: &str, id: &str) -> bool {
        let Some(m) = self.messages.get_mut(jid).and_then(|l| l.iter_mut().find(|x| x.id == id))
        else {
            return false;
        };
        if m.revoked {
            return false;
        }
        m.revoked = true;
        self.dirty_jids.insert(jid.to_string());
        true
    }

    // Marks receipts as already sent for these messages, so "send
    // message viewed" (opt-in) doesn't ack them twice.
    pub fn mark_read_sent(&mut self, jid: &str, ids: &[String]) {
        let Some(list) = self.messages.get_mut(jid) else { return };
        let mut changed = false;
        for m in list.iter_mut().filter(|m| ids.contains(&m.id) && !m.read_sent) {
            m.read_sent = true;
            changed = true;
        }
        if changed {
            self.dirty_jids.insert(jid.to_string());
        }
    }

    // Removes messages locally only ("delete for me").
    pub fn remove_messages(&mut self, jid: &str, ids: &[String]) -> bool {
        let jid = self.canon_owned(jid);
        if ids.is_empty() { return false; }
        if let Some(list) = self.messages.get_mut(&jid) {
            list.retain(|m| !ids.contains(&m.id));
        }
        for id in ids {
            self.pending_removals.insert((jid.clone(), id.clone()));
            self.search.remove(&jid, id);
        }
        self.dirty_jids.insert(jid);
        true
    }

    // Updates the delivery status of an own message (monotonic).
    pub fn receipt_participants(&self, m: &StoredMessage) -> (bool, Vec<(String, ParticipantReceipt)>) {
        let known = !is_group(&m.jid) || self.members.contains_key(&m.jid);
        let mut rows: HashMap<String, ParticipantReceipt> = HashMap::new();
        let excluded = |jid: &str| self.self_jids.iter().any(|s| self.canon(s) == jid)
            || (!m.sender_jid.is_empty() && self.canon(&m.sender_jid) == jid);
        if m.from_me {
            if is_group(&m.jid) {
                for member in self.members_of(&m.jid) {
                    let jid = self.canon_owned(&normalize_jid(&member.jid));
                    if !excluded(&jid) { rows.entry(jid).or_default(); }
                }
            } else {
                let jid = self.canon_owned(&m.jid);
                if !excluded(&jid) {
                    let row = rows.entry(jid).or_default();
                    row.level = m.status;
                }
            }
            for (jid, receipt) in &m.receipts {
                let jid = self.canon_owned(&normalize_jid(jid));
                if excluded(&jid) { continue; }
                let row = rows.entry(jid).or_default();
                row.level = row.level.max(receipt.status());
                for (status, time) in [(3, receipt.delivered), (4, receipt.read), (5, receipt.played)] {
                    if let Some(time) = time { row.promote(status, time); }
                }
            }
        }
        let mut rows: Vec<_> = rows.into_iter().collect();
        rows.sort_by(|a, b| self.chat_name(&a.0).cmp(&self.chat_name(&b.0)));
        (known, rows)
    }

    pub fn apply_receipt(&mut self, jid: &str, id: &str, participant: &str, status: u32, timestamp: i64) -> bool {
        let participant = self.canon_owned(&normalize_jid(participant));
        if participant.is_empty() || self.self_jids.iter().any(|s| self.canon(s) == participant) { return false; }
        let Some(m) = self.messages.get_mut(jid).and_then(|v| v.iter_mut().find(|m| m.id == id)) else { return false; };
        if !m.from_me { return false; }
        m.receipts.entry(participant).or_default().promote(status, timestamp);
        m.status = m.status.max(status);
        self.dirty_jids.insert(jid.into());
        true
    }

    pub fn set_status(&mut self, jid: &str, id: &str, status: u32) -> bool {
        let Some(m) = self.messages.get_mut(jid).and_then(|l| l.iter_mut().find(|x| x.id == id))
        else {
            return false;
        };
        if !m.from_me || m.status >= status {
            return false;
        }
        m.status = status;
        self.dirty_jids.insert(jid.to_string());
        true
    }

    // Applies (or removes, when emoji is empty) a reaction to a message.
    pub fn apply_reaction(&mut self, jid: &str, target_id: &str, reactor: &str, emoji: &str) -> bool {
        let Some(target) =
            self.messages.get_mut(jid).and_then(|l| l.iter_mut().find(|m| m.id == target_id))
        else {
            return false;
        };
        if emoji.is_empty() {
            target.reactions.remove(reactor);
        } else {
            target.reactions.insert(reactor.to_string(), clean_text(emoji));
        }
        self.dirty_jids.insert(jid.to_string());
        true
    }

    // ---- persistence (WhatsApp only sends history sync at pairing time,
    // so everything must survive restarts locally) ----

    fn flush_history(&mut self, vault: &Vault) -> Result<(), String> {
        for (jid, id) in &self.pending_removals {
            if let Some(live) = self.messages.get_mut(jid) {
                live.retain(|m| m.id != *id);
            }
            self.search.remove(jid, id);
        }
        let messages: Vec<StoredMessage> = self.dirty_jids.iter()
            .filter_map(|jid| self.messages.get(jid)).flatten().cloned().collect();
        let clears: Vec<String> = self.deleted_jids.iter().cloned().collect();
        let removals: Vec<(String, String)> = self.pending_removals.iter().cloned().collect();
        vault.history_commit(&messages, &clears, &removals, &self.pending_merges)?;
        for message in &messages {
            if message.deleted { self.search.remove(&message.jid, &message.id); }
            else { self.search.add(&message.jid, message); }
        }
        for (from, into) in std::mem::take(&mut self.pending_merges) {
            self.search.remove_chat(&from);
            self.index_from_vault(vault, &into);
        }
        self.deleted_jids.clear();
        self.pending_removals.clear();
        for jid in &self.dirty_jids {
            if !self.hydrated.contains(jid) {
                self.messages.remove(jid);
            }
        }
        self.dirty_jids.clear();
        Ok(())
    }

    pub fn save_to(&mut self, vault: &Vault) {
        if let Err(error) = self.save_checked(vault) {
            eprintln!("[store] save failed: {error}");
        }
    }

    pub fn save_checked(&mut self, vault: &Vault) -> Result<(), String> {
        self.flush_history(vault)?;
        vault.set_checked("store:chats", &to_json(&self.chats))?;
        vault.set_checked("store:drafts", &to_json(&self.drafts))?;
        vault.set_checked("store:contacts", &to_json(&self.contacts))?;
        vault.set_checked("store:pushnames", &to_json(&self.push_names))?;
        vault.set_checked("store:saved", &to_json(&self.saved_contacts))?;
        vault.set_checked("store:aliases", &to_json(&self.aliases))?;
        if self.starred_dirty {
            vault.set_checked("store:starred", &to_json(&self.starred_ids))?;
            self.starred_dirty = false;
        }
        if self.calls_dirty {
            vault.set_checked("store:calls", &to_json(&self.calls))?;
            self.calls_dirty = false;
        }
        if self.statuses_dirty {
            self.prune_statuses();
            let dehydrated: HashMap<&String, Vec<&StoredMessage>> = self
                .statuses
                .iter()
                .map(|(jid, list)| (jid, list.iter().collect()))
                .collect();
            vault.set_checked("store:statuses", &to_json(&dehydrated))?;
            self.statuses_dirty = false;
        }
        if self.members_dirty {
            vault.set_checked("store:members", &to_json(&self.members))?;
            self.members_dirty = false;
        }
        if self.labels_dirty {
            vault.set_checked("store:labels", &to_json(&self.labels))?;
            vault.set_checked("store:labelchats", &to_json(&self.label_chats))?;
            self.labels_dirty = false;
        }
        self.flush_history(vault)?;
        self.search.save_to(vault);
        Ok(())
    }


    // Indexes one cold chat straight from the vault (first-run backfill
    // for chats stored before the search index existed).
    fn refresh_audio_preview(&mut self, jid: &str) {
        let preview = self.messages.get(jid)
            .and_then(|messages| messages.iter().max_by_key(|m| m.timestamp))
            .filter(|m| m.kind == MessageKind::Audio
                && self.chats.get(jid).is_some_and(|chat| chat.timestamp == m.timestamp))
            .map(|m| compute_preview(m, self));
        if let Some(preview) = preview {
            if let Some(chat) = self.chats.get_mut(jid) {
                chat.preview = preview;
            }
        }
    }

    pub fn index_from_vault(&mut self, vault: &Vault, jid: &str) {
        let jid = self.canon_owned(jid);
        let mut cursor: Option<(i64, String)> = None;
        loop {
            let Ok(page) = vault.history_page(&jid, cursor.as_ref().map(|(ts, id)| (*ts, id.as_str())), 100) else { return };
            for message in &page {
                if !self.pending_removals.contains(&(jid.clone(), message.id.clone())) {
                    self.search.add(&jid, message);
                }
            }
            if page.len() < 100 { break; }
            cursor = page.first().map(|m| (m.timestamp, m.id.clone()));
        }
        self.search.mark_complete(&jid);
    }


    pub fn load_from(&mut self, vault: &Vault) {
        fn read<T: serde::de::DeserializeOwned>(vault: &Vault, key: &str) -> Option<T> {
            serde_json::from_str(&vault.get(key)?).ok()
        }
        if let Some(v) = read(vault, "store:chats") {
            self.chats = v;
        }
        if let Some(v) = read(vault, "store:drafts") {
            self.drafts = v;
        }
        if let Some(v) = read(vault, "store:contacts") {
            self.contacts = v;
        }
        if let Some(v) = read(vault, "store:pushnames") {
            self.push_names = v;
        }
        if let Some(v) = read(vault, "store:saved") {
            self.saved_contacts = v;
        }
        if let Some(v) = read(vault, "store:aliases") {
            self.aliases = v;
            self.pending_merges.extend(self.aliases.iter().filter(|(from, into)| from != into)
                .map(|(from, into)| (from.clone(), into.clone())));
        }
        // Message bodies stay on disk; chats hydrate on open/hover. The
        // list itself renders from ChatMeta.preview alone.
        if let Some(v) = read(vault, "store:starred") {
            self.starred_ids = v;
        }
        if let Some(v) = read(vault, "store:calls") {
            self.calls = v;
        }
        if let Some(v) = read::<HashMap<String, Vec<StoredMessage>>>(vault, "store:statuses") {
            self.statuses = v;
            self.prune_statuses();
        }
        if let Some(v) = read(vault, "store:members") {
            self.members = v;
        }
        if let Some(v) = read(vault, "store:labels") {
            self.labels = v;
        }
        if let Some(v) = read(vault, "store:labelchats") {
            self.label_chats = v;
        }
        self.search.open(vault);
    }
}

fn to_json<T: serde::Serialize>(value: &T) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "null".to_string())
}

// Strips the device part and maps known server aliases (c.us -> pn).
pub fn normalize_jid(jid: &str) -> String {
    match jid.parse::<whatsapp_rust::Jid>() {
        Ok(parsed) => parsed.to_non_ad_string(),
        Err(_) => jid.to_string(),
    }
}

// One line describing a quoted message, from the copy WhatsApp embeds in
// the reply.
pub fn quoted_summary(quoted: &wa::Message) -> String {
    use whatsapp_rust::proto_helpers::MessageExt as _;
    if quoted.is_view_once() { return t("compat.view_once"); }
    if let Some(card) = crate::wa_map::compatibility_card(quoted.get_base_message()) {
        return card.chars().take(120).collect();
    }
    let inner: &wa::Message = quoted
        .ephemeral_message
        .as_option()
        .and_then(|e| e.message.as_option())
        .or_else(|| quoted.view_once_message_v2.as_option().and_then(|v| v.message.as_option()))
        .unwrap_or(quoted);
    if let Some(text) = inner
        .conversation
        .as_deref()
        .or_else(|| inner.extended_text_message.as_option().and_then(|e| e.text.as_deref()))
    {
        return clean_text(text).chars().take(120).collect();
    }
    if let Some(img) = inner.image_message.as_option() {
        return match img.caption.as_deref() {
            Some(c) if !c.is_empty() => format!("{} {}", t("preview.photo"), clean_text(c)),
            _ => t("preview.photo"),
        };
    }
    if inner.sticker_message.is_set() {
        return t("preview.sticker");
    }
    if let Some(audio) = inner.audio_message.as_option() {
        return audio_preview(audio.ptt.unwrap_or(false), audio.seconds.unwrap_or(0));
    }
    if let Some(v) = inner.video_message.as_option() {
        return match v.caption.as_deref() {
            Some(c) if !c.is_empty() => format!("{} {}", t("preview.video"), clean_text(c)),
            _ => t("preview.video"),
        };
    }
    if let Some(doc) = inner.document_message.as_option() {
        let name = doc.file_name.as_deref().unwrap_or("");
        return ta("preview.document", &[&clean_text(name)]);
    }
    String::new()
}

// What a message looks like in the chat list, without the sender prefix.
fn audio_preview(voice: bool, seconds: u32) -> String {
    let label = t(if voice { "preview.voice" } else { "preview.audio" });
    if seconds > 0 { format!("{label} ({})", format_duration(seconds)) } else { label }
}

pub fn preview_body(stored: &StoredMessage) -> String {
    match stored.kind {
        MessageKind::Image => t("preview.photo"),
        MessageKind::Audio => audio_preview(stored.is_voice_message(), stored.duration_sec),
        MessageKind::Video => {
            if stored.gif { t("preview.gif") } else { t("preview.video") }
        }
        MessageKind::Doc => ta("preview.document", &[&stored.text]),
        MessageKind::Text | MessageKind::System => {
            stored.text.split_whitespace().collect::<Vec<_>>().join(" ")
        }
    }
}

pub fn compute_preview(stored: &StoredMessage, store: &Store) -> String {
    let body = store.named_mentions(&preview_body(stored));
    if stored.kind == MessageKind::System {
        return body;
    }
    let prefix = if stored.from_me {
        "✓ ".to_string()
    } else if is_group(&stored.jid) && !stored.sender.is_empty() {
        format!("{}: ", clean_text(&stored.sender).split(' ').next().unwrap_or(""))
    } else {
        String::new()
    };
    format!("{prefix}{body}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn history_message(jid: &str, n: i64) -> StoredMessage {
        let mut message = crate::wa_map::system_message(jid, &format!("{n:05}"), n / 3, format!("history body {n}"));
        message.kind = MessageKind::Text;
        message
    }

    #[test]
    fn durable_history_restart_pages_and_off_window_mutations() {
        let path = std::env::temp_dir().join(format!("zapive_history_restart_{}.db", std::process::id()));
        crate::paths::remove_db(&path);
        {
            let mut vault = Vault::open_at(&path.to_string_lossy()).unwrap();
            vault.open().unwrap();
            let mut store = Store::default();
            for n in 0..751 { store.add_message(history_message("chat", n)); }
            store.save_checked(&vault).unwrap();
            assert_eq!(vault.history_page("chat", None, 1000).unwrap().len(), 751);
            assert!(store.messages_for("chat").is_empty());
        }
        {
            let mut vault = Vault::open_at(&path.to_string_lossy()).unwrap();
            vault.open().unwrap();
            let mut store = Store::default();
            store.hydrate(&vault, "chat");
            assert_eq!(store.messages_for("chat").len(), 100);
            assert!(store.hydrate_id(&vault, "chat", "00001"));
            assert!(store.mark_deleted("chat", "00001"));
            assert!(store.remove_messages("chat", &["00002".into()]));
            store.save_checked(&vault).unwrap();
            store.evict("chat");
            store.hydrate(&vault, "chat");
            assert!(store.load_around(&vault, "chat", "00200"));
            while store.load_older(&vault, "chat") {}
            assert_eq!(store.messages_for("chat").len(), 750);
            assert!(store.messages_for("chat").iter().find(|m| m.id == "00001").unwrap().deleted);
            assert!(vault.history_id("chat", "00002").unwrap().is_none());
            store.add_message(history_message("chat", 999));
            vault.lock();
            assert!(store.save_checked(&vault).is_err());
            store.evict("chat");
            assert!(store.messages_for("chat").iter().any(|m| m.id == "00999"));
            vault.open().unwrap();
            store.save_checked(&vault).unwrap();
            assert!(vault.history_id("chat", "00999").unwrap().is_some());
        }
        crate::paths::remove_db(&path);
    }

    #[test]
    fn durable_history_removals_survive_loading_dirty_save_and_restart() {
        for around in [false, true] {
            let path = std::env::temp_dir().join(format!(
                "zapive_history_removals_{}_{around}.db", std::process::id()
            ));
            crate::paths::remove_db(&path);
            {
                let mut vault = Vault::open_at(&path.to_string_lossy()).unwrap();
                vault.open().unwrap();
                let mut store = Store::default();
                for n in 0..300 { store.add_message(history_message("chat", n)); }
                store.save_checked(&vault).unwrap();
                if around { store.hydrate(&vault, "chat"); }
                let removed = if around { ["00249", "00251"] } else { ["00298", "00299"] };
                assert!(store.remove_messages("chat", &removed.map(String::from)));
                if around {
                    assert!(store.load_around(&vault, "chat", "00250"));
                } else {
                    store.hydrate(&vault, "chat");
                }
                assert!(store.messages_for("chat").iter().all(|m| !removed.contains(&m.id.as_str())));
                assert!(!store.hydrate_id(&vault, "chat", removed[0]));
                assert!(!store.load_around(&vault, "chat", removed[1]));
                assert!(store.add_message(history_message("chat", 999)));
                store.save_checked(&vault).unwrap();
            }
            {
                let mut vault = Vault::open_at(&path.to_string_lossy()).unwrap();
                vault.open().unwrap();
                let mut store = Store::default();
                store.load_from(&vault);
                store.hydrate(&vault, "chat");
                while store.load_older(&vault, "chat") {}
                let removed = if around { ["00249", "00251"] } else { ["00298", "00299"] };
                assert_eq!(store.messages_for("chat").len(), 299);
                for id in removed {
                    assert!(vault.history_id("chat", id).unwrap().is_none());
                    assert!(store.messages_for("chat").iter().all(|m| m.id != id));
                }
                assert!(store.messages_for("chat").iter().any(|m| m.id == "00999"));
            }
            crate::paths::remove_db(&path);
        }
    }

    #[test]
    fn durable_history_pending_removals_filter_merges_and_resident_flush() {
        let mut vault = Vault::open_at(":memory:").unwrap();
        vault.open().unwrap();
        let mut store = Store::default();
        store.hydrated.insert("chat".into());
        store.add_message(history_message("chat", 1));
        store.save_checked(&vault).unwrap();
        assert!(store.remove_messages("chat", &["00001".into()]));
        assert!(!store.merge_page("chat", vec![history_message("chat", 1)]));
        assert!(!store.add_message(history_message("chat", 1)));
        assert!(!store.hydrate_id(&vault, "chat", "00001"));
        store.add_message(history_message("old@lid", 1));
        store.learn_alias("old@lid", "chat");
        assert!(store.messages_for("chat").is_empty());
        store.messages.get_mut("chat").unwrap().push(history_message("chat", 1));
        store.save_checked(&vault).unwrap();
        assert!(store.messages_for("chat").is_empty());
        assert!(store.pending_removals.is_empty());
        store.add_message(history_message("chat", 2));
        store.save_checked(&vault).unwrap();
        let mut restarted = Store::default();
        restarted.hydrate(&vault, "chat");
        assert_eq!(restarted.messages_for("chat").len(), 1);
        assert_eq!(restarted.messages_for("chat")[0].id, "00002");
        assert!(vault.history_id("chat", "00001").unwrap().is_none());
    }

    #[test]
    fn durable_history_legacy_migration_rollback_and_account_isolation() {
        let mut vault = Vault::open_at(":memory:").unwrap();
        vault.open().unwrap();
        let legacy: Vec<_> = (0..610).map(|n| history_message("chat", n)).collect();
        vault.set_checked("store:msgs:chat", &to_json(&legacy)).unwrap();
        vault.set_checked("store:msgs:broken", "invalid json").unwrap();
        vault.history_commit(&[history_message("broken", 900)], &["broken".into()], &[], &[]).unwrap();
        assert_eq!(vault.history_page("broken", None, 1000).unwrap().len(), 1);
        assert!(vault.get("store:msgs:chat").is_some());
        assert!(vault.get("store:msgs:broken").is_some());
        let mut store = Store::default();
        store.hydrate(&vault, "chat");
        assert_eq!(store.messages_for("chat").len(), 100);
        assert!(vault.get("store:msgs:chat").is_none());
        assert_eq!(vault.history_page("chat", None, 1000).unwrap().len(), 610);
        vault.set_checked("store:msgs:chat", &to_json(&legacy)).unwrap();
        assert_eq!(vault.history_page("chat", None, 1000).unwrap().len(), 610);
        vault.set_account("other");
        assert!(vault.history_page("chat", None, 1000).unwrap().is_empty());
        vault.history_commit(&[history_message("chat", 999)], &[], &[], &[]).unwrap();
        vault.set_account("");
        store.remove_messages("chat", &["00000".into()]);
        store.save_checked(&vault).unwrap();
        assert!(vault.history_id("chat", "00000").unwrap().is_none());
        store.clear_messages("chat");
        store.add_message(history_message("chat", 888));
        store.save_checked(&vault).unwrap();
        assert_eq!(vault.history_page("chat", None, 1000).unwrap().len(), 1);
        vault.set_account("other");
        assert_eq!(vault.history_page("chat", None, 1000).unwrap()[0].id, "00999");
        vault.wipe_account("");
        assert_eq!(vault.history_page("chat", None, 1000).unwrap().len(), 1);
        vault.wipe_account("other");
        assert!(vault.history_page("chat", None, 1000).unwrap().is_empty());
    }

    #[test]
    fn durable_history_cold_alias_merge_and_delete_chat() {
        let mut vault = Vault::open_at(":memory:").unwrap();
        vault.open().unwrap();
        let source: Vec<_> = (0..620).map(|n| history_message("old@lid", n)).collect();
        vault.set_checked("store:msgs:old@lid", &to_json(&source)).unwrap();
        vault.history_commit(&[history_message("new", 900)], &[], &[], &[]).unwrap();
        let mut store = Store::default();
        store.learn_alias("old@lid", "new");
        store.save_checked(&vault).unwrap();
        assert!(vault.history_page("old@lid", None, 1000).unwrap().is_empty());
        let merged = vault.history_page("new", None, 1000).unwrap();
        assert_eq!(merged.len(), 621);
        assert!(merged.iter().all(|m| m.jid == "new"));
        store.delete_chat("new");
        store.save_checked(&vault).unwrap();
        assert!(vault.history_page("new", None, 1000).unwrap().is_empty());
    }

    #[test]
    fn sidebar_time_uses_calendar_day_boundaries() {
        let now = chrono::NaiveDate::from_ymd_opt(2026, 1, 2).unwrap();
        let at = |year, month, day, hour, minute| {
            chrono::NaiveDate::from_ymd_opt(year, month, day).unwrap()
                .and_hms_opt(hour, minute, 0).unwrap()
        };
        assert_eq!(format_time(0), "");
        assert_eq!(format_time_at(at(2026, 1, 2, 9, 5), now), "09:05");
        assert_eq!(format_time_at(at(2026, 1, 1, 23, 59), now), t("day.yesterday"));
        for (day, key) in [(31, "day.wednesday"), (30, "day.tuesday"),
            (29, "day.monday"), (28, "day.sunday"), (27, "day.saturday")] {
            assert_eq!(format_time_at(at(2025, 12, day, 23, 59), now), t(key));
        }
        assert_eq!(format_time_at(at(2025, 12, 26, 23, 59), now), "26/12");
        assert_eq!(format_time_at(at(2025, 1, 2, 9, 5), now), "02/01");
        assert_eq!(format_time_at(at(2026, 1, 3, 9, 5), now), "03/01");
        let now = chrono::NaiveDate::from_ymd_opt(2026, 1, 4).unwrap();
        assert_eq!(format_time_at(at(2026, 1, 2, 0, 0), now), t("day.friday"));
        assert_eq!(format_time_at(at(2026, 1, 1, 0, 0), now), t("day.thursday"));
    }

    #[test]
    fn sidebar_time_uses_local_dates_across_offset_changes() {
        for (message, now, key) in [
            ("2026-03-07T23:30:00-05:00", "2026-03-09T00:15:00-04:00", "day.saturday"),
            ("2026-10-31T00:15:00-04:00", "2026-11-01T23:30:00-05:00", "day.yesterday"),
        ] {
            let message = chrono::DateTime::parse_from_rfc3339(message).unwrap();
            let now = chrono::DateTime::parse_from_rfc3339(now).unwrap();
            assert_eq!(format_time_at(message.naive_local(), now.date_naive()), t(key));
        }
    }

    #[test]
    fn draft_sorting_preserves_pinned_precedence() {
        let mut store = Store::default();
        for (jid, timestamp, pinned, text, promoted) in [
            ("pin-new", 1, 20, "", 0),
            ("pin-old", 2, 10, "draft", 999),
            ("draft-new", 3, 0, "draft", 200),
            ("draft-old", 4, 0, "draft", 100),
            ("normal", 50, 0, "", 0),
            ("blank", 40, 0, " \n\t", 999),
            ("unpromoted", 30, 0, "draft", 0),
        ] {
            store.chats.insert(jid.into(), ChatMeta {
                jid: jid.into(), timestamp, pinned, ..Default::default()
            });
            store.drafts.insert(jid.into(), Draft {
                text: text.into(), promoted, ..Default::default()
            });
        }
        assert_eq!(store.sorted_chats().iter().map(|chat| chat.jid.as_str()).collect::<Vec<_>>(),
            ["pin-new", "pin-old", "draft-new", "draft-old", "normal", "blank", "unpromoted"]);
        assert_eq!(store.chats["draft-new"].timestamp, 3);
        assert_eq!(store.chats["draft-old"].timestamp, 4);
    }

    #[test]
    fn draft_removal_restores_timestamp_order() {
        let mut store = Store::default();
        for (jid, timestamp) in [("older", 10), ("newer", 20)] {
            store.chats.insert(jid.into(), ChatMeta {
                jid: jid.into(), timestamp, ..Default::default()
            });
        }
        store.drafts.insert("older".into(), Draft {
            text: "draft".into(), promoted: 1, ..Default::default()
        });
        assert_eq!(store.sorted_chats()[0].jid, "older");
        store.drafts.remove("older");
        assert_eq!(store.sorted_chats().iter().map(|chat| chat.jid.as_str()).collect::<Vec<_>>(), ["newer", "older"]);
        assert_eq!(store.chats["older"].timestamp, 10);
        assert_eq!(store.chats["newer"].timestamp, 20);
    }

    #[test]
    fn draft_serialization_preserves_mentions() {
        let draft = Draft {
            text: "Hello @Alice and @Bob 👋".into(),
            mentions: vec![("Alice".into(), "1@s.whatsapp.net".into()), ("Bob".into(), "2@lid".into())],
            promoted: u64::MAX,
        };
        let drafts = HashMap::from([("group@g.us".to_string(), draft.clone())]);
        let restored: HashMap<String, Draft> = serde_json::from_str(&to_json(&drafts)).unwrap();
        assert_eq!(restored["group@g.us"].text, draft.text);
        assert_eq!(restored["group@g.us"].mentions, draft.mentions);
        assert_eq!(restored["group@g.us"].promoted, draft.promoted);
    }

    #[test]
    fn draft_merge_keeps_newest_nonblank_draft_and_mentions() {
        let mut store = Store::default();
        for (source_text, source_promoted, target_text, target_promoted, expected) in [
            ("source", 20, "target", 10, "source"),
            ("source", 10, "target", 20, "target"),
            ("source", 10, "target", 10, "target"),
            (" \t", 30, "target", 20, "target"),
            ("source", 0, "", 20, "source"),
        ] {
            for (jid, text, promoted) in [("from", source_text, source_promoted), ("into", target_text, target_promoted)] {
                store.drafts.insert(jid.into(), Draft {
                    text: text.into(), mentions: vec![(text.into(), jid.into())], promoted,
                });
            }
            store.merge_jid("from", "into");
            assert!(!store.drafts.contains_key("from"));
            assert_eq!(store.drafts["into"].text, expected);
            assert_eq!(store.drafts["into"].mentions[0].0, expected);
        }
        store.drafts.remove("into");
        store.drafts.insert("from".into(), Draft {
            text: "only source".into(), ..Default::default()
        });
        store.merge_jid("from", "into");
        assert_eq!(store.drafts["into"].text, "only source");
    }

    #[test]
    fn delete_chat_removes_draft() {
        let mut store = Store::default();
        store.chats.insert("chat".into(), ChatMeta { jid: "chat".into(), ..Default::default() });
        store.drafts.insert("chat".into(), Draft { text: "draft".into(), ..Default::default() });
        store.delete_chat("chat");
        assert!(!store.chats.contains_key("chat"));
        assert!(!store.drafts.contains_key("chat"));
    }

    #[test]
    fn sidebar_counts_include_archived_and_community_groups() {
        let mut store = Store::default();
        for (jid, unread, community, shell) in [
            ("a@s.whatsapp.net", 3, "", false),
            ("b@g.us", 5, "community@g.us", false),
            ("community@g.us", 2, "", true),
            ("c@newsletter", 7, "", false),
        ] {
            store.chats.insert(jid.into(), ChatMeta {
                jid: jid.into(), unread, community: community.into(), is_community: shell,
                archived: true, ..Default::default()
            });
        }
        assert_eq!(store.sidebar_unread_counts(), (2, 1, 2));
        store.chats.get_mut("a@s.whatsapp.net").unwrap().unread = u32::MAX;
        assert_eq!(store.sidebar_unread_counts(), (2, 1, 2));
        store.chats.get_mut("b@g.us").unwrap().unread = 0;
        assert_eq!(store.sidebar_unread_counts(), (1, 1, 1));
        for chat in store.chats.values_mut() {
            chat.unread = 0;
        }
        assert_eq!(store.sidebar_unread_counts(), (0, 0, 0));
    }

    #[test]
    fn missed_call_badge_clears_and_persists_seen_state() {
        let mut store = Store::default();
        store.upsert_call("incoming", "a@s.whatsapp.net", "offer", false, false, 1, false);
        assert_eq!(store.unseen_calls(), 0);
        store.upsert_call("incoming", "a@s.whatsapp.net", "timeout", false, false, 1, false);
        store.upsert_call("outgoing", "a@s.whatsapp.net", "timeout", false, false, 1, true);
        assert_eq!(store.unseen_calls(), 1);
        store.mark_calls_seen();
        assert_eq!(store.unseen_calls(), 0);
        let restored: Vec<CallEntry> = serde_json::from_str(&serde_json::to_string(&store.calls).unwrap()).unwrap();
        assert!(restored.iter().all(|call| call.seen));
        let legacy: CallEntry = serde_json::from_str(r#"{"id":"old","jid":"a@s.whatsapp.net","video":false,"group":false,"status":"timeout","timestamp":1}"#).unwrap();
        assert!(!legacy.seen);
    }

    #[test]
    fn edit_versions_roundtrip_dedupe_and_out_of_order() {
        let mut m = crate::wa_map::system_message("g@g.us", "edit", 100, "original".into());
        assert!(m.record_edit("second".into(), 200));
        assert!(!m.record_edit("second".into(), 201));
        assert!(m.record_edit("current".into(), 400));
        assert!(m.record_edit("middle".into(), 300));
        assert_eq!(m.text, "current");
        assert!(!m.record_edit("second".into(), 200));
        let restored: StoredMessage = serde_json::from_str(&serde_json::to_string(&m).unwrap()).unwrap();
        assert_eq!(restored.edit_versions.iter().map(|v| v.text.as_str()).collect::<Vec<_>>(), ["original", "second", "middle", "current"]);
        assert_eq!(restored.edit_versions[0].timestamp, Some(100));
        let mut value = serde_json::to_value(&m).unwrap();
        value.as_object_mut().unwrap().remove("edit_versions");
        value.as_object_mut().unwrap().remove("receipts");
        let mut legacy: StoredMessage = serde_json::from_value(value).unwrap();
        assert!(legacy.edit_versions.is_empty());
        assert!(legacy.record_edit("observed".into(), 500));
        assert_eq!(legacy.edit_versions[0].timestamp, None);
    }

    #[test]
    fn receipts_promote_without_inventing_times_and_group_canonical_roster() {
        let mut store = Store::default();
        store.set_self(&["self@s.whatsapp.net"]);
        store.learn_alias("person@lid", "person@s.whatsapp.net");
        let mut m = crate::wa_map::system_message("g@g.us", "receipt", 100, "body".into());
        m.from_me = true;
        store.add_message(m);
        assert!(!store.receipt_participants(&store.messages_for("g@g.us")[0]).0);
        assert!(store.apply_receipt("g@g.us", "receipt", "person@lid", 4, 400));
        assert!(store.apply_receipt("g@g.us", "receipt", "person@s.whatsapp.net", 3, 300));
        assert!(store.apply_receipt("g@g.us", "receipt", "person@lid", 3, 350));
        assert!(!store.apply_receipt("g@g.us", "receipt", "self@s.whatsapp.net", 4, 200));
        store.set_members("g@g.us", ["person@s.whatsapp.net", "person@lid", "self@s.whatsapp.net", "other@s.whatsapp.net"].iter().map(|jid| Member {
            jid: (*jid).into(), phone: String::new(), name: String::new(), admin: false,
        }).collect());
        let m = &store.messages_for("g@g.us")[0];
        let (known, rows) = store.receipt_participants(m);
        assert!(known);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows.iter().find(|(jid, _)| jid == "person@s.whatsapp.net").unwrap().1.status(), 4);
        assert_eq!(rows.iter().find(|(jid, _)| jid == "other@s.whatsapp.net").unwrap().1.status(), 0);
        let restored: StoredMessage = serde_json::from_str(&serde_json::to_string(m).unwrap()).unwrap();
        let receipt = &restored.receipts["person@s.whatsapp.net"];
        assert_eq!(receipt.delivered, Some(300));
        assert_eq!(receipt.read, Some(400));
        assert_eq!(restored.status, 4);
        let mut only_read = ParticipantReceipt::default();
        only_read.promote(4, 500);
        assert_eq!(only_read.status(), 4);
        assert_eq!(only_read.delivered, None);
        let mut no_time = ParticipantReceipt::default();
        no_time.promote(3, 0);
        assert_eq!(no_time.status(), 3);
        assert_eq!(no_time.delivered, None);
    }

    #[test]
    fn private_legacy_receipts_keep_status_without_fabricated_time() {
        let store = Store::default();
        let mut m = crate::wa_map::system_message("peer@s.whatsapp.net", "private", 100, "body".into());
        m.from_me = true;
        m.status = 4;
        let (known, rows) = store.receipt_participants(&m);
        assert!(known);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].1.status(), 4);
        assert_eq!(rows[0].1.read, None);
        assert_eq!(rows[0].1.delivered, None);
        m.from_me = false;
        assert!(store.receipt_participants(&m).1.is_empty());
    }

    #[test]
    fn deleting_message_discards_observed_edit_bodies() {
        let mut store = Store::default();
        let mut m = crate::wa_map::system_message("peer@s.whatsapp.net", "deleted", 100, "original".into());
        m.record_edit("updated".into(), 200);
        store.add_message(m);
        assert!(store.mark_deleted("peer@s.whatsapp.net", "deleted"));
        let m = &store.messages_for("peer@s.whatsapp.net")[0];
        assert!(m.edit_versions.is_empty());
        assert!(m.text.is_empty());
    }

    fn audio_message(ptt: Option<bool>) -> StoredMessage {
        let raw = wa::Message {
            audio_message: wa::message::AudioMessage { ptt, seconds: Some(65), ..Default::default() }.into(),
            ..Default::default()
        };
        let mut stored = crate::wa_map::system_message("123@s.whatsapp.net", "audio", 100, String::new());
        stored.kind = MessageKind::Audio;
        stored.duration_sec = 65;
        stored.raw = Some(Arc::new(raw));
        stored
    }

    #[test]
    fn audio_preview_distinguishes_ptt_and_defaults_to_file() {
        for ptt in [None, Some(false), Some(true)] {
            let stored = audio_message(ptt);
            assert_eq!(stored.is_voice_message(), ptt == Some(true));
            let label = t(if ptt == Some(true) { "preview.voice" } else { "preview.audio" });
            assert_eq!(preview_body(&stored), format!("{label} (1:05)"));
            assert_eq!(quoted_summary(stored.raw.as_ref().unwrap()), preview_body(&stored));
            let restored: StoredMessage = serde_json::from_str(&serde_json::to_string(&stored).unwrap()).unwrap();
            assert_eq!(restored.is_voice_message(), stored.is_voice_message());
        }
        let mut stored = audio_message(Some(true));
        stored.raw = None;
        stored.duration_sec = 0;
        assert!(!stored.is_voice_message());
        assert_eq!(preview_body(&stored), t("preview.audio"));
    }

    #[test]
    fn audio_filename_uses_only_original_sources_or_document_names() {
        let mut stored = audio_message(Some(false));
        assert_eq!(stored.audio_filename(Some(r"D:\Music\original song.mp3")), "original song.mp3");
        assert_eq!(stored.audio_filename(Some("/music/original.flac")), "original.flac");
        assert_eq!(stored.audio_filename(None), t("audio.fallback_name"));
        stored.kind = MessageKind::Doc;
        stored.raw = Some(Arc::new(wa::Message {
            document_message: wa::message::DocumentMessage {
                mimetype: Some("audio/mpeg".into()), file_name: Some("received.mp3".into()), ..Default::default()
            }.into(),
            ..Default::default()
        }));
        assert_eq!(stored.audio_filename(None), "received.mp3");
        assert_eq!(stored.audio_filename(Some("")), "received.mp3");
    }

    #[test]
    fn audio_preview_refresh_preserves_prefix_and_newer_chat_metadata() {
        let mut store = Store::default();
        let mut stored = audio_message(Some(true));
        stored.from_me = true;
        let jid = stored.jid.clone();
        store.add_message(stored.clone());
        store.chats.get_mut(&jid).unwrap().preview = "old preview".into();
        store.refresh_audio_preview(&jid);
        assert_eq!(store.chats[&jid].preview, format!("✓ {}", preview_body(&stored)));
        store.chats.get_mut(&jid).unwrap().timestamp += 1;
        store.chats.get_mut(&jid).unwrap().preview = "newer preview".into();
        store.refresh_audio_preview(&jid);
        assert_eq!(store.chats[&jid].preview, "newer preview");
    }

    #[test]
    fn builtin_filters_use_protocol_type_not_names_or_label_ids() {
        for list_type in [1, 2, 3] {
            for name in ["Unread", "Não lidas", "Groups", "Grupos", "Favorites", "Favoritos", "renamed"] {
                let label = Label { id: "123".into(), name: name.into(), list_type: Some(list_type), ..Default::default() };
                assert!(label.is_builtin_filter());
                let restored: Label = serde_json::from_str(&serde_json::to_string(&label).unwrap()).unwrap();
                assert!(restored.is_builtin_filter());
            }
        }
        for list_type in [None, Some(0), Some(4), Some(5), Some(6), Some(7), Some(12)] {
            for id in ["1", "2", "3", "unread", "groups", "favorites"] {
                let label = Label { id: id.into(), name: "Unread".into(), list_type, ..Default::default() };
                assert!(!label.is_builtin_filter());
            }
        }
        let legacy: Label = serde_json::from_str(r#"{"id":"1","name":"Groups"}"#).unwrap();
        assert!(!legacy.is_builtin_filter());
    }

    fn text_msg(jid: &str, id: &str, text: &str) -> StoredMessage {
        let mut m = crate::wa_map::system_message(jid, id, 1, text.to_string());
        m.kind = MessageKind::Text;
        m
    }

    #[test]
    fn mark_deleted_wipes_the_body_mark_revoked_keeps_it() {
        let mut store = Store::default();
        store.add_message(text_msg("a@g.us", "1", "hello"));
        store.add_message(text_msg("a@g.us", "2", "world"));

        // "Apply deletions" off: the message stays, only flagged.
        assert!(store.mark_revoked("a@g.us", "1"));
        let kept = store.messages_for("a@g.us").iter().find(|m| m.id == "1").unwrap();
        assert!(kept.revoked);
        assert!(!kept.deleted);
        assert_eq!(kept.text, "hello");

        // "Apply deletions" on (or a revoke from this account's own
        // devices): the destructive path wipes the body.
        assert!(store.mark_deleted("a@g.us", "2"));
        let wiped = store.messages_for("a@g.us").iter().find(|m| m.id == "2").unwrap();
        assert!(wiped.revoked);
        assert!(wiped.deleted);
        assert!(wiped.text.is_empty());

        // Idempotent: a repeat reports no change and doesn't clobber
        // anything already set.
        assert!(!store.mark_revoked("a@g.us", "1"));
        assert!(!store.mark_deleted("a@g.us", "2"));
    }
}
