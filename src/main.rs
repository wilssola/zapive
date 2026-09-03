slint::include_modules!();

mod audio;
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


    // Developer probe: exercises the whole in-process audio path
    // (opus encode -> decode with atempo -> waveform) and exits.
    if std::env::args().any(|a| a == "--audio-selftest") {
        audio_selftest();
        single::release_single_instance();
        return;
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

    // Toast identity (header name and icon) comes from the AppUserModelID.
    let icon_file = paths::data_dir().join("zapive.ico");
    let _ = std::fs::write(&icon_file, include_bytes!("../ui/zapive.ico"));
    platform::register_app_id(&icon_file.to_string_lossy());

    // Tray icon (Windows): keeps the app alive with the window hidden.
    #[cfg(windows)]
    let tray = make_tray();
    #[cfg(windows)]
    let tray_poll = slint::Timer::default();
    #[cfg(windows)]
    if let Some((_tray, open_id, exit_id)) = &tray {
        ui.window().on_close_requested(|| slint::CloseRequestResponse::HideWindow);
        let handle = ui.as_weak();
        let (open_id, exit_id) = (open_id.clone(), exit_id.clone());
        tray_poll.start(
            slint::TimerMode::Repeated,
            std::time::Duration::from_millis(100),
            move || {
                let show = |handle: &slint::Weak<AppWindow>| {
                    if let Some(ui) = handle.upgrade() {
                        let _ = ui.show();
                        platform::focus_window();
                    }
                };
                while let Ok(event) = tray_icon::TrayIconEvent::receiver().try_recv() {
                    if matches!(
                        event,
                        tray_icon::TrayIconEvent::Click {
                            button: tray_icon::MouseButton::Left,
                            button_state: tray_icon::MouseButtonState::Up,
                            ..
                        }
                    ) {
                        show(&handle);
                    }
                }
                while let Ok(event) = tray_icon::menu::MenuEvent::receiver().try_recv() {
                    if event.id() == &open_id {
                        show(&handle);
                    } else if event.id() == &exit_id {
                        slint::quit_event_loop().ok();
                    }
                }
            },
        );
    }

    ui.run().expect("event loop failed");
    wa.send(wa::Cmd::Shutdown);
    single::release_single_instance();
}

fn audio_selftest() {
    // Two seconds of 440Hz at 16kHz mono in, opus out.
    let in_rate = 16_000u32;
    let samples: Vec<f32> = (0..in_rate * 2)
        .map(|i| (i as f32 * 440.0 * std::f32::consts::TAU / in_rate as f32).sin() * 0.5)
        .collect();
    let tmp = std::env::temp_dir().join("zapive_selftest.ogg");
    match audio::encode_voice_ogg(&samples, in_rate, &tmp) {
        Some(secs) => println!("[selftest] encoded {secs}s voice note at {}", tmp.display()),
        None => {
            println!("[selftest] FAIL: encode");
            return;
        }
    }
    for rate in [1.0, 1.5, 3.0] {
        match audio::decode_with_tempo(&tmp, rate) {
            Some(buf) => println!(
                "[selftest] decode at {rate}x: {:.2}s ({} samples)",
                buf.duration_secs(),
                buf.samples.len()
            ),
            None => println!("[selftest] FAIL: decode at {rate}x"),
        }
    }
    match audio::waveform(&tmp) {
        Some(w) => println!("[selftest] waveform {}x{}", w.w, w.h),
        None => println!("[selftest] FAIL: waveform"),
    }
    let strip = audio::message_waveform(&samples);
    println!("[selftest] message waveform {} points, peak {}", strip.len(), strip.iter().max().unwrap_or(&0));
    let _ = std::fs::remove_file(&tmp);
}

#[cfg(windows)]
fn make_tray() -> Option<(tray_icon::TrayIcon, tray_icon::menu::MenuId, tray_icon::menu::MenuId)> {
    use tray_icon::menu::{Menu, MenuItem};
    // Raw RGBA came out garbled in the shell; LoadImage on the .ico (the
    // same art the exe embeds, written to the data dir at startup) lets
    // Windows pick the right size and pixel format itself.
    let ico = crate::paths::data_dir().join("zapive.ico");
    let icon = tray_icon::Icon::from_path(&ico, Some((32, 32))).ok().or_else(|| {
        let img = image::load_from_memory(include_bytes!("../ui/zapive.png"))
            .ok()?
            .resize_exact(32, 32, image::imageops::FilterType::Triangle)
            .into_rgba8();
        let (w, h) = img.dimensions();
        tray_icon::Icon::from_rgba(img.into_raw(), w, h).ok()
    })?;
    let open = MenuItem::new(i18n::t("tray.open"), true, None);
    let exit = MenuItem::new(i18n::t("tray.exit"), true, None);
    let (open_id, exit_id) = (open.id().clone(), exit.id().clone());
    let menu = Menu::new();
    menu.append_items(&[&open, &exit]).ok()?;
    let tray = tray_icon::TrayIconBuilder::new()
        .with_icon(icon)
        .with_tooltip("Zapive")
        .with_menu(Box::new(menu))
        .build()
        .ok()?;
    Some((tray, open_id, exit_id))
}
