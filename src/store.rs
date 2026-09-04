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
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub gif: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub starred: bool,
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

pub fn reaction_summary(m: &StoredMessage) -> String {
    let values: Vec<&String> = m.reactions.values().filter(|v| !v.is_empty()).collect();
    if values.is_empty() {
        return String::new();
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
    let joined: String = unique.concat();
    if values.len() > 1 { format!("{joined} {}", values.len()) } else { joined }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct CallEntry {
    pub id: String,
    pub jid: String, // caller (or group) jid
    pub video: bool,
    pub group: bool,
    pub status: String, // offer | accept | reject | timeout | terminate
    pub timestamp: i64,
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
}

fn is_zero_i64(n: &i64) -> bool {
    *n == 0
}

fn now_secs() -> i64 {
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
    let d = local_date(timestamp);
    let today = chrono::Local::now().date_naive();
    if d.date_naive() == today {
        d.format("%H:%M").to_string()
    } else {
        d.format("%d/%m").to_string()
    }
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

#[derive(Default)]
pub struct Store {
    pub chats: HashMap<String, ChatMeta>,
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
        if self.aliases.get(lid).map(String::as_str) == Some(pn) {
            return false;
        }
        self.aliases.insert(lid.to_string(), pn.to_string());
        self.merge_jid(lid, pn);
        true
    }

    fn merge_jid(&mut self, from: &str, into: &str) {
        if let Some(src) = self.chats.remove(from) {
            let dst = self.chats.get(into);
            let merged = ChatMeta {
                jid: into.to_string(),
                name: dst
                    .filter(|d| !d.name.is_empty())
                    .map(|d| d.name.clone())
                    .unwrap_or(src.name),
                preview: if dst.map(|d| d.timestamp).unwrap_or(0) >= src.timestamp {
                    dst.map(|d| d.preview.clone()).unwrap_or(src.preview)
                } else {
                    src.preview
                },
                timestamp: src.timestamp.max(dst.map(|d| d.timestamp).unwrap_or(0)),
                unread: src.unread + dst.map(|d| d.unread).unwrap_or(0),
                mentioned: src.mentioned || dst.map(|d| d.mentioned).unwrap_or(false),
                pinned: src.pinned.max(dst.map(|d| d.pinned).unwrap_or(0)),
                archived: src.archived || dst.map(|d| d.archived).unwrap_or(false),
                community: dst.map(|d| d.community.clone()).unwrap_or(src.community),
                is_community: src.is_community || dst.map(|d| d.is_community).unwrap_or(false),
            };
            self.chats.insert(into.to_string(), merged);
        }
        if let Some(src_msgs) = self.messages.remove(from) {
            let dst_msgs = self.messages.entry(into.to_string()).or_default();
            let seen: HashSet<String> = dst_msgs.iter().map(|m| m.id.clone()).collect();
            for mut m in src_msgs {
                if !seen.contains(&m.id) {
                    m.jid = into.to_string();
                    dst_msgs.push(m);
                }
            }
            dst_msgs.sort_by_key(|m| m.timestamp);
            self.dirty_jids.insert(into.to_string());
            self.dirty_jids.remove(from);
        }
        self.deleted_jids.insert(from.to_string());
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
            b.timestamp.cmp(&a.timestamp)
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
        let existing = self.chats.get(&jid);
        let meta = ChatMeta {
            jid: jid.clone(),
            name: name
                .filter(|n| !n.is_empty())
                .map(str::to_string)
                .or_else(|| existing.map(|e| e.name.clone()))
                .unwrap_or_default(),
            preview: existing.map(|e| e.preview.clone()).unwrap_or_default(),
            timestamp: timestamp.max(existing.map(|e| e.timestamp).unwrap_or(0)),
            unread: unread.unwrap_or_else(|| existing.map(|e| e.unread).unwrap_or(0)),
            mentioned: existing.map(|e| e.mentioned).unwrap_or(false),
            pinned: pinned.unwrap_or_else(|| existing.map(|e| e.pinned).unwrap_or(0)),
            archived: archived.unwrap_or_else(|| existing.map(|e| e.archived).unwrap_or(false)),
            community: existing.map(|e| e.community.clone()).unwrap_or_default(),
            is_community: existing.map(|e| e.is_community).unwrap_or(false),
        };
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
        if list.len() > 500 {
            let excess = list.len() - 500;
            list.drain(..excess);
        }
        self.dirty_jids.insert(jid.clone());

        let existing = self.chats.get(&jid);
        if existing.map(|e| timestamp >= e.timestamp).unwrap_or(true) {
            let preview = compute_preview(&preview_msg, self);
            let existing = self.chats.get(&jid);
            let meta = ChatMeta {
                jid: jid.clone(),
                name: existing.map(|e| e.name.clone()).unwrap_or_default(),
                preview,
                timestamp: timestamp.max(existing.map(|e| e.timestamp).unwrap_or(0)),
                unread: existing.map(|e| e.unread).unwrap_or(0),
                mentioned: existing.map(|e| e.mentioned).unwrap_or(false),
                pinned: existing.map(|e| e.pinned).unwrap_or(0),
                archived: existing.map(|e| e.archived).unwrap_or(false),
                community: existing.map(|e| e.community.clone()).unwrap_or_default(),
                is_community: existing.map(|e| e.is_community).unwrap_or(false),
            };
            self.chats.insert(jid.clone(), meta);
        }
        let msg = self.messages.get(&jid).and_then(|l| l.iter().find(|m| m.timestamp == timestamp));
        if let Some(m) = msg
            && !m.from_me
            && !m.sender.is_empty()
            && (jid.ends_with("@s.whatsapp.net") || jid.ends_with("@lid"))
        {
            let sender = m.sender.clone();
            self.push_names.insert(jid, sender);
        }
        true
    }

    pub fn messages_for(&self, jid: &str) -> &[StoredMessage] {
        self.messages.get(jid).map(Vec::as_slice).unwrap_or(&[])
    }

    // Loads a chat's message list from the vault, merging with whatever
    // arrived over the socket while the chat was cold.
    pub fn hydrate(&mut self, vault: &Vault, jid: &str) {
        // A locked vault reads as "no data": marking the chat hydrated
        // here would let a later save truncate its history on disk.
        if vault.locked() {
            return;
        }
        let jid = self.canon_owned(jid);
        if !self.hydrated.insert(jid.clone()) {
            return;
        }
        let Some(text) = vault.get(&format!("store:msgs:{jid}")) else { return };
        let Ok(mut disk) = serde_json::from_str::<Vec<StoredMessage>>(&text) else { return };
        let live = self.messages.entry(jid).or_default();
        if !live.is_empty() {
            let known: HashSet<String> = disk.iter().map(|m| m.id.clone()).collect();
            for m in live.drain(..) {
                if !known.contains(&m.id) {
                    disk.push(m);
                }
            }
            disk.sort_by_key(|m| m.timestamp);
        }
        *live = disk;
    }

    // Drops a hydrated chat back to metadata-only. Dirty chats stay: their
    // unsaved messages would be lost.
    pub fn evict(&mut self, jid: &str) {
        let jid = self.canon_owned(jid);
        if self.dirty_jids.contains(&jid) {
            return;
        }
        self.messages.remove(&jid);
        self.hydrated.remove(&jid);
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

    // Marks a message as deleted-for-everyone (protocol REVOKE).
    pub fn mark_deleted(&mut self, jid: &str, id: &str) -> bool {
        let Some(m) = self.messages.get_mut(jid).and_then(|l| l.iter_mut().find(|x| x.id == id))
        else {
            return false;
        };
        if m.deleted {
            return false;
        }
        m.deleted = true;
        m.kind = MessageKind::Text;
        m.text = String::new();
        m.raw = None;
        self.dirty_jids.insert(jid.to_string());
        true
    }

    // Updates the delivery status of an own message (monotonic).
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

    pub fn save_to(&mut self, vault: &Vault) {
        vault.set("store:chats", &to_json(&self.chats));
        vault.set("store:contacts", &to_json(&self.contacts));
        vault.set("store:pushnames", &to_json(&self.push_names));
        vault.set("store:saved", &to_json(&self.saved_contacts));
        vault.set("store:aliases", &to_json(&self.aliases));
        for jid in self.deleted_jids.drain() {
            vault.del(&format!("store:msgs:{jid}"));
        }
        if self.starred_dirty {
            vault.set("store:starred", &to_json(&self.starred_ids));
            self.starred_dirty = false;
        }
        if self.calls_dirty {
            vault.set("store:calls", &to_json(&self.calls));
            self.calls_dirty = false;
        }
        if self.statuses_dirty {
            self.prune_statuses();
            let dehydrated: HashMap<&String, Vec<&StoredMessage>> = self
                .statuses
                .iter()
                .map(|(jid, list)| (jid, list.iter().collect()))
                .collect();
            vault.set("store:statuses", &to_json(&dehydrated));
            self.statuses_dirty = false;
        }
        let mut saved = 0;
        let dirty: Vec<String> = self.dirty_jids.drain().collect();
        for jid in dirty {
            // A cold chat that only collected new arrivals must merge with
            // the vault first, or the write would truncate its history.
            let was_cold = !self.hydrated.contains(&jid);
            self.hydrate(vault, &jid);
            if let Some(list) = self.messages.get(&jid) {
                // Only the last 300 survive the disk (500 in memory).
                let tail: Vec<&StoredMessage> =
                    list.iter().skip(list.len().saturating_sub(300)).collect();
                vault.set(&format!("store:msgs:{jid}"), &to_json(&tail));
                saved += 1;
            }
            if was_cold {
                self.messages.remove(&jid);
                self.hydrated.remove(&jid);
            }
        }
        if saved > 0 {
            println!("[store] saved {saved} chat message list(s)");
        }
    }

    pub fn load_from(&mut self, vault: &Vault) {
        fn read<T: serde::de::DeserializeOwned>(vault: &Vault, key: &str) -> Option<T> {
            serde_json::from_str(&vault.get(key)?).ok()
        }
        if let Some(v) = read(vault, "store:chats") {
            self.chats = v;
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
    if inner.audio_message.is_set() {
        return t("preview.audio");
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
pub fn preview_body(stored: &StoredMessage) -> String {
    match stored.kind {
        MessageKind::Image => t("preview.photo"),
        MessageKind::Audio => t("preview.audio"),
        MessageKind::Video => {
            if stored.gif { t("preview.gif") } else { t("preview.video") }
        }
        MessageKind::Doc => ta("preview.document", &[&stored.text]),
        MessageKind::Text => {
            let collapsed: String = stored.text.split_whitespace().collect::<Vec<_>>().join(" ");
            collapsed.chars().take(80).collect()
        }
    }
}

pub fn compute_preview(stored: &StoredMessage, store: &Store) -> String {
    let body = store.named_mentions(&preview_body(stored));
    let prefix = if stored.from_me {
        "✓ ".to_string()
    } else if is_group(&stored.jid) && !stored.sender.is_empty() {
        format!("{}: ", clean_text(&stored.sender).split(' ').next().unwrap_or(""))
    } else {
        String::new()
    };
    format!("{prefix}{body}")
}
