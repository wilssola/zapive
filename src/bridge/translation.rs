// Local message translation: settings, per-message state and the engine requests.
use super::*;
use crate::TranslationModelItem;
use crate::i18n::tp;
use std::collections::BTreeSet;

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub(super) struct TranslationSettings {
    pub(super) source: String,
    pub(super) target: String,
    pub(super) policies: HashMap<String, String>,
}

impl Default for TranslationSettings {
    fn default() -> Self {
        Self {
            source: "auto".into(),
            target: "pt".into(),
            policies: HashMap::new(),
        }
    }
}

/// Language pairs known to the engine: the cached catalog and what is installed.
#[derive(Default)]
pub(super) struct TranslationPairs {
    pub(super) catalog: BTreeSet<(String, String)>,
    pub(super) installed: BTreeSet<(String, String)>,
}

pub(super) struct TranslationEntry {
    pub(super) request: u64,
    pub(super) original: String,
    pub(super) source: String,
    pub(super) text: String,
    pub(super) error: String,
    pub(super) busy: bool,
    pub(super) hidden: bool,
    pub(super) automatic: bool,
}

pub(super) fn translation_language_name(code: &str) -> String {
    t(match code {
        "en" => "translation.language.en",
        "pt" => "translation.language.pt",
        "es" => "translation.language.es",
        "fr" => "translation.language.fr",
        "de" => "translation.language.de",
        "zh" => "translation.language.zh",
        "ja" => "translation.language.ja",
        "ru" => "translation.language.ru",
        "it" => "translation.language.it",
        "ar" => "translation.language.ar",
        _ => "translation.language.other",
    })
}

pub(super) fn translation_error(code: &str) -> String {
    t(match code {
        "invalid_model_archive" | "invalid_model_file" | "model_file_missing" => {
            "translation.error.model_invalid"
        }
        "model_install_failed" => "translation.error.install_failed",
        "invalid_catalog" => "translation.error.catalog_invalid",
        "insecure_download" => "translation.error.download_insecure",
        "download_too_large" => "translation.error.download_too_large",
        "worker_unavailable" => "translation.error.engine_unavailable",
        "runtime_missing" => "translation.error.runtime_missing",
        "source_required" => "translation.error.source_required",
        "model_not_installed" | "pair_unavailable" => "translation.error.model_missing",
        "worker_busy" => "translation.error.busy",
        "worker_timeout" => "translation.error.timeout",
        "request_too_large" => "translation.error.too_long",
        "network_tls_certificate" => "translation.error.network_tls_certificate",
        "network_tls" => "translation.error.network_tls",
        "network_access_denied" => "translation.error.network_access_denied",
        "network_http_unauthorized" => "translation.error.network_http_unauthorized",
        "network_http_forbidden" => "translation.error.network_http_forbidden",
        "network_proxy_auth" => "translation.error.network_proxy_auth",
        "network_rate_limited" | "network_server" => "translation.error.network_unavailable",
        "network_not_found" => "translation.error.network_not_found",
        "model_download_failed"
        | "network_failed"
        | "network_timeout"
        | "network_dns"
        | "network_refused"
        | "network_interrupted"
        | "network_unreachable"
        | "network_http" => "translation.error.download_failed",
        "model_unsupported" => "translation.error.model_unsupported",
        "model_remove_failed" => "translation.models.remove_failed",
        _ => "translation.error.unavailable",
    })
}

pub(super) fn translation_size(bytes: u64) -> String {
    let value = format!("{:.1}", bytes as f64 / (1024.0 * 1024.0));
    let value = if crate::i18n::current_locale() == crate::i18n::Locale::Pt {
        value.replace('.', ",")
    } else {
        value
    };
    ta("translation.models.size_mb", &[&value])
}

fn translation_pair_set(pairs: &str) -> BTreeSet<(String, String)> {
    pairs
        .split(',')
        .filter_map(|pair| pair.split_once(':'))
        .map(|(a, b)| (a.into(), b.into()))
        .collect()
}

