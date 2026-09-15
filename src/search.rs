// Full-text search over message bodies. Every chat keeps a flat list of
// (id, timestamp, folded text) entries in RAM, persisted per chat in the
// vault next to the message list, so a query never has to hydrate a
// conversation: one pass over a few megabytes of folded text answers it
// in milliseconds. Folding is lowercase plus Latin diacritics stripped, so
// "joao" finds "João" and "ACAO" finds "ação".
use crate::store::{MessageKind, StoredMessage};
use crate::vault::Vault;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

const KEY_PREFIX: &str = "store:search:";
// Entries per chat kept in the index; matches the on-disk message tail.
const PER_CHAT_MAX: usize = 400;

#[derive(Clone, Serialize, Deserialize)]
pub struct Entry {
    pub id: String,
    pub ts: i64,
    // Folded body used for matching.
    pub norm: String,
    // Original body, for the snippet.
    pub text: String,
    #[serde(default)]
    pub from_me: bool,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub sender: String,
}

pub struct Hit {
    pub jid: String,
    pub id: String,
    pub ts: i64,
    pub snippet: String,
    pub from_me: bool,
    pub sender: String,
}

#[derive(Default)]
pub struct SearchIndex {
    chats: HashMap<String, Vec<Entry>>,
    dirty: HashSet<String>,
    // Chats whose index was removed and must be deleted from the vault.
    dropped: HashSet<String>,
}

// Lowercase with Latin accents folded to their base letter.
pub fn fold(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        let base = match c {
            'À'..='Å' | 'à'..='å' | 'Ā' | 'ā' | 'Ă' | 'ă' | 'Ą' | 'ą' => 'a',
            'Ç' | 'ç' | 'Ć' | 'ć' | 'Č' | 'č' => 'c',
            'È'..='Ë' | 'è'..='ë' | 'Ē' | 'ē' | 'Ę' | 'ę' | 'Ě' | 'ě' => 'e',
            'Ì'..='Ï' | 'ì'..='ï' | 'Ī' | 'ī' | 'İ' | 'ı' => 'i',
            'Ñ' | 'ñ' | 'Ń' | 'ń' | 'Ň' | 'ň' => 'n',
            'Ò'..='Ö' | 'ò'..='ö' | 'Ø' | 'ø' | 'Ō' | 'ō' | 'Ő' | 'ő' => 'o',
            'Ù'..='Ü' | 'ù'..='ü' | 'Ū' | 'ū' | 'Ů' | 'ů' | 'Ű' | 'ű' => 'u',
            'Ý' | 'ý' | 'ÿ' | 'Ÿ' => 'y',
            'Š' | 'š' | 'Ś' | 'ś' => 's',
            'Ž' | 'ž' | 'Ź' | 'ź' | 'Ż' | 'ż' => 'z',
            'ß' => 's',
            other => {
                for lower in other.to_lowercase() {
                    out.push(lower);
                }
                continue;
            }
        };
        out.push(base);
    }
    out
}

fn indexable(m: &StoredMessage) -> Option<String> {
    if m.deleted || m.text.trim().is_empty() {
        return None;
    }
    match m.kind {
        // Captions and file names are searchable, like WhatsApp.
        MessageKind::Text | MessageKind::Image | MessageKind::Video | MessageKind::Doc => {
            Some(m.text.clone())
        }
        MessageKind::Audio | MessageKind::System => None,
    }
}

fn entry_of(m: &StoredMessage) -> Option<Entry> {
    let text = indexable(m)?;
    Some(Entry {
        id: m.id.clone(),
        ts: m.timestamp,
        norm: fold(&text),
        text,
        from_me: m.from_me,
        sender: m.sender.clone(),
    })
}

impl SearchIndex {
    // Adds one message; a duplicate id is ignored.
    pub fn add(&mut self, jid: &str, m: &StoredMessage) {
        let Some(entry) = entry_of(m) else { return };
        let list = self.chats.entry(jid.to_string()).or_default();
        if list.iter().any(|e| e.id == entry.id) {
            return;
        }
        let at = list.partition_point(|e| e.ts <= entry.ts);
        list.insert(at, entry);
        if list.len() > PER_CHAT_MAX {
            let excess = list.len() - PER_CHAT_MAX;
            list.drain(..excess);
        }
        self.dirty.insert(jid.to_string());
    }

    // Replaces a chat's entries with what its full message list holds.
    pub fn rebuild(&mut self, jid: &str, messages: &[StoredMessage]) {
        let mut list: Vec<Entry> = messages.iter().filter_map(entry_of).collect();
        if list.len() > PER_CHAT_MAX {
            let excess = list.len() - PER_CHAT_MAX;
            list.drain(..excess);
        }
        self.chats.insert(jid.to_string(), list);
        self.dirty.insert(jid.to_string());
    }

    pub fn remove(&mut self, jid: &str, id: &str) {
        if let Some(list) = self.chats.get_mut(jid)
            && let Some(at) = list.iter().position(|e| e.id == id)
        {
            list.remove(at);
            self.dirty.insert(jid.to_string());
        }
    }

