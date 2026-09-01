// Every OS-specific call the app makes, in one place: clipboard, file
// dialogs, opening links, the system theme, killing helper processes,
// capturing the microphone and protecting the vault key.
//
// Windows uses PowerShell (DPAPI, WinForms dialogs, NotifyIcon tray),
// macOS uses the shipped command line tools (osascript, pbcopy, security)
// and Linux uses the freedesktop ones (xdg-open, wl-copy/xclip, zenity,
// secret-tool), each with a fallback when the tool is missing.
import { execFile, execFileSync, spawn, spawnSync } from "node:child_process";
import { existsSync, readdirSync } from "node:fs";
import { promisify } from "node:util";
import { join } from "node:path";
import { IS_LINUX, IS_MAC, IS_WIN } from "./paths.ts";
import { t } from "./i18n.ts";

const execFileAsync = promisify(execFile);

function has(cmd: string): boolean {
  const probe = IS_WIN ? "where" : "which";
  const r = spawnSync(probe, [cmd], { stdio: "ignore" });
  return r.status === 0;
}

async function run(cmd: string, args: string[], timeout = 15_000): Promise<string> {
  try {
    const { stdout } = await execFileAsync(cmd, args, { timeout });
    return stdout;
  } catch {
    return "";
  }
}

async function powershell(script: string, sta = false, timeout = 15_000): Promise<string> {
  const args = ["-NoProfile", ...(sta ? ["-STA"] : []), "-Command", script];
  return run("powershell", args, timeout);
}

// ---- Opening files and links in the desktop's own handlers ----

export function openPath(target: string): void {
  const [cmd, args] = IS_WIN
    ? ["cmd", ["/c", "start", "", target.replace(/&/g, "^&")]]
    : IS_MAC
      ? ["open", [target]]
      : ["xdg-open", [target]];
  try {
    spawn(cmd as string, args as string[], { detached: true, stdio: "ignore" }).unref();
  } catch (err) {
    console.error("[platform] open failed:", err);
  }
}

// ---- Clipboard ----

export async function clipboardWrite(text: string): Promise<void> {
  if (IS_WIN) {
    const b64 = Buffer.from(text, "utf8").toString("base64");
    await powershell(
      "Set-Clipboard -Value ([Text.Encoding]::UTF8.GetString(" +
        `[Convert]::FromBase64String('${b64}')))`,
    );
    return;
  }
  const [cmd, args] = IS_MAC
    ? ["pbcopy", []]
    : has("wl-copy")
      ? ["wl-copy", []]
      : ["xclip", ["-selection", "clipboard"]];
  try {
    const proc = spawn(cmd as string, args as string[], { stdio: ["pipe", "ignore", "ignore"] });
    proc.stdin.end(text);
  } catch {
    // clipboard is best-effort
  }
}

export async function clipboardRead(): Promise<string> {
  if (IS_WIN) {
    const out = await powershell("Get-Clipboard -Raw");
    return out.replace(/\r?\n$/, "");
  }
  if (IS_MAC) return (await run("pbpaste", [])).replace(/\n$/, "");
  if (has("wl-paste")) return (await run("wl-paste", ["--no-newline"])).replace(/\n$/, "");
  return (await run("xclip", ["-selection", "clipboard", "-o"])).replace(/\n$/, "");
}