pub(super) fn translation_progress_text(value: &serde_json::Value) -> String {
    let (step, steps) = (
        value["step"].as_u64().unwrap_or(1).to_string(),
        value["steps"].as_u64().unwrap_or(1).to_string(),
    );
    if value["phase"].as_str() == Some("install") {
        return ta("translation.models.progress_install", &[&step, &steps]);
    }
    let heading = ta("translation.models.progress_download", &[&step, &steps]);
    let (downloaded, total) = (
        value["downloaded"].as_u64().unwrap_or(0),
        value["total"].as_u64().unwrap_or(0),
    );
    let bytes = if total > 0 {
        ta(
            "translation.models.progress_bytes",
            &[&translation_size(downloaded), &translation_size(total)],
        )
    } else if downloaded > 0 {
        translation_size(downloaded)
    } else {
        return heading;
    };
    format!("{heading} · {bytes}")
}

pub(super) fn translation_eligible(m: &StoredMessage) -> bool {
    !m.deleted
        && !m.revoked
        && !m.sticker
        && !m.text.trim().is_empty()
        && m.text.len() <= 16_000
        && matches!(
            m.kind,
            MessageKind::Text | MessageKind::Image | MessageKind::Video
        )
        && crate::wa_map::special_card(m).kind.is_empty()
}

impl Bridge {
    pub(super) fn load_translation_settings(&mut self) {
        self.translation_settings = self
            .vault
            .as_ref()
            .and_then(|v| v.get("settings:translation"))
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        self.ui
            .set_translation_source(self.translation_settings.source.clone().into());
        self.ui
            .set_translation_target(self.translation_settings.target.clone().into());
    }

    pub(super) fn save_translation_settings(&self) {
        if let Some(vault) = &self.vault {
            if let Ok(value) = serde_json::to_string(&self.translation_settings) {
                vault.set("settings:translation", &value);
            }
        }
    }

    pub(super) fn reset_translation(&mut self) {
        self.translation_gen = self.translation_gen.wrapping_add(1);
        self.translation_cache.clear();
        self.translation = crate::translation::Worker::new();
        self.translation_monitor = None;
        self.ui.set_translation_running(false);
        self.ui.set_translation_busy(false);
        self.ui
            .set_translation_status(t("translation.service.stopped").into());
        self.ui.set_translation_detail("".into());
        self.ui.set_translation_installed(ModelRc::default());
        self.ui.set_translation_pairs("".into());
        self.translation_pairs = TranslationPairs::default();
        self.translation_progress_clear();
        self.update_translation_pair_state();
    }

    fn translation_progress_clear(&mut self) {
        self.ui.set_translation_installing(false);
        self.ui.set_translation_progress(-1.0);
        self.ui.set_translation_progress_text("".into());
    }

    pub(super) fn translation_model_selected(&mut self, source: &str) {
        self.ui.set_translation_model_source(source.into());
        self.update_translation_pair_state();
    }

    /// Whether the pair chosen for installation is installed, downloadable or unknown.
    pub(super) fn update_translation_pair_state(&mut self) {
        let source = self.ui.get_translation_model_source().to_string();
        let target = self.translation_settings.target.clone();
        let pairs = &self.translation_pairs;
        let (state, detail) = if source == target {
            (0, String::new())
        } else {
            match crate::translation::missing_models(
                &source,
                &target,
                &pairs.catalog,
                &pairs.installed,
            ) {
                Some(0) => (2, t("translation.models.state_installed")),
                Some(n) => (1, tp("translation.models.state_available", n as i64, &[])),
                None if pairs.catalog.is_empty() => (0, t("translation.models.state_unknown")),
                None => (3, t("translation.models.state_unavailable")),
            }
        };
        self.ui.set_translation_pair_state(state);
        self.ui.set_translation_pair_detail(detail.into());
    }

    fn apply_translation_status(&mut self, value: &serde_json::Value) {
        if let Some(models) = value["installed"].as_array() {
            let mut installed = BTreeSet::new();
            let items: Vec<TranslationModelItem> = models
                .iter()
                .filter_map(|model| {
                    let (source, target) = (model["source"].as_str()?, model["target"].as_str()?);
                    installed.insert((source.to_string(), target.to_string()));
                    Some(TranslationModelItem {
                        source: source.into(),
                        target: target.into(),
                        label: format!(
                            "{} → {}",
                            translation_language_name(source),
                            translation_language_name(target)
                        )
                        .into(),
                        size: translation_size(model["size"].as_u64().unwrap_or(0)).into(),
                    })
                })
                .collect();
            self.translation_pairs.installed = installed;
            self.ui
                .set_translation_installed(ModelRc::new(VecModel::from(items)));
        }
        if let Some(pairs) = value["pairs"].as_str() {
            self.translation_pairs.catalog = translation_pair_set(pairs);
            self.ui
                .set_translation_pairs(self.translation_pairs_label(pairs).into());
        }
        self.update_translation_pair_state();
    }

