// Builds ui/zapive.ico from ui/zapive.png. ICO entries are PNG-encoded
// (valid since Vista), so this is just resized PNGs in an ICO container.
// Usage: node scripts/make-ico.mjs
import { readFileSync, writeFileSync } from "node:fs";
import { join, resolve, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import { createRequire } from "node:module";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const sharp = createRequire(join(ROOT, "package.json"))("sharp");

const SIZES = [16, 24, 32, 48, 64, 128, 256];
const src = readFileSync(join(ROOT, "ui", "zapive.png"));

const pngs = await Promise.all(
  SIZES.map((size) =>
    sharp(src).resize(size, size, { fit: "cover" }).png().toBuffer(),
  ),
);

// ICONDIR + one ICONDIRENTRY per image, then the PNG payloads.
const header = Buffer.alloc(6);
header.writeUInt16LE(0, 0); // reserved
header.writeUInt16LE(1, 2); // type: icon
header.writeUInt16LE(SIZES.length, 4);

const entries = [];
let offset = 6 + SIZES.length * 16;
for (let i = 0; i < SIZES.length; i++) {
  const e = Buffer.alloc(16);
  e.writeUInt8(SIZES[i] === 256 ? 0 : SIZES[i], 0); // width (0 = 256)
  e.writeUInt8(SIZES[i] === 256 ? 0 : SIZES[i], 1); // height
  e.writeUInt8(0, 2); // palette
  e.writeUInt8(0, 3); // reserved
  e.writeUInt16LE(1, 4); // color planes
  e.writeUInt16LE(32, 6); // bits per pixel
  e.writeUInt32LE(pngs[i].length, 8);
  e.writeUInt32LE(offset, 12);
  offset += pngs[i].length;
  entries.push(e);
}

const out = join(ROOT, "ui", "zapive.ico");
writeFileSync(out, Buffer.concat([header, ...entries, ...pngs]));
console.log(`${out}: ${SIZES.join("/")}px, ${(offset / 1024).toFixed(0)} KB`);
