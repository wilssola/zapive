// The only module that talks to the operating system directly. Everything
// per-OS funnels through here so the rest of the app stays portable.
use std::path::PathBuf;

#[derive(Clone, Debug, Default)]
pub struct ToastPayload {
    pub token: Option<String>,
    pub thumbnail: Option<PathBuf>,
    pub play: bool,
    pub reply: bool,
}
#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::*;

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub use macos::*;

#[cfg(all(unix, not(target_os = "macos")))]
mod linux;
#[cfg(all(unix, not(target_os = "macos")))]
pub use linux::*;
