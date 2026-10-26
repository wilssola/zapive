use crate::paths::data_dir;
use std::fs::{File, OpenOptions, TryLockError};
use std::io;
use std::path::{Path, PathBuf};

pub struct InstanceGuard {
    _lock: File,
    directory: PathBuf,
}

impl Drop for InstanceGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(self.directory.join("zapive.pid"));
        let _ = std::fs::remove_file(self.directory.join("zapive.raise"));
    }
}

fn try_claim(directory: &Path) -> io::Result<Option<InstanceGuard>> {
    std::fs::create_dir_all(directory)?;
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(directory.join("zapive.instance.lock"))?;
    match lock.try_lock() {
        Ok(()) => {}
        Err(TryLockError::WouldBlock) => return Ok(None),
        Err(TryLockError::Error(error)) => return Err(error),
    }
    let guard = InstanceGuard {
        _lock: lock,
        directory: directory.to_path_buf(),
    };
    match std::fs::remove_file(directory.join("zapive.raise")) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    std::fs::write(directory.join("zapive.pid"), std::process::id().to_string())?;
    Ok(Some(guard))
}

pub fn claim_single_instance() -> Option<InstanceGuard> {
    let directory = data_dir();
    match try_claim(&directory) {
        Ok(Some(guard)) => Some(guard),
        Ok(None) => {
            if let Ok(text) = std::fs::read_to_string(directory.join("zapive.pid"))
                && let Ok(pid) = text.trim().parse::<u32>()
            {
                crate::platform::allow_foreground(pid);
            }
            if let Err(error) = std::fs::write(directory.join("zapive.raise"), b"") {
                eprintln!("[single] cannot request foreground: {error}");
            }
            println!("another instance is running; raising it instead");
            None
        }
        Err(error) => {
            eprintln!("[single] cannot acquire instance lock: {error}");
            None
        }
    }
}

pub fn take_raise_request() -> bool {
    std::fs::remove_file(data_dir().join("zapive.raise")).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    struct TempDirectory(PathBuf);

    impl TempDirectory {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let directory = std::env::temp_dir().join(format!(
                "zapive-single-{}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir(&directory).unwrap();
            Self(directory)
        }
    }

    impl Drop for TempDirectory {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn contention_and_reacquisition_keep_stable_lock_file() {
        let directory = TempDirectory::new();
        let first = try_claim(&directory.0).unwrap().unwrap();
        assert!(try_claim(&directory.0).unwrap().is_none());
        drop(first);
        assert!(directory.0.join("zapive.instance.lock").exists());
        assert!(!directory.0.join("zapive.pid").exists());
        let second = try_claim(&directory.0).unwrap().unwrap();
        assert!(try_claim(&directory.0).unwrap().is_none());
        drop(second);
    }

    #[test]
    fn stale_pid_is_ignored_and_unwind_releases_lock() {
        let directory = TempDirectory::new();
        std::fs::write(directory.0.join("zapive.pid"), "1").unwrap();
        std::fs::write(directory.0.join("zapive.raise"), b"").unwrap();
        let result = std::panic::catch_unwind(|| {
            let _guard = try_claim(&directory.0).unwrap().unwrap();
            assert!(!directory.0.join("zapive.raise").exists());
            panic!("release on unwind");
        });
        assert!(result.is_err());
        assert!(try_claim(&directory.0).unwrap().is_some());
    }

    #[test]
    fn invalid_lock_path_fails_closed() {
        let directory = TempDirectory::new();
        std::fs::create_dir(directory.0.join("zapive.instance.lock")).unwrap();
        assert!(try_claim(&directory.0).is_err());
        assert!(!directory.0.join("zapive.pid").exists());
    }
}
