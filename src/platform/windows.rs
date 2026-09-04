use std::os::windows::process::CommandExt as _;
use windows::Win32::UI::WindowsAndMessaging::{
    AllowSetForegroundWindow, FindWindowW, IsIconic, SW_RESTORE, SetForegroundWindow, ShowWindow,
};
use windows::core::w;

// Console programs spawned from a GUI build flash a black window; this
// keeps them invisible.
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

fn quiet(program: &str) -> std::process::Command {
    let mut cmd = std::process::Command::new(program);
    cmd.creation_flags(CREATE_NO_WINDOW);
    cmd
}

// Brings the running Zapive window to the foreground. Callers make the
// window visible through Slint first: unhiding it behind Slint's back
// leaves the toolkit believing it is still parked in the tray, and the
// next click on X then hides nothing.
pub fn focus_window() {
    unsafe {
        if let Ok(hwnd) = FindWindowW(None, w!("Zapive")) {
            if IsIconic(hwnd).as_bool() {
                let _ = ShowWindow(hwnd, SW_RESTORE);
            }
            let _ = SetForegroundWindow(hwnd);
        }
    }
}

// A ringing call has to be seen, so the call window is pulled forward the
// same way the tray pulls the main one. Its title is deliberately not
// translated: that is what identifies the window here.
pub fn focus_call_window() {
    unsafe {
        if let Ok(hwnd) = FindWindowW(None, w!("Zapive Call")) {
            if IsIconic(hwnd).as_bool() {
                let _ = ShowWindow(hwnd, SW_RESTORE);
            }
            let _ = SetForegroundWindow(hwnd);
        }
    }
}

// Windows only lets the process that owns the foreground hand it over. A
// second instance the user just launched holds that right, so it passes
// it to the instance that will actually raise its window.
pub fn allow_foreground(pid: u32) {
    unsafe {
        let _ = AllowSetForegroundWindow(pid);
    }
}

// ---- start with Windows: a per-user Run entry, no elevation needed ----

const RUN_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";

pub fn autostart_enabled() -> bool {
    quiet("reg")
        .args(["query", RUN_KEY, "/v", "Zapive"])
        .output()
        .is_ok_and(|out| out.status.success())
}

pub fn autostart_set(on: bool) -> Result<(), String> {
    if !on && !autostart_enabled() {
        return Ok(());
    }
    let out = if on {
        // Rewritten on every enable so a moved or updated install keeps a
        // valid path. --autostart is what makes the login launch land in
        // the tray instead of popping the window open.
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        let command = format!("\"{}\" --autostart", exe.display());
        quiet("reg")
            .args(["add", RUN_KEY, "/v", "Zapive", "/t", "REG_SZ", "/d", &command, "/f"])
            .output()
    } else {
        quiet("reg").args(["delete", RUN_KEY, "/v", "Zapive", "/f"]).output()
    };
    let out = out.map_err(|e| e.to_string())?;
    if out.status.success() {
        return Ok(());
    }
    Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
}

// Opens a file or URL with whatever the system associates with it.
pub fn open_path(target: &str) {
    let _ = quiet("cmd")
        .args(["/c", "start", "", &target.replace('&', "^&")])
        .spawn();
}

// Toast identity: the header name/icon come from the AppUserModelID
// registration, not the toast payload. Re-registered every launch.
pub fn register_app_id(icon_path: &str) {
    unsafe {
        use windows::core::w;
        let _ = windows::Win32::UI::Shell::SetCurrentProcessExplicitAppUserModelID(w!("Zapive"));
    }
    let key = r"HKCU\Software\Classes\AppUserModelId\Zapive";
    for (name, value) in [("DisplayName", "Zapive"), ("IconUri", icon_path)] {
        let _ = quiet("reg")
            .args(["add", key, "/v", name, "/t", "REG_SZ", "/d", value, "/f"])
            .output();
    }
}

// A native toast; clicking it reopens the conversation.
pub fn toast(title: &str, body: &str, jid: Option<String>) {
    use tauri_winrt_notification::Toast;
    let toast = Toast::new("Zapive")
        .title(title)
        .text1(if body.is_empty() { " " } else { body });
    let result = match jid {
        Some(jid) => toast
            .on_activated(move |_| {
                let jid = jid.clone();
                crate::bridge::ui_apply(move |b| b.on_notification_activated(&jid));
                Ok(())
            })
            .show(),
        None => toast.show(),
    };
    if let Err(e) = result {
        eprintln!("[notify] toast failed: {e}");
    }
}

// Whether Windows apps are set to dark mode (defaults dark on error).
pub fn system_dark() -> bool {
    let out = quiet("reg")
        .args([
            "query",
            r"HKCU\Software\Microsoft\Windows\CurrentVersion\Themes\Personalize",
            "/v",
            "AppsUseLightTheme",
        ])
        .output();
    match out {
        Ok(out) => !String::from_utf8_lossy(&out.stdout).contains("0x1"),
        Err(_) => true,
    }
}

// ---- vault key wrapping: DPAPI binds the data key to this Windows user ----

use windows::Win32::Foundation::{HLOCAL, LocalFree};
use windows::Win32::Security::Cryptography::{
    CRYPT_INTEGER_BLOB, CryptProtectData, CryptUnprotectData,
};

fn dpapi(input: &[u8], protect: bool) -> Result<Vec<u8>, String> {
    let blob_in =
        CRYPT_INTEGER_BLOB { cbData: input.len() as u32, pbData: input.as_ptr() as *mut u8 };
    let mut blob_out = CRYPT_INTEGER_BLOB::default();
    unsafe {
        let result = if protect {
            CryptProtectData(&blob_in, None, None, None, None, 0, &mut blob_out)
        } else {
            CryptUnprotectData(&blob_in, None, None, None, None, 0, &mut blob_out)
        };
        result.map_err(|e| e.to_string())?;
        let out = std::slice::from_raw_parts(blob_out.pbData, blob_out.cbData as usize).to_vec();
        let _ = LocalFree(HLOCAL(blob_out.pbData as _));
        Ok(out)
    }
}

pub fn wrap_secret(inner: &str) -> String {
    use base64::Engine as _;
    match dpapi(inner.as_bytes(), true) {
        Ok(wrapped) => format!("dpapi:{}", base64::engine::general_purpose::STANDARD.encode(wrapped)),
        Err(e) => {
            eprintln!("[vault] DPAPI protect failed ({e}); storing with weak protection");
            format!("raw:{}", base64::engine::general_purpose::STANDARD.encode(inner.as_bytes()))
        }
    }
}

pub fn unwrap_secret(stored: &str) -> Result<String, String> {
    use base64::Engine as _;
    if let Some(b64) = stored.strip_prefix("dpapi:") {
        let wrapped = base64::engine::general_purpose::STANDARD
            .decode(b64)
            .map_err(|e| e.to_string())?;
        let plain = dpapi(&wrapped, false)
            .map_err(|e| format!("DPAPI unprotect failed (different Windows user?): {e}"))?;
        return String::from_utf8(plain).map_err(|e| e.to_string());
    }
    if let Some(b64) = stored.strip_prefix("raw:") {
        let plain = base64::engine::general_purpose::STANDARD
            .decode(b64)
            .map_err(|e| e.to_string())?;
        return String::from_utf8(plain).map_err(|e| e.to_string());
    }
    Err("unknown key wrapping format".into())
}
