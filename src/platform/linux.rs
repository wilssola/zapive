pub fn focus_window() {
    let _ = std::process::Command::new("wmctrl").args(["-a", "Zapive"]).spawn();
}

// A ringing call has to be seen. The title is deliberately not
// translated: that is what identifies the window here.
pub fn focus_call_window() {
    let _ = std::process::Command::new("wmctrl").args(["-a", "Zapive Call"]).spawn();
}

// Only Windows guards the foreground; wmctrl needs no handover.
pub fn allow_foreground(_pid: u32) {}

// ---- start at login: the XDG autostart entry ----

fn autostart_file() -> std::path::PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            std::path::PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".config")
        })
        .join("autostart")
        .join("zapive.desktop")
}

pub fn autostart_enabled() -> bool {
    autostart_file().is_file()
}

pub fn autostart_set(on: bool) -> Result<(), String> {
    let file = autostart_file();
    if !on {
        return match std::fs::remove_file(&file) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.to_string()),
            _ => Ok(()),
        };
    }
    // Inside an AppImage current_exe() points at the throwaway mount under
    // /tmp; $APPIMAGE is the path that still exists after a reboot.
    let exe = match std::env::var_os("APPIMAGE") {
        Some(path) => std::path::PathBuf::from(path),
        None => std::env::current_exe().map_err(|e| e.to_string())?,
    };
    let entry = format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name=Zapive\n\
         Exec=\"{}\" --autostart\n\
         Icon=io.github.wilssola.Zapive\n\
         Terminal=false\n\
         X-GNOME-Autostart-enabled=true\n",
        exe.display()
    );
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    std::fs::write(&file, entry).map_err(|e| e.to_string())
}

pub fn open_path(target: &str) {
    let _ = std::process::Command::new("xdg-open").arg(target).spawn();
}

pub fn register_app_id(_icon_path: &str) {}

// D-Bus notification tagged with our desktop entry, so the banner carries
// the app icon and works inside the Flatpak sandbox (where notify-send may
// not exist). notify-send stays as the fallback.
pub fn toast(title: &str, body: &str, _jid: Option<String>) {
    let sent = notify_rust::Notification::new()
        .summary(title)
        .body(body)
        .appname("Zapive")
        .icon("io.github.wilssola.Zapive")
        .hint(notify_rust::Hint::DesktopEntry("io.github.wilssola.Zapive".into()))
        .show()
        .is_ok();
    if !sent {
        let _ = std::process::Command::new("notify-send").args([title, body]).spawn();
    }
}

pub fn system_dark() -> bool {
    std::process::Command::new("gsettings")
        .args(["get", "org.gnome.desktop.interface", "color-scheme"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).contains("dark"))
        .unwrap_or(true)
}

// ---- vault key wrapping: the Secret Service holds the secret itself ----

pub fn wrap_secret(inner: &str) -> String {
    use base64::Engine as _;
    use std::io::Write as _;
    let b64 = base64::engine::general_purpose::STANDARD.encode(inner.as_bytes());
    let child = std::process::Command::new("secret-tool")
        .args(["store", "--label=Zapive", "service", "Zapive", "account", "vault"])
        .stdin(std::process::Stdio::piped())
        .spawn();
    if let Ok(mut child) = child {
        if let Some(stdin) = child.stdin.as_mut() {
            let _ = stdin.write_all(b64.as_bytes());
        }
        if child.wait().map(|s| s.success()).unwrap_or(false) {
            return "secret:".to_string();
        }
    }
    eprintln!("[vault] secret-tool store failed; storing with weak protection");
    format!("raw:{b64}")
}

pub fn unwrap_secret(stored: &str) -> Result<String, String> {
    use base64::Engine as _;
    let b64 = if stored == "secret:" {
        let out = std::process::Command::new("secret-tool")
            .args(["lookup", "service", "Zapive", "account", "vault"])
            .output()
            .map_err(|e| e.to_string())?;
        if !out.status.success() {
            return Err("secret-tool lookup failed".into());
        }
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    } else if let Some(rest) = stored.strip_prefix("raw:") {
        rest.to_string()
    } else {
        return Err("unknown key wrapping format".into());
    };
    let plain = base64::engine::general_purpose::STANDARD
        .decode(b64.as_bytes())
        .map_err(|e| e.to_string())?;
    String::from_utf8(plain).map_err(|e| e.to_string())
}
