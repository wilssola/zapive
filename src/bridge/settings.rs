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

    // Chat wallpaper: both colors are #RRGGBB; an empty pair means "use the
    // theme defaults".
    pub(super) fn handle_wallpaper_colors(&mut self, bg: slint::Color, fg: slint::Color) {
        let (bg_hex, fg_hex) = (color_hex(bg), color_hex(fg));
        self.ui.set_wallpaper_bg(bg);
        self.ui.set_wallpaper_fg(slint::Color::from_argb_u8(
            DOODLE_ALPHA,
            fg.red(),
            fg.green(),
            fg.blue(),
        ));
        self.ui.set_wallpaper_custom(true);
        if let Some(vault) = &self.vault {
            vault.setting_set("chat_wallpaper_bg", &bg_hex);
            vault.setting_set("chat_wallpaper_fg", &fg_hex);
        }
    }

    // A hex typed in settings: only a valid #RRGGBB changes anything.
    pub(super) fn wallpaper_hex_entered(&mut self, fg: bool, hex: &str) {
        let Some((r, g, b)) = parse_hex_color(hex) else {
            return;
        };
        let typed = slint::Color::from_rgb_u8(r, g, b);
        let (bg, doodle) = (self.ui.get_wallpaper_bg(), self.ui.get_wallpaper_fg());
        if fg {
            self.handle_wallpaper_colors(bg, typed);
        } else {
            self.handle_wallpaper_colors(typed, doodle);
        }
    }

    pub(super) fn reset_wallpaper(&mut self) {
        self.ui.set_wallpaper_custom(false);
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
            .and_then(parse_hex_color);
        match (bg, fg) {
            (Some(bg), Some(fg)) => {
                self.ui
                    .set_wallpaper_bg(slint::Color::from_rgb_u8(bg.0, bg.1, bg.2));
                self.ui.set_wallpaper_fg(slint::Color::from_argb_u8(
                    DOODLE_ALPHA,
                    fg.0,
                    fg.1,
                    fg.2,
                ));
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

// The doodle pattern is always drawn at this alpha so it stays subtle,
// whatever opacity a color picker hands back (only #RRGGBB is stored).
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
    fn hex_round_trips_and_ignores_alpha() {
        let color = slint::Color::from_argb_u8(40, 0x12, 0xAB, 0xEF);
        assert_eq!(color_hex(color), "#12ABEF");
        assert_eq!(parse_hex_color(&color_hex(color)), Some((0x12, 0xAB, 0xEF)));
    }
}
