slint::include_modules!();

mod bridge;
mod i18n;
mod paths;
mod platform;
mod qr;
mod single;
mod wa;

use std::sync::OnceLock;
use tokio::runtime::Runtime;

static RT: OnceLock<Runtime> = OnceLock::new();

fn main() {
    paths::ensure_dirs();
    if !single::claim_single_instance() {
        println!("another instance is running; raising it instead");
        return;
    }

    // Language: system locale for now; the vault setting takes over in
    // phase 2 (settings live behind the PIN there).
    let system_pt = sys_locale::get_locale()
        .map(|l| l.to_lowercase().starts_with("pt"))
        .unwrap_or(false);
    if system_pt {
        i18n::set_locale(i18n::Locale::Pt);
        slint::select_bundled_translation("pt").ok();
    } else {
        i18n::set_locale(i18n::Locale::En);
    }

    let rt = RT.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("failed to build the tokio runtime")
    });

    // Phase-0 spike: statically linked FFmpeg with the codecs the app needs.
    match ffmpeg_next::init() {
        Ok(()) => {
            let opus = ffmpeg_next::decoder::find_by_name("opus").is_some();
            let h264 = ffmpeg_next::encoder::find_by_name("libopenh264").is_some();
            println!("[spike] ffmpeg ready (opus decode: {opus}, h264 encode: {h264})");
        }
        Err(e) => eprintln!("[spike] ffmpeg init failed: {e}"),
    }

    let ui = AppWindow::new().expect("failed to create the main window");

    let (wa, registered) = match wa::WaService::start(rt) {
        Ok(pair) => pair,
        Err(e) => {
            eprintln!("[wa] failed to start: {e}");
            // The window still opens so the error is visible in the UI.
            ui.set_screen("login".into());
            ui.set_status_text(i18n::ta("status.connectFailed", &[&e]).into());
            ui.run().expect("event loop failed");
            return;
        }
    };
    ui.set_screen(if registered { "main" } else { "login" }.into());

    bridge::install(&ui, wa.clone());
    // Connect only now, so no pairing event beats the bridge install.
    wa.send(wa::Cmd::Start);

    // Tray icon (Windows): keeps the app alive with the window hidden.
    #[cfg(windows)]
    let _tray = make_tray();

    ui.run().expect("event loop failed");
    wa.send(wa::Cmd::Shutdown);
    single::release_single_instance();
}

#[cfg(windows)]
fn make_tray() -> Option<tray_icon::TrayIcon> {
    let img = image::load_from_memory(include_bytes!("../ui/zapive.png")).ok()?.into_rgba8();
    let (w, h) = img.dimensions();
    let icon = tray_icon::Icon::from_rgba(img.into_raw(), w, h).ok()?;
    tray_icon::TrayIconBuilder::new()
        .with_icon(icon)
        .with_tooltip("Zapive")
        .build()
        .ok()
}
