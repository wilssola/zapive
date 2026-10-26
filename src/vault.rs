// Envelope-encryption storage (the model password managers use), port of
// src/db.ts on master minus the legacy migrations (fresh data by design):
// - a random 256-bit Data Key (DK) encrypts every kv row and media file;
// - the DK is stored wrapped: AES-256-GCM under a KEK derived from the PIN
//   (scrypt, N=2^17) when a PIN is set, and always wrapped again by the OS
//   key store (DPAPI / keychain / Secret Service), binding the vault to
//   this account;
// - the PIN itself is never stored; changing it only re-wraps the DK.
use crate::paths::vault_path;
use crate::platform::{unwrap_secret, wrap_secret};
use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use hmac::{Hmac, Mac};
use rand::RngCore as _;
use rusqlite::Connection;
use sha2::Sha256;
use std::sync::{Arc, RwLock};
use std::time::Instant;
use zeroize::Zeroizing;

const FILE_MAGIC: &[u8; 5] = b"ZENC1";

type Key = Zeroizing<[u8; 32]>;

// The data key, shareable with the tokio side for media file encryption.
// Empty while the vault is locked.
#[derive(Clone, Default)]
pub struct KeyHandle(Arc<RwLock<Option<Key>>>);

impl KeyHandle {
    fn set(&self, key: Option<Key>) {
        *self.0.write().unwrap() = key;
    }

    fn with<T>(&self, f: impl FnOnce(Option<&[u8; 32]>) -> T) -> T {
        f(self.0.read().unwrap().as_deref())
    }

    pub fn unlocked(&self) -> bool {
        self.with(|k| k.is_some())
    }

    // A key for a specific purpose, derived from the DK so it needs no
    // storage of its own and dies with the vault lock. Used to key the
    // FTS5 search index (src/search.rs) without giving it the DK itself.
    pub fn derive(&self, label: &[u8]) -> Option<[u8; 32]> {
        self.with(|key| {
            let key = key?;
            let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(key).expect("HMAC accepts any key size");
            mac.update(label);
            let out = mac.finalize().into_bytes();
            Some(out.into())
        })
    }

    // ZENC1 | iv(12) | GCM tag(16) | ciphertext. Plaintext passthrough
    // only while locked (never expected in practice).
    pub fn encrypt_bytes(&self, plain: &[u8]) -> Vec<u8> {
        self.with(|key| {
            let Some(key) = key else { return plain.to_vec() };
            let mut iv = [0u8; 12];
            rand::rng().fill_bytes(&mut iv);
            let cipher = Aes256Gcm::new(key.into());
            // aes-gcm appends the tag; the file format keeps it before the
            // ciphertext, matching the Node build.
            let mut sealed = cipher
                .encrypt(Nonce::from_slice(&iv), Payload::from(plain))
                .expect("AES-GCM encryption cannot fail");
            let tag = sealed.split_off(sealed.len() - 16);
            let mut out = Vec::with_capacity(5 + 12 + 16 + sealed.len());
            out.extend_from_slice(FILE_MAGIC);
            out.extend_from_slice(&iv);
            out.extend_from_slice(&tag);
            out.extend_from_slice(&sealed);
            out
        })
    }

    pub fn decrypt_bytes(&self, data: &[u8]) -> Result<Vec<u8>, String> {
        if data.len() < 33 || &data[..5] != FILE_MAGIC {
            return Ok(data.to_vec()); // legacy plaintext file
        }
        self.with(|key| {
            let key = key.ok_or("file is encrypted and vault is locked")?;
            let iv = &data[5..17];
            let tag = &data[17..33];
            let mut ct = data[33..].to_vec();
            ct.extend_from_slice(tag);
            let cipher = Aes256Gcm::new(key.into());
            cipher
                .decrypt(Nonce::from_slice(iv), Payload::from(ct.as_slice()))
                .map_err(|_| "media decryption failed".to_string())
        })
    }
}

