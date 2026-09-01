import { getContentType, jidNormalizedUser, proto } from "@whiskeysockets/baileys";
import type { Chat, Contact, WAMessage } from "@whiskeysockets/baileys";
import { t } from "./i18n.ts";

export type MessageKind = "text" | "image" | "audio" | "doc";

export interface StoredMessage {
  id: string;
  jid: string;
  kind: MessageKind;
  text: string;
  fromMe: boolean;
  sender: string;
  senderJid?: string;
  forwarded?: boolean;
  deleted?: boolean;
  timestamp: number;
  mimetype?: string;
  durationSec?: number;
  status?: number; // WAMessageStatus for own messages (2 ack, 3 delivered, 4 read)
  reactions?: Record<string, string>; // reactor id -> emoji
  raw?: WAMessage;
}

export function ticksFor(m: StoredMessage): { ticks: string; ticksBlue: boolean } {
  if (!m.fromMe) return { ticks: "", ticksBlue: false };
  const s = m.status ?? 0;
  if (s >= 4) return { ticks: "✓✓", ticksBlue: true };
  if (s >= 3) return { ticks: "✓✓", ticksBlue: false };
  return { ticks: "✓", ticksBlue: false };
}

export function reactionSummary(m: StoredMessage): string {
  const values = Object.values(m.reactions ?? {}).filter(Boolean);
  if (values.length === 0) return "";
  const unique = [...new Set(values)].slice(0, 3).join("");
  return values.length > 1 ? `${unique} ${values.length}` : unique;
}

export interface ChatMeta {
  jid: string;
  name?: string;
  preview: string;
  timestamp: number;
  unread?: number;
  pinned?: number; // pin timestamp; 0/undefined = not pinned
  archived?: boolean;
}

function toNum(t: unknown): number {
  if (typeof t === "number") return t;
  if (typeof t === "string") return parseInt(t, 10) || 0;
  if (t && typeof t === "object" && "toNumber" in t) {
    return (t as { toNumber(): number }).toNumber();
  }
  return 0;
}

export function isDisplayableJid(jid: string | null | undefined): jid is string {
  if (!jid) return false;
  if (jid === "status@broadcast") return false;
  if (jid.endsWith("@newsletter")) return false;
  if (jid.endsWith("@broadcast")) return false;
  return jid.endsWith("@s.whatsapp.net") || jid.endsWith("@g.us") || jid.endsWith("@lid");
}

export function formatTime(timestamp: number): string {
  if (!timestamp) return "";
  const d = new Date(timestamp * 1000);
  const now = new Date();
  if (d.toDateString() === now.toDateString()) {
    return d.toLocaleTimeString("pt-BR", { hour: "2-digit", minute: "2-digit" });
  }
  return d.toLocaleDateString("pt-BR", { day: "2-digit", month: "2-digit" });
}

// Slint's text renderer shows tofu for emoji followed by a variation
// selector (e.g. "❤️" = U+2764 U+FE0F); stripping the selector keeps the
// colored glyph. Also drops lone surrogates left by legacy truncation.
export function cleanText(s: string): string {
  return s
    .replace(/[︀-️]/g, "")
    .replace(/[\uD800-\uDBFF](?![\uDC00-\uDFFF])/g, "")
    .replace(/(?<![\uD800-\uDBFF])[\uDC00-\uDFFF]/g, "");
}

export function formatDay(timestamp: number): string {
  if (!timestamp) return "";
  const d = new Date(timestamp * 1000);
  const today = new Date();
  const yesterday = new Date(today.getTime() - 86_400_000);
  if (d.toDateString() === today.toDateString()) return t("day.today");
  if (d.toDateString() === yesterday.toDateString()) return t("day.yesterday");
  return d.toLocaleDateString("pt-BR");
}

// A readable identity for a jid without a known name.
export function displayId(jid: string): string {
  const user = jid.split("@")[0] ?? jid;
  if (jid.endsWith("@s.whatsapp.net") && /^\d{10,15}$/.test(user)) return `+${user}`;
  return user;
}

