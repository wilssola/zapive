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