pub struct Vault {
    conn: Connection,
    key: KeyHandle,
    // The account whose rows `get`/`set` reach. One vault (one PIN, one
    // data key) serves every account; what separates them is this prefix
    // on the kv keys. The first account has the empty id and no prefix,
    // which is the layout a single-account install already has.
    account: std::cell::RefCell<String>,
    failed_attempts: u32,
    next_try_at: Option<Instant>,
}

// One linked WhatsApp account. `jid` is whoever paired it last -- kept
// after a logout so a new pairing can tell whether the conversations on
// disk are its own -- and `name` is what the switcher shows.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Account {
    pub id: String,
    #[serde(default)]
    pub jid: String,
    #[serde(default)]
    pub name: String,
}

pub const MAX_ACCOUNTS: usize = 5;

#[derive(Debug, PartialEq, Eq)]
pub enum PinError {
    WrongPin,
    BadFormat,
}

impl Vault {
    pub fn new() -> Result<Self, String> {
        Self::open_at(&vault_path().to_string_lossy())
    }

    pub fn open_at(path: &str) -> Result<Self, String> {
        let conn = Connection::open(path).map_err(|e| e.to_string())?;
        conn.pragma_update(None, "journal_mode", "WAL").map_err(|e| e.to_string())?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS kv (k TEXT PRIMARY KEY, v TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS meta (k TEXT PRIMARY KEY, v TEXT NOT NULL);",
        )
        .map_err(|e| e.to_string())?;
        Ok(Self {
            conn,
            key: KeyHandle::default(),
            account: Default::default(),
            failed_attempts: 0,
            next_try_at: None,
        })
    }

    pub fn key_handle(&self) -> KeyHandle {
        self.key.clone()
    }

    pub fn has_pin(&self) -> bool {
        self.meta_get("pin_salt").is_some()
    }

    pub fn locked(&self) -> bool {
        !self.key.unlocked()
    }

    // Opens the vault when no PIN is set (DK protected by the OS only).
    pub fn open(&mut self) -> Result<(), String> {
        if !self.locked() || self.has_pin() {
            return Ok(());
        }
        if let Some(stored) = self.meta_get("dk") {
            let inner = unwrap_secret(&stored)?;
            let raw = inner.strip_prefix("k:").ok_or("vault key corrupted")?;
            self.key.set(Some(decode_key(raw)?));
            return Ok(());
        }
        // First run: create the DK.
        let mut dk = Zeroizing::new([0u8; 32]);
        rand::rng().fill_bytes(&mut dk[..]);
        let wrapped = wrap_secret(&format!("k:{}", B64.encode(&dk[..])));
        self.key.set(Some(dk));
        self.meta_set("dk", &wrapped);
        Ok(())
    }

    pub fn unlock(&mut self, pin: &str) -> bool {
        if !self.has_pin() {
            return self.open().is_ok();
        }
        if let Some(at) = self.next_try_at
            && Instant::now() < at
        {
            return false;
        }
        let attempt = || -> Result<Key, ()> {
            let salt = hex_decode(&self.meta_get("pin_salt").ok_or(())?)?;
            let stored = self.meta_get("dk").ok_or(())?;
            let inner = unwrap_secret(&stored).map_err(|_| ())?;
            let kek = derive_kek(pin, &salt);
            let dk_b64 = decrypt_str(&inner, &kek).map_err(|_| ())?; // GCM auth fails on wrong PIN
            decode_key(&dk_b64).map_err(|_| ())
        };
        match attempt() {
            Ok(dk) => {
                self.key.set(Some(dk));
                self.failed_attempts = 0;
                self.next_try_at = None;
                true
            }
            Err(()) => self.register_failure(),
        }
    }

    fn register_failure(&mut self) -> bool {
        self.failed_attempts += 1;
        if self.failed_attempts >= 3 {
            let delay = (500u64 * 2u64.pow(self.failed_attempts - 3)).min(8000);
            self.next_try_at = Some(Instant::now() + std::time::Duration::from_millis(delay));
        }
        false
    }

