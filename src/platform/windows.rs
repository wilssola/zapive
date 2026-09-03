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
