import { ArrayModel } from "slint-ui";
import { jidNormalizedUser, proto } from "@whiskeysockets/baileys";
import type { WAMessage } from "@whiskeysockets/baileys";
import { Store, formatTime, formatDay, reactionSummary, ticksFor } from "./store.ts";
import { Notify } from "./notify.ts";
import { t } from "./i18n.ts";
import type { Db } from "./db.ts";
import type { StoredMessage } from "./store.ts";
import { qrToImageData, EMPTY_IMAGE } from "./qr.ts";
import type { SlintImageData } from "./qr.ts";
import type { WAListener, WhatsAppService } from "./whatsapp.ts";
import type { MediaService } from "./media.ts";

interface ChatRow {
  jid: string;
  name: string;
  preview: string;
  time: string;
  avatar: SlintImageData;
  hasAvatar: boolean;
  initial: string;
  colorIdx: number;
  unread: number;
  pinned: boolean;
}

interface MessageRow {
  id: string;
  kind: string;
  text: string;
  fromMe: boolean;
  sender: string;
  showSender: boolean;
  firstOfRun: boolean;
  time: string;
  picture: SlintImageData;
  picW: number;
  picH: number;
  mediaPath: string;
  mediaReady: boolean;
  reactions: string;
  dayLabel: string;
  ticks: string;
  ticksBlue: boolean;
  senderJid: string;
  forwarded: boolean;
  deleted: boolean;
}

// The generated AppWindow component; slint-ui has no generated typings, so
// the property surface is declared here. slint-node maps dashed identifiers
// to underscores (status-text -> status_text).
export interface AppWindow {
  screen: string;
  status_text: string;
  dark_theme: boolean;
  theme_mode: string;
  theme_changed: (mode: string) => void;
  language_mode: string;
  language_changed: (mode: string) => void;
  lock_error: string;
  qr_image: SlintImageData;
  pairing_mode: boolean;
  pairing_code: string;
  chats: ArrayModel<ChatRow>;
  selected_chat: number;
  chat_tab: string;
  search_changed: (text: string) => void;
  tab_changed: (tab: string) => void;
  chat_open: boolean;
  sync_banner: string;
  current_chat_name: string;
  current_status: string;
  load_older: () => void;
  attach_doc: () => void;
  open_dm: (jid: string) => void;
  request_forward: (msgId: string) => void;
  forward_open: boolean;
  forward_to: (index: number) => void;
  paste_clipboard: () => void;
  append_composer: (t: string) => void;
  preview_open: boolean;
  preview_image: SlintImageData;
  confirm_send_image: (caption: string) => void;
  cancel_send_image: () => void;
  current_avatar: SlintImageData;
  current_avatar_has: boolean;
  current_initial: string;
  current_color_idx: number;
  messages: ArrayModel<MessageRow>;
  settings_open: boolean;
  pin_set: boolean;
  settings_status: string;
  lightbox_open: boolean;
  lightbox_image: SlintImageData;
  unlock: (pin: string) => void;
  save_pin: (current: string, next: string) => void;
  remove_pin: (current: string) => void;
  logout: () => void;
  request_pairing_code: (phone: string) => void;
  open_chat: (index: number) => void;
  send_message: (text: string) => void;
  attach_image: () => void;
  attach_audio: () => void;
  play_audio: (path: string) => void;
  scroll_conversation_end: () => void;
  scroll_conversation_top: () => void;
  show(): void;
  run(): Promise<void>;
}

const AVATAR_RETRIES = 3;

function initialOf(name: string): string {
  const ch = [...name.trim()][0];
  return ch ? ch.toUpperCase() : "?";
}

function colorIdxOf(jid: string): number {
  let h = 0;
  for (let i = 0; i < jid.length; i++) h = (h + jid.charCodeAt(i)) | 0;
  return Math.abs(h) % 8;
}

