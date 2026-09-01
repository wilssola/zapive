// Slint's gettext runtime never resolves the catalog on Windows (the C
// locale stays "C", so libintl ignores LANGUAGE and the .mo is never
// read), which left the markup half-translated. The catalog is applied
// when the source is loaded instead: every @tr("...") literal is swapped
// for its translation before the interpreter sees the file.
import { readFileSync } from "node:fs";

// Minimal .po reader: msgid/msgstr pairs with continuation lines.
export function loadCatalog(poPath: string): Map<string, string> {
  const out = new Map<string, string>();
  let msgid: string | null = null;
  let msgstr: string | null = null;
  let mode: "id" | "str" | null = null;
  const unquote = (s: string): string => {
    const body = s.trim().replace(/^"|"$/g, "");
    return body.replace(/\\(.)/g, (_, c: string) =>
      c === "n" ? "\n" : c === "t" ? "\t" : c,
    );
  };
  const flush = () => {
    if (msgid && msgstr) out.set(msgid, msgstr);
    msgid = msgstr = mode = null;
  };
  let src = "";
  try {
    src = readFileSync(poPath, "utf8");
  } catch {
    return out;
  }
  for (const raw of src.split(/\r?\n/)) {
    const line = raw.trim();
    if (line === "" || line.startsWith("#")) continue;
    if (line.startsWith("msgid ")) {
      flush();
      msgid = unquote(line.slice(6));
      mode = "id";
    } else if (line.startsWith("msgstr ")) {
      msgstr = unquote(line.slice(7));
      mode = "str";
    } else if (line.startsWith('"')) {
      const part = unquote(line);
      if (mode === "id") msgid = (msgid ?? "") + part;
      else if (mode === "str") msgstr = (msgstr ?? "") + part;
    }
  }
  flush();
  return out;
}

// Escapes a translation back into a Slint string literal.
function escapeSlint(s: string): string {
  return s.replace(/\\/g, "\\\\").replace(/"/g, '\\"').replace(/\n/g, "\\n");
}

function unescapeSlint(s: string): string {
  return s.replace(/\\(.)/g, (_, c: string) => (c === "n" ? "\n" : c));
}

export function translateSlintSource(src: string, catalog: Map<string, string>): string {
  if (catalog.size === 0) return src;
  return src.replace(/@tr\("((?:[^"\\]|\\.)*)"/g, (whole, literal: string) => {
    const hit = catalog.get(literal) ?? catalog.get(unescapeSlint(literal));
    return hit ? `@tr("${escapeSlint(hit)}"` : whole;
  });
}