    pub(super) fn translation_language(&mut self, kind: &str, value: &str) {
        if self.vault.is_none() || self.switching {
            return;
        }
        if !matches!(
            value,
            "en" | "pt" | "es" | "fr" | "de" | "zh" | "ja" | "ru" | "it" | "ar"
        ) && !(kind == "source" && value == "auto")
        {
            return;
        }
        match kind {
            "source" => self.translation_settings.source = value.into(),
            "target" => self.translation_settings.target = value.into(),
            _ => return,
        }
        self.save_translation_settings();
        self.ui
            .set_translation_source(self.translation_settings.source.clone().into());
        self.ui
            .set_translation_target(self.translation_settings.target.clone().into());
        self.update_translation_pair_state();
        if let Some(jid) = self.current_jid.clone() {
            self.rebuild_conversation(&jid);
            self.auto_translate_chat(&jid);
        }
    }

    pub(super) fn install_translation_model(&mut self, source: &str) {
        if !matches!(
            source,
            "en" | "pt" | "es" | "fr" | "de" | "zh" | "ja" | "ru" | "it" | "ar"
        ) || source == self.translation_settings.target
        {
            return;
        }
        let target = self.translation_settings.target.clone();
        self.run_translation_action("install", Some((source, &target)));
    }

    pub(super) fn remove_translation_model(&mut self, source: &str, target: &str) {
        if !self
            .translation_pairs
            .installed
            .contains(&(source.to_string(), target.to_string()))
        {
            return;
        }
        self.run_translation_action("remove", Some((source, target)));
    }

    pub(super) fn translation_action(&mut self, action: &str) {
        if action == "cancel" && !self.ui.get_translation_installing() {
            return;
        }
        self.run_translation_action(action, None);
    }

    pub(super) fn run_translation_action(&mut self, action: &str, model: Option<(&str, &str)>) {
        log::info!("[translation] requested action={action}");
        if self.vault.is_none()
            || self.switching
            || (self.ui.get_translation_busy() && !matches!(action, "stop" | "cancel"))
        {
            log::warn!(
                "[translation] action ignored: unlocked={} switching={} busy={}",
                self.vault.is_some(),
                self.switching,
                self.ui.get_translation_busy()
            );
            return;
        }
        let op = match action {
            "start" => "status",
            "restart" => "restart",
            "stop" => "stop",
            "refresh" => "refresh",
            "install" => "install",
            "remove" => "remove",
            "cancel" => "cancel",
            _ => return,
        };
        if matches!(op, "restart" | "stop" | "cancel") {
            self.translation_gen = self.translation_gen.wrapping_add(1);
            for e in self.translation_cache.values_mut() {
                if e.busy {
                    e.busy = false;
                    e.error = t("translation.status.cancelled");
                }
            }
            if let Some(jid) = self.current_jid.clone() {
                self.rebuild_conversation(&jid);
            }
        }
        let generation = self.translation_gen;
        let epoch = self.epoch;
        let account = self.account.clone();
        let stopped = op == "stop";
        let cancelled = op == "cancel";
        self.ui.set_translation_busy(true);
        self.ui.set_translation_detail("".into());
        if op == "install" {
            self.ui.set_translation_installing(true);
            self.ui.set_translation_progress(-1.0);
            self.ui
                .set_translation_progress_text(t("translation.models.installing").into());
        } else {
            self.translation_progress_clear();
        }
        let (source, target) = model.unwrap_or((
            &self.translation_settings.source,
            &self.translation_settings.target,
        ));
        log::info!("[translation] action={op} source={source} target={target}");
        let request = serde_json::json!({"op":op, "source":source, "target":target});
        let progress_account = account.clone();
        let progress = move |value: serde_json::Value| {
            let account = progress_account.clone();
            ui_apply(move |b| {
                if b.translation_gen != generation
                    || b.epoch != epoch
                    || b.account != account
                    || b.switching
                    || !b.ui.get_translation_installing()
                {
                    return;
                }
                let (downloaded, total) = (
                    value["downloaded"].as_u64().unwrap_or(0),
                    value["total"].as_u64().unwrap_or(0),
                );
                let known = value["phase"].as_str() == Some("download") && total > 0;
                b.ui.set_translation_progress(if known {
                    (downloaded as f64 / total as f64).clamp(0.0, 1.0) as f32
                } else {
                    -1.0
                });
                b.ui.set_translation_progress_text(translation_progress_text(&value).into());
            });
        };
        let done = move |result: Result<serde_json::Value, String>| {
            ui_apply(move |b| {
                if b.translation_gen != generation
                    || b.epoch != epoch
                    || b.account != account
                    || b.switching
                {
                    return;
                }
                b.ui.set_translation_busy(false);
                b.translation_progress_clear();
                match result {
                    Ok(value) => {
                        log::info!("[translation] action completed");
                        b.ui.set_translation_detail(
                            if cancelled {
                                t("translation.models.cancelled")
                            } else {
                                String::new()
                            }
                            .into(),
                        );
                        b.ui.set_translation_running(
                            b.translation.status()["running"].as_bool().unwrap_or(false),
                        );
                        b.ui.set_translation_status(
                            if stopped {
                                t("translation.service.stopped")
                            } else {
                                t("translation.service.ready")
                            }
                            .into(),
                        );
                        b.apply_translation_status(&value);
                        if !stopped {
                            b.monitor_translation(generation);
                            if let Some(jid) = b.current_jid.clone() {
                                b.auto_translate_chat(&jid);
                            }
                        }
                    }
                    Err(error) => b.translation_failed(&error),
                }
            });
        };
        let result = if op == "install" {
            self.translation
                .request_with_progress(request, progress, done)
        } else {
            self.translation.request(request, done)
        };
        if let Err(error) = result {
            self.translation_failed(&error);
        }
    }

