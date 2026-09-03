slint::include_modules!();

mod bridge;
mod i18n;
mod markup;
mod media;
mod video;
mod paths;
mod platform;
mod qr;
mod single;
mod store;
mod vault;
mod wa;
mod wa_map;

use slint::ComponentHandle;
use std::sync::OnceLock;
use tokio::runtime::Runtime;

static RT: OnceLock<Runtime> = OnceLock::new();

fn main() {
    paths::ensure_dirs();
    if !single::claim_single_instance() {
        println!("another instance is running; raising it instead");
        return;
    }

    let mut app_vault = match vault::Vault::new() {
        Ok(vault) => vault,
        Err(e) => {
            eprintln!("[vault] cannot open: {e}");
            single::release_single_instance();
            return;
        }
    };

    // Language: the saved setting wins; "system" falls back to the locale.
    let language = app_vault.setting_get("language").unwrap_or_else(|| "system".into());
    let system_pt = sys_locale::get_locale()
        .map(|l| l.to_lowercase().starts_with("pt"))
        .unwrap_or(false);
    let use_pt = language == "pt" || (language == "system" && system_pt);
    if use_pt {
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

    if let Err(e) = ffmpeg_next::init() {
        eprintln!("[media] ffmpeg init failed: {e}");
    }

    let ui = AppWindow::new().expect("failed to create the main window");
    ui.set_language_mode(language.as_str().into());

    // Theme: explicit modes win; "system" follows the OS with a slow poll.
    let theme = app_vault.setting_get("theme").unwrap_or_else(|| "dark".into());
    ui.set_theme_mode(theme.as_str().into());
    ui.set_dark_theme(match theme.as_str() {
        "light" => false,
        "system" => platform::system_dark(),
        _ => true,
    });
    let theme_poll = slint::Timer::default();
    {
        let handle = ui.as_weak();
        theme_poll.start(
            slint::TimerMode::Repeated,
            std::time::Duration::from_secs(15),
            move || {
                if let Some(ui) = handle.upgrade()
                    && ui.get_theme_mode() == "system"
                {
                    ui.set_dark_theme(platform::system_dark());
                }
            },
        );
    }

    let (wa, registered) = match wa::WaService::start(rt) {
        Ok(pair) => pair,
        Err(e) => {
            eprintln!("[wa] failed to start: {e}");
            ui.set_screen("login".into());
            ui.set_status_text(i18n::ta("status.connectFailed", &[&e]).into());
            ui.run().expect("event loop failed");
            single::release_single_instance();
            return;
        }
    };

    bridge::install(&ui, wa.clone());

    // With a PIN the vault stays locked until the user types it; without
    // one it opens right away, bound to the OS account.
    if app_vault.has_pin() {
        bridge::ui_apply(move |b| b.park_locked_vault(app_vault, registered));
    } else {
        match app_vault.open() {
            Ok(()) => bridge::ui_apply(move |b| b.boot(app_vault, registered)),
            Err(e) => {
                eprintln!("[vault] open failed: {e}");
                ui.set_status_text(i18n::ta("status.connectFailed", &[&e]).into());
            }
        }
    }

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
