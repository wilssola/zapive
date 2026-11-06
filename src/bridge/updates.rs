// Self-update.
use super::*;

impl Bridge {
    // ---- self-update ----

    pub(super) fn load_changelog(&mut self, refresh: bool) {
        if self.ui.get_changelog_loading() {
            return;
        }
        if !self.ui.get_changelog_loaded()
            && let Some(cached) = self.vault.as_ref().and_then(|v| v.get("release-history"))
            && let Ok((releases, page, more)) =
                serde_json::from_str::<(Vec<crate::update::ReleaseNote>, i32, bool)>(&cached)
        {
            self.set_changelog(releases);
            self.ui.set_changelog_page(page.max(1));
            self.ui.set_changelog_more(more);
        }
        let page = if refresh || !self.ui.get_changelog_loaded() {
            1
        } else {
            self.ui.get_changelog_page().max(1) as u32
        };
        if page == 1 {
            self.ui.set_changelog_verified(false);
        }
        self.ui.set_changelog_loading(true);
        self.ui.set_changelog_error("".into());
        std::thread::spawn(move || {
            let result = crate::update::release_notes(page);
            ui_apply(move |b| {
                b.ui.set_changelog_loading(false);
                match result {
                    Ok(result) => {
                        let mut releases = if page == 1 {
                            Vec::new()
                        } else {
                            b.changelog_entries()
                        };
                        for release in result.releases {
                            if !releases.iter().any(|existing| existing.tag == release.tag) {
                                releases.push(release);
                            }
                        }
                        let next_page = page.saturating_add(1).min(i32::MAX as u32) as i32;
                        if let Some(vault) = &b.vault
                            && let Ok(cache) =
                                serde_json::to_string(&(&releases, next_page, result.has_more))
                        {
                            vault.set("release-history", &cache);
                        }
                        b.set_changelog(releases);
                        b.ui.set_changelog_page(next_page);
                        b.ui.set_changelog_more(result.has_more);
                        b.ui.set_changelog_loaded(true);
                        if page == 1 {
                            b.ui.set_changelog_verified(true);
                        }
                    }
                    Err(_) => b.ui.set_changelog_error(t("changelog.failed").into()),
                }
            });
        });
    }

    pub(super) fn changelog_entries(&self) -> Vec<crate::update::ReleaseNote> {
        self.ui
            .get_releases()
            .iter()
            .map(|release| crate::update::ReleaseNote {
                tag: release.tag.to_string(),
                date: release.date.to_string(),
                notes: release.notes.to_string(),
            })
            .collect()
    }

    pub(super) fn set_changelog(&self, releases: Vec<crate::update::ReleaseNote>) {
        let items: Vec<ReleaseItem> = releases
            .into_iter()
            .map(|release| ReleaseItem {
                installed: release.tag.trim_start_matches('v') == crate::update::current_version(),
                newer: crate::update::is_newer_release(&release.tag),
                tag: release.tag.into(),
                date: release.date.into(),
                notes: release.notes.into(),
            })
            .collect();
        self.ui
            .set_changelog_has_new(items.iter().any(|item| item.newer));
        self.ui.set_releases(ModelRc::new(VecModel::from(items)));
    }

    pub(super) fn update_tick(&mut self) {
        crate::jobs::check_update();
        // Check again every hour while the app stays open: a release
        // published during the day shows up the same day.
        self.once(3_600_000, |b| b.update_tick());
    }

    pub fn on_update_checked(&mut self, result: Result<Option<String>, String>) {
        let manual = std::mem::take(&mut self.update_manual);
        match result {
            Ok(Some(version)) => {
                self.ui.set_update_version(version.into());
                if self.ui.get_update_state() == 0 {
                    self.ui.set_update_state(1);
                }
                if manual {
                    self.ui.set_settings_status("".into());
                }
            }
            Ok(None) => {
                if manual {
                    self.ui.set_settings_status(t("update.latest").into());
                }
            }
            Err(_) => {
                if manual {
                    self.ui.set_settings_status(t("changelog.failed").into());
                }
            }
        }
    }

    pub fn on_update_applied(&mut self, result: Result<String, String>) {
        match result {
            Ok(version) => {
                self.ui.set_update_version(version.into());
                self.ui.set_update_state(3);
            }
            // Said out loud: a release build has no console, and going
            // quietly back to "update available" reads as a button that
            // does nothing.
            Err(e) => {
                log::error!("[update] failed: {e}");
                self.ui.set_update_error(e.into());
                self.ui.set_update_state(4);
            }
        }
    }
}