export class Bridge implements WAListener {
  private store = new Store();
  private chatsModel = new ArrayModel<ChatRow>([]);
  private messagesModel = new ArrayModel<MessageRow>([]);
  private currentJid: string | null = null;
  private service!: WhatsAppService;
  private refreshTimer: NodeJS.Timeout | null = null;
  private groupsFetched = false;
  private requestedAvatars = new Set<string>();
  private avatarQueue: string[] = [];
  private avatarBusy = false;

  private win: AppWindow;
  private media: MediaService;
  private db!: Db;
  private avatarTries = new Map<string, number>();
  private searchText = "";
  private tab = "all";
  private notify = new Notify();

  constructor(win: AppWindow, media: MediaService) {
    this.win = win;
    this.media = media;
    win.chats = this.chatsModel;
    win.messages = this.messagesModel;

    win.open_chat = (i) => this.openChatAt(i);
    win.search_changed = (text) => {
      this.searchText = text.trim().toLowerCase();
      this.refreshChats();
    };
    win.tab_changed = (tab) => {
      this.tab = tab;
      this.refreshChats();
    };
    win.send_message = (text) => void this.handleSendText(text);
    win.attach_image = () => void this.handleAttach("image");
    win.attach_audio = () => void this.handleAttach("audio");
    win.attach_doc = () => void this.handleAttach("doc");
    win.load_older = () => this.handleScrollUpLoad();
    win.paste_clipboard = () => void this.handlePaste();
    win.open_dm = (jid) => this.openDm(jid);
    win.request_forward = (msgId) => {
      if (!this.currentJid) return;
      this.pendingForward = { jid: this.currentJid, id: msgId };
      win.forward_open = true;
    };
    win.forward_to = (i) => void this.handleForwardTo(i);
    win.confirm_send_image = (caption) => void this.confirmSendImage(caption);
    win.cancel_send_image = () => {
      this.pendingImage = null;
      win.preview_open = false;
    };
    win.play_audio = (path) => this.media.play(path);
    win.request_pairing_code = (phone) => void this.handlePairing(phone);
  }

  setService(service: WhatsAppService) {
    this.service = service;
  }

  // Called after the db is unlocked: loads persisted chats/messages.
  init(db: Db) {
    this.db = db;
    try {
      this.store.loadFrom(db);
    } catch (err) {
      console.error("[store] load failed:", err);
    }
    console.log(
      `[store] loaded chats=${this.store.chats.size} msgChats=${this.store.messages.size} ` +
        `total=${this.store.totalMessages()} oldestTs=${this.store.oldestMessage()?.timestamp}`,
    );
    this.refreshChats();
  }

  importLegacyStore(json: string) {
    this.store.importLegacyJson(json);
    this.store.saveTo(this.db);
    this.refreshChats();
  }

  // ---- WAListener ----

  onQr(qr: string) {
    this.win.qr_image = qrToImageData(qr);
  }

  onStatus(text: string) {
    this.win.status_text = text;
  }

  onOpen() {
    this.win.status_text = t("status.connected");
    if (this.win.screen === "login") this.win.screen = "main";
    if (!this.groupsFetched) {
      this.groupsFetched = true;
      void this.fetchGroupNames();
    }
    const fullResync = this.db?.settingGet("appstate_seeded") !== "1";
    void this.service.resyncAppState(fullResync).then(() => {
      if (fullResync) this.db?.settingSet("appstate_seeded", "1");
    });
    // Backfill old conversations via on-demand history sync (the pairing
    // history payload from the phone arrives empty).
    setTimeout(() => void this.pullOlderHistory(), 8000);
    // Kick avatar loading for chats restored from disk before the connection.
    this.scheduleRefreshChats();
  }

  private historyBatches = 0;
  private historyPending = false;
  private static readonly MAX_HISTORY_BATCHES = 20;

  private setPending(v: boolean) {
    this.historyPending = v;
    this.win.sync_banner = v ? t("sync.older") : "";
  }

