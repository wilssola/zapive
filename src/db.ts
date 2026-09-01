import { DatabaseSync } from "node:sqlite";
import { scryptSync, randomBytes, createCipheriv, createDecipheriv } from "node:crypto";
import { existsSync, readdirSync, readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { spawnSync } from "node:child_process";
import { initAuthCreds, BufferJSON, proto } from "@whiskeysockets/baileys";
import type { AuthenticationState } from "@whiskeysockets/baileys";

// Envelope-encryption storage (the model password managers use):
// - a random 256-bit Data Key (DK) encrypts every kv row and media file;
// - the DK is stored wrapped: AES-256-GCM under a KEK derived from the PIN
//   (scrypt, N=2^17) when a PIN is set, and always wrapped again with
//   Windows DPAPI, binding the vault to this Windows user account;
// - the PIN itself is never stored; changing it only re-wraps the DK.

const SCRYPT_OPTS = { N: 131072, r: 8, p: 1, maxmem: 256 * 1024 * 1024 };

export class Db {
  private db: DatabaseSync;
  private key: Buffer | null = null;
  private failedAttempts = 0;
  private nextTryAt = 0;

  constructor(path = "zapive.db") {
    this.db = new DatabaseSync(path);
    this.db.exec("PRAGMA journal_mode = WAL");
    this.db.exec("CREATE TABLE IF NOT EXISTS kv (k TEXT PRIMARY KEY, v TEXT NOT NULL)");
    this.db.exec("CREATE TABLE IF NOT EXISTS meta (k TEXT PRIMARY KEY, v TEXT NOT NULL)");
  }

  hasPin(): boolean {
    return this.metaGet("pin_salt") !== null;
  }

  get locked(): boolean {
    return this.key === null;
  }

  get encrypted(): boolean {
    return this.key !== null;
  }

  // Opens the vault when no PIN is set (DK protected by DPAPI only).
  // Also migrates legacy plaintext installs to encrypted storage.
  open(): void {
    if (this.key || this.hasPin()) return;
    const stored = this.metaGet("dk");
    if (stored) {
      const inner = unwrapDpapi(stored);
      if (!inner.startsWith("k:")) throw new Error("vault key corrupted");
      this.key = Buffer.from(inner.slice(2), "base64");
      return;
    }
    // First run (or legacy plaintext data): create the DK and encrypt
    // whatever plaintext already exists.
    const dk = randomBytes(32);
    this.key = dk;
    this.reencryptAll();
    this.metaSet("dk", wrapDpapi("k:" + dk.toString("base64")));
  }

  unlock(pin: string): boolean {
    if (!this.hasPin()) {
      this.open();
      return true;
    }
    if (Date.now() < this.nextTryAt) return false;

    const salt = Buffer.from(this.metaGet("pin_salt")!, "hex");
    const stored = this.metaGet("dk");

    // Legacy v1 scheme: data was encrypted directly with scrypt(pin).
    if (!stored) {
      const check = this.metaGet("pin_check");
      const oldKey = scryptSync(pin, salt, 32); // v1 used default params
      try {
        if (check && decrypt(check, oldKey) === "zapive-ok") {
          // The old key becomes the DK (data stays readable); store it in
          // the new wrapped format.
          this.key = oldKey;
          this.persistWrappedDk(pin);
          this.db.prepare("DELETE FROM meta WHERE k = 'pin_check'").run();
          return true;
        }
      } catch {
        // wrong pin
      }
      return this.registerFailure();
    }

    try {
      const inner = unwrapDpapi(stored);
      const kek = scryptSync(pin, salt, 32, SCRYPT_OPTS);
      const dkB64 = decrypt(inner, kek); // GCM auth fails on wrong PIN
      this.key = Buffer.from(dkB64, "base64");
      this.failedAttempts = 0;
      return true;
    } catch {
      return this.registerFailure();
    }
  }

  private registerFailure(): boolean {
    this.failedAttempts++;
    if (this.failedAttempts >= 3) {
      const delay = Math.min(500 * 2 ** (this.failedAttempts - 3), 8000);
      this.nextTryAt = Date.now() + delay;
    }
    return false;
  }

  // Zeroes and drops the in-memory key.
  lock(): void {
    this.key?.fill(0);
    this.key = null;
  }

  // Set, change, or remove (next = null) the PIN. Only re-wraps the DK —
  // the data itself is never re-encrypted. Returns an error code or null.
  changePin(current: string, next: string | null): "wrong-pin" | "bad-format" | null {
    if (this.hasPin()) {
      const priorKey = this.key;
      if (!this.unlock(current)) return "wrong-pin";
      // keep the already-loaded key object if unlock replaced it
      if (priorKey && this.key !== priorKey) priorKey.fill(0);
    } else {
      this.open();
    }
    if (next !== null && !/^\d{4,10}$/.test(next)) return "bad-format";
    if (next !== null) {
      this.persistWrappedDk(next);
    } else {
      this.metaSet("dk", wrapDpapi("k:" + this.key!.toString("base64")));
      this.db.prepare("DELETE FROM meta WHERE k IN ('pin_salt','pin_check')").run();
    }
    return null;
  }

  private persistWrappedDk(pin: string) {
    const salt = randomBytes(16);
    const kek = scryptSync(pin, salt, 32, SCRYPT_OPTS);
    this.metaSet("pin_salt", salt.toString("hex"));
    this.metaSet("dk", wrapDpapi(encrypt(this.key!.toString("base64"), kek)));
    kek.fill(0);
  }

  // One-time migration of legacy plaintext rows/files to the DK.
  private reencryptAll() {
    const rows = this.db.prepare("SELECT k, v FROM kv WHERE v LIKE 'p:%'").all() as {
      k: string;
      v: string;
    }[];
    const write = this.db.prepare("UPDATE kv SET v = ? WHERE k = ?");
    for (const row of rows) {
      write.run(this.encode(row.v.slice(2)), row.k);
    }
    this.reencryptDir("media_cache");
    if (rows.length > 0) console.log(`[vault] migrated ${rows.length} rows to encrypted storage`);
  }

  private reencryptDir(dir: string) {
    if (!existsSync(dir)) return;
    for (const entry of readdirSync(dir, { withFileTypes: true })) {
      const p = join(dir, entry.name);
      if (entry.isDirectory()) {
        if (entry.name === ".tmp") continue;
        this.reencryptDir(p);
        continue;
      }
      try {
        const data = readFileSync(p);
        if (data.length >= 5 && data.subarray(0, 5).equals(FILE_MAGIC)) continue;
        writeFileSync(p, encryptBuf(data, this.key!));
      } catch (err) {
        console.error(`encrypt failed for ${p}:`, err);
      }
    }
  }

  private decode(stored: string): string {
    if (stored.startsWith("p:")) return stored.slice(2);
    if (!this.key) throw new Error("database is locked");
    return decrypt(stored, this.key);
  }

  private encode(plain: string): string {
    return this.key ? encrypt(plain, this.key) : "p:" + plain;
  }

  private metaGet(k: string): string | null {
    const row = this.db.prepare("SELECT v FROM meta WHERE k = ?").get(k) as
      | { v: string }
      | undefined;
    return row?.v ?? null;
  }

  private metaSet(k: string, v: string) {
    this.db
      .prepare("INSERT INTO meta(k,v) VALUES(?,?) ON CONFLICT(k) DO UPDATE SET v=excluded.v")
      .run(k, v);
  }

  // ---- plaintext settings (needed before unlock, e.g. theme) ----

  settingGet(k: string): string | null {
    return this.metaGet(`setting:${k}`);
  }

  settingSet(k: string, v: string) {
    this.metaSet(`setting:${k}`, v);
  }

  // ---- encrypted file helpers (media cache) ----

  encryptBytes(plain: Buffer): Buffer {
    return this.key ? encryptBuf(plain, this.key) : plain;
  }

  decryptBytes(data: Buffer): Buffer {
    return decryptBuf(data, this.key);
  }

  // ---- kv ----

  get(k: string): string | null {
    const row = this.db.prepare("SELECT v FROM kv WHERE k = ?").get(k) as
      | { v: string }
      | undefined;
    return row ? this.decode(row.v) : null;
  }

  set(k: string, v: string) {
    this.db
      .prepare("INSERT INTO kv(k,v) VALUES(?,?) ON CONFLICT(k) DO UPDATE SET v=excluded.v")
      .run(k, this.encode(v));
  }

  del(k: string) {
    this.db.prepare("DELETE FROM kv WHERE k = ?").run(k);
  }

  delPrefix(prefix: string) {
    this.db.prepare("DELETE FROM kv WHERE k LIKE ?").run(prefix + "%");
  }

  keys(prefix: string): string[] {
    const rows = this.db.prepare("SELECT k FROM kv WHERE k LIKE ?").all(prefix + "%") as {
      k: string;
    }[];
    return rows.map((r) => r.k);
  }
}

// ---- Windows DPAPI (CryptProtectData): binds the wrapped DK to the
// current Windows user account, the same OS facility Chrome and Signal
// Desktop use for their local keys. Falls back to raw storage (prefixed)
// if DPAPI is unavailable. ----

function dpapi(op: "Protect" | "Unprotect", b64: string): string | null {
  const script =
    "Add-Type -AssemblyName System.Security; " +
    `[Convert]::ToBase64String([System.Security.Cryptography.ProtectedData]::${op}(` +
    `[Convert]::FromBase64String('${b64}'), $null, ` +
    "[System.Security.Cryptography.DataProtectionScope]::CurrentUser))";
  const r = spawnSync("powershell", ["-NoProfile", "-Command", script], {
    encoding: "utf8",
    timeout: 20_000,
  });
  const out = r.stdout?.trim();
  return r.status === 0 && out ? out : null;
}

function wrapDpapi(inner: string): string {
  const protectedB64 = dpapi("Protect", Buffer.from(inner, "utf8").toString("base64"));
  if (protectedB64) return "dpapi:" + protectedB64;
  console.warn("[vault] DPAPI unavailable — storing key without OS binding");
  return "raw:" + Buffer.from(inner, "utf8").toString("base64");
}

function unwrapDpapi(stored: string): string {
  if (stored.startsWith("dpapi:")) {
    const plain = dpapi("Unprotect", stored.slice(6));
    if (!plain) throw new Error("DPAPI unprotect failed (different Windows user?)");
    return Buffer.from(plain, "base64").toString("utf8");
  }
  if (stored.startsWith("raw:")) {
    return Buffer.from(stored.slice(4), "base64").toString("utf8");
  }
  throw new Error("unknown key wrapping format");
}

const FILE_MAGIC = Buffer.from("ZENC1");

function encryptBuf(plain: Buffer, key: Buffer): Buffer {
  const iv = randomBytes(12);
  const cipher = createCipheriv("aes-256-gcm", key, iv);
  const ct = Buffer.concat([cipher.update(plain), cipher.final()]);
  return Buffer.concat([FILE_MAGIC, iv, cipher.getAuthTag(), ct]);
}

function decryptBuf(data: Buffer, key: Buffer | null): Buffer {
  if (data.length < 33 || !data.subarray(0, 5).equals(FILE_MAGIC)) {
    return data; // legacy plaintext file
  }
  if (!key) throw new Error("file is encrypted and vault is locked");
  const decipher = createDecipheriv("aes-256-gcm", key, data.subarray(5, 17));
  decipher.setAuthTag(data.subarray(17, 33));
  return Buffer.concat([decipher.update(data.subarray(33)), decipher.final()]);
}

function encrypt(plain: string, key: Buffer): string {
  const iv = randomBytes(12);
  const cipher = createCipheriv("aes-256-gcm", key, iv);
  const ct = Buffer.concat([cipher.update(plain, "utf8"), cipher.final()]);
  const tag = cipher.getAuthTag();
  return `e:${iv.toString("base64")}:${tag.toString("base64")}:${ct.toString("base64")}`;
}

function decrypt(stored: string, key: Buffer): string {
  const [marker, ivB64, tagB64, ctB64] = stored.split(":");
  if (marker !== "e" || !ivB64 || !tagB64 || !ctB64) throw new Error("bad ciphertext");
  const decipher = createDecipheriv("aes-256-gcm", key, Buffer.from(ivB64, "base64"));
  decipher.setAuthTag(Buffer.from(tagB64, "base64"));
  return Buffer.concat([
    decipher.update(Buffer.from(ctB64, "base64")),
    decipher.final(),
  ]).toString("utf8");
}

// ---- Baileys auth state stored in the Db (SQLite equivalent of
// useMultiFileAuthState) ----

export function useDbAuthState(db: Db): {
  state: AuthenticationState;
  saveCreds: () => Promise<void>;
} {
  const rawCreds = db.get("auth:creds");
  const creds = rawCreds
    ? JSON.parse(rawCreds, BufferJSON.reviver)
    : initAuthCreds();

  const state: AuthenticationState = {
    creds,
    keys: {
      get: async (type, ids) => {
        const out: Record<string, unknown> = {};
        for (const id of ids) {
          const v = db.get(`auth:${type}-${id}`);
          if (v !== null) {
            let value = JSON.parse(v, BufferJSON.reviver);
            if (type === "app-state-sync-key" && value) {
              value = proto.Message.AppStateSyncKeyData.fromObject(value);
            }
            out[id] = value;
          }
        }
        return out as never;
      },
      set: async (data) => {
        for (const [type, byId] of Object.entries(data)) {
          for (const [id, value] of Object.entries(byId as Record<string, unknown>)) {
            const k = `auth:${type}-${id}`;
            if (value) db.set(k, JSON.stringify(value, BufferJSON.replacer));
            else db.del(k);
          }
        }
      },
    },
  };

  return {
    state,
    saveCreds: async () => {
      db.set("auth:creds", JSON.stringify(creds, BufferJSON.replacer));
    },
  };
}
