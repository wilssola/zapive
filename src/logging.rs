// One log file for the whole process. whatsapp-rust reports through the
// `log` crate, so with no logger installed its diagnostics go nowhere --
// including the lines that say why a call never rang on the other side
// ("no relay in offer ack", the resolved callee device list, the offer's
// per-device encrypt skips). A release build has no console at all, so
// the file is the only place those can land.
use std::io::Write;
use std::sync::Mutex;

// Enough for a long session; the previous run is kept next to it.
const MAX_BYTES: u64 = 4 * 1024 * 1024;

pub fn path() -> std::path::PathBuf {
    crate::paths::data_dir().join("zapive.log")
}

struct FileLog {
    level: log::LevelFilter,
    file: Mutex<Option<std::fs::File>>,
}

impl log::Log for FileLog {
    fn enabled(&self, meta: &log::Metadata) -> bool {
        meta.level() <= self.level
    }

    fn log(&self, record: &log::Record) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let line = format!(
            "{} {:5} {} {}\n",
            chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f"),
            record.level(),
            record.target(),
            record.args()
        );
        if cfg!(debug_assertions) {
            eprint!("{line}");
        }
        if let Ok(mut file) = self.file.lock()
            && let Some(file) = file.as_mut()
        {
            let _ = file.write_all(line.as_bytes());
            let _ = file.flush();
        }
    }

    fn flush(&self) {
        if let Ok(mut file) = self.file.lock()
            && let Some(file) = file.as_mut()
        {
            let _ = file.flush();
        }
    }
}

// ZAPIVE_LOG picks the level; the default keeps warnings and the
// lifecycle lines, which is what a failed call leaves behind. `debug`
// adds the protocol chatter (stanzas, device resolution, relay).
pub fn install() {
    let level = match std::env::var("ZAPIVE_LOG").unwrap_or_default().to_lowercase().as_str() {
        "off" => log::LevelFilter::Off,
        "error" => log::LevelFilter::Error,
        "warn" => log::LevelFilter::Warn,
        "debug" => log::LevelFilter::Debug,
        "trace" => log::LevelFilter::Trace,
        _ => log::LevelFilter::Info,
    };
    let path = path();
    // Keep one previous run: a call that failed yesterday is still worth
    // reading after the app has been restarted.
    if std::fs::metadata(&path).is_ok_and(|m| m.len() > MAX_BYTES) {
        let _ = std::fs::rename(&path, path.with_extension("log.old"));
    }
    let file = std::fs::OpenOptions::new().create(true).append(true).open(&path).ok();
    let logger = Box::new(FileLog { level, file: Mutex::new(file) });
    if log::set_boxed_logger(logger).is_ok() {
        log::set_max_level(level);
        log::info!(
            "zapive {} starting on {}; log level {level}",
            env!("CARGO_PKG_VERSION"),
            std::env::consts::OS
        );
    }
}
