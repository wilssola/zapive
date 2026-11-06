// Demo mode: `zapive --demo` runs the real UI on invented data so the team
// can take screenshots without exposing a real account. It is compiled
// only with `--features demo`, so release builds do not carry it.
//
// What it never does: open the real vault or cache (the data directories
// are redirected to a throwaway folder before anything reads them), claim
// the single-instance lock, create a WhatsApp client, open a network
// connection, register a tray icon, check for updates, start the MCP
// server or raise notifications. `bridge::install` is not called either,
// so no UI callback reaches the WhatsApp service.
mod art;
mod data;

use slint::ComponentHandle;
use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use crate::{AppWindow, i18n};

const DEFAULT_SIZE: (u32, u32) = (1440, 900);
const SHOTS: [(&str, &str, &str, &str); 15] = [
    ("chat-dark-glass.png", "chat", "dark", "glass"),
    ("chat-light-glass.png", "chat", "light", "glass"),
    ("chat-dark-opaque.png", "chat", "dark", "opaque"),
    ("chat-light-opaque.png", "chat", "light", "opaque"),
    ("settings.png", "settings", "dark", "glass"),
    ("info-panel.png", "info", "dark", "glass"),
    ("login.png", "login", "dark", "glass"),
    ("splash.png", "splash", "dark", "glass"),
    ("sync.png", "sync", "dark", "glass"),
    ("poll.png", "poll", "dark", "glass"),
    ("starred.png", "starred", "dark", "glass"),
    ("shortcuts.png", "shortcuts", "dark", "glass"),
    ("reactions.png", "reactions", "dark", "glass"),
    ("drop.png", "drop", "dark", "glass"),
    ("poll-composer.png", "poll-composer", "dark", "glass"),
];

#[derive(Clone, Copy, PartialEq)]
enum Page {
    Chat,
    Settings,
    Info,
    Login,
    Splash,
    Sync,
    Poll,
    Starred,
    Shortcuts,
    Reactions,
    Drop,
    PollComposer,
}

struct Options {
    page: Page,
    dark: bool,
    glass: bool,
    english: bool,
    shot: Option<PathBuf>,
    size: (u32, u32),
    shots_all: Option<PathBuf>,
    group: bool,
    hover: Option<(i32, i32)>,
}

pub fn requested() -> bool {
    std::env::args().any(|a| a == "--demo" || a.starts_with("--demo-"))
}

fn value_of(name: &str) -> Option<String> {
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        if arg == name {
            return args.next();
        }
        if let Some(rest) = arg.strip_prefix(name).and_then(|r| r.strip_prefix('=')) {
            return Some(rest.to_string());
        }
    }
    None
}

fn parse() -> Result<Options, String> {
    let page = match value_of("--demo-page").as_deref() {
        None | Some("chat") => Page::Chat,
        Some("settings") => Page::Settings,
        Some("info") => Page::Info,
        Some("login") => Page::Login,
        Some("splash") => Page::Splash,
        Some("sync") => Page::Sync,
        Some("poll") => Page::Poll,
        Some("starred") => Page::Starred,
        Some("shortcuts") => Page::Shortcuts,
        Some("reactions") => Page::Reactions,
        Some("drop") => Page::Drop,
        Some("poll-composer") => Page::PollComposer,
        Some(other) => {
            return Err(format!(
                "unknown --demo-page `{other}` (chat|settings|info|login|splash|sync|poll|starred|shortcuts|reactions|drop|poll-composer)"
            ));
        }
    };
    let dark = match value_of("--demo-theme").as_deref() {
        None | Some("dark") => true,
        Some("light") => false,
        Some(other) => return Err(format!("unknown --demo-theme `{other}` (dark|light)")),
    };
    let glass = match value_of("--demo-style").as_deref() {
        None | Some("glass") => true,
        Some("opaque") => false,
        Some(other) => return Err(format!("unknown --demo-style `{other}` (glass|opaque)")),
    };
    let english = match value_of("--demo-lang").as_deref() {
        None | Some("en") => true,
        Some("pt") => false,
        Some(other) => return Err(format!("unknown --demo-lang `{other}` (en|pt)")),
    };
    let size = match value_of("--demo-size") {
        None => DEFAULT_SIZE,
        Some(text) => text
            .split_once(['x', 'X'])
            .and_then(|(w, h)| Some((w.parse::<u32>().ok()?, h.parse::<u32>().ok()?)))
            .filter(|(w, h)| (400..=4000).contains(w) && (300..=3000).contains(h))
            .ok_or_else(|| format!("invalid --demo-size `{text}` (expected WxH, e.g. 1440x900)"))?,
    };
    Ok(Options {
        page,
        dark,
        glass,
        english,
        shot: value_of("--demo-shot").map(PathBuf::from),
        size,
        shots_all: value_of("--demo-shots-all").map(PathBuf::from),
        group: matches!(page, Page::Info | Page::Poll | Page::Reactions)
            || std::env::args().any(|a| a == "--demo-group"),
        hover: value_of("--demo-hover")
            .map(|value| {
                let (x, y) = value
                    .split_once(',')
                    .ok_or_else(|| format!("invalid --demo-hover `{value}` (expected x,y)"))?;
                Ok::<_, String>((
                    x.trim()
                        .parse()
                        .map_err(|_| format!("invalid --demo-hover `{value}` (expected x,y)"))?,
                    y.trim()
                        .parse()
                        .map_err(|_| format!("invalid --demo-hover `{value}` (expected x,y)"))?,
                ))
            })
            .transpose()?,
    })
}