export function formatDuration(seconds: number): string {
  const m = Math.floor(seconds / 60);
  const s = Math.floor(seconds % 60);
  return `${m}:${String(s).padStart(2, "0")}`;
}

export class Store {
  chats = new Map<string, ChatMeta>();
  messages = new Map<string, StoredMessage[]>();
  contacts = new Map<string, string>();
  aliases = new Map<string, string>(); // @lid jid -> phone-number jid
  deletedJids = new Set<string>();

  // Resolves a jid through the LID->PN alias table.
  canon(jid: string): string {
    return this.aliases.get(jid) ?? jid;
  }

  // Learns that `lid` and `pn` are the same contact; merges duplicates.
  learnAlias(lid: string, pn: string): boolean {
    if (this.aliases.get(lid) === pn) return false;
    this.aliases.set(lid, pn);
    this.mergeJid(lid, pn);
    return true;
  }

  private mergeJid(from: string, into: string) {
    const src = this.chats.get(from);
    if (src) {
      const dst = this.chats.get(into);
      this.chats.set(into, {
        jid: into,
        name: dst?.name ?? src.name,
        preview: (dst?.timestamp ?? 0) >= src.timestamp ? (dst?.preview ?? src.preview) : src.preview,
        timestamp: Math.max(src.timestamp, dst?.timestamp ?? 0),
        unread: (src.unread ?? 0) + (dst?.unread ?? 0),
        pinned: Math.max(src.pinned ?? 0, dst?.pinned ?? 0),
        archived: (src.archived ?? false) || (dst?.archived ?? false),
      });
      this.chats.delete(from);
    }
    const srcMsgs = this.messages.get(from);
    if (srcMsgs) {
      const dstMsgs = this.messages.get(into) ?? [];
      const seen = new Set(dstMsgs.map((m) => m.id));
      for (const m of srcMsgs) {
        if (!seen.has(m.id)) dstMsgs.push({ ...m, jid: into });
      }
      dstMsgs.sort((a, b) => a.timestamp - b.timestamp);
      this.messages.set(into, dstMsgs);
      this.messages.delete(from);
      this.dirtyJids.add(into);
      this.dirtyJids.delete(from);
    }
    this.deletedJids.add(from);
    const name = this.contacts.get(from);
    if (name && !this.contacts.has(into)) this.contacts.set(into, name);
  }

  chatName(jid: string): string {
    const meta = this.chats.get(jid);
    if (meta?.name) return cleanText(meta.name);
    const contact = this.contacts.get(jid);
    if (contact) return cleanText(contact);
    const user = jid.split("@")[0] ?? jid;
    // Bare phone-number DMs read better in international format.
    if (jid.endsWith("@s.whatsapp.net") && /^\d{10,15}$/.test(user)) {
      return `+${user}`;
    }
    return user;
  }

  sortedChats(): ChatMeta[] {
    return [...this.chats.values()].sort((a, b) => {
      const pa = a.pinned ?? 0;
      const pb = b.pinned ?? 0;
      if (pa > 0 !== pb > 0) return pa > 0 ? -1 : 1;
      if (pa > 0) return pb - pa;
      return b.timestamp - a.timestamp;
    });
  }

  upsertContact(c: Contact) {
    const jid = jidNormalizedUser(c.id);
    const name = c.name ?? c.notify ?? c.verifiedName;
    if (jid && name) this.contacts.set(jid, name);
  }

  upsertChat(c: Chat) {
    if (!c.id) return;
    const jid = this.canon(jidNormalizedUser(c.id));
    if (!isDisplayableJid(jid)) return;
    const existing = this.chats.get(jid);
    const ts = toNum(c.conversationTimestamp);
    const extra = c as { pinned?: number | null; archived?: boolean | null; archive?: boolean | null };
    const archivedRaw = extra.archived ?? extra.archive;
    this.chats.set(jid, {
      jid,
      name: c.name ?? existing?.name,
      preview: existing?.preview ?? "",
      timestamp: Math.max(ts, existing?.timestamp ?? 0),
      unread: existing?.unread ?? 0,
      pinned:
        extra.pinned === undefined ? (existing?.pinned ?? 0) : toNum(extra.pinned ?? 0),
      archived: archivedRaw === undefined || archivedRaw === null
        ? (existing?.archived ?? false)
        : !!archivedRaw,
    });
  }

