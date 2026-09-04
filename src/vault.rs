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
use rand::RngCore as _;
use rusqlite::Connection;
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
    failed_attempts: u32,
    next_try_at: Option<Instant>,
}

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
        Ok(Self { conn, key: KeyHandle::default(), failed_attempts: 0, next_try_at: None })
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

    // ---- kv (values encrypted with the DK) ----

    pub fn get(&self, k: &str) -> Option<String> {
        let stored = self
            .conn
            .query_row("SELECT v FROM kv WHERE k = ?1", [k], |row| row.get::<_, String>(0))
            .ok()?;
        self.key.with(|key| decrypt_str(&stored, key?).ok())
    }

    pub fn set(&self, k: &str, v: &str) {
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
        let _ = self.conn.execute("DELETE FROM kv WHERE k = ?1", [k]);
    }

    pub fn del_prefix(&self, prefix: &str) {
        let _ = self
            .conn
            .execute("DELETE FROM kv WHERE k LIKE ?1", [format!("{prefix}%")]);
    }

    pub fn keys(&self, prefix: &str) -> Vec<String> {
        let mut out = Vec::new();
        if let Ok(mut stmt) = self.conn.prepare("SELECT k FROM kv WHERE k LIKE ?1")
            && let Ok(rows) = stmt.query_map([format!("{prefix}%")], |row| row.get::<_, String>(0))
        {
            for k in rows.flatten() {
                out.push(k);
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
