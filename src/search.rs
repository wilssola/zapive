// Full-text search over message bodies, backed by SQLite FTS5
// (https://www.sqlite.org/fts5.html) in its own file, search.db, kept
// next to vault.db. The vault's own schema and encryption are untouched
// by this: nothing here changes how chats, contacts or settings are
// stored.
//
// FTS5 has to see the tokens it indexes, so a word cannot simply be an
// AES-GCM blob the way everything else in this app is stored. Instead,
// every folded word is turned into a *keyed, prefix-preserving* term
// before it ever reaches SQLite (see `term`). The key is derived from
// the vault's data key and is never itself stored, so turning a term
// back into a word needs the vault unlocked -- the same protection the
// rest of the app already gives everything else -- while `term` still
// lets FTS5 answer a prefix query ("reuni*") for as-you-type search,
// which hashing the whole word could not give. The message text kept
// for the result snippet is separately AES-256-GCM ciphertext in a
// plain column; FTS5 never sees it, only the hashed terms.
//
// This intentionally still leaks two things any deterministic
// searchable index leaks: a word's length (a term is twice as many
// characters) and how often a word or word pair appears. Nothing else
// about the plaintext is recoverable from search.db without the vault's
// key, and a wrong or rotated key (a freshly created vault.db, most
// likely) is detected at `open` and the index is dropped and rebuilt
// rather than silently returning garbage.
//
// Folding is lowercase plus Latin diacritics stripped, so "joao" finds
// "João" and "ACAO" finds "ação". Matching is by word, and the last word
// of a query also matches as a prefix, so "reuni" finds "reunião" -- but
// "uniao" no longer finds it mid-word, unlike the old substring scan
// this replaces. Every FTS5 candidate is re-verified against the
// decrypted text before being ranked, both to rule out a term collision
// (2^-10 per character position) and to reproduce the old ranking
// exactly: the whole phrase outranks scattered words, which outrank
// nothing at all.
use crate::store::{MessageKind, StoredMessage};
use crate::vault::{KeyHandle, Vault};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::Path;

// Bumped when the table shape changes; a mismatch (or a keycheck that no
// longer decrypts, meaning the data key changed under it) drops and
// recreates the tables. There is no migration framework here, matching
// vault.rs's own "fresh data by design" stance.
const SCHEMA_VERSION: &[u8] = b"1";
const KEYCHECK_MAGIC: &[u8] = b"zapive-search-v1";
// HMAC label the index key is derived under; distinct from any other
// purpose so a future derived key can never collide with this one.
const INDEX_KEY_LABEL: &[u8] = b"zapive/search-index/v1";
// A term encodes at most this many leading characters of a word; a
// longer word still matches on its first 24 chars, already far more
// specific than any real query needs.
const MAX_TERM_CHARS: usize = 24;
// Entries kept per chat; ties to the 300-message tail Store::save_to
// persists (store.rs) -- indexing more would produce hits `open_hit`
// cannot jump to once the chat goes cold and reloads from disk.
const PER_CHAT_MAX: usize = 300;

pub struct Hit {
    pub jid: String,
    pub id: String,
    pub ts: i64,
    pub snippet: String,
    pub from_me: bool,
    pub sender: String,
    pub sender_jid: String,
}

// One message as fed to the index.
struct Entry {
    id: String,
    ts: i64,
    from_me: bool,
    sender: String,
    sender_jid: String,
    // Folded body used to build search terms and to re-verify a hit.
    norm: String,
    // Original body, encrypted into `doc.blob` for the snippet.
    text: String,
}

// What actually sits (encrypted) in `doc.blob`.
#[derive(Serialize, Deserialize)]
struct DocBody {
    text: String,
    sender: String,
    sender_jid: String,
}