// Points every per-user directory at a scratch folder. Runs before
// anything reads `paths`, and aborts rather than ever touching real data.
fn isolate() -> PathBuf {
    let root = std::env::temp_dir().join(format!("zapive-demo-{}", std::process::id()));
    let data = root.join("data");
    let cache = root.join("cache");
    let _ = std::fs::create_dir_all(&data);
    let _ = std::fs::create_dir_all(&cache);
    // SAFETY: called first thing in main, before any other thread exists.
    unsafe {
        std::env::set_var("APPDATA", &data);
        std::env::set_var("LOCALAPPDATA", &cache);
        std::env::set_var("XDG_DATA_HOME", &data);
        std::env::set_var("XDG_CACHE_HOME", &cache);
        std::env::set_var("XDG_CONFIG_HOME", &data);
        if !cfg!(windows) {
            std::env::set_var("HOME", &root);
        }
        // Deterministic pixels for screenshots; the live window keeps the
        // system scale (or SLINT_SCALE_FACTOR if the caller set one).
        if std::env::args().any(|a| a.starts_with("--demo-shot")) {
            std::env::set_var("SLINT_SCALE_FACTOR", "1");
        }
    }
    for dir in [crate::paths::data_dir(), crate::paths::cache_dir()] {
        if !dir.starts_with(&root) {
            eprintln!(
                "[demo] refusing to run: {} is outside the scratch directory",
                dir.display()
            );
            std::process::exit(2);
        }
    }
    root
}

pub fn run() -> i32 {
    let options = match parse() {
        Ok(options) => options,
        Err(message) => {
            eprintln!("[demo] {message}");
            return 2;
        }
    };
    if let Some(dir) = options.shots_all.clone() {
        return shots_all(&options, &dir);
    }
    let root = isolate();
    let code = show(&options);
    let _ = std::fs::remove_dir_all(&root);
    code
}

// One process per screenshot keeps their state apart.
fn shots_all(options: &Options, dir: &Path) -> i32 {
    if let Err(e) = std::fs::create_dir_all(dir) {
        eprintln!("[demo] cannot create {}: {e}", dir.display());
        return 1;
    }
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(e) => {
            eprintln!("[demo] cannot find the executable: {e}");
            return 1;
        }
    };
    let mut failed = 0;
    for (file, page, theme, style) in SHOTS {
        let target = dir.join(file);
        let status = std::process::Command::new(&exe)
            .arg("--demo-shot")
            .arg(&target)
            .args([
                "--demo-page",
                page,
                "--demo-theme",
                theme,
                "--demo-style",
                style,
            ])
            .args([
                "--demo-size",
                &format!("{}x{}", options.size.0, options.size.1),
            ])
            .args(["--demo-lang", if options.english { "en" } else { "pt" }])
            .status();
        match status {
            Ok(s) if s.success() => println!("[demo] wrote {}", target.display()),
            other => {
                eprintln!("[demo] {file} failed: {other:?}");
                failed += 1;
            }
        }
    }
    i32::from(failed > 0)
}

