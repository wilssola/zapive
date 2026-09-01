import {
  makeWASocket,
  fetchLatestBaileysVersion,
  DisconnectReason,
  Browsers,
  downloadMediaMessage,
  jidNormalizedUser,
  proto,
} from "@whiskeysockets/baileys";
import type { WASocket, WAMessage, AnyMessageContent } from "@whiskeysockets/baileys";
import pino from "pino";
import { extname } from "node:path";
import { useDbAuthState } from "./db.ts";
import type { Db } from "./db.ts";
import { t } from "./i18n.ts";

export interface WAListener {
  onQr(qr: string): void;
  onStatus(text: string): void;
  onOpen(): void;
  onLoggedOut(): void;
  onHistorySet(payload: unknown): void;
  onChatsUpsert(chats: unknown[]): void;
  onContactsUpsert(contacts: unknown[]): void;
  onMessagesUpsert(messages: WAMessage[]): void;
  onMessagesUpdate(updates: { key?: WAMessage["key"]; update?: { status?: unknown } }[]): void;
  onCall(calls: unknown[]): void;
  onPresence(update: {
    id: string;
    presences: Record<string, { lastKnownPresence?: string }>;
  }): void;
}

// The logger doubles as a detector for app-state keys the phone never
// shared: Baileys logs "failed to find key \"<id>\"" while decoding, and we
// then request those keys from the phone (APP_STATE_SYNC_KEY_REQUEST).
let missingKeySink: ((keyId: string) => void) | null = null;
const logger = pino(
  { level: "info" },
  {
    write(line: string) {
      const m = line.match(/failed to find key[^A-Za-z0-9+/=]+([A-Za-z0-9+/=]{6,})/);
      if (m?.[1] && missingKeySink) missingKeySink(m[1]);
      if (process.env.ZAPIVE_LOG) process.stdout.write(line);
    },
  },
);

export class WhatsAppService {
  private sock: WASocket | null = null;
  private stopped = false;
  private _connected = false;
  private retryDelay = 2000;

  get connected(): boolean {
    return this._connected;
  }

  private listener: WAListener;
  private db: Db;
  private missingKeys = new Set<string>();
  private requestedKeys = new Set<string>();
  private keyReqTimer: NodeJS.Timeout | null = null;

  constructor(listener: WAListener, db: Db) {
    this.listener = listener;
    this.db = db;
    missingKeySink = (keyId) => this.noteMissingAppStateKey(keyId);
  }

  private noteMissingAppStateKey(keyId: string) {
    if (this.requestedKeys.has(keyId)) return;
    this.missingKeys.add(keyId);
    if (this.keyReqTimer) return;
    this.keyReqTimer = setTimeout(() => {
      this.keyReqTimer = null;
      void this.requestAppStateKeys();
    }, 1500);
  }

  // Asks the phone (via a peer protocol message) to share the app-state
  // sync keys we're missing; Baileys resumes the parked collection sync
  // automatically when the APP_STATE_SYNC_KEY_SHARE arrives.
  private async requestAppStateKeys() {
    const sock = this.sock;
    const ids = [...this.missingKeys].filter((k) => !this.requestedKeys.has(k));
    if (!sock || !sock.user?.id || ids.length === 0) return;
    for (const id of ids) this.requestedKeys.add(id);
    this.missingKeys.clear();
    try {
      console.log(`[appstate] requesting ${ids.length} sync key(s) from phone`);
      await sock.relayMessage(
        jidNormalizedUser(sock.user.id),
        {
          protocolMessage: {
            type: proto.Message.ProtocolMessage.Type.APP_STATE_SYNC_KEY_REQUEST,
            appStateSyncKeyRequest: {
              keyIds: ids.map((b64) => ({ keyId: Buffer.from(b64, "base64") })),
            },
          },
        },
        { additionalAttributes: { category: "peer", push_priority: "high_force" } },
      );
    } catch (err) {
      console.log("[appstate] key request failed:", String(err));
    }
  }