#[derive(Default)]
pub struct SearchIndex {
    db: Option<Connection>,
    key: KeyHandle,
    // Chats with at least one row in `doc`; mirrors `SELECT DISTINCT jid`
    // so `has_chat` stays O(1) without a round trip per check.
    indexed: HashSet<String>,
    // Set once the old per-chat vault blobs (`store:search:<jid>`) have
    // been swept, so `save_to` stops re-checking on every debounced save.
    purged: bool,
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

// Turns one character-prefix of a folded word into two base32 chars from
// 10 bits of HMAC-SHA256(key, prefix). Every prefix gets its own tag, so
// `term(key, "reu")` is a literal string-prefix of `term(key, "reuniao")`
// -- which lets FTS5's own `xyz*` prefix query answer as-you-type search
// over what is otherwise an opaque keyed hash.
fn term(key: &[u8; 32], word: &str) -> String {
    use hmac::{Hmac, Mac};
    use sha2::Sha256;
    const ALPHABET: &[u8; 32] = b"abcdefghijklmnopqrstuvwxyz234567";
    let mut out = String::with_capacity(MAX_TERM_CHARS * 2);
    let mut prefix = String::new();
    for ch in word.chars().take(MAX_TERM_CHARS) {
        prefix.push(ch);
        let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(key).expect("HMAC accepts any key size");
        mac.update(prefix.as_bytes());
        let digest = mac.finalize().into_bytes();
        let bits = (u16::from(digest[0]) << 8 | u16::from(digest[1])) & 0x3ff;
        out.push(ALPHABET[usize::from(bits >> 5) & 0x1f] as char);
        out.push(ALPHABET[usize::from(bits) & 0x1f] as char);
    }
    out
}

// Every word of a folded body, hashed into terms, space-separated for
// the `fts.terms` column. Words repeat if they repeat in the body: FTS5
// needs the real word count to answer phrase-adjacent queries.
fn terms_of(key: &[u8; 32], norm: &str) -> String {
    norm.split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(|w| term(key, w))
        .collect::<Vec<_>>()
        .join(" ")
}

fn indexable(m: &StoredMessage) -> Option<String> {
    if m.deleted || m.text.trim().is_empty() {
        return None;
    }
    match m.kind {
        // Captions and file names are searchable, like WhatsApp. A
        // message merely *revoked* (not destructively deleted) keeps its
        // real text here, same as everywhere else it is rendered.
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
        sender_jid: m.sender_jid.clone(),
    })
}

impl SearchIndex {
    // Opens (creating if needed) search.db and makes sure its schema and
    // keyed terms match the vault currently unlocked. Called once at
    // boot from Store::load_from, after the vault is guaranteed unlocked
    // (Bridge::boot only ever runs post-unlock). A no-op on any I/O
    // error or while the vault is locked: search then quietly returns
    // nothing, the same policy Vault::set uses for a dropped write.
    pub fn open(&mut self, vault: &Vault) {
        self.open_at(vault, &crate::paths::search_index_path());
    }

    // Drops and recreates search.db under the vault currently unlocked.
    // Used at logout: the on-disk data key can outlive the account (only
    // the vault's own `store:` keys are wiped there), so the keycheck
    // alone would not notice a different account reusing the same key.
    pub fn reset(&mut self, vault: &Vault) {
        self.reset_at(vault, &crate::paths::search_index_path());
    }

    fn open_at(&mut self, vault: &Vault, path: &Path) {
        if self.db.is_some() {
            return;
        }
        self.key = vault.key_handle();
        let Ok(conn) = Connection::open(path) else { return };
        if self.prepare_schema(&conn).is_err() {
            return;
        }
        self.indexed = load_indexed(&conn);
        self.db = Some(conn);
    }