  private async pullOlderHistory() {
    if (this.historyPending || this.historyBatches >= Bridge.MAX_HISTORY_BATCHES) return;
    const oldest = this.store.oldestMessage();
    if (!oldest?.raw?.key) {
      // No message to anchor on yet (fresh pairing) — retry once messages
      // start flowing in.
      console.log("[history] no anchor yet; retrying in 30s");
      setTimeout(() => void this.pullOlderHistory(), 30_000);
      return;
    }
    this.setPending(true);
    this.historyBatches++;
    console.log(
      `[history] on-demand batch ${this.historyBatches} (before ts=${oldest.timestamp}, total=${this.store.totalMessages()})`,
    );
    const sent = await this.service.fetchOlderHistory(50, oldest.raw.key, oldest.timestamp);
    if (!sent) {
      this.setPending(false);
      return;
    }
    // If the phone never answers, unblock after a while.
    setTimeout(() => {
      if (this.historyPending) this.setPending(false);
    }, 20_000);
  }

  onLoggedOut() {
    this.currentJid = null;
    this.groupsFetched = false;
    this.win.pairing_mode = false;
    this.win.pairing_code = "";
    this.win.chat_open = false;
    this.win.screen = "login";
    // Wipe local conversation data along with the session.
    this.store = new Store();
    this.requestedAvatars.clear();
    this.avatarQueue.length = 0;
    this.chatsModel.splice(0, this.chatsModel.length);
    this.messagesModel.splice(0, this.messagesModel.length);
    this.db?.delPrefix("store:");
    this.db?.settingSet("appstate_seeded", "0");
  }

  onHistorySet(payload: unknown) {
    const { chats, contacts, messages } = payload as {
      chats?: unknown[];
      contacts?: unknown[];
      messages?: WAMessage[];
    };
    for (const c of contacts ?? []) this.store.upsertContact(c as never);
    for (const c of chats ?? []) this.store.upsertChat(c as never);
    let added = 0;
    let addedToCurrent = false;
    for (const m of messages ?? []) {
      const stored = this.ingest(m);
      if (stored) {
        added++;
        if (stored.jid === this.currentJid) addedToCurrent = true;
      }
    }
    // Older messages may have arrived for the open conversation — rebuild it.
    if (addedToCurrent && this.currentJid) {
      const list = this.store.messagesFor(this.currentJid);
      const rows = list.map((m, i) => this.toRow(m, i > 0 ? list[i - 1] : undefined));
      this.messagesModel.splice(0, this.messagesModel.length, ...rows);
      if (this.scrollUpFetch) {
        // user was reading the top — stay there
        setTimeout(() => {
          try {
            this.win.scroll_conversation_top();
          } catch {
            // layout not ready
          }
        }, 60);
      } else {
        this.scrollToEnd();
      }
      void this.loadMediaForChat(this.currentJid);
    }
    this.scrollUpFetch = false;
    this.scheduleRefreshChats();
    if (this.historyPending) {
      this.setPending(false);
      if (added > 0) {
        // keep walking back while the phone still has older messages
        setTimeout(() => void this.pullOlderHistory(), 3000);
      } else {
        this.historyBatches = Bridge.MAX_HISTORY_BATCHES;
        console.log("[history] backfill complete (no more messages)");
      }
    }
  }

  onChatsUpsert(chats: unknown[]) {
    for (const c of chats) {
      const u = c as { id?: string; pinned?: unknown; archived?: unknown };
      if (u.pinned !== undefined || u.archived !== undefined) {
        console.log(`[chatflag] ${u.id} pinned=${u.pinned} archived=${u.archived}`);
      }
      if (u.id) this.store.upsertChat(c as never);
    }
    this.scheduleRefreshChats();
  }

  onContactsUpsert(contacts: unknown[]) {
    for (const c of contacts) this.store.upsertContact(c as never);
    this.scheduleRefreshChats();
  }

