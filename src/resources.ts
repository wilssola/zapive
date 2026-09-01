// Everything the app needs at runtime — the Slint markup, the icons, the
// translation catalogs and the three packages with native binaries —
// travels inside the executable as a single compressed archive.
//
// Native addons cannot be loaded from memory (dlopen needs a real file),
// so on the first run the archive is unpacked into the user's cache
// under a directory named after its content hash. Later runs find it
// already there and skip the work; a new build unpacks beside the old
// one and the stale directories are removed.
import { gunzipSync } from "node:zlib";
import { chmodSync, existsSync, mkdirSync, readdirSync, rmSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { cacheDir, isPackaged, resourceRoot } from "./paths.ts";

export const PACK_MAGIC = "ZPAK1";
export const PACK_ASSET = "resources.pack";

export interface PackEntry {
  p: string; // path relative to the root
  o: number; // offset in the payload
  n: number; // byte length
  m: number; // file mode
}

export interface PackManifest {
  id: string;
  files: PackEntry[];
}

export function readPack(pack: Buffer): { manifest: PackManifest; payload: Buffer } {
  if (pack.subarray(0, PACK_MAGIC.length).toString("utf8") !== PACK_MAGIC) {
    throw new Error("resource archive is corrupt");
  }
  const headerLen = pack.readUInt32LE(PACK_MAGIC.length);
  const start = PACK_MAGIC.length + 4;
  const manifest = JSON.parse(
    pack.subarray(start, start + headerLen).toString("utf8"),
  ) as PackManifest;
  return { manifest, payload: gunzipSync(pack.subarray(start + headerLen)) };
}

function unpack(pack: Buffer, dest: string): void {
  const { manifest, payload } = readPack(pack);
  for (const entry of manifest.files) {
    const target = join(dest, entry.p);
    mkdirSync(dirname(target), { recursive: true });
    writeFileSync(target, payload.subarray(entry.o, entry.o + entry.n));
    try {
      chmodSync(target, entry.m);
    } catch {
      // modes are advisory on Windows
    }
  }
}

let root: string | null = null;

// Where ui/, i18n/ and node_modules live for this run.
export function runtimeRoot(): string {
  if (root) return root;
  if (!isPackaged()) {
    root = resourceRoot();
    return root;
  }
  const sea = process.getBuiltinModule("node:sea") as {
    getAsset(key: string): ArrayBuffer;
  };
  const pack = Buffer.from(sea.getAsset(PACK_ASSET));
  const { manifest } = readPack(pack);
  const base = join(cacheDir(), "runtime");
  const dir = join(base, manifest.id);
  const stamp = join(dir, ".unpacked");
  if (!existsSync(stamp)) {
    rmSync(dir, { recursive: true, force: true });
    unpack(pack, dir);
    writeFileSync(stamp, manifest.id);
    console.log(`[resources] unpacked ${manifest.files.length} files to ${dir}`);
    // Drop what an earlier build left behind.
    for (const old of readdirSync(base)) {
      if (old !== manifest.id) rmSync(join(base, old), { recursive: true, force: true });
    }
  }
  root = dir;
  return root;
}
