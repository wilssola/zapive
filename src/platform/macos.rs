pub fn focus_window() {
    let script = format!(
        "tell application \"System Events\" to set frontmost of (first process whose unix id is {}) to true",
        std::process::id()
    );
    let _ = std::process::Command::new("osascript").args(["-e", &script]).spawn();
}

pub fn open_path(target: &str) {
    let _ = std::process::Command::new("open").arg(target).spawn();
}

pub fn register_app_id(_icon_path: &str) {}

// Banner through the user notification center under our own bundle id, so
// launches from Zapive.app show the app icon instead of the script icon.
// osascript stays as the fallback for bare-binary dev runs.
pub fn toast(title: &str, body: &str, _jid: Option<String>) {
    static BUNDLE: std::sync::Once = std::sync::Once::new();
    BUNDLE.call_once(|| {
        let _ = notify_rust::set_application("io.github.wilssola.Zapive");
    });
    if notify_rust::Notification::new().summary(title).body(body).show().is_ok() {
        return;
    }
    let script = format!(
        "display notification \"{}\" with title \"{}\"",
        body.replace('"', "'"),
        title.replace('"', "'")
    );
    let _ = std::process::Command::new("osascript").args(["-e", &script]).spawn();
}

pub fn system_dark() -> bool {
    std::process::Command::new("defaults")
        .args(["read", "-g", "AppleInterfaceStyle"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).to_lowercase().contains("dark"))
        .unwrap_or(false)
}

// ---- vault key wrapping: the login keychain holds the secret itself ----

const SERVICE: &str = "Zapive";
const ACCOUNT: &str = "vault";

pub fn wrap_secret(inner: &str) -> String {
    use base64::Engine as _;
    let b64 = base64::engine::general_purpose::STANDARD.encode(inner.as_bytes());
    let ok = std::process::Command::new("security")
        .args(["add-generic-password", "-U", "-s", SERVICE, "-a", ACCOUNT, "-w", &b64])
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if ok {
        "keychain:".to_string()
    } else {
        eprintln!("[vault] keychain store failed; storing with weak protection");
        format!("raw:{b64}")
    }
}

pub fn unwrap_secret(stored: &str) -> Result<String, String> {
    use base64::Engine as _;
    let b64 = if stored == "keychain:" {
        let out = std::process::Command::new("security")
            .args(["find-generic-password", "-s", SERVICE, "-a", ACCOUNT, "-w"])
            .output()
            .map_err(|e| e.to_string())?;
        if !out.status.success() {
            return Err("keychain lookup failed".into());
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