  async start(): Promise<void> {
    const { state, saveCreds } = useDbAuthState(this.db);

    let version: [number, number, number] | undefined;
    try {
      ({ version } = await fetchLatestBaileysVersion());
    } catch {
      // offline / fetch failed: Baileys falls back to its baked-in version
    }

    const sock = makeWASocket({
      version,
      auth: state,
      logger,
      browser: Browsers.macOS("Desktop"),
      markOnlineOnConnect: false,
      // syncFullHistory: true breaks registration on 7.0.0-rc14 (instant 428
      // loop); the explicit callback keeps the essential history sync enabled.
      syncFullHistory: false,
      shouldSyncHistoryMessage: () => true,
      getMessage: async () => undefined,
    });
    this.sock = sock;

    sock.ev.on("creds.update", saveCreds);

    sock.ev.on("connection.update", (update) => {
      const { connection, lastDisconnect, qr } = update;
      console.log(
        "[conn]",
        connection ?? (qr ? "qr" : "?"),
        lastDisconnect?.error ? String(lastDisconnect.error) : "",
      );
      if (qr) {
        this.listener.onQr(qr);
        this.listener.onStatus(t("status.scanQr"));
      }
      if (connection === "connecting") {
        this.listener.onStatus(t("status.connecting"));
      }
      if (connection === "open") {
        this._connected = true;
        this.retryDelay = 2000;
        this.listener.onOpen();
      }
      if (connection === "close") {
        this._connected = false;
        const statusCode = (lastDisconnect?.error as { output?: { statusCode?: number } })
          ?.output?.statusCode;
        console.log(`[conn] close statusCode=${statusCode}`);
        if (statusCode === DisconnectReason.loggedOut) {
          this.listener.onStatus(t("status.sessionEnded"));
          void this.resetAuthAndRestart();
        } else if (!this.stopped) {
          this.listener.onStatus(t("status.reconnecting"));
          setTimeout(() => void this.start(), this.retryDelay);
          this.retryDelay = Math.min(this.retryDelay * 2, 60_000);
        }
      }
    });

    sock.ev.on("messaging-history.set", (payload) => {
      const p = payload as { chats?: unknown[]; messages?: unknown[]; isLatest?: boolean };
      console.log(
        `[history] chats=${p.chats?.length ?? 0} messages=${p.messages?.length ?? 0} isLatest=${p.isLatest}`,
      );
      this.listener.onHistorySet(payload);
    });
    sock.ev.on("chats.upsert", (chats) => this.listener.onChatsUpsert(chats));
    sock.ev.on("chats.update", (chats) => this.listener.onChatsUpsert(chats));
    sock.ev.on("contacts.upsert", (contacts) => this.listener.onContactsUpsert(contacts));
    sock.ev.on("messages.upsert", ({ messages }) => {
      this.listener.onMessagesUpsert(messages);
    });
    sock.ev.on("messages.update", (updates) => {
      this.listener.onMessagesUpdate(updates as never);
    });
    sock.ev.on("call", (calls) => {
      this.listener.onCall(calls as unknown[]);
    });
    sock.ev.on("presence.update", (update) => {
      this.listener.onPresence(update as never);
    });
  }

  // Baileys can detect and decline calls, but never answer them.
  async rejectCall(callId: string, callFrom: string): Promise<void> {
    try {
      await this.sock?.rejectCall(callId, callFrom);
    } catch (err) {
      console.log("[call] reject failed:", String(err));
    }
  }

  async subscribePresence(jid: string): Promise<void> {
    try {
      await this.sock?.presenceSubscribe(jid);
    } catch {
      // presence is best-effort
    }
  }

  // User-initiated logout: unlink this device on the server. Baileys then
  // closes the socket with DisconnectReason.loggedOut, and our
  // connection.update handler clears local credentials and shows a new QR.
  async logout(): Promise<void> {
    try {
      await this.sock?.logout();
      console.log("[logout] remove-companion-device sent to server");
    } catch (err) {
      console.log("[logout] server-side unlink failed:", String(err));
      this._connected = false;
      await this.resetAuthAndRestart();
    }
  }

  private async resetAuthAndRestart(): Promise<void> {
    this.listener.onLoggedOut();
    this.db.delPrefix("auth:");
    if (!this.stopped) await this.start();
  }

  async requestPairingCode(phone: string): Promise<string> {
    const digits = phone.replace(/\D/g, "");
    if (!digits) throw new Error(t("error.invalidNumber"));
    if (!this.sock) throw new Error(t("error.stillConnecting"));
    const code = await this.sock.requestPairingCode(digits);
    return code.length === 8 ? `${code.slice(0, 4)}-${code.slice(4)}` : code;
  }

  private async send(jid: string, content: AnyMessageContent): Promise<WAMessage | null> {
    if (!this.sock) throw new Error(t("error.notConnected"));
    const result = await this.sock.sendMessage(jid, content);
    return result ?? null;
  }

  sendText(jid: string, text: string): Promise<WAMessage | null> {
    return this.send(jid, { text });
  }

  sendImage(jid: string, filePath: string, caption?: string): Promise<WAMessage | null> {
    return this.send(jid, { image: { url: filePath }, caption });
  }

  // Voice note; viewOnce wraps it so it can be played a single time.
  sendVoice(jid: string, filePath: string, viewOnce: boolean): Promise<WAMessage | null> {
    return this.send(jid, {
      audio: { url: filePath },
      mimetype: "audio/ogg; codecs=opus",
      ptt: true,
      viewOnce,
    });
  }

  sendGif(jid: string, filePath: string): Promise<WAMessage | null> {
    return this.send(jid, {
      video: { url: filePath },
      gifPlayback: true,
      mimetype: "video/mp4",
    });
  }

