// The only module that touches the Slint window. Lives on the UI thread;
// the WhatsApp side reaches it through ui_apply, and user actions leave
// through WaService commands. Port of src/bridge.ts on master (phase 1:
// login, pairing, connection status and the fatal-error modal).
use crate::AppWindow;
use crate::i18n::t;
use crate::qr::qr_image;
use crate::wa::{Cmd, WaService};
use slint::ComponentHandle;
use std::cell::RefCell;

pub struct Bridge {
    ui: AppWindow,
    wa: WaService,
}

thread_local! {
    static BRIDGE: RefCell<Option<Bridge>> = const { RefCell::new(None) };
}

// Runs a closure against the Bridge on the UI thread, from any thread.
// Dropped silently if the app is shutting down.
pub fn ui_apply(f: impl FnOnce(&mut Bridge) + Send + 'static) {
    let _ = slint::invoke_from_event_loop(move || {
        BRIDGE.with(|cell| {
            if let Some(bridge) = cell.borrow_mut().as_mut() {
                f(bridge);
            }
        });
    });
}

pub fn install(ui: &AppWindow, wa: WaService) {
    wire_callbacks(ui, &wa);
    let bridge = Bridge { ui: ui.clone_strong(), wa };
    BRIDGE.with(|cell| *cell.borrow_mut() = Some(bridge));
}

fn wire_callbacks(ui: &AppWindow, wa: &WaService) {
    ui.set_status_text(t("status.connecting").into());

    {
        let wa = wa.clone();
        let handle = ui.as_weak();
        ui.on_request_pairing_code(move |phone| {
            let digits: String = phone.chars().filter(|c| c.is_ascii_digit()).collect();
            let Some(ui) = handle.upgrade() else { return };
            if digits.len() < 8 {
                ui.set_pairing_code(t("error.invalidNumber").into());
                return;
            }
            ui.set_pairing_code(t("pairing.generating").into());
            wa.send(Cmd::PairWithCode(digits));
        });
    }

    {
        let wa = wa.clone();
        let handle = ui.as_weak();
        ui.on_alert_retry(move || {
            let Some(ui) = handle.upgrade() else { return };
            ui.set_alert_open(false);
            ui.set_status_text(t("status.connecting").into());
            wa.send(Cmd::Resume);
        });
    }

    {
        let handle = ui.as_weak();
        ui.on_alert_dismiss(move || {
            if let Some(ui) = handle.upgrade() {
                ui.set_alert_open(false);
            }
        });
    }

    {
        let wa = wa.clone();
        let handle = ui.as_weak();
        ui.on_logout(move || {
            if let Some(ui) = handle.upgrade() {
                ui.set_settings_open(false);
                ui.set_status_text(t("status.loggingOut").into());
            }
            wa.send(Cmd::Logout);
        });
    }

    {
        let handle = ui.as_weak();
        ui.on_theme_changed(move |mode| {
            if let Some(ui) = handle.upgrade() {
                ui.set_theme_mode(mode.clone());
                // "system" follows the OS poll in main; explicit modes win.
                match mode.as_str() {
                    "dark" => ui.set_dark_theme(true),
                    "light" => ui.set_dark_theme(false),
                    _ => {}
                }
            }
        });
    }
}

impl Bridge {
    // ---- called from the WhatsApp pump via ui_apply ----

    pub fn on_qr(&mut self, code: &str) {
        self.ui.set_qr_image(qr_image(code));
        self.ui.set_status_text(t("status.scanQr").into());
    }

    pub fn on_pairing_code(&mut self, pretty: &str) {
        self.ui.set_pairing_code(pretty.into());
        self.ui.set_status_text(t("status.pairingHint").into());
    }

    pub fn on_pairing_failed(&mut self) {
        self.ui.set_pairing_code(t("pairing.failed").into());
    }

    pub fn on_status(&mut self, text: &str) {
        self.ui.set_status_text(text.into());
    }

    pub fn on_open(&mut self, pn: &str, lid: &str) {
        let _ = (pn, lid); // used from phase 2 on (self jids for the store)
        self.ui.set_alert_open(false);
        self.ui.set_status_text(t("status.connected").into());
        if self.ui.get_screen() == "login" {
            self.ui.set_screen("main".into());
        }
    }

    // Errors the user must act on get a modal instead of a silent retry
    // loop in the status line.
    pub fn on_fatal(&mut self, kind: &str) {
        let conflict = kind == "conflict";
        self.ui
            .set_alert_title(t(if conflict { "alert.conflictTitle" } else { "alert.offlineTitle" }).into());
        self.ui
            .set_alert_text(t(if conflict { "alert.conflictBody" } else { "alert.offlineBody" }).into());
        self.ui
            .set_alert_action(t(if conflict { "alert.reconnect" } else { "alert.retry" }).into());
        self.ui.set_alert_open(true);
    }

    pub fn on_logged_out(&mut self) {
        self.ui.set_pairing_mode(false);
        self.ui.set_pairing_code("".into());
        self.ui.set_chat_open(false);
        self.ui.set_screen("login".into());
    }

    #[allow(dead_code)]
    pub fn wa(&self) -> &WaService {
        &self.wa
    }
}