    pub(super) fn translation_pairs_label(&self, pairs: &str) -> String {
        pairs
            .split(',')
            .filter_map(|pair| pair.split_once(':'))
            .map(|(a, b)| {
                format!(
                    "{} → {}",
                    translation_language_name(a),
                    translation_language_name(b)
                )
            })
            .collect::<Vec<_>>()
            .join(", ")
    }

    pub(super) fn translation_failed(&mut self, error: &str) {
        log::error!("[translation] operation failed: {error}");
        self.ui.set_translation_busy(false);
        self.translation_progress_clear();
        self.ui.set_translation_running(
            self.translation.status()["running"]
                .as_bool()
                .unwrap_or(false),
        );
        self.ui
            .set_translation_status(t("translation.service.unavailable").into());
        self.ui
            .set_translation_detail(translation_error(error).into());
        if self.ui.get_translation_running() {
            self.monitor_translation(self.translation_gen);
        }
    }

    pub(super) fn monitor_translation(&mut self, generation: u64) {
        if self.translation_monitor == Some(generation) {
            return;
        }
        self.translation_monitor = Some(generation);
        self.once(5000, move |b| {
            if b.translation_monitor == Some(generation) {
                b.translation_monitor = None;
            }
            if b.translation_gen != generation || !b.ui.get_translation_running() {
                return;
            }
            if b.translation.status()["running"].as_bool() != Some(true) {
                b.translation_failed("runtime_failed");
                for e in b.translation_cache.values_mut().filter(|e| e.busy) {
                    e.busy = false;
                    e.error = translation_error("runtime_failed");
                }
                if let Some(jid) = b.current_jid.clone() {
                    b.rebuild_conversation(&jid);
                }
            } else {
                b.monitor_translation(generation);
            }
        });
    }

    pub(super) fn translation_key(&self, jid: &str, id: &str) -> (String, String, String, String) {
        (
            jid.into(),
            id.into(),
            self.translation_settings.source.clone(),
            self.translation_settings.target.clone(),
        )
    }

    pub(super) fn translation_entry(&self, m: &StoredMessage) -> Option<&TranslationEntry> {
        if !translation_eligible(m) {
            return None;
        }
        self.translation_cache
            .get(&self.translation_key(&m.jid, &m.id))
            .filter(|e| e.original == m.text)
    }

    pub(super) fn refresh_translation_row(&self, jid: &str, id: &str) {
        if self.current_jid.as_deref() != Some(jid) {
            return;
        }
        if let Some(m) = self.store.messages_for(jid).iter().find(|m| m.id == id) {
            let fresh = self.to_row(m, None);
            self.patch_row(id, |row| {
                row.translationText = fresh.translationText;
                row.translationLabel = fresh.translationLabel;
                row.translationBusy = fresh.translationBusy;
                row.translationError = fresh.translationError;
                row.translationSource = fresh.translationSource;
                row.translationPolicy = fresh.translationPolicy;
            });
        }
    }

