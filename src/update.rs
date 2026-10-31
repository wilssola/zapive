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

pub fn is_newer_release(tag: &str) -> bool {
    release_update(current_version(), tag).ok().flatten().is_some()
}

fn release_update(current: &str, tag: &str) -> Result<Option<String>, String> {
    let version = tag.trim_start_matches('v');
    let current_order = current.split('+').next().unwrap_or(current);
    let version_order = version.split('+').next().unwrap_or(version);
    let newer = self_update::version::bump_is_greater(current_order, version_order)
        .map_err(|_| "invalid release version".to_string())?;
    Ok(if newer { Some(version.to_string()) } else { None })
}

// While the repository is private, the API needs a token; once public
// this just works unauthenticated.
fn token() -> Option<String> {
    std::env::var("ZAPIVE_GH_TOKEN").ok().filter(|t| !t.is_empty())
}

// Set by the Restart button; main() relaunches once the instance lock is
// released. Spawning from the button itself raced that lock: the new
// process found the old one still holding it, raised it and exited, so
// the app just closed.
static RESTART: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn request_restart() {
    RESTART.store(true, std::sync::atomic::Ordering::SeqCst);
}

// Starts the updated app. Inside an AppImage current_exe() is the old
// binary on the squashfs mount that goes away with this process; the
// .AppImage file at $APPIMAGE is what apply() replaced.
pub fn relaunch_if_requested() {
    if !RESTART.load(std::sync::atomic::Ordering::SeqCst) {
        return;
    }
    let exe = match std::env::var_os("APPIMAGE") {
        Some(path) => std::path::PathBuf::from(path),
        None => match std::env::current_exe() {
            Ok(exe) => exe,
            Err(e) => {
                log::error!("[update] cannot find the executable to relaunch: {e}");
                return;
            }
        },
    };
    if let Err(e) = std::process::Command::new(&exe).spawn() {
        log::error!("[update] relaunch of {} failed: {e}", exe.display());
    }
}

