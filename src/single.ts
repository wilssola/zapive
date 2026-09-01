// WhatsApp allows one desktop session per account: a second copy of the
// app makes the server drop the stream with "conflict" (status 440) and
// both copies reconnect in a loop. A lock file holding the running pid
// keeps that from happening — a second launch brings the first window
// forward and exits.
import { existsSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { dataDir } from "./paths.ts";
import { focusWindow } from "./platform.ts";

const LOCK = join(dataDir(), "instance.lock");

function alive(pid: number): boolean {
  if (!pid || pid === process.pid) return false;
  try {
    process.kill(pid, 0);
    return true;
  } catch (err) {
    // EPERM means the process exists but belongs to someone else.
    return (err as { code?: string }).code === "EPERM";
  }
}

// True when this process may run; false when another instance owns the
// session (it is raised instead).
export function claimSingleInstance(): boolean {
  try {
    if (existsSync(LOCK)) {
      const pid = Number(readFileSync(LOCK, "utf8").trim());
      if (alive(pid)) {
        void focusWindow();
        return false;
      }
    }
    writeFileSync(LOCK, String(process.pid));
    const release = () => {
      try {
        if (readFileSync(LOCK, "utf8").trim() === String(process.pid)) {
          rmSync(LOCK, { force: true });
        }
      } catch {
        // already released
      }
    };
    process.on("exit", release);
    for (const signal of ["SIGINT", "SIGTERM", "SIGHUP"] as const) {
      process.on(signal, () => {
        release();
        process.exit(0);
      });
    }
  } catch {
    // a lock we cannot write should never stop the app
  }
  return true;
}