  onMessagesUpsert(messages: WAMessage[]) {
    for (const raw of messages) {
      const pm = raw.message?.protocolMessage;
      if (pm?.key?.id && pm.type === proto.Message.ProtocolMessage.Type.REVOKE) {
        const chatJid = this.store.canon(
          jidNormalizedUser(raw.key?.remoteJid ?? pm.key.remoteJid ?? ""),
        );
        const m = this.store.markDeleted(chatJid, pm.key.id);
        if (m && chatJid === this.currentJid) {
          this.patchRow(m.id, { deleted: true, kind: "text", text: t("msg.deleted") });
        }
        this.scheduleRefreshChats();
        continue;
      }
      const rm = raw.message?.reactionMessage;
      if (rm?.key?.id) {
        this.applyReaction(
          raw.key?.remoteJid,
          rm.key.id,
          raw.key?.participant ?? (raw.key?.fromMe ? "me" : (raw.key?.remoteJid ?? "?")),
          rm.text ?? "",
        );
        continue;
      }
      const stored = this.ingest(raw);
      if (!stored) continue;
      if (stored.jid === this.currentJid) {
        this.pushMessageRow(stored);
      } else if (!stored.fromMe) {
        const meta = this.store.chats.get(stored.jid);
        if (meta) meta.unread = (meta.unread ?? 0) + 1;
        const body =
          stored.kind === "image"
            ? t("preview.photo")
            : stored.kind === "audio"
              ? t("preview.audio")
              : stored.kind === "doc"
                ? t("preview.document", stored.text)
                : stored.text;
        const isGroup = stored.jid.endsWith("@g.us");
        const title = this.store.chatName(stored.jid);
        const text = isGroup && stored.sender ? `${stored.sender}: ${body}` : body;
        const jid = stored.jid;
        void this.media
          .fetchAvatar(jid)
          .then(() => this.media.avatarIcon(jid))
          .then((icon) => this.notify.push(title, text, icon));
      }
    }
    this.scheduleRefreshChats();
  }

  onMessagesUpdate(updates: { key?: WAMessage["key"]; update?: { status?: unknown } }[]) {
    for (const u of updates) {
      const status = Number(u.update?.status ?? 0);
      if (!status || !u.key?.remoteJid || !u.key.id) continue;
      const jid = this.store.canon(jidNormalizedUser(u.key.remoteJid));
      const m = this.store.setStatus(jid, u.key.id, status);
      if (m && jid === this.currentJid) {
        const t = ticksFor(m);
        this.patchRow(m.id, { ticks: t.ticks, ticksBlue: t.ticksBlue });
      }
    }
    this.scheduleSave();
  }

  onPresence(update: {
    id: string;
    presences: Record<string, { lastKnownPresence?: string }>;
  }) {
    const jid = this.store.canon(jidNormalizedUser(update.id));
    if (jid !== this.currentJid) return;
    const states = Object.values(update.presences ?? {}).map(
      (p) => p.lastKnownPresence ?? "",
    );
    this.win.current_status = states.includes("composing")
      ? t("presence.typing")
      : states.includes("recording")
        ? t("presence.recording")
        : states.includes("available")
          ? t("presence.online")
          : "";
  }

  private lastScrollLoad = 0;

  private handleScrollUpLoad() {
    const jid = this.currentJid;
    if (!jid || this.historyPending) return;
    const now = Date.now();
    if (now - this.lastScrollLoad < 2500) return;
    const first = this.store.messagesFor(jid)[0];
    if (!first?.raw?.key || this.store.messagesFor(jid).length < 5) return;
    this.lastScrollLoad = now;
    this.scrollUpFetch = true;
    this.setPending(true);
    void this.service.fetchOlderHistory(50, first.raw.key, first.timestamp).then((ok) => {
      if (!ok) this.setPending(false);
      else
        setTimeout(() => {
          if (this.historyPending) this.setPending(false);
        }, 20_000);
    });
  }

