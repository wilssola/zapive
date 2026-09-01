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
        const file = join(dir, `${avatarFileName(jid)}.png`);
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

  // Loads avatars cached on disk so the chat list has pictures right
  // away instead of waiting for the network round-trips.
  async preloadAvatars(onReady: (jid: string) => void): Promise<void> {
    const dir = join(CACHE_DIR, "avatars");
    if (!existsSync(dir)) return;
    for (const file of readdirSync(dir)) {
      if (!file.endsWith(".png")) continue;
      const jid = decodeAvatarFileName(file.slice(0, -4));
      if (!jid || this.avatars.has(jid)) continue;
      try {
        const full = join(dir, file);
        const buf = this.db.decryptBytes(await readFile(full));
        const { data, info } = await sharp(buf)
          .resize(96, 96, { fit: "cover" })
          .ensureAlpha()
          .raw()
          .toBuffer({ resolveWithObject: true });
        this.avatars.set(jid, {
          width: info.width,
          height: info.height,
          data: new Uint8ClampedArray(data.buffer, data.byteOffset, data.length),
          displayW: info.width,
          displayH: info.height,
        });
        this.avatarFiles.set(jid, resolve(full));
        onReady(jid);
      } catch {
        // unreadable cache entry: it will be fetched again
      }
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

  // Decodes a sticker into its frames (animated webp yields many); the
  // caller cycles them to animate.
  async stickerFrames(msgId: string, filePath: string): Promise<DecodedImage[]> {
    const cached = this.stickerAnim.get(msgId);
    if (cached) return cached;
    const frames: DecodedImage[] = [];
    try {
      const buf = this.db.decryptBytes(await readFile(filePath));
      const meta = await sharp(buf, { animated: true }).metadata();
      const pages = Math.min(meta.pages ?? 1, 24);
      for (let page = 0; page < pages; page++) {
        const { data, info } = await sharp(buf, { page })
          .resize(180, 180, { fit: "inside", withoutEnlargement: true })
          .ensureAlpha()
          .raw()
          .toBuffer({ resolveWithObject: true });
        frames.push({
          width: info.width,
          height: info.height,
          data: new Uint8ClampedArray(data.buffer, data.byteOffset, data.length),
          displayW: info.width,
          displayH: info.height,
        });
      }
    } catch {
      // fall through with whatever decoded
    }
    this.stickerAnim.set(msgId, frames);
    return frames;
  }

  // Extracts frames from a short clip so GIFs can loop inside the bubble
  // instead of opening an external player.
  async videoFrames(msgId: string, filePath: string): Promise<DecodedImage[]> {
    const cached = this.stickerAnim.get(msgId);
    if (cached) return cached;
    const frames: DecodedImage[] = [];
    try {
      const src = await this.plainAudioPath(msgId, filePath);
      const dir = join(CACHE_DIR, ".tmp", `frames_${sanitize(msgId)}`);
      await mkdir(dir, { recursive: true });
      await execFileAsync(
        this.findFfmpeg()!,
        ["-hide_banner", "-loglevel", "error", "-y", "-i", src,
         "-vf", "fps=15,scale=320:-2", "-frames:v", "45", join(dir, "f_%03d.png")],
        { timeout: 60_000 },
      );
      for (const file of readdirSync(dir).sort()) {
        const { data, info } = await sharp(join(dir, file))
          .ensureAlpha()
          .raw()
          .toBuffer({ resolveWithObject: true });
        frames.push({
          width: info.width,
          height: info.height,
          data: new Uint8ClampedArray(data.buffer, data.byteOffset, data.length),
          displayW: info.width,
          displayH: info.height,
        });
      }
      await rm(dir, { recursive: true, force: true });
    } catch {
      // leave whatever decoded; caller falls back to the thumbnail
    }
    this.stickerAnim.set(msgId, frames);
    return frames;
  }

  // Frames already extracted for a clip (empty when not decoded yet).
  cachedFrames(msgId: string): DecodedImage[] {
    return this.stickerAnim.get(msgId) ?? [];
  }

  // ---- In-app video playback ----
  // Slint has no video item, so ffmpeg streams raw frames that the UI
  // paints, while ffplay renders the audio track in lockstep.

  private videoProcs: ReturnType<typeof spawn>[] = [];

  private async probeSize(src: string): Promise<{ w: number; h: number }> {
    const ffprobe = this.findFfmpeg()!.replace(/ffmpeg\.exe$/i, "ffprobe.exe");
    try {
      const { stdout } = await execFileAsync(ffprobe, [
        "-v", "error", "-select_streams", "v:0",
        "-show_entries", "stream=width,height",
        "-of", "csv=p=0", src,
      ], { timeout: 20_000 });
      const [w, h] = stdout.trim().split(",").map((n) => parseInt(n, 10));
      if (w && h) return { w, h };
    } catch {
      // fall through to a default
    }
    return { w: 640, h: 360 };
  }

  async playVideo(
    msgId: string,
    filePath: string,
    onFrame: (img: DecodedImage) => void,
    onEnd: () => void,
    withAudio = true,
  ): Promise<{ width: number; height: number } | null> {
    this.stopVideo();
    try {
      const src = await this.plainAudioPath(msgId, filePath);
      const { w, h } = await this.probeSize(src);
      const targetW = Math.min(560, w % 2 === 0 ? w : w - 1);
      const targetH = Math.max(2, Math.round((h * targetW) / w / 2) * 2);
      const frameBytes = targetW * targetH * 4;
      const fps = 15;

      const video = spawn(
        this.findFfmpeg()!,
        ["-hide_banner", "-loglevel", "error", "-re", "-i", src,
         "-vf", `fps=${fps},scale=${targetW}:${targetH}`,
         "-f", "rawvideo", "-pix_fmt", "rgba", "-"],
        { stdio: ["ignore", "pipe", "ignore"] },
      );
      this.videoProcs = [video];
      if (withAudio) {
        const ffplay = this.findFfmpeg()!.replace(/ffmpeg\.exe$/i, "ffplay.exe");
        this.videoProcs.push(
          spawn(ffplay, ["-nodisp", "-autoexit", "-vn", "-loglevel", "quiet", src], {
            stdio: "ignore",
          }),
        );
      }

      let pending: Buffer = Buffer.alloc(0);
      video.stdout?.on("data", (chunk: Buffer) => {
        pending = pending.length === 0 ? Buffer.from(chunk) : Buffer.concat([pending, chunk]);
        while (pending.length >= frameBytes) {
          const frame = pending.subarray(0, frameBytes);
          pending = pending.subarray(frameBytes);
          onFrame({
            width: targetW,
            height: targetH,
            data: new Uint8ClampedArray(
              frame.buffer.slice(frame.byteOffset, frame.byteOffset + frameBytes),
            ),
            displayW: targetW,
            displayH: targetH,
          });
        }
      });
      video.once("close", () => {
        if (this.videoProcs.includes(video)) onEnd();
      });
      return { width: targetW, height: targetH };
    } catch {
      this.stopVideo();
      return null;
    }
  }

  stopVideo(): void {
    const procs = this.videoProcs;
    this.videoProcs = [];
    for (const proc of procs) {
      try {
        proc.kill();
        if (proc.pid) {
          spawn("taskkill", ["/PID", String(proc.pid), "/T", "/F"], { stdio: "ignore" });
        }
      } catch {
        // already gone
      }
    }
  }

  // ---- GIF search (Giphy) ----
  // Baileys has no GIF API, so searching mirrors what WhatsApp does:
  // query a GIF provider, then send the mp4 as a gif-playback video.

  async giphy(apiKey: string, query: string): Promise<{ id: string; preview: string; mp4: string }[]> {
    const base = query.trim()
      ? `https://api.giphy.com/v1/gifs/search?q=${encodeURIComponent(query.trim())}`
      : "https://api.giphy.com/v1/gifs/trending?";
    const url = `${base}&api_key=${encodeURIComponent(apiKey)}&limit=24&rating=pg-13`;
    try {
      const res = await fetch(url);
      if (!res.ok) return [];
      const body = (await res.json()) as {
        data?: {
          id?: string;
          images?: {
            fixed_width_small?: { url?: string };
            fixed_width?: { mp4?: string; url?: string };
            original?: { mp4?: string };
          };
        }[];
      };
      return (body.data ?? [])
        .map((g) => ({
          id: g.id ?? "",
          preview: g.images?.fixed_width_small?.url ?? g.images?.fixed_width?.url ?? "",
          mp4: g.images?.fixed_width?.mp4 ?? g.images?.original?.mp4 ?? "",
        }))
        .filter((g) => g.id && g.preview && g.mp4);
    } catch {
      return [];
    }
  }

  // Keyless provider: Openverse indexes openly licensed media and needs
  // no credentials. Results are .gif files, converted to mp4 on send.
  async openverse(query: string): Promise<{ id: string; preview: string; gif: string }[]> {
    const q = query.trim() || "funny";
    const url =
      "https://api.openverse.org/v1/images/?extension=gif&page_size=20&q=" +
      encodeURIComponent(q);
    try {
      const res = await fetch(url, { headers: { "User-Agent": "Zapive" } });
      if (!res.ok) return [];
      const body = (await res.json()) as {
        results?: { id?: string; url?: string; thumbnail?: string }[];
      };
      return (body.results ?? [])
        .map((r) => ({
          id: r.id ?? "",
          preview: r.thumbnail || r.url || "",
          gif: r.url ?? "",
        }))
        .filter((r) => r.id && r.gif);
    } catch {
      return [];
    }
  }

  // WhatsApp plays "GIFs" as looping mp4, so convert before sending.
  async gifToMp4(gifPath: string): Promise<string | null> {
    try {
      const dir = join(CACHE_DIR, ".tmp");
      await mkdir(dir, { recursive: true });
      const out = resolve(join(dir, `gif_${Date.now()}.mp4`));
      await execFileAsync(this.findFfmpeg()!, [
        "-hide_banner", "-loglevel", "error", "-y", "-i", gifPath,
        "-movflags", "faststart", "-pix_fmt", "yuv420p",
        "-vf", "scale=trunc(iw/2)*2:trunc(ih/2)*2",
        out,
      ], { timeout: 60_000 });
      return out;
    } catch {
      return null;
    }
  }

  // Fetches a remote image (GIF preview) and decodes its first frame.
  async decodeUrl(url: string): Promise<DecodedImage | null> {
    try {
      const res = await fetch(url);
      if (!res.ok) return null;
      return await this.decodeRaw(Buffer.from(await res.arrayBuffer()));
    } catch {
      return null;
    }
  }

  // Downloads an mp4 into the temp folder so it can be sent.
  async downloadTemp(url: string, ext: string): Promise<string | null> {
    try {
      const res = await fetch(url);
      if (!res.ok) return null;
      const dir = join(CACHE_DIR, ".tmp");
      await mkdir(dir, { recursive: true });
      const out = resolve(join(dir, `dl_${Date.now()}.${ext}`));
      await writeFile(out, Buffer.from(await res.arrayBuffer()));
      return out;
    } catch {
      return null;
    }
  }

  // ---- Audio playback (ffplay) and waveforms ----

  private playProcs = new Set<ReturnType<typeof spawn>>();
  private playToken = 0;
  private plainAudio = new Map<string, string>();
  private waveCache = new Map<string, DecodedImage | null>();
  private stickerAnim = new Map<string, DecodedImage[]>();

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
    return this.playProcs.size > 0;
  }

  // Starts (or restarts at an offset) playback of a cached audio file.
  // Decrypting is async, so a token discards starts that a newer click
  // has already superseded.
  async playAudio(msgId: string, filePath: string, offsetSec: number): Promise<boolean> {
    this.stopAudio();
    const token = this.playToken;
    try {
      const src = await this.plainAudioPath(msgId, filePath);
      if (token !== this.playToken) return false;
      const ffplay = this.findFfmpeg()!.replace(/ffmpeg\.exe$/i, "ffplay.exe");
      const proc = spawn(
        ffplay,
        ["-nodisp", "-autoexit", "-loglevel", "quiet", "-ss", String(offsetSec), src],
        { stdio: "ignore" },
      );
      this.playProcs.add(proc);
      proc.once("exit", () => this.playProcs.delete(proc));
      // A stop that arrived while the OS was still creating the process
      // would be lost, so honor it as soon as it exists.
      proc.once("spawn", () => {
        if (token !== this.playToken) this.hardKill(proc);
      });
      return true;
    } catch {
      return false;
    }
  }

  private hardKill(proc: ReturnType<typeof spawn>) {
    try {
      proc.kill();
      if (proc.pid) {
        spawn("taskkill", ["/PID", String(proc.pid), "/T", "/F"], { stdio: "ignore" });
      }
    } catch {
      // already gone
    }
  }

  stopAudio(): void {
    this.playToken++;
    for (const proc of this.playProcs) {
      this.hardKill(proc);
      // Covers the case where the process had not been created yet.
      proc.once("spawn", () => this.hardKill(proc));
    }
    this.playProcs.clear();
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

// Avatar files are named from the jid so the cache can be reloaded.
function avatarFileName(jid: string): string {
  return Buffer.from(jid, "utf8").toString("base64url");
}

function decodeAvatarFileName(name: string): string | null {
  try {
    const jid = Buffer.from(name, "base64url").toString("utf8");
    return jid.includes("@") ? jid : null;
  } catch {
    return null;
  }
}

function sanitize(id: string): string {
  return id.replace(/[^A-Za-z0-9_-]/g, "_");
}
