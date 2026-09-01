import { extensionForMediaMessage } from "@whiskeysockets/baileys";
import type { WAMessage } from "@whiskeysockets/baileys";
import sharp from "sharp";
import { spawn, execFile } from "node:child_process";
import { promisify } from "node:util";
import { mkdir, writeFile, readFile, access, rm } from "node:fs/promises";
import { join, resolve, basename } from "node:path";
import type { SlintImageData } from "./qr.ts";
import type { WhatsAppService } from "./whatsapp.ts";
import type { Db } from "./db.ts";
import { t } from "./i18n.ts";

const execFileAsync = promisify(execFile);

const CACHE_DIR = "media_cache";
const DECODE_CACHE_MAX = 50;

export interface DecodedImage extends SlintImageData {
  displayW: number;
  displayH: number;
}

export class MediaService {
  private decoded = new Map<string, DecodedImage>();
  private downloads = new Map<string, Promise<string | null>>();
  private wa!: WhatsAppService;
  private db!: Db;

  setService(wa: WhatsAppService) {
    this.wa = wa;
  }

  setDb(db: Db) {
    this.db = db;
    // Transient decrypted copies from previous sessions.
    void rm(join(CACHE_DIR, ".tmp"), { recursive: true, force: true });
  }

  // Writes a plaintext copy into media_cache/.tmp for consumers that need a
  // real file (system audio player, toast icons). No-op when unencrypted.
  private async tempPlainCopy(filePath: string): Promise<string> {
    if (!this.db.encrypted) return filePath;
    const dir = join(CACHE_DIR, ".tmp");
    await mkdir(dir, { recursive: true });
    const out = join(dir, basename(filePath));
    await writeFile(out, this.db.decryptBytes(await readFile(filePath)));
    return out;
  }

  // Downloads (or reuses) the decrypted media file for a message.
  // Returns the cached file path, or null if the media is unavailable.
  ensureCached(msgId: string, raw: WAMessage): Promise<string | null> {
    let pending = this.downloads.get(msgId);
    if (!pending) {
      pending = this.doEnsureCached(msgId, raw).finally(() => {
        this.downloads.delete(msgId);
      });
      this.downloads.set(msgId, pending);
    }
    return pending;
  }

  private async doEnsureCached(msgId: string, raw: WAMessage): Promise<string | null> {
    await mkdir(CACHE_DIR, { recursive: true });
    let ext = "bin";
    try {
      ext = extensionForMediaMessage(raw.message!) || "bin";
    } catch {
      // fall through with .bin
    }
    const filePath = join(CACHE_DIR, `${sanitize(msgId)}.${ext}`);
    try {
      await access(filePath);
      return filePath;
    } catch {
      // not cached yet
    }
    try {
      const buffer = await this.wa.downloadMedia(raw);
      await writeFile(filePath, this.db.encryptBytes(buffer));
      return filePath;
    } catch {
      return null;
    }
  }

  async decodeImage(msgId: string, filePath: string): Promise<DecodedImage | null> {
    const cached = this.decoded.get(msgId);
    if (cached) return cached;
    try {
      const source = this.db.decryptBytes(await readFile(filePath));
      const { data, info } = await sharp(source)
        .rotate()
        .resize(1280, 1280, { fit: "inside", withoutEnlargement: true })
        .ensureAlpha()
        .raw()
        .toBuffer({ resolveWithObject: true });
      const img: DecodedImage = {
        width: info.width,
        height: info.height,
        data: new Uint8ClampedArray(data.buffer, data.byteOffset, data.length),
        displayW: info.width,
        displayH: info.height,
      };
      if (this.decoded.size >= DECODE_CACHE_MAX) {
        const oldest = this.decoded.keys().next().value;
        if (oldest !== undefined) this.decoded.delete(oldest);
      }
      this.decoded.set(msgId, img);
      return img;
    } catch {
      return null;
    }
  }

  private avatars = new Map<string, DecodedImage | null>();
  private avatarFiles = new Map<string, string>();

  // Plaintext PNG path for a fetched avatar (used as the notification
  // icon); decrypts into .tmp when the cache is encrypted.
  async avatarIcon(jid: string): Promise<string | null> {
    const file = this.avatarFiles.get(jid);
    if (!file) return null;
    try {
      return await this.tempPlainCopy(file);
    } catch {
      return null;
    }
  }

