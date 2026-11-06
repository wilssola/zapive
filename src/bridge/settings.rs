// Appearance, notification and privacy settings and the PIN.
use super::*;

impl Bridge {
    pub(super) fn handle_style(&mut self, style: &str) {
        self.ui.set_theme_style(style.into());
        if let Some(vault) = &self.vault {
            vault.setting_set("theme_style", style);
        }
        self.sync_call_theme();
        #[cfg(windows)]
        crate::platform::set_title_bar_theme(self.ui.get_dark_theme(), self.ui.get_glass());
    }

    pub(super) fn handle_notifications(&mut self, enabled: bool) {
        self.notifications = enabled;
        self.ui.set_notifications(enabled);
        if let Some(vault) = &self.vault {
            vault.setting_set("notifications", if enabled { "1" } else { "0" });
        }
        // Drop whatever is still waiting to flush.
        if !enabled {
            self.clear_notification_sensitive_state();
        }
    }

    pub(super) fn handle_message_sound(&mut self, enabled: bool) {
        self.ui.set_message_sound(enabled);
        if let Some(vault) = &self.vault {
            vault.setting_set("message_sound", if enabled { "1" } else { "0" });
        }
    }

    pub(super) fn handle_wallpaper_colors(&mut self, bg: slint::Color, fg: slint::Color) {
        let (bg_hex, fg_hex) = (color_hex(bg), wallpaper_fg_encode(fg));
        self.ui.set_wallpaper_bg(bg);
        self.ui.set_wallpaper_fg(fg);
        self.ui.set_wallpaper_custom(true);
        if let Some(vault) = &self.vault {
            vault.setting_set("chat_wallpaper_bg", &bg_hex);
            vault.setting_set("chat_wallpaper_fg", &fg_hex);
        }
    }

    pub(super) fn handle_wallpaper_scale_reset(&mut self) {
        self.ui.set_wallpaper_scale(1.0);
        if let Some(vault) = &self.vault {
            vault.setting_set("chat_wallpaper_scale", "1.0");
        }
    }

    pub(super) fn handle_wallpaper_scale(&mut self, scale: f32) {
        if !scale.is_finite() {
            return;
        }
        let scale = scale.clamp(0.5, 2.0);
        self.ui.set_wallpaper_scale(scale);
        if let Some(vault) = &self.vault {
            vault.setting_set("chat_wallpaper_scale", &scale.to_string());
        }
    }

    pub(super) fn load_graphics_choice(&mut self, choice: crate::gfx::GfxChoice) {
        let compiled = crate::gfx::Compiled::from_build();
        let choices = crate::gfx::available_choices(&compiled);
        let choice = crate::gfx::effective_choice(choice, &compiled);
        let index = choices
            .iter()
            .position(|candidate| *candidate == choice)
            .unwrap_or(0);
        self.ui.set_graphics_index(index as i32);
    }

    pub(super) fn handle_graphics_choice(&mut self, index: i32) {
        let compiled = crate::gfx::Compiled::from_build();
        let choices = crate::gfx::available_choices(&compiled);
        let Ok(index) = usize::try_from(index) else {
            return;
        };
        let Some(choice) = choices.get(index).copied() else {
            return;
        };
        let dir = crate::paths::data_dir();
        let saved = crate::gfx::effective_choice(crate::gfx::read_choice(&dir), &compiled);
        if choice != saved
            && let Err(error) = crate::gfx::save_choice(&dir, choice)
        {
            log::warn!("[gfx] failed to save renderer choice: {error}");
            self.load_graphics_choice(saved);
            return;
        }
        self.ui.set_graphics_index(index as i32);
        self.ui
            .set_graphics_restart_needed(crate::gfx::restart_needed(choice, &compiled));
    }

    // A hex typed in settings: only a valid #RRGGBB changes anything.
    pub(super) fn wallpaper_hex_entered(&mut self, fg: bool, hex: &str) {
        let Some((r, g, b)) = parse_hex_color(hex) else {
            return;
        };
        let (bg, doodle) = (self.ui.get_wallpaper_bg(), self.ui.get_wallpaper_fg());
        let typed =
            slint::Color::from_argb_u8(if fg { doodle.alpha() } else { bg.alpha() }, r, g, b);
        if fg {
            self.handle_wallpaper_colors(bg, typed);
        } else {
            self.handle_wallpaper_colors(typed, doodle);
        }
    }