  // Group subjects / contact names discovered outside the chat events
  // (e.g. groupFetchAllParticipating).
  setName(jid: string, name: string) {
    const existing = this.chats.get(jid);
    if (existing) {
      existing.name = name;
    } else {
      this.contacts.set(jid, name);
    }
  }

  normalize(msg: WAMessage): StoredMessage | null {
    const remoteJid = msg.key?.remoteJid;
    if (!remoteJid) return null;
    // v7 LID addressing: remoteJidAlt carries the other identity of the
    // same DM contact — learn it so lid/pn chats collapse into one.
    const alt = (msg.key as { remoteJidAlt?: string | null }).remoteJidAlt;
    if (alt) {
      const a = jidNormalizedUser(remoteJid);
      const b = jidNormalizedUser(alt);
      const lid = a.endsWith("@lid") ? a : b.endsWith("@lid") ? b : null;
      const pn = a.endsWith("@s.whatsapp.net") ? a : b.endsWith("@s.whatsapp.net") ? b : null;
      if (lid && pn) this.learnAlias(lid, pn);
    }
    const jid = this.canon(jidNormalizedUser(remoteJid));
    if (!isDisplayableJid(jid)) return null;
    const id = msg.key?.id;
    if (!id || !msg.message) return null;

    const content = msg.message;
    const type = getContentType(content);
    const timestamp = toNum(msg.messageTimestamp);
    const fromMe = !!msg.key?.fromMe;

    // Learn the group participant's lid<->pn pair when present.
    const participant = msg.key?.participant;
    const pAlt = (msg.key as { participantAlt?: string | null }).participantAlt;
    if (participant && pAlt) {
      const a = jidNormalizedUser(participant);
      const b = jidNormalizedUser(pAlt);
      const lid = a.endsWith("@lid") ? a : b.endsWith("@lid") ? b : null;
      const pn = a.endsWith("@s.whatsapp.net") ? a : b.endsWith("@s.whatsapp.net") ? b : null;
      if (lid && pn) this.learnAlias(lid, pn);
    }

    const senderJid = this.canon(participant ? jidNormalizedUser(participant) : jid);
    if (!fromMe && msg.pushName) this.contacts.set(senderJid, msg.pushName);
    const sender = cleanText(
      fromMe
        ? ""
        : (this.contacts.get(senderJid) ?? msg.pushName ?? displayId(senderJid)),
    );

    // Forwarded flag lives in the content's contextInfo.
    const ctx = (Object.values(content).find(
      (v) => v && typeof v === "object" && "contextInfo" in v,
    ) as { contextInfo?: { isForwarded?: boolean | null } } | undefined)?.contextInfo;
    const forwarded = !!ctx?.isForwarded;

    // History-synced messages carry their accumulated reactions inline.
    const reactions: Record<string, string> = {};
    for (const r of (msg as { reactions?: { key?: { participant?: string | null; remoteJid?: string | null; id?: string | null }; text?: string | null }[] }).reactions ?? []) {
      const reactor = r.key?.participant ?? r.key?.remoteJid ?? r.key?.id ?? "?";
      if (r.text) reactions[reactor] = cleanText(r.text);
    }

    const base = {
      id,
      jid,
      fromMe,
      sender,
      senderJid,
      timestamp,
      raw: msg,
      ...(forwarded ? { forwarded: true } : {}),
      ...(fromMe ? { status: toNum(msg.status) || 2 } : {}),
      ...(Object.keys(reactions).length > 0 ? { reactions } : {}),
    };

    if (type === "conversation") {
      return { ...base, kind: "text", text: cleanText(content.conversation ?? "") };
    }
    if (type === "extendedTextMessage") {
      return {
        ...base,
        kind: "text",
        text: cleanText(content.extendedTextMessage?.text ?? ""),
      };
    }
    if (type === "stickerMessage") {
      return {
        ...base,
        kind: "image",
        text: "",
        mimetype: content.stickerMessage?.mimetype ?? "image/webp",
      };
    }
    if (type === "imageMessage") {
      return {
        ...base,
        kind: "image",
        text: cleanText(content.imageMessage?.caption ?? ""),
        mimetype: content.imageMessage?.mimetype ?? "image/jpeg",
      };
    }
    if (type === "documentMessage") {
      return {
        ...base,
        kind: "doc",
        text: cleanText(content.documentMessage?.fileName ?? t("doc.fallbackName")),
        mimetype: content.documentMessage?.mimetype ?? "application/octet-stream",
      };
    }
    if (type === "audioMessage") {
      const seconds = toNum(content.audioMessage?.seconds);
      return {
        ...base,
        kind: "audio",
        text: formatDuration(seconds),
        mimetype: content.audioMessage?.mimetype ?? "audio/ogg",
        durationSec: seconds,
      };
    }
    return null;
  }

