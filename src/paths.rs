// Where Zapive keeps its files, per platform. Nothing is written next to
// the executable: the vault and the media cache live in the user's own
// directories.
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
    cache_dir().join("media")
}

pub fn ensure_dirs() {
    remove_node_era();
    let _ = std::fs::create_dir_all(data_dir());
    let _ = std::fs::create_dir_all(media_cache());
}

// The Node build left its vault, its unencrypted media cache and the
// unpacked runtime (node_modules with .node DLLs) behind; none of it is
// readable by this build, so the first run sweeps it. The Rust cache
// briefly lived at `media-v2` and moves onto the freed-up name.
fn remove_node_era() {
    let data = data_dir();
    // zapive.pid and zapive.ico stay: this build reuses those names (the
    // pid file is the single-instance lock — deleting it would let a
    // second instance through).
    for name in ["zapive.db", "zapive.db-wal", "zapive.db-shm"] {
        let _ = std::fs::remove_file(data.join(name));
    }
    for dir in ["auth_info", "auth_info.bak"] {
        let _ = std::fs::remove_dir_all(data.join(dir));
    }
    let _ = std::fs::remove_file(data.join("data_store.json"));
    let _ = std::fs::remove_file(data.join("data_store.json.bak"));
    let cache = cache_dir();
    let _ = std::fs::remove_dir_all(cache.join("runtime"));
    let media = cache.join("media");
    let old = cache.join("media-v2");
    if old.is_dir() {
        let _ = std::fs::remove_dir_all(&media);
        let _ = std::fs::rename(&old, &media);
    } else if media_is_node_era(&media) {
        let _ = std::fs::remove_dir_all(&media);
    }
}

// Every file this build writes is sealed and starts with the ZENC1 magic;
// the Node cache kept plain files, which is how a leftover is recognized.
fn media_is_node_era(dir: &std::path::Path) -> bool {
    fn first_file(dir: &std::path::Path, depth: u8) -> Option<PathBuf> {
        for entry in std::fs::read_dir(dir).ok()?.flatten() {
            let path = entry.path();
            if path.file_name().is_some_and(|n| n == ".tmp") {
                continue;
            }
            if path.is_file() {
                return Some(path);
            }
            if depth > 0 && path.is_dir() {
                if let Some(found) = first_file(&path, depth - 1) {
                    return Some(found);
                }
            }
        }
        None
    }
    let Some(sample) = first_file(dir, 2) else { return false };
    let mut magic = [0u8; 5];
    use std::io::Read;
    match std::fs::File::open(sample).and_then(|mut f| f.read_exact(&mut magic).map(|_| ())) {
        Ok(()) => &magic != b"ZENC1",
        Err(_) => false,
    }
}