    pub(super) fn reset_wallpaper(&mut self) {
        self.ui.set_wallpaper_custom(false);
        self.handle_wallpaper_scale_reset();
        if let Some(vault) = &self.vault {
            vault.setting_set("chat_wallpaper_bg", "");
            vault.setting_set("chat_wallpaper_fg", "");
        }
    }

    pub(super) fn load_wallpaper(&mut self, vault: &Vault) {
        let bg = vault
            .setting_get("chat_wallpaper_bg")
            .as_deref()
            .and_then(parse_hex_color);
        let fg = vault
            .setting_get("chat_wallpaper_fg")
            .as_deref()
            .and_then(wallpaper_fg_decode);
        let scale = vault
            .setting_get("chat_wallpaper_scale")
            .and_then(|value| value.parse::<f32>().ok())
            .filter(|value| value.is_finite())
            .map(|value| value.clamp(0.5, 2.0))
            .unwrap_or(1.0);
        self.ui.set_wallpaper_scale(scale);
        match (bg, fg) {
            (Some(bg), Some(fg)) => {
                self.ui
                    .set_wallpaper_bg(slint::Color::from_rgb_u8(bg.0, bg.1, bg.2));
                self.ui.set_wallpaper_fg(fg);
                self.ui.set_wallpaper_custom(true);
            }
            _ => self.ui.set_wallpaper_custom(false),
        }
    }

    pub(super) fn handle_notify_previews(&mut self, enabled: bool) {
        self.notify_previews = enabled;
        self.ui.set_notify_previews(enabled);
        if let Some(vault) = &self.vault {
            vault.setting_set("notify_previews", if enabled { "1" } else { "0" });
        }
    }

    pub(super) fn handle_privacy_blur(&mut self, category: &str, enabled: bool) {
        let key = match category {
            "names" => {
                self.ui.set_blur_names(enabled);
                "privacy_blur_names"
            }
            "photos" => {
                self.ui.set_blur_photos(enabled);
                self.repaint_avatars();
                "privacy_blur_photos"
            }
            "messages" => {
                self.ui.set_blur_messages(enabled);
                "privacy_blur_messages"
            }
            "media" => {
                self.ui.set_blur_media(enabled);
                "privacy_blur_media"
            }
            _ => return,
        };
        if let Some(vault) = &self.vault {
            vault.setting_set(key, if enabled { "1" } else { "0" });
        }
    }

    pub(super) fn handle_theme(&mut self, mode: &str) {
        self.ui.set_theme_mode(mode.into());
        if let Some(vault) = &self.vault {
            vault.setting_set("theme", mode);
        }
        match mode {
            "dark" => self.ui.set_dark_theme(true),
            "light" => self.ui.set_dark_theme(false),
            _ => self.ui.set_dark_theme(crate::platform::system_dark()),
        }
        self.sync_call_theme();
        #[cfg(windows)]
        crate::platform::set_title_bar_theme(self.ui.get_dark_theme(), self.ui.get_glass());
    }

    // The OS is the only source of truth here (a Run entry, an autostart
    // .desktop, a LaunchAgent), so the chip is refreshed from it rather
    // than from what was asked for: a rejected write shows as unchanged.
    pub(super) fn handle_autostart(&mut self, on: bool) {
        match crate::platform::autostart_set(on) {
            Ok(()) => self.ui.set_settings_status("".into()),
            Err(e) => self
                .ui
                .set_settings_status(ta("autostart.failed", &[&e]).into()),
        }
        self.ui.set_autostart(crate::platform::autostart_enabled());
    }

    // ---- PIN ----

    pub(super) fn handle_save_pin(&mut self, current: &str, next: &str) {
        let Some(vault) = &mut self.vault else { return };
        let status = match vault.change_pin(current, Some(next)) {
            Ok(()) => {
                self.ui.set_pin_set(true);
                t("pin.saved")
            }
            Err(crate::vault::PinError::WrongPin) => t("pin.wrong_current"),
            Err(crate::vault::PinError::BadFormat) => t("pin.format"),
        };
        self.ui.set_settings_status(status.into());
    }

    pub(super) fn handle_remove_pin(&mut self, current: &str) {
        let Some(vault) = &mut self.vault else { return };
        let status = match vault.change_pin(current, None) {
            Ok(()) => {
                self.ui.set_pin_set(false);
                t("pin.removed")
            }
            Err(_) => t("pin.wrong_current"),
        };
        self.ui.set_settings_status(status.into());
    }
}

const DOODLE_ALPHA: u8 = 0x14;