    // Zeroes and drops the in-memory key.
    pub fn lock(&mut self) {
        self.key.set(None);
    }

    // Set, change, or remove (next = None) the PIN. Only re-wraps the DK —
    // the data itself is never re-encrypted.
    pub fn change_pin(&mut self, current: &str, next: Option<&str>) -> Result<(), PinError> {
        if self.has_pin() {
            if !self.unlock(current) {
                return Err(PinError::WrongPin);
            }
        } else {
            self.open().map_err(|_| PinError::WrongPin)?;
        }
        if let Some(next) = next {
            if next.len() < 4 || next.len() > 10 || !next.bytes().all(|b| b.is_ascii_digit()) {
                return Err(PinError::BadFormat);
            }
            self.persist_wrapped_dk(next);
        } else {
            let inner = self.key.with(|k| format!("k:{}", B64.encode(k.expect("unlocked"))));
            let wrapped = wrap_secret(&inner);
            self.meta_set("dk", &wrapped);
            let _ = self.conn.execute("DELETE FROM meta WHERE k = 'pin_salt'", []);
        }
        Ok(())
    }

    fn persist_wrapped_dk(&mut self, pin: &str) {
        let mut salt = [0u8; 16];
        rand::rng().fill_bytes(&mut salt);
        let kek = derive_kek(pin, &salt);
        let sealed = self.key.with(|k| encrypt_str(&B64.encode(k.expect("unlocked")), &kek));
        self.meta_set("pin_salt", &hex_encode(&salt));
        self.meta_set("dk", &wrap_secret(&sealed));
    }

    // ---- plaintext settings (needed before unlock, e.g. theme) ----

    pub fn setting_get(&self, k: &str) -> Option<String> {
        self.meta_get(&format!("setting:{k}"))
    }

    pub fn setting_set(&self, k: &str, v: &str) {
        self.meta_set(&format!("setting:{k}"), v);
    }

    fn meta_get(&self, k: &str) -> Option<String> {
        self.conn
            .query_row("SELECT v FROM meta WHERE k = ?1", [k], |row| row.get::<_, String>(0))
            .ok()
    }

    fn meta_set(&self, k: &str, v: &str) {
        let _ = self.conn.execute(
            "INSERT INTO meta(k,v) VALUES(?1,?2) ON CONFLICT(k) DO UPDATE SET v=excluded.v",
            [k, v],
        );
    }

    // ---- accounts ----

    pub fn account(&self) -> String {
        self.account.borrow().clone()
    }

    pub fn set_account(&self, id: &str) {
        *self.account.borrow_mut() = id.to_string();
    }

    fn scope(&self) -> String {
        let account = self.account.borrow();
        if account.is_empty() { String::new() } else { format!("acct:{account}:") }
    }

    // The registry is plaintext on purpose: main needs the active
    // account before the PIN is typed, to open the right session.
    pub fn accounts(&self) -> Vec<Account> {
        let mut list: Vec<Account> = self
            .setting_get("accounts")
            .and_then(|raw| serde_json::from_str(&raw).ok())
            .unwrap_or_default();
        if list.is_empty() {
            // A single-account install from before the registry existed.
            list.push(Account {
                id: String::new(),
                jid: self.setting_get("self_jid").unwrap_or_default(),
                name: String::new(),
            });
        }
        list
    }

    pub fn set_accounts(&self, list: &[Account]) {
        self.setting_set("accounts", &serde_json::to_string(list).unwrap_or_else(|_| "[]".into()));
    }

    // The account to open at launch; falls back to the first one when the
    // saved id no longer exists.
    pub fn active_account(&self) -> String {
        let list = self.accounts();
        let saved = self.setting_get("active_account").unwrap_or_default();
        if list.iter().any(|a| a.id == saved) {
            saved
        } else {
            list.first().map(|a| a.id.clone()).unwrap_or_default()
        }
    }