  private scrollUpFetch = false;

  private applyReaction(
    remoteJid: string | null | undefined,
    targetId: string,
    reactor: string,
    emoji: string,
  ) {
    if (!remoteJid) return;
    const chatJid = this.store.canon(jidNormalizedUser(remoteJid));
    const updated = this.store.applyReaction(chatJid, targetId, reactor, emoji);
    if (updated && chatJid === this.currentJid) {
      this.patchRow(updated.id, { reactions: reactionSummary(updated) });
    }
    this.scheduleSave();
  }

  private pushMessageRow(stored: StoredMessage) {
    const list = this.store.messagesFor(stored.jid);
    const idx = list.findIndex((m) => m.id === stored.id);
    const prev = idx > 0 ? list[idx - 1] : undefined;
    this.messagesModel.push(this.toRow(stored, prev));
    this.scrollToEnd();
    if (stored.kind !== "text") void this.loadMediaForMessage(stored);
  }

  // ---- internals ----

  private async fetchGroupNames() {
    const groups = await this.service.fetchGroups();
    console.log(`[groups] fetched ${Object.keys(groups).length}`);
    for (const [jid, meta] of Object.entries(groups)) {
      if (!meta?.subject) continue;
      // Seed the sidebar with participating groups even before any message.
      if (!this.store.chats.has(jid)) {
        this.store.chats.set(jid, { jid, name: meta.subject, preview: "", timestamp: 0 });
      }
      this.store.setName(jid, meta.subject);
    }
    this.scheduleRefreshChats();
  }

  private ingest(raw: WAMessage): StoredMessage | null {
    const stored = this.store.normalize(raw);
    if (!stored) return null;
    return this.store.addMessage(stored) ? stored : null;
  }

  private scheduleRefreshChats() {
    if (this.refreshTimer) return;
    this.refreshTimer = setTimeout(() => {
      this.refreshTimer = null;
      this.refreshChats();
    }, 100);
  }

  private toChatRow(jid: string, preview: string, timestamp: number, unread: number): ChatRow {
    const name = this.store.chatName(jid);
    const avatar = this.media.avatarFor(jid);
    return {
      jid,
      name,
      preview,
      time: formatTime(timestamp),
      avatar: avatar ?? EMPTY_IMAGE,
      hasAvatar: !!avatar,
      initial: initialOf(name),
      colorIdx: colorIdxOf(jid),
      unread,
      pinned: (this.store.chats.get(jid)?.pinned ?? 0) > 0,
    };
  }

  private visibleChats() {
    return this.store.sortedChats().filter((meta) => {
      if (this.tab === "archived") {
        if (!meta.archived) return false;
      } else {
        if (meta.archived) return false;
        if (this.tab === "unread" && !(meta.unread && meta.unread > 0)) return false;
      }
      if (
        this.searchText &&
        !this.store.chatName(meta.jid).toLowerCase().includes(this.searchText)
      ) {
        return false;
      }
      return true;
    });
  }

  // Resolves @lid chats to phone-number chats using the LID mappings
  // Baileys persists in the auth state ("<lidUser>_reverse" -> pnUser).
  private resolveLidAliases() {
    if (!this.db) return;
    let changed = false;
    for (const meta of [...this.store.chats.values()]) {
      if (!meta.jid.endsWith("@lid")) continue;
      const lidUser = meta.jid.split("@")[0];
      const v = this.db.get(`auth:lid-mapping-${lidUser}_reverse`);
      if (!v) continue;
      let pnUser: unknown;
      try {
        pnUser = JSON.parse(v);
      } catch {
        pnUser = v;
      }
      if (typeof pnUser !== "string" || !pnUser) continue;
      if (this.store.learnAlias(meta.jid, `${pnUser}@s.whatsapp.net`)) {
        console.log(`[lid] merged ${meta.jid} -> ${pnUser}@s.whatsapp.net`);
        changed = true;
      }
    }
    if (changed && this.currentJid) {
      this.currentJid = this.store.canon(this.currentJid);
    }
  }

