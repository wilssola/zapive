// Self-update against the GitHub releases the CI publishes on every
// master build. The updater consumes the zapive-<target>.zip assets
// holding just the binary, checks it against the release's SHA256SUMS.txt
// and swaps it in for the running executable (see apply); the new version
// starts on the next launch. The installer,
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
//
// The swap is two renames inside the executable's own directory, with the
// new binary already there and checked. The library's replace moved the
// running exe away first and wrote the new one afterwards: on a full disk
// the write failed and nothing put the old one back, which left the
// install directory with no executable at all.
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
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let exe = exe.canonicalize().unwrap_or(exe);
    let dir = exe.parent().ok_or("the executable has no directory")?.to_path_buf();

    let release = latest_release()?;
    let zip_name = format!("zapive-{TARGET}.zip");
    let zip_url = release.asset(&zip_name).ok_or(format!("no {zip_name} in release {}", release.version))?;

    // Staged next to the executable: the same volume, so the renames
    // below cannot run out of space, and a directory that cannot be
    // written to says so before anything is touched.
    let stage = dir.join(STAGE_DIR);
    let _ = std::fs::remove_dir_all(&stage);
    std::fs::create_dir_all(&stage)
        .map_err(|e| format!("cannot write to {}: {e}", dir.display()))?;
    let staged = stage_binary(&release, &zip_name, &zip_url, &stage);
    let result = staged.and_then(|fresh| swap_in(&exe, &fresh));
    let _ = std::fs::remove_dir_all(&stage);
    result.map(|()| release.version)
}

const STAGE_DIR: &str = ".zapive-update";

fn old_path(exe: &std::path::Path) -> std::path::PathBuf {
    let mut name = exe.file_name().unwrap_or_default().to_os_string();
    name.push(".old");
    exe.with_file_name(name)
}

// What an update leaves behind: Windows cannot delete the executable of
// a running process, so the previous binary waits for the next launch.
pub fn sweep() {
    let Ok(exe) = std::env::current_exe() else { return };
    let exe = exe.canonicalize().unwrap_or(exe);
    let _ = std::fs::remove_file(old_path(&exe));
    if let Some(dir) = exe.parent() {
        let _ = std::fs::remove_dir_all(dir.join(STAGE_DIR));
    }
}

struct LatestRelease {
    version: String,
    // (name, API url) of every asset.
    assets: Vec<(String, String)>,
}

impl LatestRelease {
    fn asset(&self, name: &str) -> Option<String> {
        self.assets.iter().find(|(n, _)| n == name).map(|(_, url)| url.clone())
    }
}

fn get(url: &str, accept: &str) -> Result<ureq::http::Response<ureq::Body>, String> {
    let mut req = ureq::get(url).header("User-Agent", "Zapive").header("Accept", accept);
    if let Some(t) = token() {
        req = req.header("Authorization", &format!("Bearer {t}"));
    }
    req.call().map_err(|e| e.to_string())
}

fn latest_release() -> Result<LatestRelease, String> {
    let url = format!("https://api.github.com/repos/{OWNER}/{REPO}/releases/latest");
    let text = get(&url, "application/vnd.github+json")?
        .body_mut()
        .read_to_string()
        .map_err(|e| e.to_string())?;
    let json: serde_json::Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    let version = json
        .get("tag_name")
        .and_then(|t| t.as_str())
        .ok_or("the latest release has no tag")?
        .trim_start_matches('v')
        .to_string();
    // The API asset url works for private repos too (with the token);
    // browser_download_url would not.
    let assets = json
        .get("assets")
        .and_then(|a| a.as_array())
        .map(|assets| {
            assets
                .iter()
                .filter_map(|a| {
                    Some((a.get("name")?.as_str()?.to_string(), a.get("url")?.as_str()?.to_string()))
                })
                .collect()
        })
        .unwrap_or_default();
    Ok(LatestRelease { version, assets })
}

fn download(url: &str, limit: u64) -> Result<Vec<u8>, String> {
    get(url, "application/octet-stream")?
        .body_mut()
        .with_config()
        .limit(limit)
        .read_to_vec()
        .map_err(|e| e.to_string())
}