    pub fn remove_chat(&mut self, jid: &str) {
        self.chats.remove(jid);
        self.dirty.remove(jid);
        self.dropped.insert(jid.to_string());
    }

    pub fn has_chat(&self, jid: &str) -> bool {
        self.chats.contains_key(jid)
    }

    // Every message matching the query, newest first. `chat` narrows the
    // search to one conversation. A query is a set of words that must all
    // appear; the whole phrase appearing ranks above scattered words.
    pub fn search(&self, query: &str, chat: Option<&str>, limit: usize) -> Vec<Hit> {
        let phrase = fold(query.trim());
        if phrase.is_empty() {
            return Vec::new();
        }
        let words: Vec<&str> = phrase.split_whitespace().collect();
        let mut hits: Vec<(u8, Hit)> = Vec::new();
        let scan = |jid: &str, list: &[Entry], hits: &mut Vec<(u8, Hit)>| {
            for e in list.iter().rev() {
                let rank = if e.norm.contains(&phrase) {
                    2
                } else if words.len() > 1 && words.iter().all(|w| e.norm.contains(w)) {
                    1
                } else {
                    continue;
                };
                hits.push((
                    rank,
                    Hit {
                        jid: jid.to_string(),
                        id: e.id.clone(),
                        ts: e.ts,
                        snippet: snippet(&e.text, &e.norm, words[0]),
                        from_me: e.from_me,
                        sender: e.sender.clone(),
                    },
                ));
            }
        };
        match chat {
            Some(jid) => {
                if let Some(list) = self.chats.get(jid) {
                    scan(jid, list, &mut hits);
                }
            }
            None => {
                for (jid, list) in &self.chats {
                    scan(jid, list, &mut hits);
                }
            }
        }
        hits.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.ts.cmp(&a.1.ts)));
        hits.into_iter().map(|(_, h)| h).take(limit).collect()
    }

    // ---- persistence ----

    pub fn load_from(&mut self, vault: &Vault) {
        for key in vault.keys(KEY_PREFIX) {
            let jid = key[KEY_PREFIX.len()..].to_string();
            if let Some(text) = vault.get(&key)
                && let Ok(list) = serde_json::from_str::<Vec<Entry>>(&text)
            {
                self.chats.insert(jid, list);
            }
        }
    }

    pub fn save_to(&mut self, vault: &Vault) {
        for jid in self.dropped.drain() {
            vault.del(&format!("{KEY_PREFIX}{jid}"));
        }
        let dirty: Vec<String> = self.dirty.drain().collect();
        for jid in dirty {
            if let Some(list) = self.chats.get(&jid) {
                let json = serde_json::to_string(list).unwrap_or_else(|_| "[]".into());
                vault.set(&format!("{KEY_PREFIX}{jid}"), &json);
            }
        }
    }

}

// A window of the original text around the first match, on char
// boundaries, with ellipses where it was cut.
fn snippet(text: &str, norm: &str, word: &str) -> String {
    const RADIUS: usize = 40;
    // Folding keeps one output char per input char for the cases it
    // handles, so a char index in `norm` maps onto `text` directly.
    let at = norm.find(word).map(|byte| norm[..byte].chars().count()).unwrap_or(0);
    let chars: Vec<char> = text.chars().collect();
    let start = at.saturating_sub(RADIUS);
    let end = (at + word.chars().count() + RADIUS).min(chars.len());
    let mut out = String::new();
    if start > 0 {
        out.push('…');
    }
    out.extend(chars[start..end].iter());
    if end < chars.len() {
        out.push('…');
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(id: &str, ts: i64, text: &str) -> StoredMessage {
        let mut m = crate::wa_map::system_message("a@g.us", id, ts, text.to_string());
        m.kind = MessageKind::Text;
        m
    }

    #[test]
    fn fold_strips_accents_and_case() {
        assert_eq!(fold("João Ação ÀÉÎÕÜ"), "joao acao aeiou");
    }

    #[test]
    fn search_matches_phrases_and_words_newest_first() {
        let mut idx = SearchIndex::default();
        idx.add("a@g.us", &msg("1", 10, "Reunião amanhã às 10"));
        idx.add("a@g.us", &msg("2", 20, "amanha tem reuniao"));
        idx.add("b@g.us", &msg("3", 30, "nada a ver"));
        let hits = idx.search("reuniao amanha", None, 10);
        // The whole phrase outranks scattered words, whatever the date.
        assert_eq!(hits.iter().map(|h| h.id.as_str()).collect::<Vec<_>>(), vec!["1", "2"]);
        let hits = idx.search("REUNIÃO AMANHÃ", Some("a@g.us"), 10);
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].id, "1");
        assert!(hits[0].snippet.contains("Reunião amanhã"));
        assert!(idx.search("xyz", None, 10).is_empty());
        idx.remove("a@g.us", "1");
        assert_eq!(idx.search("reuniao", None, 10).len(), 1);
    }
}
