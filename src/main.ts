import "./env.ts";
import * as slint from "slint-ui";
import { existsSync, readdirSync, readFileSync, renameSync } from "node:fs";
import { spawn, execFile } from "node:child_process";
import { join } from "node:path";
import { Bridge } from "./bridge.ts";
import type { AppWindow } from "./bridge.ts";
import { MediaService } from "./media.ts";
import { WhatsAppService } from "./whatsapp.ts";
import { Db } from "./db.ts";
import { setLocale, t } from "./i18n.ts";
import type { Locale } from "./i18n.ts";

const db = new Db();

// ---- Language: pt / en / follow the system locale. Applied before the UI
// loads; Slint strings come from the gettext catalog under i18n/. ----
const langMode = db.settingGet("language") ?? "system";
const systemLocale: Locale = (Intl.DateTimeFormat().resolvedOptions().locale ?? "en")
  .toLowerCase()
  .startsWith("pt")
  ? "pt"
  : "en";
const locale: Locale = langMode === "pt" ? "pt" : langMode === "en" ? "en" : systemLocale;
setLocale(locale);
process.env.LANGUAGE = locale === "pt" ? "pt_BR" : "en_US";
slint.initTranslations("zapive", new URL("../i18n/", import.meta.url));

const ui = slint.loadFile(new URL("../ui/app.slint", import.meta.url)) as {
  AppWindow: new () => AppWindow;
};
const win = new ui.AppWindow();
win.language_mode = langMode;

win.language_changed = (mode: string) => {
  db.settingSet("language", mode);
  win.settings_status = t("lang.restart");
};

// One-time migration from the old file-based storage into SQLite.
function migrateLegacyFiles() {
  if (db.get("auth:creds") === null && existsSync("auth_info/creds.json")) {
    for (const f of readdirSync("auth_info")) {
      if (!f.endsWith(".json")) continue;
      const name = f.slice(0, -".json".length);
      const content = readFileSync(join("auth_info", f), "utf8");
      db.set(name === "creds" ? "auth:creds" : `auth:${name}`, content);
    }
    renameSync("auth_info", "auth_info.bak");
    console.log("[migrate] auth_info -> zapive.db");
  }
}

const media = new MediaService();
media.setDb(db);
const bridge = new Bridge(win, media);
const service = new WhatsAppService(bridge, db);
bridge.setService(service);
media.setService(service);

let booted = false;
function boot() {
  if (booted) return;
  booted = true;
  migrateLegacyFiles();
  bridge.init(db);
  if (db.get("store:chats") === null && existsSync("data_store.json")) {
    try {
      bridge.importLegacyStore(readFileSync("data_store.json", "utf8"));
      renameSync("data_store.json", "data_store.json.bak");
      console.log("[migrate] data_store.json -> zapive.db");
    } catch (err) {
      console.error("legacy store migration failed:", err);
    }
  }
  win.pin_set = db.hasPin();
  win.screen = db.get("auth:creds") !== null ? "main" : "login";
  void service.start().catch((err) => {
    win.status_text = t("status.connectFailed", err instanceof Error ? err.message : String(err));
  });
}

// ---- Theme: dark / light / follow the Windows system theme ----
let themeMode = db.settingGet("theme") ?? "dark";
let systemDark = true;

function readSystemDark(cb: (dark: boolean) => void) {
  execFile(
    "reg",
    [
      "query",
      "HKCU\\Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize",
      "/v",
      "AppsUseLightTheme",
    ],
    (err, stdout) => {
      if (err) return cb(true);
      cb(!/0x1\s*$/m.test(stdout.trim()));
    },
  );
}

function applyTheme() {
  win.theme_mode = themeMode;
  win.dark_theme = themeMode === "dark" || (themeMode === "system" && systemDark);
}

win.theme_changed = (mode) => {
  themeMode = mode;
  db.settingSet("theme", mode);
  applyTheme();
};

readSystemDark((dark) => {
  systemDark = dark;
  applyTheme();
});
setInterval(() => {
  if (themeMode !== "system") return;
  readSystemDark((dark) => {
    if (dark !== systemDark) {
      systemDark = dark;
      applyTheme();
    }
  });
}, 15_000);
applyTheme();

win.unlock = (pin) => {
  if (db.unlock(pin)) {
    win.lock_error = "";
    boot();
  } else {
    win.lock_error = t("pin.wrong");
  }
};

win.save_pin = (current, next) => {
  const err = db.changePin(current, next);
  if (err) {
    win.settings_status = err === "wrong-pin" ? t("pin.wrongCurrent") : t("pin.format");
  } else {
    win.pin_set = true;
    win.settings_status = t("pin.saved");
  }
};

win.logout = () => {
  win.settings_open = false;
  win.status_text = t("status.loggingOut");
  void service.logout();
};

win.remove_pin = (current) => {
  const err = db.changePin(current, null);
  if (err) {
    win.settings_status = err === "wrong-pin" ? t("pin.wrongCurrent") : t("pin.format");
  } else {
    win.pin_set = false;
    win.settings_status = t("pin.removed");
  }
};

if (db.hasPin()) {
  win.screen = "locked";
} else {
  db.open();
  boot();
}

// ---- System tray: closing the window hides it; the app keeps running.
// A persistent PowerShell NotifyIcon reports clicks over stdout. ----
const trayScript = [
  "Add-Type -AssemblyName System.Windows.Forms, System.Drawing;",
  "$ni = New-Object System.Windows.Forms.NotifyIcon;",
  "$ni.Icon = [System.Drawing.SystemIcons]::Application;",
  "$ni.Text = 'Zapive';",
  "$ni.Visible = $true;",
  "$menu = New-Object System.Windows.Forms.ContextMenuStrip;",
  `[void]$menu.Items.Add('${t("tray.open")}', $null, { [Console]::Out.WriteLine('show') });`,
  `[void]$menu.Items.Add('${t("tray.exit")}', $null, { [Console]::Out.WriteLine('exit'); $ni.Visible = $false; [System.Windows.Forms.Application]::Exit() });`,
  "$ni.ContextMenuStrip = $menu;",
  "$ni.add_MouseClick({ if ($_.Button -eq 'Left') { [Console]::Out.WriteLine('show') } });",
  "[System.Windows.Forms.Application]::Run();",
].join(" ");

const tray = spawn("powershell", ["-NoProfile", "-STA", "-Command", trayScript], {
  stdio: ["ignore", "pipe", "ignore"],
});
tray.stdout.setEncoding("utf8");
tray.stdout.on("data", (chunk: string) => {
  for (const line of chunk.split(/\r?\n/)) {
    const cmd = line.trim();
    if (cmd === "show") win.show();
    if (cmd === "exit") slint.quitEventLoop();
  }
});
process.on("exit", () => {
  try {
    tray.kill();
  } catch {
    // already gone
  }
});

win.show();
await slint.runEventLoop({ quitOnLastWindowClosed: false });
tray.kill();
process.exit(0);
