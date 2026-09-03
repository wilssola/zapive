use windows::Win32::UI::WindowsAndMessaging::{
    FindWindowW, IsIconic, SW_RESTORE, SetForegroundWindow, ShowWindow,
};
use windows::core::w;

// Brings the running Zapive window to the foreground (used when a second
// instance starts, and when a notification is clicked).
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

// Opens a file or URL with whatever the system associates with it.
pub fn open_path(target: &str) {
    let _ = std::process::Command::new("cmd")
        .args(["/c", "start", "", &target.replace('&', "^&")])
        .spawn();
}

// Whether Windows apps are set to dark mode (defaults dark on error).
pub fn system_dark() -> bool {
    let out = std::process::Command::new("reg")
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