  private refreshChats() {
    this.resolveLidAliases();
    const visible = this.visibleChats();
    const rows = visible.map((meta) =>
      this.toChatRow(meta.jid, meta.preview, meta.timestamp, meta.unread ?? 0),
    );
    this.chatsModel.splice(0, this.chatsModel.length, ...rows);
    this.win.selected_chat = this.currentJid
      ? visible.findIndex((m) => m.jid === this.currentJid)
      : -1;
    this.ensureAvatars();
    this.scheduleSave();
  }

  private saveTimer: NodeJS.Timeout | null = null;

  private scheduleSave() {
    if (this.saveTimer || !this.db) return;
    this.saveTimer = setTimeout(() => {
      this.saveTimer = null;
      try {
        this.store.saveTo(this.db);
      } catch (err) {
        console.error("save failed:", err);
      }
    }, 2000);
  }

  private ensureAvatars() {
    if (!this.service?.connected) return;
    for (const meta of this.store.sortedChats()) {
      if (!this.requestedAvatars.has(meta.jid)) {
        this.requestedAvatars.add(meta.jid);
        this.avatarQueue.push(meta.jid);
      }
    }
    void this.drainAvatars();
  }

  private async drainAvatars() {
    if (this.avatarBusy) return;
    this.avatarBusy = true;
    try {
      while (this.avatarQueue.length > 0) {
        const jid = this.avatarQueue.shift()!;
        const img = await this.media.fetchAvatar(jid);
        if (img) {
          this.patchChatRowByJid(jid);
        } else if (!this.media.avatarResolved(jid)) {
          // transient failure — retry a few times, at the back of the queue
          const tries = (this.avatarTries.get(jid) ?? 0) + 1;
          this.avatarTries.set(jid, tries);
          if (tries < AVATAR_RETRIES) this.avatarQueue.push(jid);
        }
        await new Promise((r) => setTimeout(r, 200));
      }
    } finally {
      this.avatarBusy = false;
    }
  }

  private patchChatRowByJid(jid: string) {
    for (let i = 0; i < this.chatsModel.length; i++) {
      const row = this.chatsModel.rowData(i);
      if (row && row.jid === jid) {
        const meta = this.store.chats.get(jid);
        this.chatsModel.setRowData(
          i,
          this.toChatRow(
            jid,
            meta?.preview ?? row.preview,
            meta?.timestamp ?? 0,
            meta?.unread ?? 0,
          ),
        );
        break;
      }
    }
    if (jid === this.currentJid) this.applyHeaderAvatar(jid);
  }

  private applyHeaderAvatar(jid: string) {
    const name = this.store.chatName(jid);
    const avatar = this.media.avatarFor(jid);
    this.win.current_chat_name = name;
    this.win.current_avatar = avatar ?? EMPTY_IMAGE;
    this.win.current_avatar_has = !!avatar;
    this.win.current_initial = initialOf(name);
    this.win.current_color_idx = colorIdxOf(jid);
  }

  private openChatAt(index: number) {
    const row = this.chatsModel.rowData(index);
    if (!row) return;
    this.openJid(row.jid, index);
  }

  // Opens (or starts) the conversation with a jid; used by the chat list
  // and by clicking a sender name inside a group.
  openDm(jidRaw: string) {
    if (!jidRaw) return;
    const jid = this.store.canon(jidRaw);
    if (!this.store.chats.get(jid)) {
      this.store.chats.set(jid, { jid, preview: "", timestamp: 0 });
      this.refreshChats();
    }
    const index = this.visibleChats().findIndex((m) => m.jid === jid);
    this.openJid(jid, index);
  }

  private pendingForward: { jid: string; id: string } | null = null;

