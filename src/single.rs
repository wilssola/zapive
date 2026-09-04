// WhatsApp allows one desktop session per account: a second copy of the
// app makes the server drop the stream with "conflict" and both copies
// reconnect in a loop. A lock file holding the running pid keeps that
// from happening — a second launch brings the first window forward and
// exits. The file name differs from the Node build's `instance.lock` so
// the two apps can coexist while the rewrite reaches parity.
use crate::paths::data_dir;
use std::path::PathBuf;

fn lock_path() -> PathBuf {
    data_dir().join("zapive.pid")
}

fn raise_path() -> PathBuf {
    data_dir().join("zapive.raise")
}

#[cfg(windows)]
fn alive(pid: u32) -> bool {
    use windows::Win32::Foundation::{CloseHandle, ERROR_ACCESS_DENIED};
    use windows::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
    if pid == 0 || pid == std::process::id() {
        return false;
    }
    unsafe {
        match OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) {
            Ok(handle) => {
                let _ = CloseHandle(handle);
                true
            }
            // Access denied means the process exists but belongs to someone else.
            Err(e) => e.code() == ERROR_ACCESS_DENIED.to_hresult(),
        }
    }
}

#[cfg(not(windows))]
fn alive(pid: u32) -> bool {
    if pid == 0 || pid == std::process::id() {
        return false;
    }
    // Signal 0 probes for existence; EPERM still means the pid is alive.
    let r = unsafe { libc::kill(pid as i32, 0) };
    r == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

// True when this process may run; false when another instance owns the
// session (it is raised instead).
pub fn claim_single_instance() -> bool {
    let lock = lock_path();
    if let Ok(text) = std::fs::read_to_string(&lock)
        && let Ok(pid) = text.trim().parse::<u32>()
        && alive(pid)
    {
        // Leave the window alone and ask its own process to raise it: a
        // window parked in the tray is hidden, and unhiding it from here
        // would desync the toolkit, which then stops honouring the close
        // button. Handing the foreground right over first is what lets
        // the older process take focus.
        crate::platform::allow_foreground(pid);
        let _ = std::fs::write(raise_path(), b"");
        return false;
    }
    // A marker left by a launch that raced with a shutdown would raise the
    // window the moment this instance starts polling.
    let _ = std::fs::remove_file(raise_path());
    let _ = std::fs::write(&lock, std::process::id().to_string());
    true
}

// Polled by the running instance: true means another launch asked for the
// window. Removing the file is the same act as taking the request, so a
// burst of launches raises the window once.
pub fn take_raise_request() -> bool {
    std::fs::remove_file(raise_path()).is_ok()
}

// Called on the way out; only removes the lock this process wrote.
pub fn release_single_instance() {
    let lock = lock_path();
    if let Ok(text) = std::fs::read_to_string(&lock)
        && text.trim() == std::process::id().to_string()
    {
        let _ = std::fs::remove_file(&lock);
        let _ = std::fs::remove_file(raise_path());
    }
}
