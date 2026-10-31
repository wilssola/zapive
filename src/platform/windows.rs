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

pub fn set_title_bar_theme(dark: bool, glass: bool) {
    use windows::Win32::Foundation::{BOOL, COLORREF};
    use windows::Win32::Graphics::Dwm::{
        DWMWA_BORDER_COLOR, DWMWA_CAPTION_COLOR, DWMWA_TEXT_COLOR,
        DWMWA_USE_IMMERSIVE_DARK_MODE, DWMWA_COLOR_DEFAULT, DwmSetWindowAttribute,
    };

    unsafe {
        let Ok(hwnd) = FindWindowW(None, w!("Zapive")) else { return };
        let dark_mode = BOOL::from(dark);
        let caption = COLORREF(if glass { DWMWA_COLOR_DEFAULT } else if dark { 0x00211b11 } else { 0x00f5f2f0 });
        let text = COLORREF(if dark { 0x00efede9 } else { 0x00211b11 });
        let border = COLORREF(if dark { 0x003e3424 } else { 0x00e7e4e0 });
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_USE_IMMERSIVE_DARK_MODE,
            &dark_mode as *const _ as _,
            std::mem::size_of_val(&dark_mode) as u32,
        );
        for (attribute, color) in [
            (DWMWA_CAPTION_COLOR, caption),
            (DWMWA_TEXT_COLOR, text),
            (DWMWA_BORDER_COLOR, border),
        ] {
            let _ = DwmSetWindowAttribute(
                hwnd,
                attribute,
                &color as *const _ as _,
                std::mem::size_of_val(&color) as u32,
            );
        }
    }
}

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
// Opens a file or URL with whatever the system associates with it.
pub fn open_path(target: &str) {
    let _ = quiet("cmd").args(["/c", "start", "", &target.replace('&', "^&")]).spawn();
}

// ---- start with Windows: a per-user Run entry, no elevation needed ----
const RUN_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";

pub fn autostart_enabled() -> bool {
    quiet("reg").args(["query", RUN_KEY, "/v", "Zapive"]).output().is_ok_and(|out| out.status.success())
}

pub fn autostart_set(on: bool) -> Result<(), String> {
    if !on && !autostart_enabled() { return Ok(()); }
    let out = if on {
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        let command = format!("\"{}\" --autostart", exe.display());
        quiet("reg").args(["add", RUN_KEY, "/v", "Zapive", "/t", "REG_SZ", "/d", &command, "/f"]).output()
    } else {
        quiet("reg").args(["delete", RUN_KEY, "/v", "Zapive", "/f"]).output()
    }.map_err(|e| e.to_string())?;
    if out.status.success() { Ok(()) } else { Err(String::from_utf8_lossy(&out.stderr).trim().to_string()) }
}

pub fn register_app_id(icon_path: &str) {
    unsafe { let _ = windows::Win32::UI::Shell::SetCurrentProcessExplicitAppUserModelID(w!("Zapive")); }
    let key = r"HKCU\Software\Classes\AppUserModelId\Zapive";
    for (name, value) in [("DisplayName", "Zapive"), ("IconUri", icon_path)] {
        let _ = quiet("reg").args(["add", key, "/v", name, "/t", "REG_SZ", "/d", value, "/f"]).output();
    }
}


