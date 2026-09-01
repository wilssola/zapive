import { extensionForMediaMessage } from "@whiskeysockets/baileys";
import type { WAMessage } from "@whiskeysockets/baileys";
import sharp from "sharp";
import { spawn, execFile } from "node:child_process";
import { promisify } from "node:util";
import { mkdir, writeFile, readFile, access, rm } from "node:fs/promises";
import { join, resolve, basename } from "node:path";
import { existsSync, readdirSync } from "node:fs";
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

  // Decodes an in-memory image buffer (e.g. an embedded video thumbnail).
  async decodeRaw(buf: Buffer): Promise<DecodedImage | null> {
    try {
      const { data, info } = await sharp(buf)
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

  // ---- Audio playback (ffplay) and waveforms ----

  private playProc: ReturnType<typeof spawn> | null = null;
  private playToken = 0;
  private plainAudio = new Map<string, string>();
  private waveCache = new Map<string, DecodedImage | null>();

  // Decrypted copy kept around so play/seek doesn't decrypt every time.
  private async plainAudioPath(msgId: string, filePath: string): Promise<string> {
    const cached = this.plainAudio.get(msgId);
    if (cached) return cached;
    const plain = await this.tempPlainCopy(filePath);
    this.plainAudio.set(msgId, plain);
    return plain;
  }

  // Renders the audio envelope as white bars on transparency; the UI
  // colorizes it (dim for the track, accent for the played part).
  async waveform(msgId: string, filePath: string): Promise<DecodedImage | null> {
    const cached = this.waveCache.get(msgId);
    if (cached !== undefined) return cached;
    let img: DecodedImage | null = null;
    try {
      const src = await this.plainAudioPath(msgId, filePath);
      const pcm = await new Promise<Buffer>((done, fail) => {
        const proc = spawn(
          this.findFfmpeg()!,
          ["-hide_banner", "-loglevel", "error", "-i", src,
           "-ac", "1", "-ar", "8000", "-f", "s16le", "-"],
          { stdio: ["ignore", "pipe", "ignore"] },
        );
        const chunks: Buffer[] = [];
        proc.stdout?.on("data", (c: Buffer) => chunks.push(c));
        proc.once("error", fail);
        proc.once("close", () => done(Buffer.concat(chunks)));
      });
      const samples = new Int16Array(
        pcm.buffer,
        pcm.byteOffset,
        Math.floor(pcm.length / 2),
      );
      const BARS = 44;
      const peaks: number[] = [];
      const per = Math.max(1, Math.floor(samples.length / BARS));
      for (let b = 0; b < BARS; b++) {
        let peak = 0;
        for (let i = b * per; i < Math.min((b + 1) * per, samples.length); i++) {
          const v = Math.abs(samples[i]!);
          if (v > peak) peak = v;
        }
        peaks.push(peak);
      }
      const max = Math.max(1, ...peaks);
      img = renderBars(peaks.map((v) => v / max));
      this.waveCache.set(msgId, img);
    } catch {
      this.waveCache.set(msgId, null);
    }
    return img;
  }

  get playing(): boolean {
    return this.playProc !== null;
  }

  // Starts (or restarts at an offset) playback of a cached audio file.
  // The token guards against fast successive clicks: decrypting the file
  // is async, so a stale start must not spawn after a newer one.
  async playAudio(msgId: string, filePath: string, offsetSec: number): Promise<boolean> {
    this.stopAudio();
    const token = ++this.playToken;
    try {
      const src = await this.plainAudioPath(msgId, filePath);
      if (token !== this.playToken) return false;
      this.stopAudio();
      const ffplay = this.findFfmpeg()!.replace(/ffmpeg\.exe$/i, "ffplay.exe");
      this.playProc = spawn(
        ffplay,
        ["-nodisp", "-autoexit", "-loglevel", "quiet", "-ss", String(offsetSec), src],
        { stdio: "ignore" },
      );
      this.playProc.once("exit", () => {
        this.playProc = null;
      });
      return true;
    } catch {
      this.playProc = null;
      return false;
    }
  }

  stopAudio(): void {
    this.playToken++;
    const proc = this.playProc;
    this.playProc = null;
    if (!proc) return;
    try {
      proc.kill();
      // ffplay can ignore the soft kill; make sure it really stops.
      if (proc.pid) {
        spawn("taskkill", ["/PID", String(proc.pid), "/T", "/F"], {
          stdio: "ignore",
        });
      }
    } catch {
      // already gone
    }
  }

  // ---- Voice recording (ffmpeg + DirectShow) ----

  private recProc: ReturnType<typeof spawn> | null = null;
  private recPath: string | null = null;
  private ffmpegPath: string | null | undefined;
  private micName: string | null | undefined;

  private findFfmpeg(): string | null {
    if (this.ffmpegPath !== undefined) return this.ffmpegPath;
    let found: string | null = null;
    const winget = join(
      process.env.LOCALAPPDATA ?? "",
      "Microsoft\\WinGet\\Packages",
    );
    if (existsSync(winget)) {
      for (const dir of readdirSync(winget)) {
        if (!dir.toLowerCase().includes("ffmpeg")) continue;
        const stack = [join(winget, dir)];
        while (stack.length > 0 && !found) {
          const cur = stack.pop()!;
          for (const entry of readdirSync(cur, { withFileTypes: true })) {
            const full = join(cur, entry.name);
            if (entry.isDirectory()) stack.push(full);
            else if (entry.name.toLowerCase() === "ffmpeg.exe") {
              found = full;
              break;
            }
          }
        }
        if (found) break;
      }
    }
    this.ffmpegPath = found ?? "ffmpeg"; // fall back to PATH
    return this.ffmpegPath;
  }

  // First DirectShow audio input reported by ffmpeg.
  private async findMicrophone(): Promise<string | null> {
    if (this.micName !== undefined) return this.micName;
    const ff = this.findFfmpeg();
    try {
      await execFileAsync(ff!, ["-hide_banner", "-list_devices", "true", "-f", "dshow", "-i", "dummy"], {
        timeout: 20_000,
      });
      this.micName = null;
    } catch (err) {
      const out = String((err as { stderr?: string }).stderr ?? "");
      const match = out.match(/"([^"]+)"\s*\(audio\)/);
      this.micName = match?.[1] ?? null;
    }
    return this.micName;
  }

  get recording(): boolean {
    return this.recProc !== null;
  }

  async startRecording(): Promise<boolean> {
    if (this.recProc) return false;
    const mic = await this.findMicrophone();
    if (!mic) return false;
    const dir = join(CACHE_DIR, ".tmp");
    await mkdir(dir, { recursive: true });
    const out = resolve(join(dir, `voice_${Date.now()}.ogg`));
    // Opus mono 32k is what WhatsApp voice notes use.
    this.recProc = spawn(
      this.findFfmpeg()!,
      ["-hide_banner", "-loglevel", "error", "-f", "dshow", "-i", `audio=${mic}`,
       "-c:a", "libopus", "-b:a", "32k", "-ar", "48000", "-ac", "1", "-y", out],
      { stdio: ["pipe", "ignore", "ignore"] },
    );
    this.recPath = out;
    return true;
  }

  // Gracefully stops ffmpeg ("q" on stdin) so the container is finalized.
  async stopRecording(): Promise<string | null> {
    const proc = this.recProc;
    const path = this.recPath;
    if (!proc || !path) return null;
    this.recProc = null;
    this.recPath = null;
    await new Promise<void>((done) => {
      const finish = () => done();
      proc.once("exit", finish);
      try {
        proc.stdin?.write("q");
        proc.stdin?.end();
      } catch {
        proc.kill();
      }
      setTimeout(() => {
        try {
          proc.kill();
        } catch {
          // already gone
        }
        finish();
      }, 4000);
    });
    return existsSync(path) ? path : null;
  }

  cancelRecording(): void {
    const proc = this.recProc;
    this.recProc = null;
    this.recPath = null;
    try {
      proc?.kill();
    } catch {
      // already gone
    }
  }

  // Converts any image into a 512x512 transparent-padded webp sticker.
  async toWebpSticker(srcPath: string): Promise<string | null> {
    try {
      const dir = join(CACHE_DIR, ".tmp");
      await mkdir(dir, { recursive: true });
      const out = resolve(join(dir, `sticker_${Date.now()}.webp`));
      await sharp(this.db.decryptBytes(await readFile(srcPath)))
        .resize(512, 512, {
          fit: "contain",
          background: { r: 0, g: 0, b: 0, alpha: 0 },
        })
        .webp({ quality: 90 })
        .toFile(out);
      return out;
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

// Waveform bitmap: white bars, centered, transparent background.
function renderBars(levels: number[]): DecodedImage {
  const barW = 3;
  const gap = 2;
  const height = 30;
  const width = levels.length * (barW + gap);
  const data = new Uint8ClampedArray(width * height * 4);
  levels.forEach((level, i) => {
    const h = Math.max(2, Math.round(level * (height - 4)));
    const top = Math.floor((height - h) / 2);
    const x0 = i * (barW + gap);
    for (let y = top; y < top + h; y++) {
      for (let x = x0; x < x0 + barW; x++) {
        const o = (y * width + x) * 4;
        data[o] = 255;
        data[o + 1] = 255;
        data[o + 2] = 255;
        data[o + 3] = 255;
      }
    }
  });
  return { width, height, data, displayW: width, displayH: height };
}

function sanitize(id: string): string {
  return id.replace(/[^A-Za-z0-9_-]/g, "_");
}
