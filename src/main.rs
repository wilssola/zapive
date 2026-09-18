// No console window on Windows release builds; debug keeps the logs.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

slint::include_modules!();

mod audio;
mod bridge;
mod call;
mod camera;
mod i18n;
mod logging;
mod markup;
mod media;
mod video;
mod paths;
mod pdf;
mod platform;
mod qr;
mod search;
mod single;
mod store;
mod update;
mod vault;
mod wa;
mod wa_map;

use slint::ComponentHandle;
use std::sync::OnceLock;
use tokio::runtime::Runtime;

static RT: OnceLock<Runtime> = OnceLock::new();

fn main() {
    paths::ensure_dirs();
    logging::install();
    // The previous binary, left by an update that ran in the last session.
    update::sweep();

    // Developer probe: walks the video-call path the way a real call
    // does -- enumerate on this thread first (the way the Settings panel
    // does), then open the camera on its own thread and encode a few
    // frames -- and exits. Runs before the single-instance claim so it
    // can be pointed at a machine that already has the app open.
    if std::env::args().any(|a| a == "--video-selftest") {
        video_selftest();
        return;
    }
    if std::env::args().any(|a| a == "--overlay-selftest") {
        overlay_selftest();
        return;
    }
    // Developer probe: runs the self-updater against this very executable
    // (copy it somewhere disposable first) and says why it failed, if so.
    if std::env::args().any(|a| a == "--update-selftest") {
        println!("[selftest] running {}, latest is {:?}", update::current_version(), update::check());
        println!("[selftest] apply: {:?}", update::apply());
        return;
    }
    if std::env::args().any(|a| a == "--player-selftest") {
        player_selftest();
        return;
    }

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
            // Two was enough for messaging, but a call adds the relay
            // socket, DTLS/SCTP and the media drive loop on top of the
            // transport; starving those stalls the call setup.
            .worker_threads(4)
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

    // The login entry passes --autostart; on Windows that means booting
    // straight into the tray instead of popping the window open.
    let autostarted = std::env::args().any(|a| a == "--autostart");

    let ui = AppWindow::new().expect("failed to create the main window");
    ui.set_language_mode(language.as_str().into());
    ui.set_autostart(platform::autostart_enabled());
    // Both off by default: the current behaviour (no receipts sent, a
    // revoke keeps its message) is what a fresh install already does.
    ui.set_send_receipts(app_vault.setting_get("send_receipts").as_deref() == Some("1"));
    ui.set_apply_deletions(app_vault.setting_get("apply_deletions").as_deref() == Some("1"));

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

    // The session to open is the account that was on screen last; the
    // vault reads and writes that account's conversations from here on.
    let account = app_vault.active_account();
    app_vault.set_account(&account);
    let (wa, registered) = match wa::WaService::start(rt, &account) {
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

    // The window's first frame is drawn before anything queued through
    // ui_apply runs, and the screen it starts on is the QR login: a
    // paired session flashed it on every launch. Pick the real screen
    // here, while the window is still hidden.
    ui.set_screen(
        if app_vault.has_pin() {
            "locked"
        } else if registered {
            "main"
        } else {
            "login"
        }
        .into(),
    );

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
    stamp_window_icons();
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
                while let Ok(event) = tray_icon::TrayIconEvent::receiver().try_recv() {
                    if matches!(
                        event,
                        tray_icon::TrayIconEvent::Click {
                            button: tray_icon::MouseButton::Left,
                            button_state: tray_icon::MouseButtonState::Up,
                            ..
                        }
                    ) {
                        raise_window(&handle);
                    }
                }
                while let Ok(event) = tray_icon::menu::MenuEvent::receiver().try_recv() {
                    if event.id() == &open_id {
                        raise_window(&handle);
                    } else if event.id() == &exit_id {
                        slint::quit_event_loop().ok();
                    }
                }
            },
        );
    }

    // Developer probe; see Bridge::accounts_selftest.
    let accounts_probe = slint::Timer::default();
    if std::env::args().any(|a| a == "--accounts-selftest") {
        let step = std::cell::Cell::new(0u32);
        accounts_probe.start(
            slint::TimerMode::Repeated,
            std::time::Duration::from_secs(3),
            move || {
                let n = step.get();
                step.set(n + 1);
                bridge::ui_apply(move |b| b.accounts_selftest(n));
            },
        );
    }

    // A second launch drops a marker and leaves this window untouched, so
    // the raise happens here, on the side that owns it.
    let raise_poll = slint::Timer::default();
    {
        let handle = ui.as_weak();
        raise_poll.start(
            slint::TimerMode::Repeated,
            std::time::Duration::from_millis(200),
            move || {
                if single::take_raise_request() {
                    raise_window(&handle);
                }
            },
        );
    }

    // run() spins the loop only until the last window is hidden, so parking
    // the window in the tray would end it. Slint counts its own tray icons
    // as keeping the loop alive, but not the tray-icon crate's, so the
    // until_quit loop is what keeps the process around; the tray's Exit
    // item and the update restart both call quit_event_loop().
    #[cfg(windows)]
    let park_in_tray = tray.is_some();
    #[cfg(not(windows))]
    let park_in_tray = false;

    if park_in_tray {
        if !autostarted {
            ui.show().expect("failed to show the main window");
        }
        slint::run_event_loop_until_quit().expect("event loop failed");
        let _ = ui.hide();
    } else {
        ui.run().expect("event loop failed");
    }
    wa.send(wa::Cmd::Shutdown);
    single::release_single_instance();
}