  private async handleForwardTo(index: number) {
    const target = this.chatsModel.rowData(index);
    const pending = this.pendingForward;
    this.pendingForward = null;
    this.win.forward_open = false;
    if (!target || !pending) return;
    const raw = this.store.messagesFor(pending.jid).find((m) => m.id === pending.id)?.raw;
    if (!raw) return;
    try {
      const sent = await this.service.sendForward(target.jid, raw);
      this.echoSent(sent, target.jid);
    } catch (err) {
      console.error("forward failed:", err);
    }
  }

  private openJid(jid: string, index: number) {
    this.currentJid = jid;
    this.win.selected_chat = index;
    const meta = this.store.chats.get(jid);
    if (meta) meta.unread = 0;
    this.win.current_status = "";
    void this.service.subscribePresence(jid);
    this.applyHeaderAvatar(jid);
    const list = this.store.messagesFor(jid);
    const rows = list.map((m, i) => this.toRow(m, i > 0 ? list[i - 1] : undefined));
    this.messagesModel.splice(0, this.messagesModel.length, ...rows);
    this.win.chat_open = true;
    this.scrollToEnd();
    this.scheduleRefreshChats();
    void this.loadMediaForChat(jid);
    // Thin conversation: ask the phone for this chat's older messages.
    const first = this.store.messagesFor(jid)[0];
    if (this.store.messagesFor(jid).length < 20 && first?.raw?.key && !this.historyPending) {
      this.setPending(true);
      void this.service.fetchOlderHistory(50, first.raw.key, first.timestamp).then((ok) => {
        if (!ok) this.setPending(false);
        else
          setTimeout(() => {
            if (this.historyPending) this.setPending(false);
          }, 20_000);
      });
    }
  }

  private scrollToEnd() {
    setTimeout(() => {
      try {
        this.win.scroll_conversation_end();
      } catch {
        // layout not ready yet — harmless
      }
    }, 60);
  }

  private toRow(m: StoredMessage, prev?: StoredMessage): MessageRow {
    const isGroup = m.jid.endsWith("@g.us");
    const firstOfRun =
      !prev || prev.fromMe !== m.fromMe || (isGroup && prev.sender !== m.sender);
    return {
      id: m.id,
      kind: m.deleted ? "text" : m.kind,
      text: m.deleted ? t("msg.deleted") : m.text,
      fromMe: m.fromMe,
      sender: m.sender,
      showSender: isGroup && !m.fromMe && firstOfRun,
      firstOfRun,
      time: formatTime(m.timestamp),
      picture: EMPTY_IMAGE,
      picW: 0,
      picH: 0,
      mediaPath: "",
      mediaReady: false,
      reactions: reactionSummary(m),
      dayLabel:
        !prev || formatDay(prev.timestamp) !== formatDay(m.timestamp)
          ? formatDay(m.timestamp)
          : "",
      ...ticksFor(m),
      senderJid:
        m.senderJid ??
        (m.raw?.key?.participant
          ? this.store.canon(jidNormalizedUser(m.raw.key.participant))
          : ""),
      forwarded: !!m.forwarded,
      deleted: !!m.deleted,
    };
  }

  private async loadMediaForChat(jid: string) {
    for (const stored of this.store.messagesFor(jid)) {
      if (this.currentJid !== jid) return;
      if (stored.kind === "text") continue;
      await this.loadMediaForMessage(stored);
    }
  }