fn show(options: &Options) -> i32 {
    slint::BackendSelector::new()
        .backend_name("winit".into())
        .renderer_name("skia".into())
        .select()
        .expect("failed to select the Skia backend");
    crate::fonts::install_emoji();
    i18n::set_locale(if options.english {
        i18n::Locale::En
    } else {
        i18n::Locale::Pt
    });
    let ui = AppWindow::new().expect("failed to create the main window");
    slint::select_bundled_translation(if options.english { "en_US" } else { "pt" })
        .expect("selected bundled locale must exist");
    data::populate(&ui, options.english, options.group);
    configure(&ui, options);

    ui.show().expect("failed to show the main window");
    ui.window().set_size(slint::LogicalSize::new(
        options.size.0 as f32,
        options.size.1 as f32,
    ));
    if options.hover.is_some() {
        ui.set_demo_hover(true);
    }
    #[cfg(windows)]
    crate::platform::set_title_bar_theme(options.dark, options.glass);

    // The list lays out lazily: anchor on the newest message once it has.
    let mut anchor: Vec<slint::Timer> = [250u64, 700, 1200]
        .into_iter()
        .map(|delay| {
            let timer = slint::Timer::default();
            let weak = ui.as_weak();
            timer.start(
                slint::TimerMode::SingleShot,
                Duration::from_millis(delay),
                move || {
                    if let Some(ui) = weak.upgrade()
                        && ui.get_chat_open()
                    {
                        ui.invoke_scroll_conversation_end();
                    }
                },
            );
            timer
        })
        .collect();

    if let Some(mode) = value_of("--demo-bench") {
        anchor.push(start_bench(&ui, &mode));
    }

    let outcome = Rc::new(Cell::new(0));
    let snapshot = slint::Timer::default();
    if let Some(path) = options.shot.clone() {
        let weak = ui.as_weak();
        let outcome = outcome.clone();
        if let Some((x, y)) = options.hover {
            let weak = ui.as_weak();
            let hover = slint::Timer::default();
            hover.start(
                slint::TimerMode::SingleShot,
                Duration::from_millis(1600),
                move || {
                    if let Some(ui) = weak.upgrade() {
                        ui.set_demo_hover_x(x as f32);
                        ui.set_demo_hover_y(y as f32);
                        ui.set_pointer_x(x as f32);
                        ui.set_pointer_y(y as f32);
                        // Simulated inside the window only; the real cursor never moves.
                        ui.window()
                            .dispatch_event(slint::platform::WindowEvent::PointerMoved {
                                position: slint::LogicalPosition::new(x as f32, y as f32),
                            });
                    }
                },
            );
            anchor.push(hover);
        }

        snapshot.start(
            slint::TimerMode::SingleShot,
            Duration::from_millis(2200),
            move || {
                if let Some(ui) = weak.upgrade() {
                    outcome.set(save_snapshot(&ui, &path));
                }
                let _ = slint::quit_event_loop();
            },
        );
    }
    slint::run_event_loop().expect("event loop failed");
    drop(anchor);
    outcome.get()
}