    pub fn set_active_account(&self, id: &str) {
        self.setting_set("active_account", id);
    }

    // Every row of one account, whichever account is active.
    pub fn wipe_account(&self, id: &str) {
        let scope = if id.is_empty() { "store:".to_string() } else { format!("acct:{id}:") };
        let _ = self.conn.execute("DELETE FROM kv WHERE k LIKE ?1", [format!("{scope}%")]);
    }

    // ---- kv (values encrypted with the DK) ----

    pub fn get(&self, k: &str) -> Option<String> {
        let k = &format!("{}{k}", self.scope());
        let stored = self
            .conn
            .query_row("SELECT v FROM kv WHERE k = ?1", [k], |row| row.get::<_, String>(0))
            .ok()?;
        self.key.with(|key| decrypt_str(&stored, key?).ok())
    }

    pub fn set(&self, k: &str, v: &str) {
        let k = &format!("{}{k}", self.scope());
        // Dropping the write beats panicking: callers race the PIN unlock
        // (the WhatsApp client connects while the vault is still locked).
        let Some(encoded) = self.key.with(|key| key.map(|key| encrypt_str(v, key))) else {
            eprintln!("[vault] write to {k} dropped: vault is locked");
            return;
        };
        let _ = self.conn.execute(
            "INSERT INTO kv(k,v) VALUES(?1,?2) ON CONFLICT(k) DO UPDATE SET v=excluded.v",
            [k, &encoded],
        );
    }

    pub fn del(&self, k: &str) {
        let k = format!("{}{k}", self.scope());
        let _ = self.conn.execute("DELETE FROM kv WHERE k = ?1", [k]);
    }

    pub fn del_prefix(&self, prefix: &str) {
        let _ = self
            .conn
            .execute("DELETE FROM kv WHERE k LIKE ?1", [format!("{}{prefix}%", self.scope())]);
    }

    // Keys come back the way the caller wrote them, without the scope.
    pub fn keys(&self, prefix: &str) -> Vec<String> {
        let scope = self.scope();
        let mut out = Vec::new();
        if let Ok(mut stmt) = self.conn.prepare("SELECT k FROM kv WHERE k LIKE ?1")
            && let Ok(rows) =
                stmt.query_map([format!("{scope}{prefix}%")], |row| row.get::<_, String>(0))
        {
            for k in rows.flatten() {
                out.push(k[scope.len()..].to_string());
            }
        }
        out
    }
}

fn derive_kek(pin: &str, salt: &[u8]) -> Key {
    let params = scrypt::Params::new(17, 8, 1, 32).expect("valid scrypt params");
    let mut kek = Zeroizing::new([0u8; 32]);
    scrypt::scrypt(pin.as_bytes(), salt, &params, &mut kek[..]).expect("scrypt cannot fail");
    kek
}

fn decode_key(b64: &str) -> Result<Key, String> {
    let raw = B64.decode(b64).map_err(|e| e.to_string())?;
    let arr: [u8; 32] = raw.try_into().map_err(|_| "vault key has the wrong size")?;
    Ok(Zeroizing::new(arr))
}

// String rows: e:<iv b64>:<tag b64>:<ct b64>, AES-256-GCM, 12-byte IV.
fn encrypt_str(plain: &str, key: &[u8; 32]) -> String {
    let mut iv = [0u8; 12];
    rand::rng().fill_bytes(&mut iv);
    let cipher = Aes256Gcm::new(key.into());
    let mut sealed = cipher
        .encrypt(Nonce::from_slice(&iv), Payload::from(plain.as_bytes()))
        .expect("AES-GCM encryption cannot fail");
    let tag = sealed.split_off(sealed.len() - 16);
    format!("e:{}:{}:{}", B64.encode(iv), B64.encode(tag), B64.encode(sealed))
}