    fn reset_at(&mut self, vault: &Vault, path: &Path) {
        self.db = None;
        self.indexed.clear();
        self.purged = false;
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
        }
        self.open_at(vault, path);
    }

    fn prepare_schema(&mut self, conn: &Connection) -> rusqlite::Result<()> {
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.execute_batch("CREATE TABLE IF NOT EXISTS meta(k TEXT PRIMARY KEY, v BLOB NOT NULL);")?;
        let stale = meta_get(conn, "schema").as_deref() != Some(SCHEMA_VERSION)
            || match meta_get(conn, "keycheck") {
                Some(stored) => self.key.decrypt_bytes(&stored).as_deref() != Ok(KEYCHECK_MAGIC),
                None => true,
            };
        if stale {
            conn.execute_batch("DROP TABLE IF EXISTS fts; DROP TABLE IF EXISTS doc;")?;
        }
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS doc(
                rowid   INTEGER PRIMARY KEY,
                jid     TEXT    NOT NULL,
                msg_id  TEXT    NOT NULL,
                ts      INTEGER NOT NULL,
                from_me INTEGER NOT NULL,
                blob    BLOB    NOT NULL
             );
             CREATE UNIQUE INDEX IF NOT EXISTS doc_msg ON doc(jid, msg_id);
             CREATE INDEX IF NOT EXISTS doc_chat_ts ON doc(jid, ts);
             -- Contentless: the hashed terms are only ever matched, never
             -- read back, so there is no reason to keep a second copy.
             CREATE VIRTUAL TABLE IF NOT EXISTS fts USING fts5(
                 terms, content='', contentless_delete=1, tokenize='ascii'
             );",
        )?;
        if stale {
            meta_set(conn, "schema", SCHEMA_VERSION)?;
            meta_set(conn, "keycheck", &self.key.encrypt_bytes(KEYCHECK_MAGIC))?;
            meta_set(conn, "purged", &[0])?;
        }
        self.purged = meta_get(conn, "purged").as_deref() == Some(&[1][..]);
        Ok(())
    }

    // Adds one message; a duplicate id is ignored.
    pub fn add(&mut self, jid: &str, m: &StoredMessage) {
        let Some(entry) = entry_of(m) else { return };
        let Some(index_key) = self.key.derive(INDEX_KEY_LABEL) else { return };
        let key = self.key.clone();
        let Some(db) = self.db.as_mut() else { return };
        let Ok(tx) = db.transaction() else { return };
        let ok = insert_entry(&tx, &key, &index_key, jid, &entry)
            .and_then(|_| trim_chat(&tx, jid, PER_CHAT_MAX));
        if ok.is_ok() && tx.commit().is_ok() {
            self.indexed.insert(jid.to_string());
        }
    }

    // Replaces a chat's entries with what its full message list holds.
    pub fn rebuild(&mut self, jid: &str, messages: &[StoredMessage]) {
        let Some(index_key) = self.key.derive(INDEX_KEY_LABEL) else { return };
        let key = self.key.clone();
        let mut entries: Vec<Entry> = messages.iter().filter_map(entry_of).collect();
        if entries.len() > PER_CHAT_MAX {
            let excess = entries.len() - PER_CHAT_MAX;
            entries.drain(..excess);
        }
        let Some(db) = self.db.as_mut() else { return };
        let Ok(tx) = db.transaction() else { return };
        let res = (|| -> rusqlite::Result<()> {
            tx.execute(
                "DELETE FROM fts WHERE rowid IN (SELECT rowid FROM doc WHERE jid=?1)",
                params![jid],
            )?;
            tx.execute("DELETE FROM doc WHERE jid=?1", params![jid])?;
            for e in &entries {
                insert_entry(&tx, &key, &index_key, jid, e)?;
            }
            Ok(())
        })();
        if res.is_ok() && tx.commit().is_ok() {
            self.indexed.insert(jid.to_string());
        }
    }

    pub fn remove(&mut self, jid: &str, id: &str) {
        let Some(db) = self.db.as_mut() else { return };
        let Ok(tx) = db.transaction() else { return };
        let rowid: Option<i64> = tx
            .query_row(
                "SELECT rowid FROM doc WHERE jid=?1 AND msg_id=?2",
                params![jid, id],
                |r| r.get(0),
            )
            .optional()
            .ok()
            .flatten();
        if let Some(rowid) = rowid {
            let ok = tx
                .execute("DELETE FROM fts WHERE rowid=?1", params![rowid])
                .and_then(|_| tx.execute("DELETE FROM doc WHERE rowid=?1", params![rowid]));
            if ok.is_ok() {
                let _ = tx.commit();
            }
        }
    }

    pub fn remove_chat(&mut self, jid: &str) {
        self.indexed.remove(jid);
        let Some(db) = self.db.as_mut() else { return };
        let Ok(tx) = db.transaction() else { return };
        let ok = tx
            .execute(
                "DELETE FROM fts WHERE rowid IN (SELECT rowid FROM doc WHERE jid=?1)",
                params![jid],
            )
            .and_then(|_| tx.execute("DELETE FROM doc WHERE jid=?1", params![jid]));
        if ok.is_ok() {
            let _ = tx.commit();
        }
    }

    pub fn has_chat(&self, jid: &str) -> bool {
        self.indexed.contains(jid)
    }

    // Every message matching the query, newest first. `chat` narrows the
    // search to one conversation. A query is a set of words that must
    // all appear (the last one also matching as a prefix, for
    // as-you-type search); the whole phrase appearing ranks above
    // scattered words.
    pub fn search(&self, query: &str, chat: Option<&str>, limit: usize) -> Vec<Hit> {
        let Some(db) = &self.db else { return Vec::new() };
        let Some(index_key) = self.key.derive(INDEX_KEY_LABEL) else { return Vec::new() };
        let phrase = fold(query.trim());
        if phrase.is_empty() {
            return Vec::new();
        }
        let words: Vec<&str> = phrase.split_whitespace().collect();
        let prefix_last = !query.ends_with(char::is_whitespace);
        let match_expr = words
            .iter()
            .enumerate()
            .map(|(i, w)| {
                let t = term(&index_key, w);
                if prefix_last && i + 1 == words.len() { format!("\"{t}\"*") } else { format!("\"{t}\"") }
            })
            .collect::<Vec<_>>()
            .join(" AND ");
        // Over-fetch: the FTS5 hit set is candidates only, verified and
        // ranked below against the decrypted text.
        let fetch = (limit.saturating_mul(4)).clamp(limit.max(1), 2000) as i64;
        let Ok(mut stmt) = db.prepare(
            "SELECT d.jid, d.msg_id, d.ts, d.from_me, d.blob \
             FROM fts JOIN doc d ON d.rowid = fts.rowid \
             WHERE fts MATCH ?1 AND (?2 IS NULL OR d.jid = ?2) \
             ORDER BY d.ts DESC LIMIT ?3",
        ) else {
            return Vec::new();
        };
        let Ok(rows) = stmt.query_map(params![match_expr, chat, fetch], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, bool>(3)?,
                row.get::<_, Vec<u8>>(4)?,
            ))
        }) else {
            return Vec::new();
        };
        let mut hits: Vec<(u8, Hit)> = Vec::new();
        for (jid, id, ts, from_me, blob) in rows.flatten() {
            let Ok(plain) = self.key.decrypt_bytes(&blob) else { continue };
            let Ok(body) = serde_json::from_slice::<DocBody>(&plain) else { continue };
            let norm = fold(&body.text);
            let rank = if norm.contains(&phrase) {
                2
            } else if words.len() > 1 && words.iter().all(|w| norm.contains(w)) {
                1
            } else {
                continue;
            };
            hits.push((
                rank,
                Hit {
                    jid,
                    id,
                    ts,
                    snippet: snippet(&body.text, &norm, words[0]),
                    from_me,
                    sender: body.sender,
                    sender_jid: body.sender_jid,
                },
            ));
        }
        hits.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.ts.cmp(&a.1.ts)));
        hits.into_iter().map(|(_, h)| h).take(limit).collect()
    }

    // Sweeps the old per-chat vault blobs once the index covers every
    // chat that has messages, so they don't linger as dead weight.
    pub fn save_to(&mut self, vault: &Vault) {
        if self.purged {
            return;
        }
        let Some(db) = &self.db else { return };
        let total_chats = vault.keys("store:msgs:").len();
        if total_chats == 0 || self.indexed.len() < total_chats {
            return; // backfill still catching up
        }
        vault.del_prefix("store:search:");
        self.purged = true;
        let _ = meta_set(db, "purged", &[1]);
    }
}