// Saves a bitmap sitting on the clipboard as PNG; null when there is none.
export async function clipboardImage(outPath: string): Promise<string | null> {
  if (IS_WIN) {
    const script =
      "Add-Type -AssemblyName System.Windows.Forms; Add-Type -AssemblyName System.Drawing; " +
      "$img = [System.Windows.Forms.Clipboard]::GetImage(); " +
      `if ($img -ne $null) { $img.Save('${outPath.replace(/'/g, "''")}', ` +
      "[System.Drawing.Imaging.ImageFormat]::Png); Write-Output 'ok' }";
    const out = await powershell(script, true);
    return out.includes("ok") ? outPath : null;
  }
  if (IS_MAC) {
    const esc = outPath.replace(/"/g, '\\"');
    const script = [
      "-e",
      'set png to (the clipboard as «class PNGf»)',
      "-e",
      `set f to open for access POSIX file "${esc}" with write permission`,
      "-e",
      "write png to f",
      "-e",
      "close access f",
    ];
    await run("osascript", script);
    return existsSync(outPath) ? outPath : null;
  }
  const [cmd, args] = has("wl-paste")
    ? ["wl-paste", ["--type", "image/png"]]
    : ["xclip", ["-selection", "clipboard", "-t", "image/png", "-o"]];
  try {
    const { writeFile } = await import("node:fs/promises");
    const out = execFileSync(cmd as string, args as string[], {
      maxBuffer: 64 * 1024 * 1024,
    });
    if (out.length === 0) return null;
    await writeFile(outPath, out);
    return outPath;
  } catch {
    return null;
  }
}

// ---- File chooser ----

export async function pickFile(kind: "image" | "audio" | "doc"): Promise<string | null> {
  const exts =
    kind === "image"
      ? ["jpg", "jpeg", "png", "webp", "gif"]
      : kind === "audio"
        ? ["ogg", "opus", "mp3", "m4a", "wav"]
        : [];
  const label =
    kind === "image" ? t("picker.images") : kind === "audio" ? t("picker.audio") : t("picker.all");

  if (IS_WIN) {
    const filter =
      exts.length > 0
        ? `${label} (${exts.map((e) => `*.${e}`).join(";")})|${exts.map((e) => `*.${e}`).join(";")}`
        : `${label} (*.*)|*.*`;
    const script = [
      "Add-Type -AssemblyName System.Windows.Forms;",
      "$d = New-Object System.Windows.Forms.OpenFileDialog;",
      `$d.Filter = '${filter}';`,
      "if ($d.ShowDialog() -eq 'OK') { $d.FileName }",
    ].join(" ");
    const out = await powershell(script, true, 120_000);
    return out.trim() || null;
  }

  if (IS_MAC) {
    const types =
      exts.length > 0 ? ` of type {${exts.map((e) => `"${e}"`).join(", ")}}` : "";
    const out = await run(
      "osascript",
      ["-e", `POSIX path of (choose file with prompt "${label}"${types})`],
      120_000,
    );
    return out.trim() || null;
  }

  if (has("zenity")) {
    const args = ["--file-selection", `--title=${label}`];
    if (exts.length > 0) args.push(`--file-filter=${label} | ${exts.map((e) => `*.${e}`).join(" ")}`);
    const out = await run("zenity", args, 120_000);
    return out.trim() || null;
  }
  if (has("kdialog")) {
    const filter = exts.length > 0 ? exts.map((e) => `*.${e}`).join(" ") : "*";
    const out = await run("kdialog", ["--getopenfilename", ".", filter], 120_000);
    return out.trim() || null;
  }
  console.error("[platform] no file dialog available (install zenity or kdialog)");
  return null;
}

// ---- System theme ----

export function systemDark(cb: (dark: boolean) => void): void {
  if (IS_WIN) {
    execFile(
      "reg",
      [
        "query",
        "HKCU\\Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize",
        "/v",
        "AppsUseLightTheme",
      ],
      (err, stdout) => cb(err ? true : !/0x1\s*$/m.test(stdout.trim())),
    );
    return;
  }
  if (IS_MAC) {
    execFile("defaults", ["read", "-g", "AppleInterfaceStyle"], (err, stdout) =>
      cb(!err && /dark/i.test(stdout)),
    );
    return;
  }
  execFile(
    "gsettings",
    ["get", "org.gnome.desktop.interface", "color-scheme"],
    (err, stdout) => {
      if (!err && stdout.trim() !== "") return cb(/dark/i.test(stdout));
      execFile("gsettings", ["get", "org.gnome.desktop.interface", "gtk-theme"], (e2, out2) =>
        cb(!e2 && /dark/i.test(out2)),
      );
    },
  );
}

// ---- Window focus (after a notification click) ----

export async function focusWindow(): Promise<void> {
  if (IS_WIN) {
    await powershell(
      "$s = New-Object -ComObject WScript.Shell; " +
        "$p = Get-Process -ErrorAction SilentlyContinue | " +
        "Where-Object { $_.MainWindowTitle -eq 'Zapive' } | Select-Object -First 1; " +
        "if ($p) { [void]$s.AppActivate($p.Id) }",
      false,
      10_000,
    );
    return;
  }
  if (IS_MAC) {
    await run("osascript", [
      "-e",
      `tell application "System Events" to set frontmost of (first process whose unix id is ${process.pid}) to true`,
    ]);
    return;
  }
  if (has("wmctrl")) await run("wmctrl", ["-a", "Zapive"]);
}

// ---- Helper processes ----

// ffplay and ffmpeg ignore a plain kill once they own a console, so the
// whole tree goes down: taskkill on Windows, the process group elsewhere.
export function killTree(proc: { pid?: number; kill: (s?: NodeJS.Signals) => boolean }): void {
  try {
    if (IS_WIN) {
      proc.kill();
      if (proc.pid) {
        spawn("taskkill", ["/PID", String(proc.pid), "/T", "/F"], { stdio: "ignore" });
      }
      return;
    }
    if (proc.pid) {
      try {
        process.kill(-proc.pid, "SIGKILL"); // the group, when detached
        return;
      } catch {
        // not a group leader
      }
    }
    proc.kill("SIGKILL");
  } catch {
    // already gone
  }
}

// ---- ffmpeg ----

const EXE = IS_WIN ? ".exe" : "";

let ffmpegCache: string | null | undefined;

export function findFfmpeg(): string {
  if (ffmpegCache !== undefined) return ffmpegCache ?? "ffmpeg";
  let found: string | null = null;
  if (IS_WIN) {
    // winget drops ffmpeg under a versioned directory that is not on PATH.
    const winget = join(process.env.LOCALAPPDATA ?? "", "Microsoft\\WinGet\\Packages");
    if (existsSync(winget)) {
      for (const dir of readdirSync(winget)) {
        if (!dir.toLowerCase().includes("ffmpeg")) continue;
        const stack = [join(winget, dir)];
        while (stack.length > 0 && !found) {
          const cur = stack.pop()!;
          for (const entry of readdirSync(cur, { withFileTypes: true })) {
            const full = join(cur, entry.name);
            if (entry.isDirectory()) stack.push(full);
            else if (entry.name.toLowerCase() === "ffmpeg.exe") {
              found = full;
              break;
            }
          }
        }
        if (found) break;
      }
    }
  } else {
    for (const dir of ["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin"]) {
      const candidate = join(dir, "ffmpeg");
      if (existsSync(candidate)) {
        found = candidate;
        break;
      }
    }
  }
  ffmpegCache = found;
  return found ?? "ffmpeg"; // fall back to PATH
}

// ffplay / ffprobe live next to ffmpeg.
export function ffTool(name: "ffplay" | "ffprobe"): string {
  const ff = findFfmpeg();
  const base = ff.endsWith(EXE) && EXE ? ff.slice(0, -EXE.length) : ff;
  const swapped = base.replace(/ffmpeg$/i, name) + EXE;
  return swapped;
}

// Microphone capture differs per platform: DirectShow needs the device
// name, avfoundation and pulse/alsa take a default index.
export async function micInput(): Promise<string[] | null> {
  if (IS_WIN) {
    const ff = findFfmpeg();
    let name: string | null = null;
    try {
      await execFileAsync(
        ff,
        ["-hide_banner", "-list_devices", "true", "-f", "dshow", "-i", "dummy"],
        { timeout: 20_000 },
      );
    } catch (err) {
      const out = String((err as { stderr?: string }).stderr ?? "");
      name = out.match(/"([^"]+)"\s*\(audio\)/)?.[1] ?? null;
    }
    return name ? ["-f", "dshow", "-i", `audio=${name}`] : null;
  }
  if (IS_MAC) return ["-f", "avfoundation", "-i", ":default"];
  if (has("pactl") || existsSync("/etc/pulse")) return ["-f", "pulse", "-i", "default"];
  return ["-f", "alsa", "-i", "default"];
}

