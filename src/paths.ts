// Where Zapive keeps its files, per platform, and where it finds the
// resources it ships with. Nothing is written next to the executable:
// the vault and the media cache live in the user's own directories.
import { existsSync, mkdirSync, readdirSync, renameSync, rmSync, statSync } from "node:fs";
import { homedir } from "node:os";
import { dirname, join, resolve } from "node:path";

const APP = "Zapive";

export const IS_WIN = process.platform === "win32";
export const IS_MAC = process.platform === "darwin";
export const IS_LINUX = !IS_WIN && !IS_MAC;

// True when running from a packaged single-file build.
export function isPackaged(): boolean {
  try {
    const sea = process.getBuiltinModule?.("node:sea") as
      | { isSea?: () => boolean }
      | undefined;
    return !!sea?.isSea?.();
  } catch {
    return false;
  }
}

// Resources (ui/, i18n/) sit beside the executable once packaged, and at
// the repository root while developing.
export function resourceRoot(): string {
  if (isPackaged()) return dirname(process.execPath);
  return resolve(dirname(currentFile()), "..");
}

function currentFile(): string {
  // The bundle is CommonJS and has __filename; running from source, the
  // entry point is the script Node was given.
  if (typeof __filename === "string") return __filename;
  return process.argv[1] ?? process.cwd();
}

// %APPDATA%\Zapive, ~/Library/Application Support/Zapive, ~/.local/share/zapive
export function dataDir(): string {
  if (IS_WIN) {
    return join(process.env.APPDATA ?? join(homedir(), "AppData", "Roaming"), APP);
  }
  if (IS_MAC) return join(homedir(), "Library", "Application Support", APP);
  return join(
    process.env.XDG_DATA_HOME ?? join(homedir(), ".local", "share"),
    APP.toLowerCase(),
  );
}

// Cache is separate: it can be deleted without losing the account.
export function cacheDir(): string {
  if (IS_WIN) {
    return join(
      process.env.LOCALAPPDATA ?? join(homedir(), "AppData", "Local"),
      APP,
      "Cache",
    );
  }
  if (IS_MAC) return join(homedir(), "Library", "Caches", APP);
  return join(
    process.env.XDG_CACHE_HOME ?? join(homedir(), ".cache"),
    APP.toLowerCase(),
  );
}

export const DB_PATH = join(dataDir(), "zapive.db");
export const MEDIA_CACHE = join(cacheDir(), "media");

export function ensureDirs(): void {
  mkdirSync(dataDir(), { recursive: true });
  mkdirSync(MEDIA_CACHE, { recursive: true });
}

// Moves a directory's contents across, keeping whatever the target
// already has, and drops the source once it is empty.
function mergeDir(from: string, to: string): number {
  let moved = 0;
  mkdirSync(to, { recursive: true });
  for (const entry of readdirSync(from, { withFileTypes: true })) {
    const src = join(from, entry.name);
    const dst = join(to, entry.name);
    if (entry.isDirectory()) {
      moved += mergeDir(src, dst);
      continue;
    }
    if (existsSync(dst)) continue;
    try {
      renameSync(src, dst);
      moved++;
    } catch {
      // a file in use: leave it behind
    }
  }
  try {
    if (readdirSync(from).length === 0) rmSync(from, { recursive: true });
  } catch {
    // not empty yet
  }
  return moved;
}

// Earlier builds kept everything in the working directory; move it over
// so an existing session and its downloads survive the upgrade.
export function migrateFromCwd(): void {
  if (existsSync("zapive.db") && !existsSync(DB_PATH)) {
    try {
      mkdirSync(dirname(DB_PATH), { recursive: true });
      renameSync("zapive.db", DB_PATH);
      console.log(`[migrate] zapive.db -> ${DB_PATH}`);
    } catch (err) {
      console.error("[migrate] zapive.db failed:", err);
    }
  }
  // The cache may already hold files downloaded since the move, so the
  // old directory is merged in rather than skipped.
  if (existsSync("media_cache") && statSync("media_cache").isDirectory()) {
    try {
      const moved = mergeDir("media_cache", MEDIA_CACHE);
      if (moved > 0) console.log(`[migrate] media_cache -> ${MEDIA_CACHE} (${moved} files)`);
    } catch (err) {
      console.error("[migrate] media_cache failed:", err);
    }
  }
  // WAL companions travel with the database.
  for (const suffix of ["-wal", "-shm"]) {
    const from = `zapive.db${suffix}`;
    if (existsSync(from) && !existsSync(DB_PATH + suffix)) {
      try {
        renameSync(from, DB_PATH + suffix);
      } catch {
        // harmless: SQLite rebuilds these
      }
    }
  }
}