  // Returns true if the message was newly added (false = duplicate/ignored).
  addMessage(stored: StoredMessage): boolean {
    const list = this.messages.get(stored.jid) ?? [];
    if (list.some((m) => m.id === stored.id)) return false;
    list.push(stored);
    list.sort((a, b) => a.timestamp - b.timestamp);
    if (list.length > 500) list.splice(0, list.length - 500);
    this.messages.set(stored.jid, list);
    this.dirtyJids.add(stored.jid);

    const existing = this.chats.get(stored.jid);
    const preview = computePreview(stored);
    if (!existing || stored.timestamp >= existing.timestamp) {
      this.chats.set(stored.jid, {
        jid: stored.jid,
        name: existing?.name,
        preview,
        timestamp: Math.max(stored.timestamp, existing?.timestamp ?? 0),
        unread: existing?.unread ?? 0,
        pinned: existing?.pinned ?? 0,
        archived: existing?.archived ?? false,
      });
    }
    if (!stored.fromMe && stored.sender && msgIsDirect(stored.jid)) {
      this.contacts.set(stored.jid, stored.sender);
    }
    return true;
  }

  messagesFor(jid: string): StoredMessage[] {
    return this.messages.get(jid) ?? [];
  }

  // The oldest stored message that still has its raw WAMessage (needed as
  // the anchor for on-demand history fetches).
  oldestMessage(): StoredMessage | null {
    let oldest: StoredMessage | null = null;
    for (const list of this.messages.values()) {
      for (const m of list) {
        if (!m.raw?.key || !m.timestamp) continue;
        if (!oldest || m.timestamp < oldest.timestamp) oldest = m;
      }
    }
    return oldest;
  }

  totalMessages(): number {
    let n = 0;
    for (const list of this.messages.values()) n += list.length;
    return n;
  }

  // Marks a message as deleted-for-everyone (protocol REVOKE).
  markDeleted(jid: string, id: string): StoredMessage | null {
    const m = this.messages.get(jid)?.find((x) => x.id === id);
    if (!m || m.deleted) return null;
    m.deleted = true;
    m.kind = "text";
    m.text = "";
    delete m.raw;
    this.dirtyJids.add(jid);
    return m;
  }

  // Updates the delivery status of an own message (messages.update event).
  setStatus(jid: string, id: string, status: number): StoredMessage | null {
    const m = this.messages.get(jid)?.find((x) => x.id === id);
    if (!m || !m.fromMe) return null;
    if ((m.status ?? 0) >= status) return null;
    m.status = status;
    this.dirtyJids.add(jid);
    return m;
  }

  // Applies (or removes, when emoji is empty) a reaction to a message.
  // Returns the updated message, or null if the target isn't stored.
  applyReaction(
    jid: string,
    targetId: string,
    reactor: string,
    emoji: string,
  ): StoredMessage | null {
    const list = this.messages.get(jid);
    const target = list?.find((m) => m.id === targetId);
    if (!target) return null;
    target.reactions ??= {};
    if (emoji) {
      target.reactions[reactor] = cleanText(emoji);
    } else {
      delete target.reactions[reactor];
    }
    this.dirtyJids.add(jid);
    return target;
  }