// Coming back from the tray goes through Slint: the toolkit tracks the
// window's visibility itself, and showing it any other way leaves it
// convinced the window is still hidden. focus_window() only raises what
// is already on screen.
fn raise_window(handle: &slint::Weak<AppWindow>) {
    if let Some(ui) = handle.upgrade() {
        let _ = ui.show();
        platform::focus_window();
    }
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

// Developer probe: drives the video player with no window -- play, pause,
// resume, seek, run to the end, replay -- against the clip given after the
// flag, or a four-second one made on the spot. A plain file passes for a
// cache entry: only sealed files carry the magic that asks for the key.
fn player_selftest() {
    use std::sync::{Arc, Mutex};
    use video::PlayerEvent;

    let given = std::env::args().skip_while(|a| a != "--player-selftest").nth(1);
    let clip = match given {
        Some(path) => std::path::PathBuf::from(path),
        None => {
            let gif = std::env::temp_dir().join("zapive_selftest.gif");
            let mp4 = std::env::temp_dir().join("zapive_selftest.mp4");
            let mut frames = Vec::new();
            for i in 0..60u32 {
                let img = image::RgbaImage::from_fn(320, 180, |x, _| {
                    if x / 8 == i { image::Rgba([255, 255, 255, 255]) } else { image::Rgba([20, 60, 90, 255]) }
                });
                frames.push(image::Frame::from_parts(img, 0, 0, image::Delay::from_numer_denom_ms(100, 1)));
            }
            let file = std::fs::File::create(&gif).expect("temp gif");
            let mut encoder = image::codecs::gif::GifEncoder::new(file);
            encoder.encode_frames(frames).expect("encode gif");
            drop(encoder);
            // The player opens a cache entry through its plain temp copy,
            // and keeps one it finds: drop the last run's.
            let _ = std::fs::remove_file(paths::media_cache().join(".tmp").join("zapive_selftest.mp4"));
            if video::gif_to_mp4(&gif, &mp4).is_none() {
                println!("[selftest] FAIL: could not build the test clip");
                return;
            }
            mp4
        }
    };
    println!("[selftest] playing {}", clip.display());

    #[derive(Default)]
    struct Seen {
        frames: u32,
        last: f64,
        resyncs: u32,
        ended: bool,
        failed: bool,
        duration: f64,
    }
    let seen = Arc::new(Mutex::new(Seen::default()));
    let player: Arc<Mutex<Option<video::Playback>>> = Arc::new(Mutex::new(None));
    let started = std::time::Instant::now();
    let playback = {
        let (seen, player) = (seen.clone(), player.clone());
        video::Playback::start(vault::KeyHandle::default(), clip, move |event| {
            let mut seen = seen.lock().unwrap();
            match event {
                PlayerEvent::Ready { w, h, duration } => {
                    seen.duration = duration;
                    println!("[selftest] ready {w}x{h}, {duration:.2}s");
                }
                PlayerEvent::Frame { frame, pos, resync } => {
                    seen.frames += 1;
                    seen.last = pos;
                    if std::env::var_os("ZAPIVE_TRACE").is_some() {
                        println!("[trace] {pos:.3} at {:.3}", started.elapsed().as_secs_f64());
                    }
                    if resync {
                        seen.resyncs += 1;
                        println!(
                            "[selftest] resync at {pos:.2}s ({}x{}), {:.2}s in",
                            frame.w,
                            frame.h,
                            started.elapsed().as_secs_f64()
                        );
                    }
                    // Stand in for the UI having drawn it.
                    if let Some(player) = player.lock().unwrap().as_ref() {
                        player.frame_shown();
                    }
                }
                PlayerEvent::Audio(buffer) => {
                    println!("[selftest] soundtrack {:.2}s", buffer.duration_secs())
                }
                PlayerEvent::Ended => {
                    seen.ended = true;
                    println!("[selftest] ended, {:.2}s in", started.elapsed().as_secs_f64());
                }
                PlayerEvent::Failed => {
                    seen.failed = true;
                    println!("[selftest] FAIL: the player gave up on the clip");
                }
            }
        })
    };
    *player.lock().unwrap() = Some(playback);
    let with = |f: &dyn Fn(&video::Playback)| f(player.lock().unwrap().as_ref().unwrap());
    let sleep = |ms| std::thread::sleep(std::time::Duration::from_millis(ms));
    let report = |what: &str| {
        let seen = seen.lock().unwrap();
        println!("[selftest] {what}: {} frames, at {:.2}s", seen.frames, seen.last);
        (seen.frames, seen.last)
    };

    // Everything below is in fractions of the clip, whatever its length.
    sleep(400);
    let total = seen.lock().unwrap().duration.max(0.5);
    let (frames, at) = report("after 0.4s of play");
    if frames < 3 || !(0.2..=0.6).contains(&at) {
        println!("[selftest] FAIL: 0.4s of play should be about 0.4s in");
    }
    with(&|p| p.pause());
    sleep(150);
    let (paused_frames, _) = report("paused");
    sleep(500);
    let (still, _) = report("still paused");
    if still != paused_frames {
        println!("[selftest] FAIL: frames kept coming while paused");
    }
    // A seek while paused shows where it landed, once.
    let target = total * 0.7;
    with(&|p| p.seek(target));
    sleep(400);
    let (after_seek, at) = report("sought to 70% while paused");
    if after_seek != still + 1 || (at - target).abs() > 0.15 {
        println!("[selftest] FAIL: a paused seek should show exactly the picture it landed on");
    }
    with(&|p| p.resume());
    sleep(150);
    let (_, at) = report("resumed");
    if at < target || at > target + 0.35 {
        println!("[selftest] FAIL: play should carry on from the seek");
    }
    // A seek lands when its keyframe-to-target run is decoded, which a
    // debug build takes its time over: wait for it instead of guessing.
    let target = total * 0.2;
    with(&|p| p.seek(target));
    let landed = (0..40).any(|_| {
        sleep(100);
        let at = seen.lock().unwrap().last;
        at >= target && at < target + 1.5
    });
    report("sought back to 20% while playing");
    if !landed {
        println!("[selftest] FAIL: a playing seek should land and keep going");
    }
    // The last second, rather than sitting through a long clip.
    with(&|p| p.seek((total - 1.0).max(0.0)));
    let ended = (0..60).any(|_| {
        sleep(100);
        seen.lock().unwrap().ended
    });
    if !ended {
        println!("[selftest] FAIL: the clip never reported its end");
    }
    // Play at the end means play again.
    with(&|p| p.resume());
    sleep(600);
    let (_, at) = report("replayed");
    if !(0.1..=0.9).contains(&at) {
        println!("[selftest] FAIL: replay should start over");
    }
    let failed = seen.lock().unwrap().failed;
    *player.lock().unwrap() = None;
    sleep(100);
    println!("[selftest] {}", if failed { "player FAILED" } else { "player done" });
}

// The NAL types in one Annex-B access unit, in order.
fn nal_types(au: &[u8]) -> Vec<u8> {
    let mut types = Vec::new();
    let mut i = 0;
    while i + 3 < au.len() {
        let short = au[i] == 0 && au[i + 1] == 0 && au[i + 2] == 1;
        let long = au[i] == 0 && au[i + 1] == 0 && au[i + 2] == 0 && au[i + 3] == 1;
        if short || long {
            let start = i + if short { 3 } else { 4 };
            if let Some(&first) = au.get(start) {
                types.push(first & 0x1f);
            }
            i = start;
        } else {
            i += 1;
        }
    }
    types
}

// Mirrors what a video call does, in the same order, so a camera that
// only fails inside the app shows the same failure here.
fn video_selftest() {
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;

    let listed = std::time::Instant::now();
    let names: Vec<String> = camera::cameras().into_iter().map(|c| c.name).collect();
    println!("[selftest] enumerated {} camera(s) in {:?}: {names:?}", names.len(), listed.elapsed());

    // The call opens the camera off the UI thread, so the probe does too:
    // COM apartments are per thread, and that is exactly what breaks.
    let wanted = std::env::args()
        .skip_while(|a| a != "--video-selftest")
        .nth(1)
        .unwrap_or_default();
    println!("[selftest] opening camera {:?} on a worker thread", wanted);
    let frames = Arc::new(AtomicU32::new(0));
    let bytes = Arc::new(AtomicU32::new(0));
    let keys = Arc::new(AtomicU32::new(0));
    let opened = std::time::Instant::now();
    let (done_tx, done_rx) = std::sync::mpsc::channel::<Option<()>>();
    {
        let frames = frames.clone();
        let bytes = bytes.clone();
        let keys = keys.clone();
        std::thread::spawn(move || {
            let previews = Arc::new(AtomicU32::new(0));
            let seen = previews.clone();
            let feed = camera::CameraFeed::start(Some(&wanted), move |pic| {
                if seen.fetch_add(1, Ordering::Relaxed) == 0 {
                    println!("[selftest] first preview {}x{}", pic.w, pic.h);
                }
                camera::PREVIEW_BUSY.store(false, Ordering::Release);
            });
            let Some(feed) = feed else {
                let _ = done_tx.send(None);
                return;
            };
            let _ = done_tx.send(Some(()));
            use whatsapp_rust::voip::VideoSource as _;
            let source = feed.source().timed_frames().expect("the camera is timestamped");
            // The engine answers a shortfall by asking for an IDR; whether the
            // encoder actually turns that request into one, and how fast, is
            // the thing a byte count never showed.
            let asked = std::sync::Arc::new(std::sync::Mutex::new(None::<std::time::Instant>));
            {
                let asked = asked.clone();
                let feed = feed.clone_switch();
                std::thread::spawn(move || {
                    for _ in 0..6 {
                        std::thread::sleep(std::time::Duration::from_millis(440));
                        *asked.lock().unwrap() = Some(std::time::Instant::now());
                        feed.request();
                    }
                });
            }
            let until = std::time::Instant::now() + std::time::Duration::from_secs(3);
            while std::time::Instant::now() < until {
                match source.recv_blocking() {
                    Ok(frame) => {
                        let au = frame.data;
                        let n = frames.fetch_add(1, Ordering::Relaxed);
                        bytes.fetch_add(au.len() as u32, Ordering::Relaxed);
                        // The engine drops every unit until one it reads as a
                        // keyframe reaches it, so what it makes of ours is the
                        // question a byte count cannot answer.
                        // `keyframe` is true for a parameter set alone; the
                        // send path needs an IDR SLICE (NAL type 5), and
                        // nothing else clears its keyframe requirement.
                        let types = nal_types(&au);
                        if types.contains(&5) {
                            keys.fetch_add(1, Ordering::Relaxed);
                        }
                        if types.contains(&5)
                            && let Some(at) = asked.lock().unwrap().take()
                        {
                            println!(
                                "[selftest] IDR {:?} after the request",
                                at.elapsed()
                            );
                        }
                        if n < 3 {
                            println!(
                                "[selftest] unit {n}: {} bytes, NAL types {types:?}, has IDR {}",
                                au.len(),
                                types.contains(&5)
                            );
                        }
                    }
                    Err(_) => break,
                }
            }
        });
    }
    // A hang here is the bug: the call path waits on this same handshake
    // with nothing to time it out.
    match done_rx.recv_timeout(std::time::Duration::from_secs(20)) {
        Ok(Some(())) => println!("[selftest] camera up in {:?}", opened.elapsed()),
        Ok(None) => {
            println!("[selftest] FAIL: the camera did not open");
            return;
        }
        Err(_) => {
            println!("[selftest] FAIL: the camera open hung for 20s");
            return;
        }
    }
    std::thread::sleep(std::time::Duration::from_secs(4));
    let count = frames.load(Ordering::Relaxed);
    println!(
        "[selftest] {count} access units, {} bytes total, {} carrying an IDR slice",
        bytes.load(Ordering::Relaxed),
        keys.load(Ordering::Relaxed)
    );
    if count == 0 {
        println!("[selftest] FAIL: the encoder produced nothing");
    }
}

// The tray crate's hidden message window carries no window icon, and
// Task Manager renders whatever garbage WM_GETICON hands back. Stamp the
// real .ico onto every window this process owns.
#[cfg(windows)]
fn stamp_window_icons() {
    use windows::Win32::Foundation::{BOOL, HWND, LPARAM, WPARAM};
    use windows::Win32::System::Threading::GetCurrentProcessId;
    use windows::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetWindowThreadProcessId, ICON_BIG, ICON_SMALL, IMAGE_ICON,
        LR_LOADFROMFILE, LoadImageW, SendMessageW, WM_SETICON,
    };
    use windows::core::PCWSTR;
    unsafe extern "system" fn stamp(hwnd: HWND, lp: LPARAM) -> BOOL {
        unsafe {
            let mut pid = 0u32;
            GetWindowThreadProcessId(hwnd, Some(&mut pid));
            if pid == GetCurrentProcessId() {
                let [small, big] = *(lp.0 as *const [isize; 2]);
                let _ = SendMessageW(hwnd, WM_SETICON, WPARAM(ICON_SMALL as usize), LPARAM(small));
                let _ = SendMessageW(hwnd, WM_SETICON, WPARAM(ICON_BIG as usize), LPARAM(big));
            }
        }
        true.into()
    }
    unsafe {
        use std::os::windows::ffi::OsStrExt;
        let ico = paths::data_dir().join("zapive.ico");
        let wide: Vec<u16> = ico.as_os_str().encode_wide().chain([0]).collect();
        let load = |size: i32| {
            LoadImageW(None, PCWSTR(wide.as_ptr()), IMAGE_ICON, size, size, LR_LOADFROMFILE)
                .map(|h| h.0 as isize)
                .unwrap_or(0)
        };
        let handles = [load(16), load(32)];
        let _ = EnumWindows(Some(stamp), LPARAM(&handles as *const _ as isize));
    }
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

// Developer probe: a styled bubble with the transparent selection overlay
// and the link's hand-cursor boxes (LinkProbeWindow in app.slint), driven
// with synthetic mouse events: hovering the link must reach its box, and
// a press, a drag and a double click on it must still reach the text
// underneath. "keep" after the flag leaves the window up for a real mouse.
fn overlay_selftest() {
    use slint::platform::{PointerEventButton, WindowEvent};
    let win = LinkProbeWindow::new().expect("selftest window");
    {
        let weak = win.as_weak();
        win.on_link_pressed(move |x, y| {
            let weak = weak.clone();
            slint::Timer::single_shot(std::time::Duration::ZERO, move || {
                if let Some(win) = weak.upgrade() {
                    bridge::replay_link_press(
                        win.window(),
                        x,
                        y,
                        &|on| win.set_link_press_through(on),
                        &|| win.get_link_release_seen(),
                    );
                }
            });
        });
    }
    let keep = std::env::args().any(|a| a == "keep");
    let weak = win.as_weak();
    let step = std::rc::Rc::new(std::cell::Cell::new(0u32));
    let script = slint::Timer::default();
    script.start(slint::TimerMode::Repeated, std::time::Duration::from_millis(150), move || {
        let Some(win) = weak.upgrade() else { return };
        let (start, end) = (win.get_link_start(), win.get_link_end());
        let (x, y) = (win.get_box_x() + win.get_box_w() / 2.0, win.get_box_y() + win.get_box_h() / 2.0);
        let at = |x: f32, y: f32| slint::LogicalPosition::new(x, y);
        let left = PointerEventButton::Left;
        let send = |event| win.window().dispatch_event(event);
        let n = step.get();
        step.set(n + 1);
        match n {
            // Let the first layout and the probe's measurement happen.
            0 | 1 => {}
            2 => {
                println!(
                    "[selftest] link box {:.0},{:.0} {:.0}x{:.0}",
                    win.get_box_x(),
                    win.get_box_y(),
                    win.get_box_w(),
                    win.get_box_h()
                );
                if win.get_box_w() < 40.0 || win.get_box_h() < 10.0 {
                    println!("[selftest] FAIL: the link was not measured");
                }
                send(WindowEvent::PointerMoved { position: at(x, y) });
            }
            3 => {
                println!("[selftest] hovering the link: box hovered = {}", win.get_hovered());
                if !win.get_hovered() {
                    println!("[selftest] FAIL: the hand-cursor box is not what the mouse is over");
                }
                send(WindowEvent::PointerMoved { position: at(30.0, y) });
            }
            4 => {
                if win.get_hovered() {
                    println!("[selftest] FAIL: the box claims plain text too");
                }
                // A press on the link, then a drag to the right.
                send(WindowEvent::PointerMoved { position: at(x, y) });
                send(WindowEvent::PointerPressed { position: at(x, y), button: left });
            }
            5 => {
                let (anchor, cursor) = (win.get_anchor(), win.get_cursor());
                println!("[selftest] press on the link: anchor={anchor} cursor={cursor}");
                if anchor != cursor || anchor < start || anchor > end {
                    println!("[selftest] FAIL: the press did not reach the text under the link");
                }
                send(WindowEvent::PointerMoved { position: at(x + 60.0, y) });
            }
            6 => {
                let (anchor, cursor) = (win.get_anchor(), win.get_cursor());
                println!("[selftest] drag from the link: anchor={anchor} cursor={cursor}");
                if cursor <= anchor {
                    println!("[selftest] FAIL: a drag that starts on a link does not select");
                }
                send(WindowEvent::PointerReleased { position: at(x + 60.0, y), button: left });
            }
            // Past the double-click interval, so the next press is a new one.
            7..=10 => {}
            11 => {
                // A plain click: down and up before the replay has run.
                send(WindowEvent::PointerPressed { position: at(x, y), button: left });
                send(WindowEvent::PointerReleased { position: at(x, y), button: left });
            }
            12 => {
                let (anchor, cursor) = (win.get_anchor(), win.get_cursor());
                println!("[selftest] click on the link: anchor={anchor} cursor={cursor}");
                if anchor != cursor || anchor < start || anchor > end {
                    println!("[selftest] FAIL: a click should put the cursor on the link");
                }
                send(WindowEvent::PointerMoved { position: at(x + 80.0, y) });
            }
            13 => {
                if win.get_anchor() != win.get_cursor() {
                    println!("[selftest] FAIL: the text thinks the button is still down after a click");
                }
                send(WindowEvent::PointerMoved { position: at(x, y) });
            }
            14..=17 => {}
            18 => {
                // Double click: selects the word under it, as on plain text.
                for _ in 0..2 {
                    send(WindowEvent::PointerPressed { position: at(x, y), button: left });
                    send(WindowEvent::PointerReleased { position: at(x, y), button: left });
                }
            }
            19 => {}
            20 => {
                let (anchor, cursor) = (win.get_anchor(), win.get_cursor());
                println!("[selftest] double click on the link: anchor={anchor} cursor={cursor}");
                if anchor == cursor {
                    println!("[selftest] FAIL: a double click on a link should select a word");
                }
                println!("[selftest] overlay done");
                if !keep {
                    slint::quit_event_loop().ok();
                }
            }
            _ => {}
        }
    });
    win.run().expect("event loop");
}