pub fn toast(title: &str, body: &str, payload: super::ToastPayload) {
    use windows::UI::Notifications::{
        ToastNotification, ToastNotificationManager,
    };
    use windows::{Data::Xml::Dom::XmlDocument, core::{HSTRING, Interface as _}};

    let title = xml_escape(title);
    let body = xml_escape(body);
    let mut actions = String::new();
    let mut inputs = String::new();
    let mut image = String::new();
    if let Some(token) = payload.token.as_deref() {
        actions.push_str(&format!(
            "<action content=\"Reply\" arguments=\"reply:{token}\" activationType=\"foreground\" hint-inputId=\"reply-input\"/>"
        ));
        if payload.play {
            actions.push_str(&format!(
                "<action content=\"Play\" arguments=\"play:{token}\" activationType=\"background\"/>"
            ));
        }
        let placeholder = xml_escape(&crate::i18n::t("notify.reply_placeholder"));
        inputs.push_str(&format!(
            "<input id=\"reply-input\" type=\"text\" placeHolderContent=\"{placeholder}\"/>"
        ));
    }
    if let Some(path) = payload.thumbnail.as_deref() {
        if let Ok(uri) = url::Url::from_file_path(path) {
            image = format!("<image placement=\"appLogoOverride\" src=\"{}\"/>", xml_escape(uri.as_str()));
        }
    }
    let xml = format!(
        "<toast><visual><binding template=\"ToastGeneric\"><text>{title}</text><text>{body}</text>{image}</binding></visual>{}<actions>{inputs}{actions}</actions></toast>",
        if payload.token.is_some() { "<header id=\"zapive\" title=\"Zapive\" arguments=\"default\"/>" } else { "" }
    );
    let result = (|| -> windows::core::Result<()> {
        let document = XmlDocument::new()?;
        document.LoadXml(&HSTRING::from(xml))?;
        let notification = ToastNotification::CreateToastNotification(&document)?;
        let notifier = ToastNotificationManager::CreateToastNotifierWithId(&HSTRING::from("Zapive"))?;
        let activation_token = payload.token.clone();
        let _registration = notification.Activated(&windows::Foundation::TypedEventHandler::<
            ToastNotification,
            windows::core::IInspectable,
        >::new(move |_, args| {
                let Some(args) = args else { return Ok(()) };
                let Ok(args) = args.cast::<windows::UI::Notifications::ToastActivatedEventArgs>() else { return Ok(()) };
                let arguments = args.Arguments().map(|value| value.to_string()).unwrap_or_default();
                let Some(token) = activation_token.as_deref() else { return Ok(()) };
                if arguments == "default" {
                    let token = token.to_string();
                    crate::bridge::ui_apply(move |bridge| bridge.on_notification_activated(&token));
                } else if arguments == format!("play:{token}") {
                    let token = token.to_string();
                    crate::bridge::ui_apply(move |bridge| bridge.on_notification_play(&token));
                } else if arguments == format!("reply:{token}") {
                    let input = args.UserInput().ok()
                        .and_then(|values| values.Lookup(&HSTRING::from("reply-input")).ok())
                        .and_then(|value| value.cast::<windows::Foundation::IPropertyValue>().ok())
                        .and_then(|value| value.GetString().ok())
                        .map(|value| value.to_string()).unwrap_or_default();
                    let token = token.to_string();
                    crate::bridge::ui_apply(move |bridge| bridge.on_notification_reply(&token, &input));
                }
                Ok(())
        }))?;
        notifier.Show(&notification)
    })();
    if let Err(error) = result {
        eprintln!("[notify] toast failed: {error}");
    }
}

fn xml_escape(value: &str) -> String {
    value.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
        .replace('\"', "&quot;").replace('\'', "&apos;")
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


#[cfg(test)]
mod notification_tests {
    use super::xml_escape;

    #[test]
    fn escapes_all_xml_metacharacters_in_action_arguments() {
        assert_eq!(xml_escape("reply:a&b<c>d\"e'f"), "reply:a&amp;b&lt;c&gt;d&quot;e&apos;f");
    }

    #[test]
    fn escapes_attribute_delimiters_in_image_paths() {
        assert_eq!(xml_escape(r#"file:///C:/A&B/<photo>"' image.jpg"#),
            "file:///C:/A&amp;B/&lt;photo&gt;&quot;&apos; image.jpg");
    }

    #[test]
    fn preserves_empty_plain_and_unicode_text() {
        for value in ["", "reply:0123456789abcdef", "Olá 世界"] {
            assert_eq!(xml_escape(value), value);
        }
    }

    #[test]
    fn escapes_existing_entities_as_literal_text() {
        assert_eq!(xml_escape("&amp; &#34;"), "&amp;amp; &amp;#34;");
    }
}