  // ---- persistence (WhatsApp only sends history sync at pairing time,
  // so everything must survive restarts locally; stored in SQLite via Db) ----

  dirtyJids = new Set<string>();

  saveTo(db: import("./db.ts").Db) {
    db.set("store:chats", JSON.stringify([...this.chats]));
    db.set("store:contacts", JSON.stringify([...this.contacts]));
    db.set("store:aliases", JSON.stringify([...this.aliases]));
    for (const jid of this.deletedJids) {
      db.del(`store:msgs:${jid}`);
    }
    this.deletedJids.clear();
    let saved = 0;
    for (const jid of this.dirtyJids) {
      const list = this.messages.get(jid) ?? [];
      db.set(`store:msgs:${jid}`, JSON.stringify(dehydrate(list)));
      saved++;
    }
    if (saved > 0) console.log(`[store] saved ${saved} chat message list(s)`);
    this.dirtyJids.clear();
  }

  loadFrom(db: import("./db.ts").Db) {
    const chats = db.get("store:chats");
    if (chats) this.chats = new Map(JSON.parse(chats));
    const contacts = db.get("store:contacts");
    if (contacts) this.contacts = new Map(JSON.parse(contacts));
    const aliases = db.get("store:aliases");
    if (aliases) this.aliases = new Map(JSON.parse(aliases));
    for (const key of db.keys("store:msgs:")) {
      const jid = key.slice("store:msgs:".length);
      const raw = db.get(key);
      if (raw) this.messages.set(jid, hydrate(JSON.parse(raw)));
    }
    // Collapse any lid/pn duplicates persisted by older builds.
    for (const [lid, pn] of this.aliases) {
      if (this.chats.has(lid) || this.messages.has(lid)) this.mergeJid(lid, pn);
    }
    // Regenerate previews: older builds persisted previews with broken
    // surrogate pairs / variation selectors.
    for (const [jid, list] of this.messages) {
      const last = list[list.length - 1];
      const meta = this.chats.get(jid);
      if (last && meta) {
        meta.preview = computePreview({ ...last, text: cleanText(last.text) });
      }
    }
  }

  // Imports the legacy data_store.json format.
  importLegacyJson(json: string) {
    const p = JSON.parse(json) as {
      chats: [string, ChatMeta][];
      contacts: [string, string][];
      messages: [string, StoredMessage[]][];
    };
    this.chats = new Map(p.chats);
    this.contacts = new Map(p.contacts);
    this.messages = new Map(p.messages.map(([jid, list]) => [jid, hydrate(list)]));
    for (const jid of this.messages.keys()) this.dirtyJids.add(jid);
  }
}

function computePreview(stored: StoredMessage): string {
  const body =
    stored.kind === "image"
      ? t("preview.photo")
      : stored.kind === "audio"
        ? t("preview.audio")
        : stored.kind === "doc"
          ? t("preview.document", stored.text)
          : [...stored.text.replace(/\s+/g, " ")].slice(0, 80).join("");
  const prefix = stored.fromMe
    ? "✓ "
    : stored.jid.endsWith("@g.us") && stored.sender
      ? `${cleanText(stored.sender).split(" ")[0]}: `
      : "";
  return prefix + body;
}

function dehydrate(list: StoredMessage[]): unknown[] {
  return list.slice(-300).map((m) => ({
    ...m,
    raw: m.raw ? ((m.raw as { toJSON?: () => unknown }).toJSON?.() ?? m.raw) : undefined,
  }));
}

function hydrate(list: StoredMessage[]): StoredMessage[] {
  return list.map((m) => ({
    ...m,
    raw: m.raw
      ? (proto.WebMessageInfo.fromObject(m.raw as object) as unknown as WAMessage)
      : undefined,
  }));
}

function msgIsDirect(jid: string): boolean {
  return jid.endsWith("@s.whatsapp.net") || jid.endsWith("@lid");
}