  sendSticker(jid: string, webpPath: string): Promise<WAMessage | null> {
    return this.send(jid, { sticker: { url: webpPath } });
  }

  sendForward(jid: string, raw: WAMessage): Promise<WAMessage | null> {
    return this.send(jid, { forward: raw } as never);
  }

  sendDocument(jid: string, filePath: string): Promise<WAMessage | null> {
    const fileName = filePath.split(/[\\/]/).pop() ?? "documento";
    return this.send(jid, {
      document: { url: filePath },
      fileName,
      mimetype: "application/octet-stream",
    });
  }

  sendAudio(jid: string, filePath: string): Promise<WAMessage | null> {
    return this.send(jid, {
      audio: { url: filePath },
      mimetype: audioMimeFromExt(filePath),
      ptt: true,
    });
  }

  // string = has picture; null = confirmed no picture; undefined = transient
  // failure (offline, rate limit) — caller should retry later.
  async profilePictureUrl(jid: string): Promise<string | null | undefined> {
    if (!this.sock || !this._connected) return undefined;
    try {
      return (await this.sock.profilePictureUrl(jid, "preview")) ?? null;
    } catch (err) {
      const msg = String(err);
      if (/not-found|not-authorized|item|404|401/i.test(msg)) return null;
      return undefined;
    }
  }

  // Pulls app-state collections (pinned/archived chats, etc.).
  // full=true wipes the stored versions to replay everything from v0
  // (needed once after pairing, since the phone's empty history sync makes
  // Baileys drop the initial updates); afterwards incremental sync applies
  // only new actions, in order.
  async resyncAppState(full: boolean): Promise<void> {
    try {
      if (full) this.db.delPrefix("auth:app-state-sync-version");
      await this.sock?.resyncAppState(
        ["critical_unblock_low", "regular_low", "regular_high", "regular"] as never,
        false,
      );
      console.log(`[appstate] resync done (full=${full})`);
    } catch (err) {
      console.log("[appstate] resync failed:", String(err));
    }
  }

  // "About" text a contact publishes on their profile.
  async fetchAbout(jid: string): Promise<string> {
    try {
      const r = await this.sock?.fetchStatus(jid);
      const entry = Array.isArray(r) ? r[0] : r;
      return (entry as { status?: { status?: string } } | undefined)?.status?.status ?? "";
    } catch {
      return "";
    }
  }

  async fetchGroupInfo(
    jid: string,
  ): Promise<{ desc?: string; participants?: unknown[] } | null> {
    try {
      return (await this.sock?.groupMetadata(jid)) ?? null;
    } catch {
      return null;
    }
  }

  async setArchived(jid: string, archived: boolean, lastMsg: WAMessage | undefined) {
    try {
      await this.sock?.chatModify(
        { archive: archived, lastMessages: lastMsg ? [lastMsg] : [] } as never,
        jid,
      );
    } catch (err) {
      console.log("[chat] archive toggle failed:", String(err));
    }
  }

  async fetchGroups(): Promise<
    Record<
      string,
      {
        subject?: string;
        linkedParent?: string;
        isCommunity?: boolean;
        isCommunityAnnounce?: boolean;
      }
    >
  > {
    if (!this.sock) return {};
    try {
      return (await this.sock.groupFetchAllParticipating()) as never;
    } catch {
      return {};
    }
  }

  // Channel (newsletter) display name.
  async fetchChannelName(jid: string): Promise<string> {
    try {
      const meta = await this.sock?.newsletterMetadata("jid", jid);
      return (meta as { name?: string } | null)?.name ?? "";
    } catch {
      return "";
    }
  }

  // Asks the phone for messages older than the given anchor (on-demand
  // history sync); the response arrives via messaging-history.set.
  async fetchOlderHistory(
    count: number,
    key: NonNullable<WAMessage["key"]>,
    timestamp: number,
  ): Promise<boolean> {
    if (!this.sock || !this._connected) return false;
    try {
      await this.sock.fetchMessageHistory(count, key, timestamp);
      return true;
    } catch (err) {
      console.log("[history] on-demand fetch failed:", String(err));
      return false;
    }
  }

  async downloadMedia(msg: WAMessage): Promise<Buffer> {
    const sock = this.sock;
    return (await downloadMediaMessage(
      msg,
      "buffer",
      {},
      sock
        ? { logger, reuploadRequest: sock.updateMediaMessage }
        : (undefined as never),
    )) as Buffer;
  }
}

function audioMimeFromExt(filePath: string): string {
  switch (extname(filePath).toLowerCase()) {
    case ".ogg":
    case ".opus":
      return "audio/ogg; codecs=opus";
    case ".mp3":
      return "audio/mpeg";
    case ".m4a":
    case ".aac":
      return "audio/mp4";
    case ".wav":
      return "audio/wav";
    default:
      return "audio/mpeg";
  }
}