// ---- Vault key protection ----
//
// The wrapped data key never leaves the machine unprotected: Windows
// binds it to the account with DPAPI, macOS stores it in the login
// keychain and Linux hands it to the Secret Service (libsecret). When no
// service is available the key is kept base64 with a warning, exactly as
// before.

const KEYCHAIN_SERVICE = "Zapive";
const KEYCHAIN_ACCOUNT = "vault";

function dpapi(op: "Protect" | "Unprotect", b64: string): string | null {
  const script =
    "Add-Type -AssemblyName System.Security; " +
    `[Convert]::ToBase64String([System.Security.Cryptography.ProtectedData]::${op}(` +
    `[Convert]::FromBase64String('${b64}'), $null, ` +
    "[System.Security.Cryptography.DataProtectionScope]::CurrentUser))";
  const r = spawnSync("powershell", ["-NoProfile", "-Command", script], {
    encoding: "utf8",
    timeout: 20_000,
  });
  const out = r.stdout?.trim();
  return r.status === 0 && out ? out : null;
}

export function wrapSecret(inner: string): string {
  const b64 = Buffer.from(inner, "utf8").toString("base64");
  if (IS_WIN) {
    const guarded = dpapi("Protect", b64);
    if (guarded) return "dpapi:" + guarded;
  } else if (IS_MAC) {
    const r = spawnSync(
      "security",
      [
        "add-generic-password",
        "-U",
        "-s",
        KEYCHAIN_SERVICE,
        "-a",
        KEYCHAIN_ACCOUNT,
        "-w",
        b64,
      ],
      { stdio: "ignore" },
    );
    if (r.status === 0) return "keychain:";
  } else if (has("secret-tool")) {
    const r = spawnSync(
      "secret-tool",
      ["store", "--label=Zapive", "service", KEYCHAIN_SERVICE, "account", KEYCHAIN_ACCOUNT],
      { input: b64, stdio: ["pipe", "ignore", "ignore"] },
    );
    if (r.status === 0) return "secret:";
  }
  console.warn("[vault] no OS key store available — storing the key without OS binding");
  return "raw:" + b64;
}

