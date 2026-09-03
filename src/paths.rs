// Where Zapive keeps its files, per platform. Nothing is written next to
// the executable: the vault and the media cache live in the user's own
// directories. File names differ from the Node build's (`vault.db` vs
// `zapive.db`, `media-v2` vs `media`) so both apps can coexist during the
// migration period without touching each other's data.
use std::path::PathBuf;

const APP: &str = "Zapive";

fn home() -> PathBuf {
    std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

// %APPDATA%\Zapive, ~/Library/Application Support/Zapive, ~/.local/share/zapive
pub fn data_dir() -> PathBuf {
    if cfg!(windows) {
        std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| home().join("AppData").join("Roaming"))
            .join(APP)
    } else if cfg!(target_os = "macos") {
        home().join("Library").join("Application Support").join(APP)
    } else {
        std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home().join(".local").join("share"))
            .join(APP.to_lowercase())
    }
}

// Cache is separate: it can be deleted without losing the account.
pub fn cache_dir() -> PathBuf {
    if cfg!(windows) {
        std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| home().join("AppData").join("Local"))
            .join(APP)
            .join("Cache")
    } else if cfg!(target_os = "macos") {
        home().join("Library").join("Caches").join(APP)
    } else {
        std::env::var_os("XDG_CACHE_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home().join(".cache"))
            .join(APP.to_lowercase())
    }
}

pub fn vault_path() -> PathBuf {
    data_dir().join("vault.db")
}

// whatsapp-rust's own protocol/session store; its schema, not ours.
pub fn wa_session_path() -> PathBuf {
    data_dir().join("wa.db")
}

pub fn media_cache() -> PathBuf {
    cache_dir().join("media-v2")
}

pub fn ensure_dirs() {
    let _ = std::fs::create_dir_all(data_dir());
    let _ = std::fs::create_dir_all(media_cache());
}