fn insert_entry(
    conn: &Connection,
    key: &KeyHandle,
    index_key: &[u8; 32],
    jid: &str,
    e: &Entry,
) -> rusqlite::Result<()> {
    let exists: Option<i64> = conn
        .query_row("SELECT rowid FROM doc WHERE jid=?1 AND msg_id=?2", params![jid, e.id], |r| r.get(0))
        .optional()?;
    if exists.is_some() {
        return Ok(());
    }
    let body = DocBody { text: e.text.clone(), sender: e.sender.clone(), sender_jid: e.sender_jid.clone() };
    let plain = serde_json::to_vec(&body).unwrap_or_default();
    let blob = key.encrypt_bytes(&plain);
    conn.execute(
        "INSERT INTO doc(jid, msg_id, ts, from_me, blob) VALUES(?1,?2,?3,?4,?5)",
        params![jid, e.id, e.ts, e.from_me, blob],
    )?;
    let rowid = conn.last_insert_rowid();
    let terms = terms_of(index_key, &e.norm);
    conn.execute("INSERT INTO fts(rowid, terms) VALUES(?1, ?2)", params![rowid, terms])?;
    Ok(())
}

fn trim_chat(conn: &Connection, jid: &str, max: usize) -> rusqlite::Result<()> {
    let count: i64 = conn.query_row("SELECT COUNT(*) FROM doc WHERE jid=?1", params![jid], |r| r.get(0))?;
    let excess = count - max as i64;
    if excess <= 0 {
        return Ok(());
    }
    let rowids: Vec<i64> = {
        let mut stmt = conn.prepare("SELECT rowid FROM doc WHERE jid=?1 ORDER BY ts ASC LIMIT ?2")?;
        let rows = stmt.query_map(params![jid, excess], |r| r.get::<_, i64>(0))?;
        rows.flatten().collect()
    };
    for rowid in rowids {
        conn.execute("DELETE FROM fts WHERE rowid=?1", params![rowid])?;
        conn.execute("DELETE FROM doc WHERE rowid=?1", params![rowid])?;
    }
    Ok(())
}

