import { ArrayModel, StyledText } from "slint-ui";
import { jidNormalizedUser, proto } from "@whiskeysockets/baileys";
import type { WAMessage } from "@whiskeysockets/baileys";
import {
  Store,
  formatTime,
  formatDay,
  formatDuration,
  reactionSummary,
  ticksFor,
  displayId,
  formatNumber,
  isChannel,
} from "./store.ts";
import { Notify } from "./notify.ts";
import { hasMarkup, toMarkdown } from "./markup.ts";
import { t } from "./i18n.ts";
import type { Db } from "./db.ts";
import type { StoredMessage } from "./store.ts";
import { qrToImageData, EMPTY_IMAGE } from "./qr.ts";
import type { SlintImageData } from "./qr.ts";
import type { WAListener, WhatsAppService } from "./whatsapp.ts";
import type { MediaService } from "./media.ts";

interface CallRow {
  id: string;
  from: string;
  name: string;
  detail: string;
  time: string;
  avatar: SlintImageData;
  hasAvatar: boolean;
  initial: string;
  colorIdx: number;
  video: boolean;
}

interface StickerCell {
  id: string;
  pic: SlintImageData;
  ready: boolean;
}

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
  mentioned: boolean;
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
  senderAvatar: SlintImageData;
  senderHasAvatar: boolean;
  senderInitial: string;
  senderColorIdx: number;
  voiceJid: string;
  voiceAvatar: SlintImageData;
  voiceHasAvatar: boolean;
  voiceInitial: string;
  voiceColorIdx: number;
  groupIndent: boolean;
  showAvatar: boolean;
  senderNumber: string;
  senderLabel: string;
  sticker: boolean;
  gif: boolean;
  styled: unknown;
  hasStyled: boolean;
  linkTitle: string;
  linkDesc: string;
  linkHost: string;
  linkUrl: string;
  hasLink: boolean;
  linkThumb: SlintImageData;
  hasLinkThumb: boolean;
  linkThumbW: number;
  linkThumbH: number;
  wave: SlintImageData;
  hasWave: boolean;
  playing: boolean;
  progress: number;
  posLabel: string;
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
  statuses: ArrayModel<ChatRow>;
  emoji_rows: string[][];
  sticker_rows: ArrayModel<StickerCell[]>;
  fav_rows: ArrayModel<StickerCell[]>;
  gif_rows: ArrayModel<StickerCell[]>;
  gif_send: (id: string) => void;
  gif_search: (query: string) => void;
  gif_hint: string;
  fav_hint: string;
  conv_scroll: number;
  rec_active: boolean;
  rec_elapsed: string;
  rec_view_once: boolean;
  jump_latest: () => void;
  info_open: boolean;
  info_name: string;
  info_id: string;
  info_about: string;
  info_desc: string;
  info_is_group: boolean;
  info_members: string;
  info_archived: boolean;
  info_avatar: SlintImageData;
  info_has_avatar: boolean;
  info_initial: string;
  info_color_idx: number;
  info_media: ArrayModel<StickerCell[]>;
  open_info: () => void;
  close_info: () => void;
  toggle_archive: () => void;
  clear_chat: () => void;
  rec_start: () => void;
  rec_stop: () => void;
  rec_cancel: () => void;
  set_conversation_scroll: (y: number) => void;
  picker_open: boolean;
  emoji_pick: (e: string) => void;
  sticker_send: (id: string) => void;
  picker_opened: () => void;
  picker_closed: () => void;
  attach_sticker: () => void;
  sidebar_view: string;
  view_changed: (view: string) => void;
  calls: ArrayModel<CallRow>;
  call_ringing: boolean;
  call_name: string;
  call_detail: string;
  call_avatar: SlintImageData;
  call_has_avatar: boolean;
  call_initial: string;
  call_color_idx: number;
  decline_call: () => void;
  dismiss_call: () => void;
  status_open: (jid: string) => void;
  sv_open: boolean;
  sv_name: string;
  sv_time: string;
  sv_text: string;
  sv_has_image: boolean;
  sv_image: SlintImageData;
  sv_index: number;
  sv_count: number;
  status_next: () => void;
  status_prev: () => void;
  status_close: () => void;
  selected_jid: string;
  stick_bottom: boolean;
  conv_ready: boolean;
  conv_viewport_h: number;
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
  forward_to: (jid: string) => void;
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
  video_open: boolean;
  video_frame: SlintImageData;
  video_w: number;
  video_h: number;
  open_video: (id: string) => void;
  close_video: () => void;
  unlock: (pin: string) => void;
  save_pin: (current: string, next: string) => void;
  remove_pin: (current: string) => void;
  logout: () => void;
  request_pairing_code: (phone: string) => void;
  open_chat: (jid: string) => void;
  send_message: (text: string) => void;
  attach_image: () => void;
  attach_audio: () => void;
  play_audio: (path: string) => void;
  copy_text: (id: string) => void;
  audio_toggle: (id: string) => void;
  audio_seek: (id: string, frac: number) => void;
  audio_rate_label: string;
  jump_id: string;
  jump_report: (offset: number) => void;
  conv_list_h: number;
  audio_cycle_rate: () => void;
  mini_audio: boolean;
  mini_audio_name: string;
  mini_audio_avatar: SlintImageData;
  mini_audio_avatar_has: boolean;
  mini_audio_initial: string;
  mini_audio_color_idx: number;
  mini_audio_playing: boolean;
  mini_audio_progress: number;
  mini_audio_toggle: () => void;
  mini_audio_open: () => void;
  mini_audio_close: () => void;
  scroll_conversation_end: () => void;
  scroll_conversation_top: () => void;
  show(): void;
  run(): Promise<void>;
}

const AVATAR_RETRIES = 3;

function chunk<T>(arr: T[], size: number): T[][] {
  const out: T[][] = [];
  for (let i = 0; i < arr.length; i += size) out.push(arr.slice(i, i + size));
  return out;
}

// Curated emoji palette for the picker (variation selectors stripped —
// see cleanText — so every glyph renders in Slint).
const EMOJIS = (
  "😀 😃 😄 😁 😆 😅 🤣 😂 " +
  "🙂 😉 😊 😇 🥰 😍 🤩 😘 " +
  "😜 🤪 🤑 🤗 🤭 🤫 🤔 🤐 " +
  "🤨 😐 😑 😶 😏 😒 🙄 😬 " +
  "🤥 😌 😔 😪 🤤 😴 😷 🤒 " +
  "🤕 🤢 🤮 🤧 🥵 🥶 🥴 😵 " +
  "🤯 🤠 🥳 😎 🤓 🧐 😕 😟 " +
  "🙁 😮 😯 😲 😳 🥺 😦 😧 " +
  "😨 😰 😥 😢 😭 😱 😖 😣 " +
  "😞 😓 😩 😫 🥱 😤 😡 😠 " +
  "🤬 😈 👿 💀 💩 🤡 👹 👻 " +
  "👽 🤖 😺 😸 😹 😻 😼 😽 " +
  "🙌 👏 🤝 👍 👎 👊 ✊ 🤛 " +
  "🤜 🤞 ✌ 🤟 🤘 👌 🤏 👈 " +
  "👉 👆 👇 ☝ ✋ 🤚 🖐 🖖 " +
  "👋 🤙 💪 🙏 ❤ 🧡 💛 💚 " +
  "💙 💜 🖤 🤍 💔 💕 💞 💓 " +
  "💯 💥 💫 🔥 ⭐ 🌟 ⚡ 🎉 " +
  "🎈 🎁 🏆 ⚽ 🎮 🎵 ☕ 🍻 " +
  "🍕 🍔 🎂 🍫 🚀 ✈ 🚗 💰"
).split(" ");

// Scales media dimensions into the bubble's 330x380 box (stickers use a
// smaller square), falling back to a sane default when unknown.
function mediaBox(m: StoredMessage): { picW: number; picH: number } {
  if (m.kind !== "image" && m.kind !== "video") return { picW: 0, picH: 0 };
  const maxW = m.sticker ? 180 : 330;
  const maxH = m.sticker ? 180 : 380;
  const w = m.mediaW && m.mediaW > 0 ? m.mediaW : 260;
  const h = m.mediaH && m.mediaH > 0 ? m.mediaH : 200;
  const scale = Math.min(maxW / w, maxH / h, 1);
  return {
    picW: Math.max(1, Math.round(w * scale)),
    picH: Math.max(1, Math.round(h * scale)),
  };
}