    pub(super) fn auto_translate_chat(&mut self, jid: &str) {
        let ids: Vec<String> = self
            .store
            .messages_for(jid)
            .iter()
            .rev()
            .take(40)
            .filter(|m| !m.from_me && translation_eligible(m))
            .map(|m| m.id.clone())
            .collect();
        for id in ids {
            self.request_translation(jid, &id, true);
        }
    }

    pub(super) fn request_translation(&mut self, jid: &str, id: &str, automatic: bool) {
        if self.vault.is_none() || self.switching || self.ui.get_translation_busy() {
            return;
        }
        let Some(m) = self
            .store
            .messages_for(jid)
            .iter()
            .find(|m| m.id == id)
            .cloned()
        else {
            return;
        };
        if !translation_eligible(&m) {
            return;
        }
        let source = self.translation_settings.source.clone();
        let target = self.translation_settings.target.clone();
        if source == target {
            return;
        }
        if automatic
            && (m.from_me
                || !self.ui.get_translation_running()
                || !self
                    .translation_settings
                    .policies
                    .values()
                    .any(|p| p == "always")
                || (source != "auto"
                    && self
                        .translation_settings
                        .policies
                        .get(&source)
                        .map(String::as_str)
                        != Some("always")))
        {
            return;
        }
        let key = self.translation_key(jid, id);
        if let Some(e) = self.translation_cache.get_mut(&key) {
            if e.original == m.text {
                if automatic || e.busy {
                    return;
                }
                if !e.text.is_empty() {
                    e.hidden = false;
                    self.refresh_translation_row(jid, id);
                    return;
                }
            }
        }
        if self.translation_cache.values().filter(|e| e.busy).count() >= 4 {
            return;
        }
        while self.translation_cache.len() >= 128 {
            let Some(old) = self
                .translation_cache
                .iter()
                .find(|(_, e)| !e.busy)
                .map(|(k, _)| k.clone())
            else {
                return;
            };
            self.translation_cache.remove(&old);
        }
        let original = m.text.clone();
        self.translation_request = self.translation_request.wrapping_add(1);
        let request = self.translation_request;
        self.translation_cache.insert(
            key.clone(),
            TranslationEntry {
                request,
                original: original.clone(),
                source: if source == "auto" {
                    String::new()
                } else {
                    source.clone()
                },
                text: String::new(),
                error: String::new(),
                busy: true,
                hidden: false,
                automatic,
            },
        );
        self.refresh_translation_row(jid, id);
        let generation = self.translation_gen;
        let epoch = self.epoch;
        let account = self.account.clone();
        let result = self.translation.request(serde_json::json!({"op":"translate", "source":source, "target":target, "text":original}), move |result| {
            ui_apply(move |b| {
                if generation != b.translation_gen || epoch != b.epoch || account != b.account || b.switching { return; }
                let valid = b.store.messages_for(&key.0).iter().any(|m| m.id == key.1 && m.text == original && translation_eligible(m));
                if !valid { b.translation_cache.remove(&key); return; }
                let Some(entry) = b.translation_cache.get_mut(&key) else { return; };
                if entry.original != original || entry.request != request { return; }
                entry.busy = false;
                match result {
                    Ok(value) => {
                        let detected = value["source"].as_str().unwrap_or("");
                        let text = value["text"].as_str().unwrap_or("");
                        if value["target"].as_str() != Some(key.3.as_str()) || detected.is_empty()
                            || (key.2 != "auto" && detected != key.2) || text.len() > 64_000 {
                            entry.error = translation_error("runtime_failed");
                        } else {
                            entry.source = detected.into();
                            let allowed = !automatic || (detected != key.3 && b.translation_settings.policies.get(detected).map(String::as_str) == Some("always"));
                            if allowed { entry.text = text.into(); } else { entry.hidden = true; }
                        }
                    }
                    Err(error) => entry.error = translation_error(&error),
                }
                let running = b.translation.status()["running"].as_bool().unwrap_or(false);
                if running && !b.ui.get_translation_running() {
                    b.ui.set_translation_running(true);
                    b.ui.set_translation_status(t("translation.service.ready").into());
                    b.monitor_translation(generation);
                } else if !running { b.translation_failed("runtime_failed"); }
                if b.translation_settings.source == key.2 && b.translation_settings.target == key.3 {
                    b.refresh_translation_row(&key.0, &key.1);
                }
                if automatic {
                    if let Some(jid) = b.current_jid.clone() { b.auto_translate_chat(&jid); }
                }
            });
        });
        if let Err(error) = result {
            let key = self.translation_key(jid, id);
            if let Some(e) = self.translation_cache.get_mut(&key) {
                e.busy = false;
                e.error = translation_error(&error);
            }
            self.refresh_translation_row(jid, id);
        }
    }

