// WhatsApp's lightweight markup (*bold*, _italic_, ~strike~, ```mono```)
// translated into the markdown subset Slint's StyledText understands.
// Everything outside a marker is escaped so stray symbols stay literal.

const SPECIAL = /[\\`*_~\[\]()#>+\-.!|]/g;

function escapeMd(text: string): string {
  return text.replace(SPECIAL, (c) => `\\${c}`);
}

const TOKEN =
  /```([\s\S]+?)```|`([^`\n]+?)`|\*([^*\n]+?)\*|_([^_\n]+?)_|~([^~\n]+?)~/g;

export function hasMarkup(text: string): boolean {
  TOKEN.lastIndex = 0;
  return TOKEN.test(text);
}

export function toMarkdown(text: string): string {
  let out = "";
  let last = 0;
  TOKEN.lastIndex = 0;
  for (let m = TOKEN.exec(text); m !== null; m = TOKEN.exec(text)) {
    out += escapeMd(text.slice(last, m.index));
    const [, fence, code, bold, italic, strike] = m;
    if (fence !== undefined || code !== undefined) {
      out += `\`${(fence ?? code)!.replace(/`/g, "")}\``;
    } else if (bold !== undefined) {
      out += `**${escapeMd(bold)}**`;
    } else if (italic !== undefined) {
      out += `*${escapeMd(italic)}*`;
    } else if (strike !== undefined) {
      out += `~~${escapeMd(strike)}~~`;
    }
    last = m.index + m[0].length;
  }
  out += escapeMd(text.slice(last));
  return out;
}
