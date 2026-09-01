// Compiles i18n/*.po into gettext .mo catalogs (msgfmt replacement, since
// GNU gettext tools aren't available on Windows by default).
// Usage: node scripts/build-mo.mjs
import { readFileSync, writeFileSync, mkdirSync, readdirSync } from "node:fs";
import { join, basename } from "node:path";

const I18N_DIR = new URL("../i18n/", import.meta.url).pathname.replace(/^\/(\w:)/, "$1");
const DOMAIN = "zapive";

function parsePo(src) {
  const entries = new Map();
  let msgid = null;
  let msgstr = null;
  let mode = null;
  const flush = () => {
    // The empty msgid carries the catalog metadata (charset above all);
    // without it gettext falls back to ASCII and drops the catalog.
    if (msgid !== null && msgstr !== null) entries.set(msgid, msgstr);
    msgid = msgstr = mode = null;
  };
  for (const rawLine of src.split(/\r?\n/)) {
    const line = rawLine.trim();
    if (line === "" || line.startsWith("#")) continue;
    const unq = (s) =>
      JSON.parse(s.replace(/\\([^"\\nrt])/g, "\\\\$1")); // tolerate stray escapes
    if (line.startsWith("msgid ")) {
      flush();
      msgid = unq(line.slice(6));
      mode = "id";
    } else if (line.startsWith("msgstr ")) {
      msgstr = unq(line.slice(7));
      mode = "str";
    } else if (line.startsWith('"')) {
      const part = unq(line);
      if (mode === "id") msgid += part;
      else if (mode === "str") msgstr += part;
    }
  }
  flush();
  return entries;
}

function buildMo(entries) {
  const items = [...entries.entries()]
    .map(([id, str]) => [Buffer.from(id, "utf8"), Buffer.from(str, "utf8")])
    .sort((a, b) => Buffer.compare(a[0], b[0]));
  const n = items.length;
  const header = 28;
  const origTable = header;
  const transTable = origTable + n * 8;
  let dataOff = transTable + n * 8;
  const origMeta = [];
  const transMeta = [];
  const blobs = [];
  for (const [id] of items) {
    origMeta.push([id.length, dataOff]);
    blobs.push(id, Buffer.from([0]));
    dataOff += id.length + 1;
  }
  for (const [, str] of items) {
    transMeta.push([str.length, dataOff]);
    blobs.push(str, Buffer.from([0]));
    dataOff += str.length + 1;
  }
  const buf = Buffer.alloc(transTable + n * 8);
  buf.writeUInt32LE(0x950412de, 0); // magic
  buf.writeUInt32LE(0, 4); // revision
  buf.writeUInt32LE(n, 8);
  buf.writeUInt32LE(origTable, 12);
  buf.writeUInt32LE(transTable, 16);
  buf.writeUInt32LE(0, 20); // hash size
  buf.writeUInt32LE(0, 24); // hash offset
  items.forEach((_, i) => {
    buf.writeUInt32LE(origMeta[i][0], origTable + i * 8);
    buf.writeUInt32LE(origMeta[i][1], origTable + i * 8 + 4);
    buf.writeUInt32LE(transMeta[i][0], transTable + i * 8);
    buf.writeUInt32LE(transMeta[i][1], transTable + i * 8 + 4);
  });
  return Buffer.concat([buf, ...blobs]);
}

for (const file of readdirSync(I18N_DIR)) {
  if (!file.endsWith(".po")) continue;
  const locale = basename(file, ".po");
  const entries = parsePo(readFileSync(join(I18N_DIR, file), "utf8"));
  if (!entries.has("")) {
    entries.set(
      "",
      `Content-Type: text/plain; charset=UTF-8
Content-Transfer-Encoding: 8bit
Language: ${locale}
MIME-Version: 1.0
`,
    );
  }
  const mo = buildMo(entries);
  for (const dir of [locale, locale.split("_")[0]]) {
    const out = join(I18N_DIR, dir, "LC_MESSAGES");
    mkdirSync(out, { recursive: true });
    writeFileSync(join(out, `${DOMAIN}.mo`), mo);
  }
  console.log(`${file}: ${entries.size - 1} strings -> ${locale}/LC_MESSAGES/${DOMAIN}.mo`);
}