  // Fetches (once) and decodes the profile picture for a jid; null = none.
  // Transient failures are NOT cached so the caller can retry.
  async fetchAvatar(jid: string): Promise<DecodedImage | null> {
    const cached = this.avatars.get(jid);
    if (cached !== undefined) return cached;
    if (!this.wa || !this.wa.connected) return null;
    const url = await this.wa.profilePictureUrl(jid);
    if (url === undefined) return null; // transient — don't cache
    if (url === null) {
      this.avatars.set(jid, null); // confirmed: no picture
      return null;
    }
    try {
      const res = await fetch(url);
      if (!res.ok) return null; // transient — don't cache
      const buf = Buffer.from(await res.arrayBuffer());
      try {
        const dir = join(CACHE_DIR, "avatars");
        await mkdir(dir, { recursive: true });
        const file = join(dir, `${sanitize(jid)}.png`);
        // Circular crop (alpha mask) so the toast icon is round like
        // WhatsApp's notifications.
        const size = 128;
        const circleMask = Buffer.from(
          `<svg width="${size}" height="${size}"><circle cx="${size / 2}" cy="${size / 2}" r="${size / 2}" fill="#fff"/></svg>`,
        );
        const png = await sharp(buf)
          .resize(size, size, { fit: "cover" })
          .composite([{ input: circleMask, blend: "dest-in" }])
          .png()
          .toBuffer();
        await writeFile(file, this.db.encryptBytes(png));
        this.avatarFiles.set(jid, resolve(file));
      } catch {
        // notification icon is optional
      }
      const { data, info } = await sharp(buf)
        .resize(96, 96, { fit: "cover" })
        .ensureAlpha()
        .raw()
        .toBuffer({ resolveWithObject: true });
      const img: DecodedImage = {
        width: info.width,
        height: info.height,
        data: new Uint8ClampedArray(data.buffer, data.byteOffset, data.length),
        displayW: info.width,
        displayH: info.height,
      };
      this.avatars.set(jid, img);
      return img;
    } catch {
      return null; // transient — don't cache
    }
  }

  avatarResolved(jid: string): boolean {
    return this.avatars.has(jid);
  }

  avatarFor(jid: string): DecodedImage | null {
    return this.avatars.get(jid) ?? null;
  }

  // Decodes an arbitrary (plaintext) image file for the send-preview UI.
  async decodePreview(path: string): Promise<DecodedImage | null> {
    try {
      const { data, info } = await sharp(this.db.decryptBytes(await readFile(path)))
        .rotate()
        .resize(1280, 1280, { fit: "inside", withoutEnlargement: true })
        .ensureAlpha()
        .raw()
        .toBuffer({ resolveWithObject: true });
      return {
        width: info.width,
        height: info.height,
        data: new Uint8ClampedArray(data.buffer, data.byteOffset, data.length),
        displayW: info.width,
        displayH: info.height,
      };
    } catch {
      return null;
    }
  }

  // Saves a clipboard image (if any) to a temp PNG and returns its path.
  async clipboardImage(): Promise<string | null> {
    const dir = join(CACHE_DIR, ".tmp");
    await mkdir(dir, { recursive: true });
    const out = resolve(join(dir, `paste_${Date.now()}.png`));
    const script =
      "Add-Type -AssemblyName System.Windows.Forms; Add-Type -AssemblyName System.Drawing; " +
      "$img = [System.Windows.Forms.Clipboard]::GetImage(); " +
      `if ($img -ne $null) { $img.Save('${out.replace(/'/g, "''")}', [System.Drawing.Imaging.ImageFormat]::Png); Write-Output 'ok' }`;
    try {
      const { stdout } = await execFileAsync(
        "powershell",
        ["-NoProfile", "-STA", "-Command", script],
        { timeout: 15_000 },
      );
      return stdout.includes("ok") ? out : null;
    } catch {
      return null;
    }
  }

  async clipboardText(): Promise<string> {
    try {
      const { stdout } = await execFileAsync(
        "powershell",
        ["-NoProfile", "-Command", "Get-Clipboard -Raw"],
        { timeout: 10_000 },
      );
      return stdout.replace(/\r?\n$/, "");
    } catch {
      return "";
    }
  }

  play(filePath: string): void {
    if (!filePath) return;
    void this.tempPlainCopy(filePath)
      .then((playable) => {
        spawn("cmd", ["/c", "start", "", playable], {
          detached: true,
          stdio: "ignore",
        }).unref();
      })
      .catch((err) => console.error("play failed:", err));
  }

  async pickFile(kind: "image" | "audio" | "doc"): Promise<string | null> {
    const filter =
      kind === "image"
        ? `${t("picker.images")} (*.jpg;*.jpeg;*.png;*.webp;*.gif)|*.jpg;*.jpeg;*.png;*.webp;*.gif`
        : kind === "audio"
          ? `${t("picker.audio")} (*.ogg;*.opus;*.mp3;*.m4a;*.wav)|*.ogg;*.opus;*.mp3;*.m4a;*.wav`
          : `${t("picker.all")} (*.*)|*.*`;
    const script = [
      "Add-Type -AssemblyName System.Windows.Forms;",
      "$d = New-Object System.Windows.Forms.OpenFileDialog;",
      `$d.Filter = '${filter}';`,
      "if ($d.ShowDialog() -eq 'OK') { $d.FileName }",
    ].join(" ");
    try {
      const { stdout } = await execFileAsync(
        "powershell",
        ["-NoProfile", "-STA", "-Command", script],
        { timeout: 120_000 },
      );
      const path = stdout.trim();
      return path || null;
    } catch {
      return null;
    }
  }
}

function sanitize(id: string): string {
  return id.replace(/[^A-Za-z0-9_-]/g, "_");
}