// "#RRGGBB" (case-insensitive) -> bytes; anything else is rejected.
pub(super) fn parse_hex_color(text: &str) -> Option<(u8, u8, u8)> {
    let hex = text.trim().strip_prefix('#')?;
    if hex.len() != 6 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let byte = |range: std::ops::Range<usize>| u8::from_str_radix(&hex[range], 16).ok();
    Some((byte(0..2)?, byte(2..4)?, byte(4..6)?))
}

pub(super) fn color_hex(color: slint::Color) -> String {
    format!(
        "#{:02X}{:02X}{:02X}",
        color.red(),
        color.green(),
        color.blue()
    )
}

pub(super) fn wallpaper_fg_encode(color: slint::Color) -> String {
    format!("{}{:02X}", color_hex(color), color.alpha())
}

pub(super) fn wallpaper_fg_decode(text: &str) -> Option<slint::Color> {
    let text = text.trim();
    if text.len() == 9 {
        let rgb = parse_hex_color(&text[..7])?;
        if !text[7..].bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        let alpha = u8::from_str_radix(&text[7..9], 16).ok()?;
        return Some(slint::Color::from_argb_u8(alpha, rgb.0, rgb.1, rgb.2));
    }
    let (r, g, b) = parse_hex_color(text)?;
    Some(slint::Color::from_argb_u8(DOODLE_ALPHA, r, g, b))
}

#[cfg(test)]
mod wallpaper_tests {
    use super::*;

    #[test]
    fn parses_only_rrggbb() {
        assert_eq!(parse_hex_color("#0b141a"), Some((0x0b, 0x14, 0x1a)));
        assert_eq!(parse_hex_color(" #FFAA00 "), Some((255, 170, 0)));
        for bad in [
            "",
            "#",
            "0b141a",
            "#fff",
            "#12345",
            "#1234567",
            "#gggggg",
            "#12 456",
            "rgb(1,2,3)",
        ] {
            assert_eq!(parse_hex_color(bad), None, "{bad}");
        }
    }

    #[test]
    fn wallpaper_intensity_slider_value_reaches_the_displayed_doodle_alpha() {
        for pct in [0u8, 8, 20, 40] {
            let alpha = (f32::from(pct) * 255.0 / 100.0).round() as u8;
            let requested = slint::Color::from_argb_u8(alpha, 0x12, 0xAB, 0xEF);
            let shown = wallpaper_fg_decode(&wallpaper_fg_encode(requested)).unwrap();
            assert_eq!(shown.alpha(), alpha, "slider {pct}% was overridden");
            assert_eq!(
                (shown.red(), shown.green(), shown.blue()),
                (0x12, 0xAB, 0xEF)
            );
        }
    }

    #[test]
    fn wallpaper_intensity_survives_save_and_load() {
        for alpha in [0u8, 20, 51, 102] {
            let color = slint::Color::from_argb_u8(alpha, 0x12, 0xAB, 0xEF);
            let stored = wallpaper_fg_encode(color);
            let loaded = wallpaper_fg_decode(&stored)
                .unwrap_or_else(|| panic!("stored value {stored:?} did not load back"));
            assert_eq!(loaded.alpha(), alpha, "stored as {stored:?}");
            assert_eq!(
                (loaded.red(), loaded.green(), loaded.blue()),
                (0x12, 0xAB, 0xEF)
            );
        }
    }

    #[test]
    fn wallpaper_color_saved_before_intensity_existed_still_loads_with_the_default_alpha() {
        let loaded = wallpaper_fg_decode("#12ABEF").expect("legacy #RRGGBB value");
        assert_eq!(loaded.alpha(), DOODLE_ALPHA);
        assert_eq!(
            (loaded.red(), loaded.green(), loaded.blue()),
            (0x12, 0xAB, 0xEF)
        );
        assert!(wallpaper_fg_decode("").is_none());
        assert!(wallpaper_fg_decode("#12ABEF99X").is_none());
    }

    #[test]
    fn hex_round_trips_and_ignores_alpha() {
        let color = slint::Color::from_argb_u8(40, 0x12, 0xAB, 0xEF);
        assert_eq!(color_hex(color), "#12ABEF");
        assert_eq!(parse_hex_color(&color_hex(color)), Some((0x12, 0xAB, 0xEF)));
    }
}

#[cfg(test)]
mod shortcuts_row_tests {
    use crate::AppWindow;
    use slint::ComponentHandle;
    use slint::platform::{PointerEventButton, WindowEvent};
    use std::time::Duration;

    const SIZE: (f32, f32) = (1100.0, 700.0);