// Returns the newer version tag, if one is published.
pub fn check() -> Result<Option<String>, String> {
    if std::env::var_os("FLATPAK_ID").is_some() {
        return Ok(None);
    }
    let mut list = self_update::backends::github::ReleaseList::configure();
    list.repo_owner(OWNER).repo_name(REPO);
    if let Some(t) = token() {
        list.auth_token(&t);
    }
    let releases = match list.build().and_then(|l| l.fetch()) {
        Ok(r) => r,
        Err(e) => {
            // To the log file: a release build has no console.
            log::warn!("[update] check failed: {e}");
            return Err("could not fetch releases".into());
        }
    };
    let releases = releases.into_vec();
    let Some(latest) = releases.first() else {
        return Ok(None);
    };
    release_update(current_version(), latest.version())
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

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct ReleaseNote {
    pub tag: String,
    pub date: String,
    pub notes: String,
}

pub struct ReleasePage {
    pub releases: Vec<ReleaseNote>,
    pub has_more: bool,
}

pub fn release_notes(page: u32) -> Result<ReleasePage, String> {
    let url = format!(
        "https://api.github.com/repos/{OWNER}/{REPO}/releases?per_page=20&page={}",
        page.max(1)
    );
    let text = get(&url, "application/vnd.github+json")
        .map_err(|_| "cannot load release notes")?
        .body_mut()
        .with_config()
        .limit(2 * 1024 * 1024)
        .read_to_string()
        .map_err(|_| "cannot read release notes")?;
    parse_release_notes(&text)
}

fn parse_release_notes(text: &str) -> Result<ReleasePage, String> {
    let json: serde_json::Value =
        serde_json::from_str(text).map_err(|_| "invalid release notes")?;
    let entries = json.as_array().ok_or("invalid release notes")?;
    let releases = entries.iter().filter_map(|entry| {
        if entry.get("draft").and_then(|v| v.as_bool()).unwrap_or(false)
            || entry.get("prerelease").and_then(|v| v.as_bool()).unwrap_or(false)
        {
            return None;
        }
        let tag = entry.get("tag_name")?.as_str()?.trim();
        if tag.is_empty() {
            return None;
        }
        let date = entry.get("published_at")
            .and_then(|v| v.as_str())
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
            .map(|date| date.format("%Y-%m-%d").to_string())
            .unwrap_or_default();
        let body = entry.get("body").and_then(|v| v.as_str()).unwrap_or_default();
        Some(ReleaseNote { tag: tag.to_string(), date, notes: plain_release_notes(body) })
    }).collect();
    Ok(ReleasePage { releases, has_more: entries.len() >= 20 })
}

fn plain_release_notes(body: &str) -> String {
    let mut lines = Vec::new();
    let mut fence: Option<(char, usize)> = None;
    let mut checksum_level = None;
    for line in body.lines() {
        let line = line.trim();
        let marker = line.chars().next().unwrap_or(' ');
        let count = line.chars().take_while(|&c| c == marker).count();
        if let Some((kind, length)) = fence {
            if marker == kind && count >= length && line[count..].trim().is_empty() {
                fence = None;
            }
            continue;
        }
        if matches!(marker, '`' | '~') && count >= 3 {
            fence = Some((marker, count));
            continue;
        }
        let level = line.chars().take_while(|&c| c == '#').count();
        let heading = (1..=6).contains(&level)
            && line[level..].starts_with(char::is_whitespace);
        if let Some(skipped_level) = checksum_level {
            if !heading || level > skipped_level {
                continue;
            }
            checksum_level = None;
        }
        let content = if heading { line[level..].trim().trim_end_matches('#').trim() } else { line };
        let title = content.trim_matches('*').trim().to_ascii_lowercase();
        if (heading || bulletless_checksum_title(&title))
            && (title.contains("sha-256") || title.contains("sha256") || title.contains("checksum"))
        {
            checksum_level = Some(if heading { level } else { 6 });
            continue;
        }
        let bullet = ["- ", "* ", "+ ", "• "].iter().find_map(|prefix| content.strip_prefix(prefix));
        let content = bullet.unwrap_or(content);
        let text = plain_release_inline(content);
        let text = text.trim();
        if !text.is_empty() {
            lines.push(if bullet.is_some() { format!("• {text}") } else { text.to_string() });
        } else if lines.last().is_some_and(|line| !line.is_empty()) {
            lines.push(String::new());
        }
    }
    lines.join("\n").trim().to_string()
}

fn bulletless_checksum_title(title: &str) -> bool {
    ["sha-256", "sha256", "checksum"].iter().any(|prefix| title.starts_with(prefix))
}

fn plain_release_inline(text: &str) -> String {
    static RULES: std::sync::OnceLock<Vec<(regex::Regex, &'static str)>> = std::sync::OnceLock::new();
    let rules = RULES.get_or_init(|| [
        (r"(?is)<script\b[^>]*>.*?</script\s*>", ""),
        (r"(?is)<style\b[^>]*>.*?</style\s*>", ""),
        (r"!?\[([^\]]*)\]\([^)]*\)", "$1"),
        (r"!?\[([^\]]*)\]\[[^\]]*\]", "$1"),
        (r"<[^>]*>", ""),
        (r"\*\*|__|~~|`", ""),
    ].into_iter().map(|(pattern, replacement)| (regex::Regex::new(pattern).unwrap(), replacement)).collect());
    let mut text = text.to_string();
    for (pattern, replacement) in rules {
        text = pattern.replace_all(&text, *replacement).into_owned();
    }
    text.chars().filter(|c| !c.is_control()).collect()
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
    req.config()
        .timeout_global((accept == "application/vnd.github+json").then(|| std::time::Duration::from_secs(20)))
        .build()
        .call()
        .map_err(|e| e.to_string())
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

    #[test]
    fn release_versions_use_semver_ordering() {
        for tag in ["1.2.4", "v1.3.0", "v1.10.0", "2.0.0"] {
            assert_eq!(release_update("1.2.3", tag).unwrap(), Some(tag.trim_start_matches('v').to_string()));
        }
        for tag in ["1.2.3", "v1.2.3", "1.2.2", "v1.1.10", "1.2.3-beta.1"] {
            assert_eq!(release_update("1.2.3", tag).unwrap(), None);
        }
        assert_eq!(release_update("1.2.3-beta.1", "v1.2.3").unwrap(), Some("1.2.3".into()));
        assert_eq!(release_update("1.2.3", "1.2.3+build.1").unwrap(), None);
    }

    #[test]
    fn invalid_release_versions_are_errors() {
        for tag in ["", "v", "not-a-version", "1.2", "v1.2.3.4"] {
            assert!(release_update("1.2.3", tag).is_err());
            assert!(!is_newer_release(tag));
        }
        assert!(!is_newer_release(current_version()));
        assert!(!is_newer_release(&format!("v{}", current_version())));
    }

    #[test]
    fn release_categories_and_legacy_markdown_are_plain_text() {
        let notes = plain_release_notes("## Features\n- **New** [settings](https://example.com)\n\n### Fixes\n* fix(ui): `Crash`\n+ Legacy item\n• Existing item\n<script>alert(1)</script><b>Text</b>");
        assert_eq!(notes, "Features\n• New settings\n\nFixes\n• fix(ui): Crash\n• Legacy item\n• Existing item\nText");
        assert_eq!(plain_release_notes("# Title #\n![image](javascript:evil)\n[x][ref]"), "Title\nimage\nx");
    }

    #[test]
    fn checksum_and_fences_do_not_hide_later_sections() {
        let body = "## Changes\n- Keep\n```rust\n## Hidden\n```\n## SHA-256\nHashes\n### Assets\n~~~text\nabc file\n~~~\n## Fixes\n- Restored\n```\nignored\n```\n- Also kept";
        assert_eq!(plain_release_notes(body), "Changes\n• Keep\nFixes\n• Restored\n• Also kept");
        assert_eq!(plain_release_notes("Checksums\nabc\n# News\n- Visible"), "News\n• Visible");
        assert_eq!(plain_release_notes("## Checksums\n### Nested\nhash\n# News\n~~~\nhidden\n~~~~\n- Visible"), "News\n• Visible");
        assert_eq!(plain_release_notes("- Fix checksum verification"), "• Fix checksum verification");
    }

    #[test]
    fn releases_filter_drafts_prereleases_and_invalid_tags() {
        let page = parse_release_notes(r###"[
            {"tag_name":"v1", "published_at":"2026-10-30T12:34:56Z", "body":"## Fixes\n- Fixed"},
            {"tag_name":"draft", "draft":true},
            {"tag_name":"preview", "prerelease":true},
            {}, {"tag_name":" "}, {"tag_name":123},
            {"tag_name":"v0", "published_at":"invalid", "body":null}
        ]"###).unwrap();
        assert!(!page.has_more);
        assert_eq!(page.releases.len(), 2);
        assert_eq!(page.releases[0].tag, "v1");
        assert_eq!(page.releases[0].date, "2026-10-30");
        assert_eq!(page.releases[0].notes, "Fixes\n• Fixed");
        assert_eq!(page.releases[1].date, "");
        assert_eq!(page.releases[1].notes, "");
        for invalid in ["{}", "null", "not json"] {
            assert!(parse_release_notes(invalid).is_err());
        }
    }

    #[test]
    fn release_pagination_uses_raw_count() {
        for count in [0, 19, 20, 21] {
            let entries = vec![serde_json::json!({"tag_name":"hidden", "draft":true}); count];
            let page = parse_release_notes(&serde_json::to_string(&entries).unwrap()).unwrap();
            assert!(page.releases.is_empty());
            assert_eq!(page.has_more, count >= 20);
        }
    }

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