    pub(super) fn translation_message_action(&mut self, id: &str, action: &str) {
        let Some(m) = self.find_message(id).cloned() else {
            return;
        };
        if !translation_eligible(&m) {
            return;
        }
        if action == "translate" {
            self.request_translation(&m.jid, id, false);
            return;
        }
        let key = self.translation_key(&m.jid, id);
        let Some(entry) = self
            .translation_cache
            .get_mut(&key)
            .filter(|e| e.original == m.text)
        else {
            return;
        };
        match action {
            "translation-hide" => entry.hidden = true,
            "translation-copy" => {
                if !entry.hidden && !entry.text.is_empty() {
                    if let Ok(mut clipboard) = arboard::Clipboard::new() {
                        let _ = clipboard.set_text(entry.text.clone());
                    }
                }
            }
            "translation-always" | "translation-never" if !entry.source.is_empty() => {
                let source = entry.source.clone();
                let policy = if action == "translation-always" {
                    "always"
                } else {
                    "never"
                };
                self.translation_settings
                    .policies
                    .insert(source.clone(), policy.into());
                if policy == "never" {
                    for entry in self
                        .translation_cache
                        .values_mut()
                        .filter(|e| e.source == source && e.automatic)
                    {
                        entry.hidden = true;
                    }
                }
                if policy == "always" {
                    self.translation_cache.retain(|_, e| {
                        !e.automatic || e.busy || !e.text.is_empty() || e.source != source
                    });
                    for e in self
                        .translation_cache
                        .values_mut()
                        .filter(|e| e.automatic && e.source == source && !e.text.is_empty())
                    {
                        e.hidden = false;
                    }
                }
                self.save_translation_settings();
                self.rebuild_conversation(&m.jid);
                self.auto_translate_chat(&m.jid);
            }
            _ => {}
        }
        self.refresh_translation_row(&m.jid, id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn translation_preferences_round_trip_with_source_policies() {
        let mut settings = TranslationSettings::default();
        settings.policies.insert("en".into(), "always".into());
        settings.policies.insert("es".into(), "never".into());
        let json = serde_json::to_string(&settings).unwrap();
        let restored: TranslationSettings = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.source, "auto");
        assert_eq!(restored.target, "pt");
        assert_eq!(restored.policies["en"], "always");
        assert_eq!(restored.policies["es"], "never");
        let defaults: TranslationSettings = serde_json::from_str("{}").unwrap();
        assert_eq!(defaults.target, "pt");
    }

    #[test]
    fn translation_errors_do_not_expose_runtime_details() {
        for code in [
            "source_required",
            "model_not_installed",
            "worker_busy",
            "worker_timeout",
            "runtime_failed",
            "private message or internal path",
        ] {
            let friendly = translation_error(code);
            assert!(!friendly.is_empty());
            assert!(!friendly.contains(code));
        }
    }

    #[test]
    fn translation_unsupported_models_have_their_own_message() {
        let unsupported = translation_error("model_unsupported");
        assert_eq!(unsupported, t("translation.error.model_unsupported"));
        assert_ne!(unsupported, translation_error("invalid_model_file"));
        assert_ne!(unsupported, translation_error("unknown"));
    }

    #[test]
    fn translation_invalid_model_errors_suggest_reinstallation() {
        let expected = t("translation.error.model_invalid");
        for code in [
            "invalid_model_archive",
            "invalid_model_file",
            "model_file_missing",
        ] {
            assert_eq!(translation_error(code), expected);
        }
    }

    #[test]
    fn translation_network_denials_distinguish_http_from_socket_permissions() {
        let socket = translation_error("network_access_denied");
        for (code, status) in [
            ("network_http_unauthorized", "401"),
            ("network_http_forbidden", "403"),
            ("network_proxy_auth", "407"),
        ] {
            let friendly = translation_error(code);
            assert!(friendly.contains(status));
            assert_ne!(friendly, socket);
            assert!(!friendly.to_lowercase().contains("firewall"));
        }
        assert!(!socket.contains("HTTP"));
        assert!(!socket.to_lowercase().contains("firewall"));
    }
}
