// Self-update against the GitHub releases the CI publishes on every
// master build. Assets are zips named zapive-<target>.zip holding just
// the binary; self_update swaps the running executable in place and the
// new version starts on the next launch.
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