export function unwrapSecret(stored: string): string {
  if (stored.startsWith("dpapi:")) {
    const plain = dpapi("Unprotect", stored.slice(6));
    if (!plain) throw new Error("DPAPI unprotect failed (different Windows user?)");
    return Buffer.from(plain, "base64").toString("utf8");
  }
  if (stored.startsWith("keychain:")) {
    const r = spawnSync(
      "security",
      ["find-generic-password", "-s", KEYCHAIN_SERVICE, "-a", KEYCHAIN_ACCOUNT, "-w"],
      { encoding: "utf8" },
    );
    const out = r.stdout?.trim();
    if (r.status !== 0 || !out) throw new Error("keychain lookup failed");
    return Buffer.from(out, "base64").toString("utf8");
  }
  if (stored.startsWith("secret:")) {
    const r = spawnSync(
      "secret-tool",
      ["lookup", "service", KEYCHAIN_SERVICE, "account", KEYCHAIN_ACCOUNT],
      { encoding: "utf8" },
    );
    const out = r.stdout?.trim();
    if (r.status !== 0 || !out) throw new Error("secret service lookup failed");
    return Buffer.from(out, "base64").toString("utf8");
  }
  if (stored.startsWith("raw:")) return Buffer.from(stored.slice(4), "base64").toString("utf8");
  throw new Error("unknown key wrapping format");
}

// ---- Tray ----
//
// Slint has no tray API, so Windows keeps its PowerShell NotifyIcon and
// the other platforms simply run without one (closing the window quits).

export function startTray(
  onShow: () => void,
  onExit: () => void,
  iconPath?: string,
): { stop: () => void } | null {
  if (!IS_WIN) return null;
  const icon =
    iconPath && existsSync(iconPath)
      ? `New-Object System.Drawing.Icon('${iconPath.replace(/'/g, "''")}')`
      : "[System.Drawing.SystemIcons]::Application";
  const script = [
    "Add-Type -AssemblyName System.Windows.Forms, System.Drawing;",
    "$ni = New-Object System.Windows.Forms.NotifyIcon;",
    `$ni.Icon = ${icon};`,
    "$ni.Text = 'Zapive';",
    "$ni.Visible = $true;",
    "$menu = New-Object System.Windows.Forms.ContextMenuStrip;",
    `[void]$menu.Items.Add('${t("tray.open")}', $null, { [Console]::Out.WriteLine('show') });`,
    `[void]$menu.Items.Add('${t("tray.exit")}', $null, { [Console]::Out.WriteLine('exit'); $ni.Visible = $false; [System.Windows.Forms.Application]::Exit() });`,
    "$ni.ContextMenuStrip = $menu;",
    "$ni.add_MouseClick({ if ($_.Button -eq 'Left') { [Console]::Out.WriteLine('show') } });",
    "[System.Windows.Forms.Application]::Run();",
  ].join(" ");
  const tray = spawn("powershell", ["-NoProfile", "-STA", "-Command", script], {
    stdio: ["ignore", "pipe", "ignore"],
  });
  tray.stdout.setEncoding("utf8");
  tray.stdout.on("data", (chunk: string) => {
    for (const line of chunk.split(/\r?\n/)) {
      const cmd = line.trim();
      if (cmd === "show") onShow();
      if (cmd === "exit") onExit();
    }
  });
  return {
    stop: () => {
      try {
        tray.kill();
      } catch {
        // already gone
      }
    },
  };
}

export { IS_WIN, IS_MAC, IS_LINUX };
