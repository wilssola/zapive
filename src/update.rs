// Self-update against the GitHub releases the CI publishes on every
// master build. The updater consumes the zapive-<target>.zip assets
// holding just the binary; self_update swaps the running executable in
// place and the new version starts on the next launch. The installer,
// AppImage and DMG assets on the same release are for first installs.
// AppImage runs replace the .AppImage file itself, and Flatpak installs
// never self-update (the store owns the lifecycle).
const OWNER: &str = "wilssola";
const REPO: &str = "zapive";

#[cfg(windows)]
const TARGET: &str = "windows-x86_64";
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const TARGET: &str = "macos-aarch64";
#[cfg(all(target_os = "macos", not(target_arch = "aarch64")))]
const TARGET: &str = "macos-x86_64";
#[cfg(all(unix, not(target_os = "macos")))]
const TARGET: &str = "linux-x86_64";

#[cfg(windows)]
const BIN: &str = "zapive.exe";
#[cfg(not(windows))]
const BIN: &str = "zapive";

pub fn current_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

// While the repository is private, the API needs a token; once public
// this just works unauthenticated.
fn token() -> Option<String> {
    std::env::var("ZAPIVE_GH_TOKEN").ok().filter(|t| !t.is_empty())
}

// Returns the newer version tag, if one is published.
pub fn check() -> Option<String> {
    if std::env::var_os("FLATPAK_ID").is_some() {
        return None;
    }
    let mut list = self_update::backends::github::ReleaseList::configure();
    list.repo_owner(OWNER).repo_name(REPO);
    if let Some(t) = token() {
        list.auth_token(&t);
    }
    let releases = match list.build().and_then(|l| l.fetch()) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("[update] check failed: {e}");
            return None;
        }
    };
    let releases = releases.into_vec();
    let latest = releases.first()?;
    let latest_version = latest.version().trim_start_matches('v');
    let newer = self_update::version::bump_is_greater(current_version(), latest_version)
        .unwrap_or(false);
    if newer { Some(latest_version.to_string()) } else { None }
}

// Downloads the matching asset and replaces the running executable.
pub fn apply() -> Result<String, String> {
    if std::env::var_os("FLATPAK_ID").is_some() {
        return Err("updates are managed by Flatpak".into());
    }
    // Inside an AppImage the running binary sits on a read-only squashfs
    // mount; the file to replace is the .AppImage itself.
    #[cfg(all(unix, not(target_os = "macos")))]
    if let Ok(appimage) = std::env::var("APPIMAGE") {
        return apply_appimage(&appimage);
    }
    let mut update = self_update::backends::github::Update::configure();
    update
        .repo_owner(OWNER)
        .repo_name(REPO)
        .bin_name(BIN)
        .target(TARGET)
        .current_version(current_version())
        .no_confirm(true)
        .show_output(false)
        .show_download_progress(false);
    if let Some(t) = token() {
        update.auth_token(&t);
    }
    let status = update
        .build()
        .map_err(|e| e.to_string())?
        .update()
        .map_err(|e| e.to_string())?;
    Ok(status.version().to_string())
}

// Downloads the release's .AppImage asset and renames it over the current
// one; the running (mounted) instance keeps working until relaunch.
#[cfg(all(unix, not(target_os = "macos")))]
fn apply_appimage(appimage: &str) -> Result<String, String> {
    use std::os::unix::fs::PermissionsExt as _;
    let latest = format!("https://api.github.com/repos/{OWNER}/{REPO}/releases/latest");
    let mut req = ureq::get(&latest).header("User-Agent", "Zapive");
    if let Some(t) = token() {
        req = req.header("Authorization", &format!("Bearer {t}"));
    }
    let mut res = req.call().map_err(|e| e.to_string())?;
    let text = res.body_mut().read_to_string().map_err(|e| e.to_string())?;
    let json: serde_json::Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    let version = json
        .get("tag_name")
        .and_then(|t| t.as_str())
        .unwrap_or_default()
        .trim_start_matches('v')
        .to_string();
    // The API asset url works for private repos too (with the token);
    // browser_download_url would not.
    let asset_url = json
        .get("assets")
        .and_then(|a| a.as_array())
        .and_then(|assets| {
            assets.iter().find(|a| {
                a.get("name")
                    .and_then(|n| n.as_str())
                    .is_some_and(|n| n.ends_with(".AppImage"))
            })
        })
        .and_then(|a| a.get("url"))
        .and_then(|u| u.as_str())
        .ok_or("no AppImage asset in the latest release")?
        .to_string();
    let mut req = ureq::get(&asset_url)
        .header("User-Agent", "Zapive")
        .header("Accept", "application/octet-stream");
    if let Some(t) = token() {
        req = req.header("Authorization", &format!("Bearer {t}"));
    }
    let mut res = req.call().map_err(|e| e.to_string())?;
    let bytes = res
        .body_mut()
        .with_config()
        .limit(512 * 1024 * 1024)
        .read_to_vec()
        .map_err(|e| e.to_string())?;
    // Same directory, then rename: atomic swap on the same filesystem.
    let tmp = format!("{appimage}.new");
    std::fs::write(&tmp, &bytes).map_err(|e| e.to_string())?;
    std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))
        .map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, appimage).map_err(|e| e.to_string())?;
    Ok(version)
}
