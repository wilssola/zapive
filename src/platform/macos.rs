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
