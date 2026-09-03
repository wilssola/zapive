pub fn focus_window() {
    let _ = std::process::Command::new("wmctrl").args(["-a", "Zapive"]).spawn();
}

pub fn open_path(target: &str) {
    let _ = std::process::Command::new("xdg-open").arg(target).spawn();
}

pub fn register_app_id(_icon_path: &str) {}

pub fn toast(title: &str, body: &str, _jid: Option<String>) {
    let _ = std::process::Command::new("notify-send").args([title, body]).spawn();
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