  private async loadMediaForMessage(stored: StoredMessage) {
    if (!stored.raw) return;
    const jid = stored.jid;
    const path = await this.media.ensureCached(stored.id, stored.raw);
    if (this.currentJid !== jid) return;
    if (!path) {
      this.patchRow(stored.id, { text: t("media.unavailable"), kind: "text" });
      return;
    }
    if (stored.kind !== "image") {
      // audio and documents just need the cached file for open/play
      this.patchRow(stored.id, { mediaPath: path, mediaReady: true });
      return;
    }
    const img = await this.media.decodeImage(stored.id, path);
    if (this.currentJid !== jid) return;
    if (!img) {
      this.patchRow(stored.id, { text: t("media.imageUnavailable"), kind: "text" });
      return;
    }
    // WhatsApp-style thumbnail box: fit within 330x380, never upscale.
    const scale = Math.min(330 / img.displayW, 380 / img.displayH, 1);
    this.patchRow(stored.id, {
      picture: img,
      picW: Math.max(1, Math.round(img.displayW * scale)),
      picH: Math.max(1, Math.round(img.displayH * scale)),
      mediaPath: path,
      mediaReady: true,
    });
  }

  private patchRow(id: string, patch: Partial<MessageRow>) {
    for (let i = 0; i < this.messagesModel.length; i++) {
      const row = this.messagesModel.rowData(i);
      if (row && row.id === id) {
        this.messagesModel.setRowData(i, { ...row, ...patch });
        return;
      }
    }
  }

  private async handleSendText(text: string) {
    const body = text.trim();
    const jid = this.currentJid;
    if (!body || !jid) return;
    try {
      const sent = await this.service.sendText(jid, body);
      this.echoSent(sent, jid);
    } catch (err) {
      console.error("sendText failed:", err);
    }
  }

  private async handleAttach(kind: "image" | "audio" | "doc") {
    const jid = this.currentJid;
    if (!jid) return;
    const path = await this.media.pickFile(kind);
    if (!path || this.currentJid !== jid) return;
    if (kind === "image") {
      await this.openImagePreview(path);
      return;
    }
    try {
      const sent =
        kind === "audio"
          ? await this.service.sendAudio(jid, path)
          : await this.service.sendDocument(jid, path);
      this.echoSent(sent, jid);
    } catch (err) {
      console.error(`send ${kind} failed:`, err);
    }
  }

  private pendingImage: string | null = null;
  private lastPasteCheck = 0;

  // Fired on any Ctrl+V while a chat is open (the key event itself is
  // rejected so text pastes normally into the composer); only acts when
  // the clipboard holds an image.
  private async handlePaste() {
    const jid = this.currentJid;
    if (!jid || this.win.preview_open) return;
    const now = Date.now();
    if (now - this.lastPasteCheck < 400) return;
    this.lastPasteCheck = now;
    const img = await this.media.clipboardImage();
    if (img) await this.openImagePreview(img);
  }

  // WhatsApp-style confirmation: show the image with a caption field
  // before actually sending.
  private async openImagePreview(path: string) {
    const img = await this.media.decodePreview(path);
    if (!img) return;
    this.pendingImage = path;
    this.win.preview_image = img;
    this.win.preview_open = true;
  }

  private async confirmSendImage(caption: string) {
    const jid = this.currentJid;
    const path = this.pendingImage;
    this.pendingImage = null;
    this.win.preview_open = false;
    if (!jid || !path) return;
    try {
      const sent = await this.service.sendImage(jid, path, caption.trim() || undefined);
      this.echoSent(sent, jid);
    } catch (err) {
      console.error("send image failed:", err);
    }
  }

  // Feeds a just-sent message through the normal pipeline; the later
  // messages.upsert echo is deduplicated by id in the store.
  private echoSent(sent: WAMessage | null, jid: string) {
    if (!sent) return;
    const stored = this.ingest(sent);
    if (stored && jid === this.currentJid) {
      this.pushMessageRow(stored);
    }
    this.scheduleRefreshChats();
  }

  private async handlePairing(phone: string) {
    this.win.pairing_code = t("pairing.generating");
    try {
      this.win.pairing_code = await this.service.requestPairingCode(phone);
      this.win.status_text = t("status.pairingHint");
    } catch (err) {
      this.win.pairing_code = "";
      this.win.status_text = err instanceof Error ? err.message : t("pairing.failed");
    }
  }
}