fn decrypt_str(stored: &str, key: &[u8; 32]) -> Result<String, String> {
    let mut parts = stored.split(':');
    let (marker, iv, tag, ct) =
        (parts.next(), parts.next(), parts.next(), parts.next());
    let (Some("e"), Some(iv), Some(tag), Some(ct)) = (marker, iv, tag, ct) else {
        return Err("bad ciphertext".into());
    };
    let iv = B64.decode(iv).map_err(|e| e.to_string())?;
    let tag = B64.decode(tag).map_err(|e| e.to_string())?;
    let mut ct = B64.decode(ct).map_err(|e| e.to_string())?;
    ct.extend_from_slice(&tag);
    let cipher = Aes256Gcm::new(key.into());
    let plain = cipher
        .decrypt(Nonce::from_slice(&iv), Payload::from(ct.as_slice()))
        .map_err(|_| "decryption failed".to_string())?;
    String::from_utf8(plain).map_err(|e| e.to_string())
}

fn hex_encode(data: &[u8]) -> String {
    data.iter().map(|b| format!("{b:02x}")).collect()
}

fn hex_decode(s: &str) -> Result<Vec<u8>, ()> {
    if s.len() % 2 != 0 {
        return Err(());
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).map_err(|_| ()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TempVault {
        vault: Vault,
        path: std::path::PathBuf,
    }

    impl Drop for TempVault {
        fn drop(&mut self) {
            crate::paths::remove_db(&self.path);
        }
    }

    fn temp_vault(tag: &str) -> TempVault {
        let path = std::env::temp_dir().join(format!("zapive_test_accounts_{}_{tag}.db", std::process::id()));
        let mut vault = Vault::open_at(&path.to_string_lossy()).expect("open vault");
        vault.open().expect("unlock vault (no pin)");
        TempVault { vault, path }
    }

    #[test]
    fn accounts_keep_their_rows_apart() {
        let t = temp_vault("rows");
        let v = &t.vault;
        // The first account is what a single-account install already has.
        v.set("store:chats", "first");
        v.set("store:msgs:a@s.whatsapp.net", "hello");
        v.set_account("1a2b3c4d");
        assert_eq!(v.get("store:chats"), None);
        v.set("store:chats", "second");
        v.set("store:msgs:b@s.whatsapp.net", "hi");
        assert_eq!(v.keys("store:msgs:"), vec!["store:msgs:b@s.whatsapp.net".to_string()]);
        v.set_account("");
        assert_eq!(v.get("store:chats").as_deref(), Some("first"));
        assert_eq!(v.keys("store:msgs:"), vec!["store:msgs:a@s.whatsapp.net".to_string()]);
        // A logout's wipe takes one account's conversations only.
        v.del_prefix("store:");
        assert_eq!(v.get("store:chats"), None);
        v.set_account("1a2b3c4d");
        assert_eq!(v.get("store:chats").as_deref(), Some("second"));
        // Removing an account works from wherever the vault is pointed.
        v.set_account("");
        v.wipe_account("1a2b3c4d");
        v.set_account("1a2b3c4d");
        assert_eq!(v.get("store:chats"), None);
    }

    #[test]
    fn registry_starts_from_the_single_account_install() {
        let t = temp_vault("registry");
        let v = &t.vault;
        v.setting_set("self_jid", "5511999998888@s.whatsapp.net");
        let list = v.accounts();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].id, "");
        assert_eq!(list[0].jid, "5511999998888@s.whatsapp.net");
        assert_eq!(v.active_account(), "");
        let mut list = list;
        list.push(Account { id: "0badc0de".into(), ..Default::default() });
        v.set_accounts(&list);
        v.set_active_account("0badc0de");
        assert_eq!(v.active_account(), "0badc0de");
        // An id that is gone falls back to the first account.
        v.set_active_account("missing");
        assert_eq!(v.active_account(), "");
    }
}