// Every media kind gets a translated one-liner in notifications.
function notificationBody(m: StoredMessage): string {
  if (m.sticker) return t("preview.sticker");
  if (m.kind === "image") return t("preview.photo");
  if (m.kind === "audio") return t("preview.audio");
  if (m.kind === "doc") return t("preview.document", m.text);
  if (m.kind === "video") return m.gif ? t("preview.gif") : t("preview.video");
  return m.text;
}

// Formatted messages render as styled text; plain ones keep the
// selectable input so text can still be dragged and copied.
// A styled-text property never accepts null, so unformatted rows carry
// an empty instance instead.
const EMPTY_STYLED = StyledText.fromPlainText("");

function hostOf(url: string | undefined): string {
  if (!url) return "";
  try {
    return new URL(url).host.replace(/^www\./, "");
  } catch {
    return "";
  }
}

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
  private selfJid = "";
  private avatarQueue: string[] = [];
  private avatarBusy = false;

  private win: AppWindow;
  private media: MediaService;
  private db!: Db;
  private avatarTries = new Map<string, number>();
  private searchText = "";
  private tab = "all";
  private view = "chats"; // chats | channels | communities
  private notify = new Notify();
  private statusModel = new ArrayModel<ChatRow>([]);
  private stickerModel = new ArrayModel<StickerCell[]>([]);
  private gifModel = new ArrayModel<StickerCell[]>([]);
  private favModel = new ArrayModel<StickerCell[]>([]);
  private stickerRawById = new Map<string, StoredMessage>();
  private gifUrlById = new Map<string, string>();
  private scrollPos = new Map<string, number>();
  private infoMediaModel = new ArrayModel<StickerCell[]>([]);
  private callsModel = new ArrayModel<CallRow>([]);
  private ringing: { id: string; from: string } | null = null;
  private viewer: { items: StoredMessage[]; idx: number } | null = null;

  constructor(win: AppWindow, media: MediaService) {
    this.win = win;
    this.media = media;
    win.chats = this.chatsModel;
    win.messages = this.messagesModel;
    win.statuses = this.statusModel;
    win.status_open = (jid) => void this.openStatusViewer(jid);
    win.status_next = () => void this.stepStatus(1);
    win.status_prev = () => void this.stepStatus(-1);
    win.status_close = () => {
      this.viewer = null;
      win.sv_open = false;
    };
    win.emoji_rows = chunk(EMOJIS, 8);
    win.sticker_rows = this.stickerModel;
    win.gif_rows = this.gifModel;
    win.fav_rows = this.favModel;
    win.gif_send = (id) => void this.sendGifById(id);
    win.gif_search = (query) => void this.searchGifs(query);
    win.jump_latest = () => {
      if (this.currentJid) this.scrollPos.delete(this.currentJid);
      this.scrollToEnd();
    };
    win.info_media = this.infoMediaModel;
    win.open_info = () => void this.openContactInfo();
    win.close_info = () => {
      win.info_open = false;
    };
    win.toggle_archive = () => void this.toggleArchive();
    win.clear_chat = () => this.clearCurrentChat();
    win.rec_start = () => void this.startRecording();
    win.rec_stop = () => void this.stopRecording();
    win.rec_cancel = () => this.cancelRecording();
    win.emoji_pick = (e) => win.append_composer(e);
    win.picker_opened = () => void this.loadStickerPanel();
    win.picker_closed = () => this.stopPickerAnimations();
    win.sticker_send = (id) => void this.sendStickerById(id);
    win.attach_sticker = () => void this.handleAttachSticker();

    win.open_chat = (jid) => this.openDm(jid);
    win.search_changed = (text) => {
      this.searchText = text.trim().toLowerCase();
      this.refreshChats();
    };
    win.tab_changed = (tab) => {
      this.tab = tab;
      this.refreshChats();
    };
    win.calls = this.callsModel;
    win.decline_call = () => void this.declineCall();
    win.dismiss_call = () => {
      this.ringing = null;
      win.call_ringing = false;
    };
    win.view_changed = (view) => {
      this.view = view;
      this.refreshChats();
    };
    win.send_message = (text) => void this.handleSendText(text);
    win.attach_image = () => void this.handleAttach("image");
    win.attach_audio = () => void this.handleAttach("audio");
    win.attach_doc = () => void this.handleAttach("doc");
    win.load_older = () => this.handleScrollUpLoad();
    win.paste_clipboard = () => void this.handlePaste();
    win.open_dm = (target) => {
      // Styled-text links carry either a jid (mention) or a real URL.
      if (/^https?:/i.test(target)) this.media.openExternal(target);
      else this.openDm(target);
    };
    win.request_forward = (msgId) => {
      if (!this.currentJid) return;
      this.pendingForward = { jid: this.currentJid, id: msgId };
      win.forward_open = true;
    };
    win.forward_to = (jid) => void this.handleForwardTo(jid);
    win.confirm_send_image = (caption) => void this.confirmSendImage(caption);
    win.cancel_send_image = () => {
      this.pendingImage = null;
      win.preview_open = false;
    };
    win.play_audio = (path) => this.media.play(path);
    win.copy_text = (id) => {
      const jid = this.currentJid;
      const msg = jid ? this.store.messagesFor(jid).find((m) => m.id === id) : null;
      if (msg) void this.media.setClipboard(msg.text);
    };
    win.open_video = (id) => void this.handleVideoClick(id);
    win.close_video = () => {
      this.stopZoomLoop();
      this.media.stopVideo();
      win.video_open = false;
    };
    win.audio_toggle = (id) => void this.toggleAudio(id);
    win.audio_seek = (id, frac) => void this.seekAudio(id, frac);
    win.audio_cycle_rate = () => this.cycleAudioRate();
    win.mini_audio_toggle = () => void this.toggleAudio(this.audio?.id ?? "");
    win.mini_audio_open = () => {
      const a = this.audio;
      if (!a) return;
      this.openDm(a.jid);
      this.jumpToMessage(a.id);
    };
    win.jump_report = (offset) => this.onJumpReport(offset);
    win.mini_audio_close = () => this.stopAudio();
    win.request_pairing_code = (phone) => void this.handlePairing(phone);
  }

  setService(service: WhatsAppService) {
    this.service = service;
    this.notify.onActivate = (jid) => {
      try {
        this.win.show(); // raise from the tray
      } catch {
        // already visible
      }
      this.openDm(jid);
    };
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
    this.refreshStatuses();
    this.refreshCalls();
    void this.media.preloadAvatars((jid) => {
      this.patchChatRowByJid(jid);
      this.patchSenderAvatar(jid);
    });
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
    const ids = this.service.selfIds();
    this.store.setSelf(ids);
    // Our own picture rides along with the voice notes we sent.
    this.selfJid = ids[0] ? jidNormalizedUser(ids[0]) : "";
    if (this.selfJid) this.queueAvatar(this.selfJid);
    if (!this.groupsFetched) {
      this.groupsFetched = true;
      void this.fetchGroupNames();
    }
    const fullResync = this.db?.settingGet("appstate_seeded_v2") !== "1";
    void this.service.resyncAppState(fullResync).then(() => {
      if (fullResync) this.db?.settingSet("appstate_seeded_v2", "1");
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
    this.db?.settingSet("appstate_seeded_v2", "0");
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
    // Older messages may have arrived for the open conversation.
    if (addedToCurrent && this.currentJid) {
      if (this.scrollUpFetch) {
        this.prependOlderRows(this.currentJid);
      } else if (this.win.stick_bottom) {
        const list = this.store.messagesFor(this.currentJid);
        const rows = list.map((m, i) => this.toRow(m, i > 0 ? list[i - 1] : undefined));
        this.messagesModel.splice(0, this.messagesModel.length, ...rows);
        this.scrollToEnd();
        void this.loadMediaForChat(this.currentJid);
      }
      // else: data is stored; the view catches up on next open/scroll-up
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
      if (raw.key?.remoteJid === "status@broadcast") {
        if (this.store.addStatus(raw)) {
          this.refreshStatuses();
          this.scheduleSave();
        }
        continue;
      }
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
        if (meta) {
          meta.unread = (meta.unread ?? 0) + 1;
          if (stored.mentionsMe) meta.mentioned = true;
        }
        const body = notificationBody(stored);
        const isGroup = stored.jid.endsWith("@g.us");
        const title = this.store.chatName(stored.jid);
        const text = isGroup && stored.sender ? `${stored.sender}: ${body}` : body;
        const jid = stored.jid;
        void this.media
          .fetchAvatar(jid)
          .then(() => this.media.avatarIcon(jid))
          .then((icon) => this.notify.push(title, text, icon, jid));
      }
    }
    this.scheduleRefreshChats();
  }

  onMessagesUpdate(
    updates: {
      key?: WAMessage["key"];
      update?: { status?: unknown; starred?: unknown };
    }[],
  ) {
    for (const u of updates) {
      if (!u.key?.remoteJid || !u.key.id) continue;
      if (typeof u.update?.starred === "boolean") {
        this.store.setStarred(
          this.store.canon(jidNormalizedUser(u.key.remoteJid)),
          u.key.id,
          u.update.starred,
        );
      }
      const status = Number(u.update?.status ?? 0);
      if (!status) continue;
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
    // Don't yank the view down while the user is reading older messages.
    if (stored.fromMe || this.win.stick_bottom) this.scrollToEnd();
    if (stored.kind !== "text") void this.loadMediaForMessage(stored);
  }

  // ---- internals ----

  private async fetchGroupNames() {
    const groups = await this.service.fetchGroups();
    console.log(`[groups] fetched ${Object.keys(groups).length}`);
    let communities = 0;
    for (const [jid, meta] of Object.entries(groups)) {
      if (!meta?.subject) continue;
      // Seed the sidebar with participating groups even before any message.
      if (!this.store.chats.has(jid)) {
        this.store.chats.set(jid, { jid, name: meta.subject, preview: "", timestamp: 0 });
      }
      this.store.setName(jid, meta.subject);
      const entry = this.store.chats.get(jid);
      if (entry) {
        entry.isCommunity = !!meta.isCommunity;
        entry.community = meta.linkedParent ?? undefined;
        if (entry.isCommunity || entry.community) communities++;
      }
    }
    if (communities > 0) console.log(`[groups] ${communities} community-linked`);
    void this.resolveChannelNames();
    this.scheduleRefreshChats();
  }

  // Channels arrive as jids only; ask the server for their display names.
  private async resolveChannelNames() {
    for (const meta of this.store.sortedChats()) {
      if (!isChannel(meta.jid) || meta.name) continue;
      const name = await this.service.fetchChannelName(meta.jid);
      if (name) this.store.setName(meta.jid, name);
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

  private toChatRow(
    jid: string,
    preview: string,
    timestamp: number,
    unread: number,
    mentioned = false,
  ): ChatRow {
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
      mentioned: mentioned && unread > 0,
      pinned: (this.store.chats.get(jid)?.pinned ?? 0) > 0,
    };
  }

  private visibleChats() {
    return this.store.sortedChats().filter((meta) => {
      const channel = isChannel(meta.jid);
      if (this.view === "channels") {
        if (!channel) return false;
      } else if (this.view === "communities") {
        if (!meta.isCommunity && !meta.community) return false;
      } else {
        // Regular chat list: channels and community shells live in their
        // own tabs.
        if (channel || meta.isCommunity) return false;
      }
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

  // Maps a single @lid identity to its phone number using the mapping
  // Baileys stores in the auth state.
  private resolveLid(jid: string): string {
    if (!jid.endsWith("@lid") || !this.db) return jid;
    const alias = this.store.canon(jid);
    if (alias !== jid) return alias;
    const user = jid.split("@")[0];
    const v = this.db.get(`auth:lid-mapping-${user}_reverse`);
    if (!v) return jid;
    let pnUser: unknown;
    try {
      pnUser = JSON.parse(v);
    } catch {
      pnUser = v;
    }
    if (typeof pnUser !== "string" || !pnUser) return jid;
    const pn = `${pnUser}@s.whatsapp.net`;
    this.store.learnAlias(jid, pn);
    return pn;
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

  // Fills the sticker tab with recently received stickers (lazy decode).
  private async loadStickerPanel() {
    this.stopPickerAnimations();
    void this.searchGifs("");
    const favs = this.store.starredStickers(32);
    this.win.fav_hint = favs.length === 0 ? t("fav.empty") : "";
    void this.fillPanel(this.favModel, favs, false);
    const items = this.store.recentStickers(32);
    await this.fillPanel(this.stickerModel, items, false);
  }

  // Picker cells animate too: frames are cycled by a shared ticker.
  private pickerAnim = new Map<
    string,
    { model: ArrayModel<StickerCell[]>; row: number; col: number; frames: SlintImageData[]; idx: number }
  >();
  private pickerTimer: NodeJS.Timeout | null = null;

  private ensurePickerTicker() {
    if (this.pickerTimer) return;
    this.pickerTimer = setInterval(() => {
      if (this.pickerAnim.size === 0) {
        clearInterval(this.pickerTimer!);
        this.pickerTimer = null;
        return;
      }
      for (const anim of this.pickerAnim.values()) {
        anim.idx = (anim.idx + 1) % anim.frames.length;
        const row = anim.model.rowData(anim.row);
        if (!row) continue;
        const copy = [...row];
        const cell = copy[anim.col];
        if (!cell) continue;
        copy[anim.col] = { ...cell, pic: anim.frames[anim.idx]!, ready: true };
        anim.model.setRowData(anim.row, copy);
      }
    }, 110);
  }

  private stopPickerAnimations() {
    this.pickerAnim.clear();
    if (this.pickerTimer) clearInterval(this.pickerTimer);
    this.pickerTimer = null;
  }

  private registerPickerCell(
    model: ArrayModel<StickerCell[]>,
    id: string,
    frames: SlintImageData[],
  ) {
    if (frames.length < 2) return;
    for (let r = 0; r < model.length; r++) {
      const row = model.rowData(r);
      if (!row) continue;
      const col = row.findIndex((c) => c.id === id);
      if (col >= 0) {
        this.pickerAnim.set(id, { model, row: r, col, frames, idx: 0 });
        this.ensurePickerTicker();
        return;
      }
    }
  }

  // Populates a picker grid; GIF cells use the embedded video thumbnail
  // instead of downloading the whole clip.
  private async fillPanel(
    model: ArrayModel<StickerCell[]>,
    items: StoredMessage[],
    fromThumbnail: boolean,
  ) {
    const cells: StickerCell[] = [];
    for (const m of items) {
      this.stickerRawById.set(m.id, m);
      cells.push({ id: m.id, pic: EMPTY_IMAGE, ready: false });
    }
    model.splice(0, model.length, ...chunk(cells, 4));
    for (const m of items) {
      let img = null;
      if (fromThumbnail) {
        const thumb = m.raw?.message?.videoMessage?.jpegThumbnail;
        if (thumb?.length) img = await this.media.decodeRaw(Buffer.from(thumb));
      } else {
        const path = await this.media.ensureCached(m.id, m.raw!);
        if (path) {
          const frames = await this.media.stickerFrames(m.id, path);
          if (frames.length > 0) {
            img = frames[0]!;
            setTimeout(() => this.registerPickerCell(model, m.id, frames), 0);
          } else {
            img = await this.media.decodeImage(m.id, path);
          }
        }
      }
      if (!img) continue;
      outer: for (let r = 0; r < model.length; r++) {
        const row = model.rowData(r);
        if (!row) continue;
        for (let c = 0; c < row.length; c++) {
          if (row[c]!.id === m.id) {
            const copy = [...row];
            copy[c] = { id: m.id, pic: img, ready: true };
            model.setRowData(r, copy);
            break outer;
          }
        }
      }
    }
  }

  // GIF picker: keyless search through Openverse.
  private async searchGifs(query: string) {
    this.win.gif_hint = "";
    const results = (await this.media.openverse(query)).map((r) => ({
      id: r.id,
      preview: r.preview,
      mp4: r.gif,
    }));
    this.gifUrlById.clear();
    const cells: StickerCell[] = [];
    for (const g of results) {
      this.gifUrlById.set(g.id, g.mp4);
      cells.push({ id: g.id, pic: EMPTY_IMAGE, ready: false });
    }
    this.gifModel.splice(0, this.gifModel.length, ...chunk(cells, 4));
    if (results.length === 0) this.win.gif_hint = t("gif.noResults");
    for (const g of results) {
      const frames = await this.media.previewFrames(g.preview);
      const img = frames[0] ?? (await this.media.decodeUrl(g.preview));
      if (!img) continue;
      if (frames.length > 1) {
        setTimeout(() => this.registerPickerCell(this.gifModel, g.id, frames), 0);
      }
      outer: for (let r = 0; r < this.gifModel.length; r++) {
        const row = this.gifModel.rowData(r);
        if (!row) continue;
        for (let c = 0; c < row.length; c++) {
          if (row[c]!.id === g.id) {
            const copy = [...row];
            copy[c] = { id: g.id, pic: img, ready: true };
            this.gifModel.setRowData(r, copy);
            break outer;
          }
        }
      }
    }
  }

  private async sendGifById(id: string) {
    const jid = this.currentJid;
    if (!jid) return;
    const url = this.gifUrlById.get(id);
    if (!url) {
      // history GIF: forward the original message
      await this.sendStickerById(id);
      return;
    }
    const isGif = url.toLowerCase().includes(".gif");
    const downloaded = await this.media.downloadTemp(url, isGif ? "gif" : "mp4");
    if (!downloaded || this.currentJid !== jid) return;
    // WhatsApp needs mp4 for gif playback.
    const path = isGif ? await this.media.gifToMp4(downloaded) : downloaded;
    if (!path || this.currentJid !== jid) return;
    try {
      this.echoSent(await this.service.sendGif(jid, path), jid);
    } catch (err) {
      console.error("gif send failed:", err);
    }
  }

  private async sendStickerById(id: string) {
    const jid = this.currentJid;
    const m = this.stickerRawById.get(id);
    if (!jid || !m?.raw) return;
    try {
      this.echoSent(await this.service.sendForward(jid, m.raw), jid);
    } catch (err) {
      console.error("sticker send failed:", err);
    }
  }

  // ---- In-app video player ----

  // A resting GIF replays on click; a playing one opens full screen.
  private async handleVideoClick(id: string) {
    const jid = this.currentJid;
    const stored = jid ? this.store.messagesFor(jid).find((m) => m.id === id) : null;
    if (stored?.gif && !this.animated.has(id)) {
      const frames = this.media.cachedFrames(id);
      if (frames.length > 1) {
        this.animated.set(id, { frames, idx: 0, loop: false });
        this.ensureStickerTicker();
        return;
      }
    }
    await this.openVideo(id);
  }

  // GIFs zoom instantly: the frames decoded for the bubble are shown at
  // once, then swapped for a sharper set as soon as it finishes.
  private zoomTimer: NodeJS.Timeout | null = null;

  private startZoomLoop(frames: SlintImageData[]) {
    this.stopZoomLoop();
    if (frames.length === 0) return;
    const first = frames[0]!;
    this.win.video_w = first.width;
    this.win.video_h = first.height;
    this.win.video_frame = first;
    if (frames.length < 2) return;
    let idx = 0;
    this.zoomTimer = setInterval(() => {
      idx = (idx + 1) % frames.length;
      this.win.video_frame = frames[idx]!;
    }, 66);
  }

  private stopZoomLoop() {
    if (this.zoomTimer) clearInterval(this.zoomTimer);
    this.zoomTimer = null;
  }

  private async openVideo(id: string) {
    const jid = this.currentJid;
    const stored = jid ? this.store.messagesFor(jid).find((m) => m.id === id) : null;
    if (!stored?.raw) return;
    const path = await this.media.ensureCached(id, stored.raw);
    if (!path || this.currentJid !== jid) return;

    if (stored.gif) {
      this.win.video_open = true;
      this.startZoomLoop(this.media.cachedFrames(id));
      const sharper = await this.media.videoFrames(id, path, 720);
      if (this.win.video_open && sharper.length > 0) this.startZoomLoop(sharper);
      return;
    }
    this.win.video_open = true;
    // GIFs keep looping while zoomed and carry no audio track.
    const loop = !!stored.gif;
    const start = async (): Promise<boolean> => {
      const size = await this.media.playVideo(
        id,
        path,
        (img) => {
          this.win.video_frame = img;
        },
        () => {
          if (loop && this.win.video_open) void start();
          else this.win.video_open = false;
        },
        !loop,
      );
      if (!size) return false;
      this.win.video_w = size.width;
      this.win.video_h = size.height;
      return true;
    };
    if (!(await start())) this.win.video_open = false;
  }

  // ---- Animated stickers ----
  // Slint images are static buffers, so animation means swapping frames
  // on a shared ticker while the conversation is open.

  private animated = new Map<
    string,
    { frames: SlintImageData[]; idx: number; loop: boolean }
  >();
  private stickerTimer: NodeJS.Timeout | null = null;

  // Animating too many clips at once wastes CPU; keep the newest few.
  private static readonly MAX_ANIMATIONS = 6;

  private ensureStickerTicker() {
    while (this.animated.size > Bridge.MAX_ANIMATIONS) {
      const oldest = this.animated.keys().next().value;
      if (oldest === undefined) break;
      this.animated.delete(oldest);
    }
    if (this.stickerTimer) return;
    this.stickerTimer = setInterval(() => {
      if (this.animated.size === 0) {
        clearInterval(this.stickerTimer!);
        this.stickerTimer = null;
        return;
      }
      for (const [id, anim] of this.animated) {
        const next = anim.idx + 1;
        if (next >= anim.frames.length && !anim.loop) {
          // GIFs play once and rest on their first frame, like WhatsApp.
          this.animated.delete(id);
          this.patchRow(id, { picture: anim.frames[0]!, playing: false });
          continue;
        }
        anim.idx = next % anim.frames.length;
        this.patchRow(id, { picture: anim.frames[anim.idx]!, playing: true });
      }
    }, 66); // ~15 fps, matching the extracted frame rate
  }

  private stopStickerAnimations() {
    this.animated.clear();
    if (this.stickerTimer) clearInterval(this.stickerTimer);
    this.stickerTimer = null;
  }

  // ---- In-bubble audio player ----

  private audio: {
    id: string;
    jid: string;
    path: string;
    duration: number;
    pos: number;
    paused: boolean;
    timer: NodeJS.Timeout | null;
  } | null = null;

  // WhatsApp cycles 1x/1.5x/2x; we carry on to 3x.
  private static readonly RATES = [1, 1.5, 2, 2.5, 3];
  private rateIdx = 0;

  private get audioRate(): number {
    return Bridge.RATES[this.rateIdx] ?? 1;
  }

  private cycleAudioRate() {
    this.rateIdx = (this.rateIdx + 1) % Bridge.RATES.length;
    this.win.audio_rate_label = this.audioRate + "x";
    // Restart the running note so the new speed takes effect right away.
    const a = this.audio;
    if (a && !a.paused) void this.startAudio(a.id, a.pos, a.jid);
  }

  private async toggleAudio(id: string) {
    const a = this.audio;
    if (a?.id === id) {
      if (a.paused) await this.startAudio(id, a.pos, a.jid);
      else this.pauseAudio();
      return;
    }
    await this.startAudio(id, 0);
  }

  private async seekAudio(id: string, frac: number) {
    const clamped = Math.max(0, Math.min(1, frac));
    const jid = this.audio?.id === id ? this.audio.jid : this.currentJid;
    const stored = jid ? this.store.messagesFor(jid).find((m) => m.id === id) : null;
    await this.startAudio(id, (stored?.durationSec ?? 0) * clamped, jid ?? undefined);
  }

  private async startAudio(id: string, posSec: number, fromJid?: string) {
    const jid = fromJid ?? this.currentJid;
    const stored = jid ? this.store.messagesFor(jid).find((m) => m.id === id) : null;
    if (!stored?.raw || !jid) return;
    const path = await this.media.ensureCached(id, stored.raw);
    if (!path) return;
    this.clearAudio();

    const duration = stored.durationSec && stored.durationSec > 0 ? stored.durationSec : 0;
    const pos = Math.max(0, posSec);
    if (!(await this.media.playAudio(id, path, pos, this.audioRate))) return;

    const timer = setInterval(() => {
      const a = this.audio;
      if (!a || a.paused) return;
      a.pos += 0.25 * this.audioRate;
      if (a.duration > 0 && a.pos >= a.duration) {
        this.stopAudio();
        return;
      }
      this.patchAudioRow(a);
    }, 250);
    this.audio = { id, jid, path, duration, pos, paused: false, timer };
    this.patchAudioRow(this.audio);
  }

  // Keeps the position so the next click resumes where it stopped.
  private pauseAudio() {
    const a = this.audio;
    if (!a) return;
    this.media.stopAudio();
    if (a.timer) clearInterval(a.timer);
    a.timer = null;
    a.paused = true;
    this.patchAudioRow(a);
  }

  private clearAudio() {
    const a = this.audio;
    this.audio = null;
    this.media.stopAudio();
    if (!a) return;
    if (a.timer) clearInterval(a.timer);
    this.patchRow(a.id, { playing: false, progress: 0, posLabel: "" });
  }

  private stopAudio() {
    this.clearAudio();
    this.win.mini_audio = false;
  }

  private patchAudioRow(a: {
    id: string;
    jid: string;
    duration: number;
    pos: number;
    paused: boolean;
  }) {
    const progress = a.duration > 0 ? Math.min(1, a.pos / a.duration) : 0;
    this.patchRow(a.id, {
      playing: !a.paused,
      progress,
      posLabel: formatDuration(a.pos),
    });
    // Playback survives leaving the chat: a mini player keeps it reachable.
    const away = a.jid !== this.currentJid;
    this.win.mini_audio = away;
    this.win.mini_audio_playing = !a.paused;
    this.win.mini_audio_progress = progress;
    if (away) {
      const name = this.store.chatName(a.jid);
      const avatar = this.media.avatarFor(a.jid);
      this.win.mini_audio_name = name;
      this.win.mini_audio_avatar = avatar ?? EMPTY_IMAGE;
      this.win.mini_audio_avatar_has = !!avatar;
      this.win.mini_audio_initial = initialOf(name);
      this.win.mini_audio_color_idx = colorIdxOf(a.jid);
    }
  }

  // Lazily renders the waveform for a voice message.
  private async loadWaveform(stored: StoredMessage, path: string) {
    const img = await this.media.waveform(stored.id, path);
    if (img && this.currentJid === stored.jid) {
      this.patchRow(stored.id, { wave: img, hasWave: true });
    }
  }

  // ---- Calls ----

  // Baileys reports call state but cannot answer; Zapive shows who is
  // calling (raising the window even from the tray), lets the user
  // decline, and keeps a history list.
  onCall(events: unknown[]) {
    for (const raw of events) {
      const ev = raw as {
        id?: string;
        from?: string;
        status?: string;
        isVideo?: boolean;
        isGroup?: boolean;
        date?: Date;
      };
      if (!ev.id || !ev.from || !ev.status) continue;
      const entry = this.store.upsertCall(ev as never);
      if (!entry) continue;

      if (ev.status === "offer") {
        const name = this.store.chatName(entry.jid);
        const detail = entry.video ? t("call.incomingVideo") : t("call.incomingVoice");
        this.ringing = { id: entry.id, from: ev.from };
        this.win.call_name = name;
        this.win.call_detail = detail;
        this.applyCallAvatar(entry.jid);
        this.win.call_ringing = true;
        try {
          this.win.show(); // bring the window back from the tray
        } catch {
          // window already visible
        }
        void this.media
          .fetchAvatar(entry.jid)
          .then(() => this.media.avatarIcon(entry.jid))
          .then((icon) => this.notify.push(name, detail, icon));
      } else if (this.ringing?.id === entry.id) {
        this.ringing = null;
        this.win.call_ringing = false;
      }
      this.refreshCalls();
      this.scheduleSave();
    }
  }

  private applyCallAvatar(jid: string) {
    const avatar = this.media.avatarFor(jid);
    const name = this.store.chatName(jid);
    this.win.call_avatar = avatar ?? EMPTY_IMAGE;
    this.win.call_has_avatar = !!avatar;
    this.win.call_initial = initialOf(name);
    this.win.call_color_idx = colorIdxOf(jid);
  }

  private async declineCall() {
    const call = this.ringing;
    this.ringing = null;
    this.win.call_ringing = false;
    if (call) await this.service.rejectCall(call.id, call.from);
  }

  private callStatusLabel(status: string): string {
    if (status === "accept") return t("call.answered");
    if (status === "reject") return t("call.declined");
    if (status === "timeout") return t("call.missed");
    if (status === "offer") return t("call.ringing");
    return t("call.ended");
  }

  private refreshCalls() {
    const rows = this.store.calls.slice(0, 60).map((c) => {
      const name = this.store.chatName(c.jid);
      const avatar = this.media.avatarFor(c.jid);
      if (!this.requestedAvatars.has(c.jid)) {
        this.requestedAvatars.add(c.jid);
        this.avatarQueue.push(c.jid);
        void this.drainAvatars();
      }
      return {
        id: c.id,
        from: c.jid,
        name,
        detail: `${c.video ? t("call.video") : t("call.voice")} · ${this.callStatusLabel(c.status)}`,
        time: formatTime(c.timestamp),
        avatar: avatar ?? EMPTY_IMAGE,
        hasAvatar: !!avatar,
        initial: initialOf(name),
        colorIdx: colorIdxOf(c.jid),
        video: c.video,
      };
    });
    this.callsModel.splice(0, this.callsModel.length, ...rows);
  }

  // ---- Contact / group info panel ----

  private async openContactInfo() {
    const jid = this.currentJid;
    if (!jid) return;
    const isGroup = jid.endsWith("@g.us");
    const name = this.store.chatName(jid);
    const avatar = this.media.avatarFor(jid);
    this.win.info_name = name;
    this.win.info_id = isGroup ? "" : displayId(jid);
    this.win.info_is_group = isGroup;
    this.win.info_about = "";
    this.win.info_desc = "";
    this.win.info_members = "";
    this.win.info_archived = !!this.store.chats.get(jid)?.archived;
    this.win.info_avatar = avatar ?? EMPTY_IMAGE;
    this.win.info_has_avatar = !!avatar;
    this.win.info_initial = initialOf(name);
    this.win.info_color_idx = colorIdxOf(jid);
    this.infoMediaModel.splice(0, this.infoMediaModel.length);
    this.win.info_open = true;

    // Shared media: reuse the thumbnails already cached for the chat.
    const shots = this.store
      .messagesFor(jid)
      .filter((m) => m.kind === "image" && m.raw)
      .slice(-12)
      .reverse();
    void this.fillPanel(this.infoMediaModel, shots, false);

    if (isGroup) {
      const meta = await this.service.fetchGroupInfo(jid);
      if (this.currentJid !== jid) return;
      this.win.info_desc = meta?.desc ?? "";
      const count = meta?.participants?.length ?? 0;
      if (count > 0) this.win.info_members = t("info.members", count);
    } else {
      const about = await this.service.fetchAbout(jid);
      if (this.currentJid === jid) this.win.info_about = about;
    }
  }

  private async toggleArchive() {
    const jid = this.currentJid;
    const meta = jid ? this.store.chats.get(jid) : null;
    if (!jid || !meta) return;
    const next = !meta.archived;
    meta.archived = next;
    this.win.info_archived = next;
    this.refreshChats();
    const last = this.store.messagesFor(jid).at(-1)?.raw;
    await this.service.setArchived(jid, next, last);
  }

  // Drops the locally stored history for this conversation.
  private clearCurrentChat() {
    const jid = this.currentJid;
    if (!jid) return;
    this.store.messages.set(jid, []);
    this.store.dirtyJids.add(jid);
    const meta = this.store.chats.get(jid);
    if (meta) meta.preview = "";
    this.messagesModel.splice(0, this.messagesModel.length);
    this.win.info_open = false;
    this.refreshChats();
    this.scheduleSave();
  }

  // ---- Voice recording ----

  private recTimer: NodeJS.Timeout | null = null;
  private recStartedAt = 0;

  private async startRecording() {
    if (!this.currentJid || this.media.recording) return;
    const ok = await this.media.startRecording();
    if (!ok) {
      this.win.status_text = t("rec.noMic");
      return;
    }
    this.recStartedAt = Date.now();
    this.win.rec_active = true;
    this.win.rec_elapsed = "0:00";
    this.recTimer = setInterval(() => {
      const secs = Math.floor((Date.now() - this.recStartedAt) / 1000);
      this.win.rec_elapsed = `${Math.floor(secs / 60)}:${String(secs % 60).padStart(2, "0")}`;
    }, 500);
  }

  private stopTimer() {
    if (this.recTimer) clearInterval(this.recTimer);
    this.recTimer = null;
    this.win.rec_active = false;
  }

  private async stopRecording() {
    const jid = this.currentJid;
    this.stopTimer();
    const path = await this.media.stopRecording();
    if (!path || !jid) return;
    try {
      const sent = await this.service.sendVoice(jid, path, this.win.rec_view_once);
      this.echoSent(sent, jid);
    } catch (err) {
      console.error("voice send failed:", err);
    }
  }

  private cancelRecording() {
    this.stopTimer();
    this.media.cancelRecording();
  }

  private async handleAttachSticker() {
    const jid = this.currentJid;
    if (!jid) return;
    const src = await this.media.pickFile("image");
    if (!src || this.currentJid !== jid) return;
    const webp = await this.media.toWebpSticker(src);
    if (!webp) return;
    try {
      this.echoSent(await this.service.sendSticker(jid, webp), jid);
    } catch (err) {
      console.error("sticker create failed:", err);
    }
  }

  private refreshStatuses() {
    const rows = this.store.statusAuthors().map((a) => {
      const name = this.store.chatName(a.jid);
      const avatar = this.media.avatarFor(a.jid);
      if (!this.requestedAvatars.has(a.jid)) {
        this.requestedAvatars.add(a.jid);
        this.avatarQueue.push(a.jid);
        void this.drainAvatars();
      }
      return {
        jid: a.jid,
        name,
        preview:
          a.latest.kind === "image" && a.latest.text === ""
            ? t("preview.photo")
            : a.latest.text,
        time: formatTime(a.latest.timestamp),
        avatar: avatar ?? EMPTY_IMAGE,
        hasAvatar: !!avatar,
        initial: initialOf(name),
        colorIdx: colorIdxOf(a.jid),
        unread: a.count,
        mentioned: false,
        pinned: false,
      };
    });
    this.statusModel.splice(0, this.statusModel.length, ...rows);
  }

  private async openStatusViewer(jid: string) {
    const items = this.store.statuses.get(jid);
    if (!items || items.length === 0) return;
    this.viewer = { items, idx: 0 };
    this.win.sv_open = true;
    await this.loadViewerItem();
  }

  private async stepStatus(delta: number) {
    if (!this.viewer) return;
    const next = this.viewer.idx + delta;
    if (next < 0 || next >= this.viewer.items.length) {
      if (next >= this.viewer.items.length) {
        this.viewer = null;
        this.win.sv_open = false;
      }
      return;
    }
    this.viewer.idx = next;
    await this.loadViewerItem();
  }

  private async loadViewerItem() {
    const v = this.viewer;
    if (!v) return;
    const item = v.items[v.idx]!;
    this.win.sv_name = this.store.chatName(item.jid);
    this.win.sv_time = formatTime(item.timestamp);
    this.win.sv_text = item.text;
    this.win.sv_index = v.idx + 1;
    this.win.sv_count = v.items.length;
    this.win.sv_has_image = false;
    if (item.kind !== "image" || !item.raw?.message) return;
    let img = null;
    const content = item.raw.message;
    if (content.imageMessage) {
      const path = await this.media.ensureCached(`status_${item.id}`, item.raw);
      if (path) img = await this.media.decodeImage(`status_${item.id}`, path);
      if (!img && content.imageMessage.jpegThumbnail?.length) {
        img = await this.media.decodeRaw(Buffer.from(content.imageMessage.jpegThumbnail));
      }
    } else if (content.videoMessage?.jpegThumbnail?.length) {
      img = await this.media.decodeRaw(Buffer.from(content.videoMessage.jpegThumbnail));
    }
    if (this.viewer !== v || v.items[v.idx] !== item) return;
    if (img) {
      this.win.sv_image = img;
      this.win.sv_has_image = true;
    }
  }

  // Updates the chat list model in place (setRowData + push/remove) so the
  // ListView keeps its scroll position instead of resetting on refresh.
  private refreshChats() {
    this.resolveLidAliases();
    const rows = this.visibleChats().map((meta) =>
      this.toChatRow(
        meta.jid,
        meta.preview,
        meta.timestamp,
        meta.unread ?? 0,
        !!meta.mentioned,
      ),
    );
    const model = this.chatsModel;
    const common = Math.min(model.length, rows.length);
    for (let i = 0; i < common; i++) {
      const cur = model.rowData(i);
      const next = rows[i]!;
      if (
        !cur ||
        cur.jid !== next.jid ||
        cur.name !== next.name ||
        cur.preview !== next.preview ||
        cur.time !== next.time ||
        cur.unread !== next.unread ||
        cur.mentioned !== next.mentioned ||
        cur.pinned !== next.pinned ||
        cur.hasAvatar !== next.hasAvatar ||
        cur.avatar !== next.avatar
      ) {
        model.setRowData(i, next);
      }
    }
    if (rows.length > model.length) {
      model.push(...rows.slice(model.length));
    } else if (rows.length < model.length) {
      model.remove(rows.length, model.length - rows.length);
    }
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

  // Three workers keep the list filling quickly without hammering the
  // server; the queue is already ordered by chat recency.
  private async drainAvatars() {
    if (this.avatarBusy) return;
    this.avatarBusy = true;
    const worker = async () => {
      while (this.avatarQueue.length > 0) {
        const jid = this.avatarQueue.shift()!;
        const img = await this.media.fetchAvatar(jid);
        if (img) {
          this.patchChatRowByJid(jid);
          this.patchSenderAvatar(jid);
        } else if (!this.media.avatarResolved(jid)) {
          // transient failure — retry a few times, at the back of the queue
          const tries = (this.avatarTries.get(jid) ?? 0) + 1;
          this.avatarTries.set(jid, tries);
          if (tries < AVATAR_RETRIES) this.avatarQueue.push(jid);
        }
        await new Promise((r) => setTimeout(r, 60));
      }
    };
    try {
      await Promise.all([worker(), worker(), worker()]);
    } finally {
      this.avatarBusy = false;
    }
  }

  // Redraws message rows whose sender avatar just finished downloading.
  private patchSenderAvatar(jid: string) {
    const avatar = this.media.avatarFor(jid);
    if (!avatar) return;
    for (let i = 0; i < this.messagesModel.length; i++) {
      const row = this.messagesModel.rowData(i);
      if (row && row.voiceJid === jid && !row.voiceHasAvatar) {
        this.messagesModel.setRowData(i, {
          ...row,
          voiceAvatar: avatar,
          voiceHasAvatar: true,
        });
      }
      if (row && row.senderJid === jid && !row.senderHasAvatar) {
        this.messagesModel.setRowData(i, {
          ...row,
          senderAvatar: avatar,
          senderHasAvatar: true,
        });
      }
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
            !!meta?.mentioned,
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

  // Opens (or starts) the conversation with a jid; used by the chat list
  // and by clicking a sender name inside a group.
  openDm(jidRaw: string) {
    if (!jidRaw) return;
    const jid = this.store.canon(jidRaw);
    if (!this.store.chats.get(jid)) {
      this.store.chats.set(jid, { jid, preview: "", timestamp: 0 });
      this.refreshChats();
    }
    this.openJid(jid);
  }

  private pendingForward: { jid: string; id: string } | null = null;

  private async handleForwardTo(targetJid: string) {
    const pending = this.pendingForward;
    this.pendingForward = null;
    this.win.forward_open = false;
    if (!targetJid || !pending) return;
    const target = { jid: targetJid };
    const raw = this.store.messagesFor(pending.jid).find((m) => m.id === pending.id)?.raw;
    if (!raw) return;
    try {
      const sent = await this.service.sendForward(target.jid, raw);
      this.echoSent(sent, target.jid);
    } catch (err) {
      console.error("forward failed:", err);
    }
  }

  private openJid(jid: string) {
    // Playback follows the user: the mini player takes over the controls.
    if (this.audio) this.patchAudioRow(this.audio);
    this.stopStickerAnimations();
    this.stopZoomLoop();
    this.media.stopVideo();
    this.win.video_open = false;
    if (this.currentJid && this.currentJid !== jid) {
      // Someone reading at the bottom expects the newest message next
      // time, not the offset that happened to be the end back then.
      if (this.win.stick_bottom) this.scrollPos.delete(this.currentJid);
      else this.scrollPos.set(this.currentJid, this.win.conv_scroll);
    }
    this.currentJid = jid;
    this.win.selected_jid = jid;
    this.win.stick_bottom = true;
    const meta = this.store.chats.get(jid);
    if (meta) {
      meta.unread = 0;
      meta.mentioned = false;
    }
    this.win.current_status = "";
    void this.service.subscribePresence(jid);
    this.applyHeaderAvatar(jid);
    const list = this.store.messagesFor(jid);
    const rows = list.map((m, i) => this.toRow(m, i > 0 ? list[i - 1] : undefined));
    this.messagesModel.splice(0, this.messagesModel.length, ...rows);
    this.win.chat_open = true;
    this.win.conv_ready = false;
    const saved = this.scrollPos.get(jid);
    // Anchor across a few layout passes while hidden, then reveal.
    for (const delay of [0, 40, 90]) {
      setTimeout(() => {
        try {
          if (saved !== undefined && saved < 0) {
            this.win.set_conversation_scroll(saved);
          } else {
            this.win.scroll_conversation_end();
          }
        } catch {
          // layout not ready yet
        }
      }, delay);
    }
    setTimeout(() => {
      this.win.conv_ready = true;
    }, 120);
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

  // Bubbles keep growing after the first layout pass (images and wrapped
  // text resolve late), so re-apply the jump until the viewport settles.
  // Inserts freshly fetched history above the current rows and shifts the
  // viewport by exactly the height that was added, so the message the
  // user was reading stays put.
  private prependOlderRows(jid: string) {
    const list = this.store.messagesFor(jid);
    const firstShown = this.messagesModel.rowData(0)?.id;
    const cut = firstShown ? list.findIndex((m) => m.id === firstShown) : -1;
    if (cut <= 0) return;

    const beforeY = this.win.conv_scroll;
    const beforeH = this.win.conv_viewport_h;
    this.win.conv_ready = false;

    const older = list
      .slice(0, cut)
      .map((m, i) => this.toRow(m, i > 0 ? list[i - 1] : undefined));
    this.messagesModel.splice(0, 0, ...older);
    // The row that used to be first now has a predecessor.
    const boundary = this.messagesModel.rowData(older.length);
    if (boundary) {
      this.messagesModel.setRowData(older.length, this.toRow(list[cut]!, list[cut - 1]));
    }

    setTimeout(() => {
      try {
        const added = this.win.conv_viewport_h - beforeH;
        this.win.set_conversation_scroll(beforeY - added);
      } catch {
        // layout not ready
      }
      this.win.conv_ready = true;
      void this.loadMediaForChat(jid);
    }, 40);
  }

  // Brings a message into view. The list only instantiates the rows
  // around the viewport, so we guess a position from the average row
  // height, let the row report where it landed, and correct from there.
  private jump: { id: string; tries: number; timer: NodeJS.Timeout | null } | null =
    null;

  private jumpToMessage(id: string) {
    this.cancelJump();
    this.jump = { id, tries: 0, timer: null };
    setTimeout(() => this.jumpStep(true), 160);
  }

  private cancelJump() {
    if (this.jump?.timer) clearTimeout(this.jump.timer);
    this.jump = null;
    try {
      this.win.jump_id = "";
    } catch {
      // window gone
    }
  }

  private jumpStep(guess: boolean) {
    const j = this.jump;
    if (!j) return;
    if (j.tries >= 6) {
      this.cancelJump();
      return;
    }
    j.tries++;
    if (guess) {
      const count = this.messagesModel.length;
      let idx = -1;
      for (let i = 0; i < count; i++) {
        if (this.messagesModel.rowData(i)?.id === j.id) {
          idx = i;
          break;
        }
      }
      if (idx < 0) {
        this.cancelJump();
        return;
      }
      const total = this.win.conv_viewport_h;
      const visible = this.win.conv_list_h;
      const avg = count > 0 ? total / count : 0;
      const top = Math.min(
        Math.max(0, avg * idx - visible / 3),
        Math.max(0, total - visible),
      );
      try {
        this.win.set_conversation_scroll(-top);
      } catch {
        // layout not ready
      }
    }
    // Re-arming the id re-creates the probe, which reports its position.
    this.win.jump_id = "";
    setTimeout(() => {
      if (this.jump === j) this.win.jump_id = j.id;
    }, 20);
    j.timer = setTimeout(() => this.jumpStep(true), 220);
  }

  private onJumpReport(offset: number) {
    const j = this.jump;
    if (!j) return;
    if (j.timer) clearTimeout(j.timer);
    const target = Math.min(120, this.win.conv_list_h / 3);
    const delta = target - offset;
    if (Math.abs(delta) < 6) {
      this.cancelJump();
      return;
    }
    try {
      this.win.set_conversation_scroll(this.win.conv_scroll + delta);
    } catch {
      // layout not ready
    }
    j.timer = setTimeout(() => this.jumpStep(false), 120);
  }

  private queueAvatar(jid: string) {
    if (!jid || this.requestedAvatars.has(jid)) return;
    this.requestedAvatars.add(jid);
    this.avatarQueue.push(jid);
    void this.drainAvatars();
  }

  private scrollToEnd() {
    for (const delay of [0, 60, 220]) {
      setTimeout(() => {
        try {
          this.win.scroll_conversation_end();
        } catch {
          // layout not ready yet — harmless
        }
      }, delay);
    }
  }

  // Formatted or mention-carrying messages render as styled text; plain
  // ones keep the selectable input.
  private styledFor(m: StoredMessage): { styled: unknown; hasStyled: boolean } {
    const body = m.deleted ? "" : m.text;
    if (!body || !hasMarkup(body)) return { styled: EMPTY_STYLED, hasStyled: false };
    const resolve = (num: string): string | null => {
      const jid = `${num}@s.whatsapp.net`;
      const canon = this.store.canon(jid);
      const known =
        this.store.contacts.get(canon) ?? this.store.chats.get(canon)?.name ?? null;
      return known ?? null;
    };
    try {
      return {
        styled: StyledText.fromMarkdown(toMarkdown(body, resolve)),
        hasStyled: true,
      };
    } catch {
      return { styled: EMPTY_STYLED, hasStyled: false };
    }
  }

  private toRow(m: StoredMessage, prev?: StoredMessage): MessageRow {
    const isGroup = m.jid.endsWith("@g.us");
    // A run also breaks after a pause, so a long stream from one sender
    // still shows who is talking, like WhatsApp does.
    const RUN_GAP = 5 * 60;
    const firstOfRun =
      !prev ||
      prev.fromMe !== m.fromMe ||
      (isGroup && prev.sender !== m.sender) ||
      m.timestamp - prev.timestamp > RUN_GAP;
    // Group messages from others are indented to leave room for the
    // sender's avatar, drawn once per run like WhatsApp does.
    const rawSender =
      m.senderJid ??
      (m.raw?.key?.participant
        ? this.store.canon(jidNormalizedUser(m.raw.key.participant))
        : "");
    const senderJid = this.resolveLid(rawSender);
    const groupIndent = isGroup && !m.fromMe;
    const senderAvatar = senderJid ? this.media.avatarFor(senderJid) : null;
    if (groupIndent && senderJid) this.queueAvatar(senderJid);
    // Voice notes carry the speaker's picture on the right, ours included.
    const voiceJid =
      m.kind === "audio" ? (m.fromMe ? this.selfJid : senderJid || m.jid) : "";
    const voiceAvatar = voiceJid ? this.media.avatarFor(voiceJid) : null;
    if (voiceJid) this.queueAvatar(voiceJid);
    return {
      id: m.id,
      kind: m.deleted ? "text" : m.kind,
      text: m.deleted ? t("msg.deleted") : m.text,
      fromMe: m.fromMe,
      sender: m.sender,
      showSender: isGroup && !m.fromMe && firstOfRun,
      // Saved contacts show their address-book name without the ~ mark.
      senderLabel:
        isGroup && senderJid && !this.store.isSaved(senderJid)
          ? `~ ${this.store.chatName(senderJid)}`
          : this.store.chatName(senderJid || m.jid),
      firstOfRun,
      time: formatTime(m.timestamp),
      picture: EMPTY_IMAGE,
      // Reserve the final box from the media's own dimensions so the
      // bubble keeps its height when the bitmap arrives.
      ...mediaBox(m),
      mediaPath: "",
      mediaReady: false,
      reactions: reactionSummary(m),
      dayLabel:
        !prev || formatDay(prev.timestamp) !== formatDay(m.timestamp)
          ? formatDay(m.timestamp)
          : "",
      ...ticksFor(m),
      senderAvatar: senderAvatar ?? EMPTY_IMAGE,
      senderHasAvatar: !!senderAvatar,
      senderInitial: initialOf(m.sender || "?"),
      senderColorIdx: colorIdxOf(senderJid || m.jid),
      voiceJid,
      voiceAvatar: voiceAvatar ?? EMPTY_IMAGE,
      voiceHasAvatar: !!voiceAvatar,
      voiceInitial: initialOf(
        (m.fromMe ? this.store.chatName(this.selfJid) : m.sender) || "?",
      ),
      voiceColorIdx: colorIdxOf(voiceJid || m.jid),
      groupIndent,
      showAvatar: groupIndent && firstOfRun,
      // Unsaved senders show their number next to the push name.
      senderNumber:
        groupIndent && senderJid && !this.store.isSaved(senderJid)
          ? formatNumber(senderJid)
          : "",
      sticker: !!m.sticker,
      gif: !!m.gif,
      ...this.styledFor(m),
      linkTitle: (m.linkTitle ?? "").trim(),
      linkDesc: (m.linkDesc ?? "").trim(),
      linkHost: hostOf(m.linkUrl),
      linkUrl: m.linkUrl ?? "",
      hasLink: !!(m.linkTitle || m.linkUrl),
      linkThumb: EMPTY_IMAGE,
      hasLinkThumb: false,
      linkThumbW: 0,
      linkThumbH: 0,
      wave: EMPTY_IMAGE,
      hasWave: false,
      playing: this.audio?.id === m.id && !this.audio.paused,
      progress:
        this.audio?.id === m.id && this.audio.duration > 0
          ? Math.min(1, this.audio.pos / this.audio.duration)
          : 0,
      posLabel: "",
      senderJid,
      forwarded: !!m.forwarded,
      deleted: !!m.deleted,
    };
  }

  // Newest first: those are the ones on screen. Three workers keep the
  // visible part of the conversation filling quickly.
  private async loadMediaForChat(jid: string) {
    const pending = this.store
      .messagesFor(jid)
      .filter((m) => m.kind !== "text" || m.linkTitle || m.linkUrl)
      .reverse();
    const worker = async () => {
      while (pending.length > 0) {
        if (this.currentJid !== jid) return;
        const stored = pending.shift()!;
        await this.loadMediaForMessage(stored);
      }
    };
    await Promise.all([worker(), worker(), worker()]);
  }

  private async loadMediaForMessage(stored: StoredMessage) {
    if (!stored.raw) return;
    const jid = stored.jid;
    if (stored.kind === "text") {
      // Link preview thumbnail travels inside the message itself.
      const thumb = stored.raw.message?.extendedTextMessage?.jpegThumbnail;
      if (thumb?.length) {
        const img = await this.media.decodeRaw(Buffer.from(thumb));
        if (img && this.currentJid === jid) {
          this.patchRow(stored.id, {
            linkThumb: img,
            hasLinkThumb: true,
            linkThumbW: img.displayW,
            linkThumbH: img.displayH,
          });
        }
      }
      return;
    }
    if (stored.kind === "video" && stored.gif) {
      // GIFs loop inline: download the short clip and cycle its frames.
      const path = await this.media.ensureCached(stored.id, stored.raw);
      if (path && this.currentJid === jid) {
        const frames = await this.media.videoFrames(stored.id, path);
        if (frames.length > 0 && this.currentJid === jid) {
          const first = frames[0]!;
          const scale = Math.min(330 / first.displayW, 380 / first.displayH, 1);
          this.patchRow(stored.id, {
            picture: first,
            picW: Math.max(1, Math.round(first.displayW * scale)),
            picH: Math.max(1, Math.round(first.displayH * scale)),
            mediaPath: path,
            mediaReady: true,
          });
          if (frames.length > 1) {
            this.animated.set(stored.id, { frames, idx: 0, loop: false });
            this.ensureStickerTicker();
          }
          return;
        }
      }
    }
    if (stored.kind === "video") {
      // Show the embedded thumbnail; the clip downloads only on click.
      const thumb = stored.raw.message?.videoMessage?.jpegThumbnail;
      if (thumb?.length) {
        const img = await this.media.decodeRaw(Buffer.from(thumb));
        if (img && this.currentJid === jid) {
          const scale = Math.min(330 / img.displayW, 380 / img.displayH, 1);
          this.patchRow(stored.id, {
            picture: img,
            picW: Math.max(1, Math.round(img.displayW * scale)),
            picH: Math.max(1, Math.round(img.displayH * scale)),
          });
        }
      }
    }
    const path = await this.media.ensureCached(stored.id, stored.raw);
    if (this.currentJid !== jid) return;
    if (!path) {
      this.patchRow(stored.id, { text: t("media.unavailable"), kind: "text" });
      return;
    }
    if (stored.kind !== "image") {
      // audio, documents and videos just need the cached file to open
      this.patchRow(stored.id, { mediaPath: path, mediaReady: true });
      if (stored.kind === "audio") void this.loadWaveform(stored, path);
      return;
    }
    if (stored.sticker) {
      const frames = await this.media.stickerFrames(stored.id, path);
      if (this.currentJid !== jid || frames.length === 0) return;
      const first = frames[0]!;
      this.patchRow(stored.id, {
        picture: first,
        picW: first.displayW,
        picH: first.displayH,
        mediaPath: path,
        mediaReady: true,
      });
      if (frames.length > 1) {
        this.animated.set(stored.id, { frames, idx: 0, loop: true });
        this.ensureStickerTicker();
      }
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

  private stickTimer: NodeJS.Timeout | null = null;

  private patchRow(id: string, patch: Partial<MessageRow>) {
    // A late-loading image must not push the newest message out of view;
    // debounced so a burst of images re-anchors only once.
    if (patch.mediaReady && this.win.stick_bottom && !this.stickTimer) {
      this.stickTimer = setTimeout(() => {
        this.stickTimer = null;
        if (this.win.stick_bottom) this.scrollToEnd();
      }, 120);
    }
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