// Downloads the zip into `stage`, checks it against the release's
// SHA256SUMS.txt and unpacks the binary. Returns the binary's path.
fn stage_binary(
    release: &LatestRelease,
    zip_name: &str,
    zip_url: &str,
    stage: &std::path::Path,
) -> Result<std::path::PathBuf, String> {
    use sha2::Digest as _;
    let bytes = download(zip_url, 512 * 1024 * 1024)?;
    // A download cut short is the other way to end up with a binary that
    // does not start. Every release carries the sums; one that somehow
    // does not is installed as before, unverified.
    match release.asset("SHA256SUMS.txt") {
        Some(sums_url) => {
            let sums = String::from_utf8_lossy(&download(&sums_url, 1024 * 1024)?).into_owned();
            let expected = sums
                .lines()
                .filter_map(|line| line.split_once(char::is_whitespace))
                .find(|(_, name)| name.trim().trim_start_matches('*') == zip_name)
                .map(|(sum, _)| sum.trim().to_ascii_lowercase())
                .ok_or(format!("{zip_name} is not listed in SHA256SUMS.txt"))?;
            let actual: String =
                sha2::Sha256::digest(&bytes).iter().map(|b| format!("{b:02x}")).collect();
            if actual != expected {
                return Err("the download does not match its checksum".into());
            }
        }
        None => log::warn!("[update] release {} has no SHA256SUMS.txt", release.version),
    }
    let zip = stage.join(zip_name);
    std::fs::write(&zip, &bytes).map_err(|e| format!("cannot save the download: {e}"))?;
    drop(bytes);
    self_update::Extract::from_source(&zip)
        .archive(self_update::ArchiveKind::Zip)
        .extract_file(stage, BIN)
        .map_err(|e| format!("cannot unpack the download: {e}"))?;
    let fresh = stage.join(BIN);
    let size = std::fs::metadata(&fresh).map(|m| m.len()).unwrap_or(0);
    if size < 1024 * 1024 {
        return Err(format!("the unpacked binary is only {size} bytes"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&fresh, std::fs::Permissions::from_mode(0o755))
            .map_err(|e| e.to_string())?;
    }
    Ok(fresh)
}

// Old binary aside, new one in, and the old one back if that fails. A
// running executable can be renamed on every platform this ships on.
fn swap_in(exe: &std::path::Path, fresh: &std::path::Path) -> Result<(), String> {
    let old = old_path(exe);
    let _ = std::fs::remove_file(&old);
    std::fs::rename(exe, &old).map_err(|e| format!("cannot move the current version aside: {e}"))?;
    if let Err(e) = std::fs::rename(fresh, exe) {
        return match std::fs::rename(&old, exe) {
            Ok(()) => Err(format!("cannot put the new version in place: {e}")),
            Err(back) => Err(format!(
                "cannot put the new version in place ({e}) nor the old one back ({back}); it is at {}",
                old.display()
            )),
        };
    }
    // Deleting it works right away everywhere but Windows, where it
    // stays until sweep() at the next launch.
    let _ = std::fs::remove_file(&old);
    Ok(())
}

// Downloads the release's .AppImage asset and renames it over the current
// one; the running (mounted) instance keeps working until relaunch.
#[cfg(all(unix, not(target_os = "macos")))]
fn apply_appimage(appimage: &str) -> Result<String, String> {
    use std::os::unix::fs::PermissionsExt as _;
    let release = latest_release()?;
    let asset_url = release
        .assets
        .iter()
        .find(|(name, _)| name.ends_with(".AppImage"))
        .map(|(_, url)| url.clone())
        .ok_or("no AppImage asset in the latest release")?;
    let bytes = download(&asset_url, 512 * 1024 * 1024)?;
    // Same directory, then rename: atomic swap on the same filesystem.
    let tmp = format!("{appimage}.new");
    std::fs::write(&tmp, &bytes).map_err(|e| e.to_string())?;
    std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))
        .map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, appimage).map_err(|e| e.to_string())?;
    Ok(release.version)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("zapive_test_update_{}_{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(STAGE_DIR)).expect("scratch dir");
        dir
    }

    #[test]
    fn swap_puts_the_new_binary_in_place() {
        let dir = scratch("swap");
        let exe = dir.join(BIN);
        let fresh = dir.join(STAGE_DIR).join(BIN);
        std::fs::write(&exe, b"old").unwrap();
        std::fs::write(&fresh, b"new").unwrap();
        swap_in(&exe, &fresh).expect("swap");
        assert_eq!(std::fs::read(&exe).unwrap(), b"new");
        assert!(!fresh.exists());
        assert!(!old_path(&exe).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    // The failure this rewrite is about: whatever goes wrong with the new
    // binary, the install must still have one that starts.
    #[test]
    fn a_failed_swap_leaves_the_old_binary_where_it_was() {
        let dir = scratch("rollback");
        let exe = dir.join(BIN);
        std::fs::write(&exe, b"old").unwrap();
        let missing = dir.join(STAGE_DIR).join(BIN);
        assert!(swap_in(&exe, &missing).is_err());
        assert_eq!(std::fs::read(&exe).unwrap(), b"old");
        assert!(!old_path(&exe).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