fn configure(ui: &AppWindow, options: &Options) {
    ui.set_theme_mode(if options.dark { "dark" } else { "light" }.into());
    ui.set_dark_theme(options.dark);
    ui.set_theme_style(if options.glass { "glass" } else { "opaque" }.into());
    ui.set_language_mode(if options.english { "en" } else { "pt" }.into());
    ui.set_autostart(false);
    ui.set_window_active(true);
    // Glass is drawn over the desktop, which a screenshot cannot see: paint
    // a colourful backdrop behind it so the effect reads.
    if options.glass {
        ui.set_native_backdrop(true);
        ui.set_demo_backdrop(art::backdrop(options.size.0, options.size.1, options.dark).image());
    }
    match options.page {
        Page::Chat => {}
        Page::Settings => {
            ui.set_settings_section("notifications".into());
            ui.set_settings_open(true);
        }
        Page::Info => ui.set_info_open(true),
        Page::Splash => {}
        Page::Sync => {
            ui.set_sync_screen_open(true);
            ui.set_sync_screen_started(true);
            ui.set_sync_screen_percent(48);
            ui.set_sync_screen_account("Demo account".into());
            ui.set_sync_screen_messages("12,430".into());
            ui.set_sync_screen_chats("87".into());
            ui.set_sync_screen_media("356".into());
            ui.set_sync_screen_files("82".into());
        }
        Page::Poll => {}
        Page::Starred => ui.set_starred_open(true),
        Page::Shortcuts => ui.set_shortcuts_open(true),
        Page::Reactions => {
            ui.set_reactions_open(true);
        }
        Page::Drop => {
            ui.set_file_drag_hover(true);
            ui.set_staged_count(3);
        }
        Page::PollComposer => ui.set_poll_composer_open(true),
        Page::Login => {
            ui.set_screen("login".into());
            ui.set_status_text(i18n::t("status.scan_qr").into());
            ui.set_qr_image(crate::qr::qr_image(
                "zapive-demo-placeholder-not-a-real-pairing-code",
            ));
        }
    }
    ui.set_app_loading(options.page == Page::Splash);
    // Shows a message's hover actions, by message id ("last" = newest).
    if let Some(id) = value_of("--demo-actions") {
        use slint::Model;
        let messages = ui.get_messages();
        let id = if id == "last" {
            messages
                .row_data(messages.row_count().saturating_sub(1))
                .map(|m| m.id.to_string())
                .unwrap_or_default()
        } else {
            id
        };
        ui.set_react_bar_id(id.into());
    }
    // Shows the chat-row drop target as if files were dragged over a row.
    if let Some(name) = value_of("--demo-drop-row") {
        let jid = demo_jid_of(ui, &name);
        ui.set_drop_target_jid(jid.into());
    }
    if std::env::args().any(|a| a == "--demo-privacy-blur") {
        ui.set_blur_names(true);
        ui.set_blur_messages(true);
        ui.set_blur_photos(true);
        ui.set_blur_media(true);
    }
}

fn save_snapshot(ui: &AppWindow, path: &Path) -> i32 {
    let buffer = match ui.window().take_snapshot() {
        Ok(buffer) => buffer,
        Err(e) => {
            eprintln!("[demo] snapshot failed: {e}");
            return 1;
        }
    };
    let (w, h) = (buffer.width(), buffer.height());
    let mut bytes = buffer.as_bytes().to_vec();
    for pixel in bytes.as_chunks_mut::<4>().0 {
        pixel[3] = 255;
    }
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        let _ = std::fs::create_dir_all(parent);
    }
    match image::save_buffer(path, &bytes, w, h, image::ColorType::Rgba8) {
        Ok(()) => {
            println!("[demo] wrote {} ({w}x{h})", path.display());
            0
        }
        Err(e) => {
            eprintln!("[demo] cannot write {}: {e}", path.display());
            1
        }
    }
}

// In-app load generator for CPU benchmarks: synthetic pointer moves and
// viewport scrolling dispatched inside the window, never real input.
fn start_bench(ui: &AppWindow, mode: &str) -> slint::Timer {
    use slint::platform::WindowEvent;
    let weak = ui.as_weak();
    let mode = mode.to_string();
    let started = std::time::Instant::now();
    let timer = slint::Timer::default();
    timer.start(
        slint::TimerMode::Repeated,
        Duration::from_millis(16),
        move || {
            let Some(ui) = weak.upgrade() else { return };
            let t = started.elapsed().as_secs_f32();
            if t < 3.0 {
                return;
            }
            let phase = (t * 0.8).sin().abs();
            match mode.as_str() {
                "scroll" => ui.invoke_set_conversation_scroll(-phase * 700.0),
                "hover-list" | "hover-chat" => {
                    let x = if mode == "hover-list" { 200.0 } else { 900.0 };
                    let y = 120.0 + phase * 640.0;
                    ui.window().dispatch_event(WindowEvent::PointerMoved {
                        position: slint::LogicalPosition::new(x, y),
                    });
                }
                _ => {}
            }
        },
    );
    timer
}

// The jid of the first chat-list row whose name contains `name`.
fn demo_jid_of(ui: &AppWindow, name: &str) -> String {
    use slint::Model;
    let chats = ui.get_chats();
    (0..chats.row_count())
        .filter_map(|i| chats.row_data(i))
        .find(|chat| chat.name.contains(name))
        .map(|chat| chat.jid.to_string())
        .unwrap_or_default()
}
