// WhatsApp's lightweight markup (*bold*, _italic_, ~strike~, ```mono```)
// translated into the markdown subset Slint's StyledText understands.
// Everything outside a marker is escaped so stray symbols stay literal.

const SPECIAL = /[\\`*_~\[\]()#>+\-.!|]/g;

function escapeMd(text: string): string {
  return text.replace(SPECIAL, (c) => `\\${c}`);
}

const TOKEN =
  /```([\s\S]+?)```|`([^`\n]+?)`|\*([^*\n]+?)\*|_([^_\n]+?)_|~([^~\n]+?)~/g;

const MENTION = /@(\d{5,20}|all|everyone)\b/gi;
const URL_RE = /https?:\/\/[^\s<>\]]+/gi;

export function hasMarkup(text: string): boolean {
  TOKEN.lastIndex = 0;
  MENTION.lastIndex = 0;
  URL_RE.lastIndex = 0;
  return TOKEN.test(text) || MENTION.test(text) || URL_RE.test(text);
}

// Mentions arrive as @<number>; show the contact name and link it to the
// conversation so clicking opens that chat.
function renderLinks(text: string): string {
  let out = "";
  let last = 0;
  URL_RE.lastIndex = 0;
  for (let m = URL_RE.exec(text); m !== null; m = URL_RE.exec(text)) {
    out += escapeMd(text.slice(last, m.index));
    const url = m[0]!;
    out += `[${escapeMd(url)}](${url})`;
    last = m.index + url.length;
  }
  out += escapeMd(text.slice(last));
  return out;
}

// Where a mention points: the chat to open and the name to show, when
// the id belongs to someone we know.
export interface MentionTarget {
  name: string | null;
  jid: string;
}

function renderMentions(
  text: string,
  resolve: (num: string) => MentionTarget | null,
): string {
  let out = "";
  let last = 0;
  MENTION.lastIndex = 0;
  for (let m = MENTION.exec(text); m !== null; m = MENTION.exec(text)) {
    out += renderLinks(text.slice(last, m.index));
    const token = m[1]!;
    if (/^\d+$/.test(token)) {
      // A mention is always a link, named or not: an unknown id still
      // opens the conversation with that person.
      const hit = resolve(token);
      const label = hit?.name ?? token;
      const jid = hit?.jid ?? `${token}@s.whatsapp.net`;
      out += `[@${escapeMd(label)}](${jid})`;
    } else {
      out += `**@${escapeMd(token)}**`;
    }
    last = m.index + m[0].length;
  }
  out += renderLinks(text.slice(last));
  return out;
}

export function toMarkdown(
  text: string,
  resolveMention: (num: string) => MentionTarget | null = () => null,
): string {
  const plain = (part: string) => renderMentions(part, resolveMention);
  let out = "";
  let last = 0;
  TOKEN.lastIndex = 0;
  for (let m = TOKEN.exec(text); m !== null; m = TOKEN.exec(text)) {
    out += plain(text.slice(last, m.index));
    const [, fence, code, bold, italic, strike] = m;
    if (fence !== undefined || code !== undefined) {
      out += `\`${(fence ?? code)!.replace(/`/g, "")}\``;
    } else if (bold !== undefined) {
      out += `**${plain(bold)}**`;
    } else if (italic !== undefined) {
      out += `*${plain(italic)}*`;
    } else if (strike !== undefined) {
      out += `~~${escapeMd(strike)}~~`;
    }
    last = m.index + m[0].length;
  }
  // The tail is literal text as well: it still needs mentions and links.
  out += plain(text.slice(last));
  return out;
}