    // Lets the 180ms modal fade and the 300ms splash fade finish.
    fn settle() {
        for _ in 0..3 {
            std::thread::sleep(Duration::from_millis(250));
            slint::platform::update_timers_and_animations();
        }
    }

    fn open_settings_general() -> AppWindow {
        let ui = AppWindow::new().expect("create Slint component");
        slint::select_bundled_translation("en_US").expect("bundled locale exists");
        ui.set_screen("main".into());
        // The splash (TouchArea over the whole window, ui/components/splash.slint)
        // stays up while app-loading is true, which is the default.
        ui.set_app_loading(false);
        ui.set_settings_section("general".into());
        ui.set_settings_open(true);
        ui.show().expect("show window");
        ui.window()
            .set_size(slint::LogicalSize::new(SIZE.0, SIZE.1));
        settle();
        ui
    }

    fn click(ui: &AppWindow, x: f32, y: f32) {
        let position = slint::LogicalPosition::new(x, y);
        let window = ui.window();
        window.dispatch_event(WindowEvent::PointerMoved { position });
        window.dispatch_event(WindowEvent::PointerPressed {
            position,
            button: PointerEventButton::Left,
        });
        window.dispatch_event(WindowEvent::PointerReleased {
            position,
            button: PointerEventButton::Left,
        });
    }

    // Runs on the process-wide Slint thread shared with the other window
    // tests (`ui_thread`), so it can coexist with them in one test run. The
    // two scenarios stay in one test: they reuse that thread sequentially.
    #[test]
    fn settings_general_shortcuts_row_opens_a_visible_dialog() {
        crate::bridge::ui_thread::run(|done| {
            let outcome = std::panic::catch_unwind(|| {
                shortcuts_row_opens_the_dialog();
                shortcuts_dialog_is_clickable_above_open_settings();
            });
            done(outcome.map_err(|payload| {
                payload
                    .downcast_ref::<String>()
                    .cloned()
                    .or_else(|| payload.downcast_ref::<&str>().map(|s| s.to_string()))
                    .unwrap_or_else(|| "shortcuts test panicked".to_string())
            }));
        });
    }

    // Clicks the content area of Settings > General (right of the category
    // list) until the "Keyboard shortcuts" row reacts.
    fn shortcuts_row_opens_the_dialog() {
        let ui = open_settings_general();
        let size = ui.window().size();
        assert!(ui.get_settings_open());
        assert!(!ui.get_shortcuts_open());
        // Content starts below the 78px header; stay under it so the back
        // button (top-right) is never hit. Diagnostics say what the clicks
        // reached when the row does not react.
        let toggled = std::rc::Rc::new(std::cell::Cell::new(false));
        let probe = toggled.clone();
        ui.on_autostart_changed(move |_| probe.set(true));
        let mut toggled_at = None;
        let mut closed_at = None;
        let mut opened_at = None;
        let mut y = 100.0;
        'scan: while y < SIZE.1 - 10.0 {
            let mut x = 380.0;
            while x < SIZE.0 - 10.0 {
                click(&ui, x, y);
                if toggled_at.is_none() && toggled.get() {
                    toggled_at = Some((x, y));
                }
                if !ui.get_settings_open() {
                    closed_at = Some((x, y));
                    break 'scan;
                }
                if ui.get_shortcuts_open() {
                    opened_at = Some((x, y));
                    break 'scan;
                }
                x += 20.0;
            }
            y += 4.0;
        }
        assert!(
            opened_at.is_some(),
            "no click reached the Keyboard shortcuts row: window={}x{} scale={} autostart_toggle_hit={toggled_at:?} settings_closed_by_click={closed_at:?}",
            size.width,
            size.height,
            ui.window().scale_factor(),
        );
    }

    // With Settings open, the shortcuts dialog must sit above it: a click on
    // its close button (top-right of the centred 520px dialog) closes it.
    fn shortcuts_dialog_is_clickable_above_open_settings() {
        let ui = open_settings_general();
        ui.set_shortcuts_open(true);
        settle();
        assert!(ui.get_settings_open());
        let mut y = 22.0;
        'scan: while y < 90.0 {
            let mut x = 700.0;
            while x < 806.0 {
                click(&ui, x, y);
                if !ui.get_shortcuts_open() {
                    break 'scan;
                }
                x += 6.0;
            }
            y += 6.0;
        }
        assert!(
            !ui.get_shortcuts_open(),
            "the shortcuts dialog did not receive clicks while Settings was open"
        );
    }
}