fn load_indexed(conn: &Connection) -> HashSet<String> {
    let mut out = HashSet::new();
    if let Ok(mut stmt) = conn.prepare("SELECT DISTINCT jid FROM doc")
        && let Ok(rows) = stmt.query_map([], |r| r.get::<_, String>(0))
    {
        for jid in rows.flatten() {
            out.insert(jid);
        }
    }
    out
}

fn meta_get(conn: &Connection, k: &str) -> Option<Vec<u8>> {
    conn.query_row("SELECT v FROM meta WHERE k=?1", [k], |r| r.get(0)).optional().ok().flatten()
}

fn meta_set(conn: &Connection, k: &str, v: &[u8]) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO meta(k,v) VALUES(?1,?2) ON CONFLICT(k) DO UPDATE SET v=excluded.v",
        params![k, v],
    )?;
    Ok(())
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
    use std::sync::atomic::{AtomicU64, Ordering};

    fn msg(id: &str, ts: i64, text: &str) -> StoredMessage {
        let mut m = crate::wa_map::system_message("a@g.us", id, ts, text.to_string());
        m.kind = MessageKind::Text;
        m
    }

    // A fresh, unlocked vault and search index over throwaway temp
    // files; the app has no test-dependency on `tempfile`, so this
    // follows the same std::env::temp_dir() convention used elsewhere
    // (bridge.rs, wa.rs, main.rs) with a counter for per-test uniqueness.
    struct TestIndex {
        index: SearchIndex,
        _vault: Vault,
        vault_path: std::path::PathBuf,
        search_path: std::path::PathBuf,
    }

    impl Drop for TestIndex {
        fn drop(&mut self) {
            for path in [&self.vault_path, &self.search_path] {
                for suffix in ["", "-wal", "-shm"] {
                    let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
                }
            }
        }
    }

    fn test_index() -> TestIndex {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir();
        let vault_path = dir.join(format!("zapive_test_vault_{}_{n}.db", std::process::id()));
        let search_path = dir.join(format!("zapive_test_search_{}_{n}.db", std::process::id()));
        let mut vault = Vault::open_at(&vault_path.to_string_lossy()).expect("open vault");
        vault.open().expect("unlock vault (no pin)");
        let mut index = SearchIndex::default();
        index.open_at(&vault, &search_path);
        TestIndex { index, _vault: vault, vault_path, search_path }
    }

    #[test]
    fn fold_strips_accents_and_case() {
        assert_eq!(fold("João Ação ÀÉÎÕÜ"), "joao acao aeiou");
    }

    #[test]
    fn term_prefix_is_a_string_prefix_of_the_whole_word() {
        let key = [7u8; 32];
        let whole = term(&key, "reuniao");
        for n in 1..="reuniao".chars().count() {
            let prefix: String = "reuniao".chars().take(n).collect();
            assert!(whole.starts_with(&term(&key, &prefix)), "prefix {n} should match");
        }
    }

    #[test]
    fn search_matches_phrases_and_words_newest_first() {
        let mut t = test_index();
        t.index.add("a@g.us", &msg("1", 10, "Reunião amanhã às 10"));
        t.index.add("a@g.us", &msg("2", 20, "amanha tem reuniao"));
        t.index.add("b@g.us", &msg("3", 30, "nada a ver"));
        let hits = t.index.search("reuniao amanha", None, 10);
        // The whole phrase outranks scattered words, whatever the date.
        assert_eq!(hits.iter().map(|h| h.id.as_str()).collect::<Vec<_>>(), vec!["1", "2"]);
        let hits = t.index.search("REUNIÃO AMANHÃ", Some("a@g.us"), 10);
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].id, "1");
        assert!(hits[0].snippet.contains("Reunião amanhã"));
        assert!(t.index.search("xyz", None, 10).is_empty());
        t.index.remove("a@g.us", "1");
        assert_eq!(t.index.search("reuniao", None, 10).len(), 1);
    }

    #[test]
    fn search_matches_as_you_type_prefix() {
        let mut t = test_index();
        t.index.add("a@g.us", &msg("1", 10, "Reunião amanhã"));
        assert_eq!(t.index.search("reuni", None, 10).len(), 1);
        assert!(t.index.search("reuni ", None, 10).is_empty(), "a trailing space closes the word");
    }

    #[test]
    fn a_stale_key_drops_and_rebuilds_the_index() {
        let mut t = test_index();
        t.index.add("a@g.us", &msg("1", 10, "hello there"));
        assert_eq!(t.index.search("hello", None, 10).len(), 1);

        // A different data key (as a freshly created vault.db would have)
        // fails the keycheck and forces a rebuild rather than returning
        // unreadable rows forever.
        let other_path =
            std::env::temp_dir().join(format!("zapive_test_vault_stale_{}.db", std::process::id()));
        let _ = std::fs::remove_file(&other_path);
        let mut other = Vault::open_at(&other_path.to_string_lossy()).expect("open second vault");
        other.open().expect("unlock second vault");
        let mut fresh = SearchIndex::default();
        fresh.open_at(&other, &t.search_path);
        assert!(fresh.search("hello", None, 10).is_empty());
        assert!(!fresh.has_chat("a@g.us"));
        let _ = std::fs::remove_file(&other_path);
    }
}
