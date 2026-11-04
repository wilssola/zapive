use super::models::{
    Catalog, Check, Pair, code, install_archive, installed, plan_route, random_hex,
};
use serde_json::{Value, json};
use std::cell::Cell;
use std::collections::BTreeSet;
use std::io::{Read, Write};
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};
use ureq::unversioned::{
    resolver::{DefaultResolver, ResolvedSocketAddrs, Resolver},
    transport::{DefaultConnector, NextTimeout},
};
use url::Url;

pub(super) const INDEX: &str =
    "https://raw.githubusercontent.com/argosopentech/argospm-index/main/index.json";
const CATALOG_LIMIT: u64 = 8 * 1024 * 1024;
const MODEL_LIMIT: u64 = 512 * 1024 * 1024;
const PROGRESS_INTERVAL: Duration = Duration::from_millis(200);

/// Receives install progress as JSON (`phase`, `step`, `steps`, `downloaded`, `total`).
pub(super) type Progress<'a> = &'a dyn Fn(Value);
type Report<'a> = &'a dyn Fn(u64, Option<u64>);

/// Lets one progress update through per interval; the final one always passes.
struct Throttle {
    interval: Duration,
    last: Cell<Option<Instant>>,
}

impl Throttle {
    fn new(interval: Duration) -> Self {
        Self {
            interval,
            last: Cell::new(None),
        }
    }

    fn ready(&self, now: Instant, last: bool) -> bool {
        let due = last
            || self
                .last
                .get()
                .is_none_or(|at| now.saturating_duration_since(at) >= self.interval);
        if due {
            self.last.set(Some(now));
        }
        due
    }
}

struct Counting<'a, R> {
    inner: R,
    read: u64,
    total: Option<u64>,
    report: Report<'a>,
}

impl<R: Read> Read for Counting<'_, R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.read += n as u64;
        (self.report)(self.read, self.total);
        Ok(n)
    }
}

pub(super) fn catalog_file(models: &Path) -> Result<PathBuf, String> {
    Ok(models
        .parent()
        .ok_or("models_directory_required")?
        .join("translation-index.json"))
}

pub(super) fn parse_catalog(bytes: &[u8]) -> Result<Catalog, String> {
    if bytes.len() as u64 > CATALOG_LIMIT {
        return Err("download_too_large".into());
    }
    let entries: Value = serde_json::from_slice(bytes).map_err(|_| "invalid_catalog")?;
    let mut catalog = Catalog::new();
    for entry in entries.as_array().ok_or("invalid_catalog")? {
        let entry = entry.as_object().ok_or("invalid_catalog")?;
        match entry.get("type") {
            None => {}
            Some(Value::String(kind)) if kind == "translate" => {}
            Some(Value::String(_)) => continue,
            Some(_) => return Err("invalid_catalog".into()),
        }
        let pair = |key: &str| {
            entry
                .get(key)
                .and_then(Value::as_str)
                .filter(|s| code(s))
                .map(str::to_owned)
                .ok_or("invalid_catalog")
        };
        let key = (pair("from_code")?, pair("to_code")?);
        let links = entry
            .get("links")
            .and_then(Value::as_array)
            .map(|links| {
                links
                    .iter()
                    .filter_map(Value::as_str)
                    .filter(|s| allowed(s, false, false))
                    .take(8)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default();
        catalog.entry(key).or_insert(links);
    }
    Ok(catalog)
}

pub(super) fn read_catalog(models: &Path, check: Check<'_>) -> Result<Option<Catalog>, String> {
    let file = match std::fs::File::open(catalog_file(models)?) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err("invalid_catalog".into()),
    };
    let mut bytes = Vec::new();
    copy(
        &mut file.take(CATALOG_LIMIT + 1),
        &mut bytes,
        CATALOG_LIMIT,
        check,
    )?;
    parse_catalog(&bytes).map(Some)
}

/// Downloads the argospm index, validates it and replaces the cached copy.
pub(super) fn refresh(models: &Path, check: Check<'_>) -> Result<Catalog, String> {
    let file = download(INDEX, true, models, check, &|_, _| {})?;
    let mut bytes = Vec::new();
    copy(
        &mut std::fs::File::open(&file.0)
            .map_err(|_| "invalid_catalog")?
            .take(CATALOG_LIMIT + 1),
        &mut bytes,
        CATALOG_LIMIT,
        check,
    )?;
    let catalog = parse_catalog(&bytes)?;
    check()?;
    let target = catalog_file(models)?;
    let temporary = Temporary(target.with_file_name(format!(".catalog-{}", random_hex())));
    std::fs::write(&temporary.0, &bytes).map_err(|_| "invalid_catalog")?;
    std::fs::rename(&temporary.0, &target).map_err(|_| "invalid_catalog")?;
    Ok(catalog)
}

/// Downloads and installs every missing model on the route from `source` to
/// `target`, refreshing the catalog first when no route is known.
pub(super) fn install(
    models: &Path,
    source: &str,
    target: &str,
    check: Check<'_>,
    progress: Progress<'_>,
) -> Result<(), String> {
    if !code(source) || !code(target) || source == target {
        return Err("invalid_language".into());
    }
    let mut installed: BTreeSet<Pair> = installed(models).into_keys().collect();
    if installed.contains(&(source.into(), target.into())) {
        return Ok(());
    }
    let mut catalog = read_catalog(models, check)?.unwrap_or_default();
    if plan_route(source, target, &catalog, &installed).is_none() {
        catalog = refresh(models, check)?;
    }
    let plan = plan_route(source, target, &catalog, &installed).ok_or("pair_unavailable")?;
    let missing: Vec<Pair> = plan
        .into_iter()
        .filter(|p| !installed.contains(p))
        .collect();
    let steps = missing.len();
    for (index, pair) in missing.into_iter().enumerate() {
        check()?;
        let step = |phase: &str, downloaded: u64, total: Option<u64>| {
            progress(json!({
                "phase": phase,
                "step": index + 1,
                "steps": steps,
                "source": pair.0,
                "target": pair.1,
                "downloaded": downloaded,
                "total": total.unwrap_or(0),
            }))
        };
        step("download", 0, None);
        let throttle = Throttle::new(PROGRESS_INTERVAL);
        let report = |downloaded: u64, total: Option<u64>| {
            if throttle.ready(Instant::now(), Some(downloaded) == total) {
                step("download", downloaded, total);
            }
        };
        let links = catalog.get(&pair).ok_or("pair_unavailable")?;
        let mut failure = "model_download_failed".to_string();
        let mut downloaded = None;
        for link in links {
            match download(link, false, models, check, &report) {
                Ok(file) => {
                    downloaded = Some(file);
                    break;
                }
                Err(e) => {
                    check()?;
                    failure = e;
                }
            }
        }
        let file = downloaded.ok_or(failure)?;
        check()?;
        step("install", 0, None);
        log::info!("[translation] installing local model");
        install_archive(models, &file.0, &pair.0, &pair.1, check)?;
        installed.insert(pair);
    }
    Ok(())
}

fn allowed(input: &str, catalog: bool, redirected: bool) -> bool {
    let Ok(url) = Url::parse(input) else {
        return false;
    };
    if url.scheme() != "https"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port_or_known_default() != Some(443)
        || url.fragment().is_some()
    {
        return false;
    }
    if catalog {
        return input == INDEX;
    }
    match url.host_str().unwrap_or_default() {
        "argos-net.com" => true,
        "github.com" | "raw.githubusercontent.com" => url.path().starts_with("/argosopentech/"),
        "release-assets.githubusercontent.com" | "objects.githubusercontent.com" => redirected,
        _ => false,
    }
}

fn public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            let [a, b, c, _] = ip.octets();
            !(a == 0
                || a == 10
                || a == 127
                || a >= 224
                || (a == 100 && (64..=127).contains(&b))
                || (a == 169 && b == 254)
                || (a == 172 && (16..=31).contains(&b))
                || (a == 192 && (b == 168 || b == 0 || (b == 88 && c == 99)))
                || (a == 198 && (b == 18 || b == 19 || (b == 51 && c == 100)))
                || (a == 203 && b == 0 && c == 113))
        }
        IpAddr::V6(ip) => {
            let s = ip.segments();
            s[0] & 0xe000 == 0x2000
                && s[0] != 0x2001
                && s[0] != 0x2002
                && !(s[0] == 0x3fff && s[1] <= 0x0fff)
        }
    }
}
#[derive(Debug)]
struct PublicResolver;
impl Resolver for PublicResolver {
    fn resolve(
        &self,
        uri: &ureq::http::Uri,
        config: &ureq::config::Config,
        timeout: NextTimeout,
    ) -> Result<ResolvedSocketAddrs, ureq::Error> {
        let addresses = DefaultResolver::default().resolve(uri, config, timeout)?;
        if addresses.iter().any(|a| !public_ip(a.ip())) {
            return Err(ureq::Error::HostNotFound);
        }
        Ok(addresses)
    }
}

struct Temporary(PathBuf);
impl Drop for Temporary {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}
fn temporary(models: &Path) -> Result<(Temporary, std::fs::File), String> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let dir = models
        .parent()
        .ok_or("models_directory_required")?
        .join("downloads");
    std::fs::create_dir_all(&dir).map_err(|_| "models_directory_required")?;
    let parent =
        std::fs::canonicalize(models.parent().unwrap()).map_err(|_| "models_directory_required")?;
    let canonical = std::fs::canonicalize(&dir).map_err(|_| "models_directory_required")?;
    if canonical != parent.join("downloads") {
        return Err("models_directory_required".into());
    }
    for _ in 0..32 {
        let path = canonical.join(format!(
            "rust-{}-{}.tmp",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(file) => return Ok((Temporary(path), file)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(_) => return Err("model_download_failed".into()),
        }
    }
    Err("model_download_failed".into())
}

fn copy(
    reader: &mut impl Read,
    writer: &mut impl Write,
    limit: u64,
    check: Check<'_>,
) -> Result<u64, String> {
    let mut total = 0u64;
    let mut buffer = [0; 64 * 1024];
    loop {
        check()?;
        let n = reader.read(&mut buffer).map_err(|e| io_error(&e))?;
        check()?;
        if n == 0 {
            return Ok(total);
        }
        total += n as u64;
        if total > limit {
            return Err("download_too_large".into());
        }
        writer
            .write_all(&buffer[..n])
            .map_err(|_| "model_download_failed")?;
    }
}

fn http_error(status: u16) -> &'static str {
    match status {
        401 => "network_http_unauthorized",
        403 => "network_http_forbidden",
        404 => "network_not_found",
        407 => "network_proxy_auth",
        429 => "network_rate_limited",
        500..=599 => "network_server",
        _ => "network_http",
    }
}
fn io_error(error: &std::io::Error) -> &'static str {
    if let Some(error) = error
        .get_ref()
        .and_then(|e| e.downcast_ref::<ureq::Error>())
    {
        return network_error(error);
    }
    use std::io::ErrorKind::*;
    match error.kind() {
        TimedOut | WouldBlock => "network_timeout",
        PermissionDenied => "network_access_denied",
        ConnectionRefused => "network_refused",
        ConnectionReset | ConnectionAborted | Interrupted | UnexpectedEof => "network_interrupted",
        _ => "network_failed",
    }
}
fn network_error(error: &ureq::Error) -> &'static str {
    match error {
        ureq::Error::Timeout(_) => "network_timeout",
        ureq::Error::HostNotFound => "network_dns",
        ureq::Error::Io(e) => io_error(e),
        ureq::Error::Rustls(e) => {
            if e.to_string().to_ascii_lowercase().contains("certificate") {
                "network_tls_certificate"
            } else {
                "network_tls"
            }
        }
        ureq::Error::Tls(_) | ureq::Error::TlsRequired => "network_tls",
        ureq::Error::ConnectionFailed => "network_unreachable",
        ureq::Error::StatusCode(s) => http_error(*s),
        _ => "network_failed",
    }
}
fn retryable(code: &str) -> bool {
    matches!(
        code,
        "network_timeout"
            | "network_dns"
            | "network_refused"
            | "network_interrupted"
            | "network_unreachable"
            | "network_rate_limited"
            | "network_server"
    )
}

fn download(
    input: &str,
    catalog: bool,
    models: &Path,
    check: Check<'_>,
    report: Report<'_>,
) -> Result<Temporary, String> {
    if !allowed(input, catalog, false) {
        return Err("insecure_download".into());
    }
    let config = ureq::Agent::config_builder()
        .proxy(None)
        .https_only(true)
        .max_redirects(0)
        .http_status_as_error(false)
        .timeout_connect(Some(Duration::from_secs(10)))
        .timeout_recv_response(Some(Duration::from_secs(15)))
        .timeout_recv_body(None)
        .timeout_global(Some(Duration::from_secs(300)))
        .build();
    let agent = ureq::Agent::with_parts(config, DefaultConnector::default(), PublicResolver);
    let deadline = Instant::now() + Duration::from_secs(if catalog { 120 } else { 300 });
    let limit = if catalog { CATALOG_LIMIT } else { MODEL_LIMIT };
    log::info!(
        "[translation] downloading {}",
        if catalog { "catalog" } else { "model" }
    );
    let mut failure = "network_failed".to_string();
    for attempt in 0..3 {
        check()?;
        if attempt > 0 {
            for _ in 0..(10 * attempt) {
                check()?;
                thread::sleep(Duration::from_millis(25));
            }
        }
        let (temporary, mut file) = temporary(models)?;
        let result = (|| {
            let mut url = Url::parse(input).map_err(|_| "insecure_download")?;
            for redirect in 0..=4 {
                check()?;
                if !allowed(url.as_str(), catalog, redirect > 0) {
                    return Err("insecure_download".into());
                }
                let remaining = deadline
                    .checked_duration_since(Instant::now())
                    .ok_or("network_timeout")?;
                let mut response = agent
                    .get(url.as_str())
                    .config()
                    .timeout_global(Some(remaining))
                    .build()
                    .call()
                    .map_err(|e| network_error(&e).to_string())?;
                check()?;
                let status = response.status().as_u16();
                if matches!(status, 301 | 302 | 303 | 307 | 308) {
                    let location = response
                        .headers()
                        .get("location")
                        .and_then(|v| v.to_str().ok())
                        .ok_or("network_http")?;
                    url = url.join(location).map_err(|_| "insecure_download")?;
                    continue;
                }
                if status != 200 {
                    return Err(http_error(status).into());
                }
                let length = response
                    .headers()
                    .get("content-length")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|s| s.parse::<u64>().ok());
                if length.is_some_and(|n| n > limit) {
                    return Err("download_too_large".into());
                }
                report(0, length);
                let bytes = copy(
                    &mut Counting {
                        inner: response.body_mut().as_reader(),
                        read: 0,
                        total: length,
                        report,
                    },
                    &mut file,
                    limit,
                    &|| {
                        check()?;
                        if Instant::now() >= deadline {
                            Err("network_timeout".into())
                        } else {
                            Ok(())
                        }
                    },
                )?;
                file.flush().map_err(|_| "model_download_failed")?;
                log::info!("[translation] download complete bytes={bytes}");
                return Ok(());
            }
            Err("insecure_download".into())
        })();
        drop(file);
        match result {
            Ok(()) => return Ok(temporary),
            Err(e) => {
                failure = e;
                drop(temporary);
            }
        }
        check()?;
        if !retryable(&failure) || Instant::now() >= deadline {
            break;
        }
        log::info!("[translation] retrying download attempt={}", attempt + 2);
    }
    Err(failure)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn catalog_parsing_keeps_translate_entries_and_filters_links() {
        let data = br#"[
            {"from_code":"en","to_code":"pt","links":["https://argos-net.com/v1/a.argosmodel","http://argos-net.com/b","https://evil.example/c"]},
            {"type":"translate","from_code":"pt","to_code":"en"},
            {"type":"stanza","from_code":"xx-YY","to_code":"en"},
            {"from_code":"en","to_code":"pt","links":["https://argos-net.com/dup"]}
        ]"#;
        let catalog = parse_catalog(data).unwrap();
        assert_eq!(catalog.len(), 2);
        assert_eq!(
            catalog[&("en".to_string(), "pt".to_string())],
            ["https://argos-net.com/v1/a.argosmodel"]
        );
        assert!(catalog[&("pt".to_string(), "en".to_string())].is_empty());
    }

    #[test]
    fn catalog_parsing_rejects_invalid_documents() {
        for bad in [
            &b"{}"[..],
            b"not json",
            b"[1]",
            br#"[{"from_code":"en"}]"#,
            br#"[{"from_code":"EN","to_code":"pt"}]"#,
            br#"[{"from_code":"en","to_code":"pt","type":5}]"#,
        ] {
            assert_eq!(parse_catalog(bad).unwrap_err(), "invalid_catalog");
        }
        let big = vec![b' '; CATALOG_LIMIT as usize + 1];
        assert_eq!(parse_catalog(&big).unwrap_err(), "download_too_large");
        assert!(parse_catalog(b"[]").unwrap().is_empty());
    }

    #[test]
    fn strict_hosts_redirects_and_public_addresses() {
        assert!(allowed(INDEX, true, false));
        assert!(!allowed(&format!("{INDEX}?secret=x"), true, false));
        assert!(allowed(
            "https://argos-net.com/v1/model.argosmodel",
            false,
            false
        ));
        assert!(allowed(
            "https://github.com/argosopentech/models/releases/download/v1/a",
            false,
            false
        ));
        assert!(allowed(
            "https://release-assets.githubusercontent.com/a?signature=x",
            false,
            true
        ));
        assert!(!allowed(
            "https://release-assets.githubusercontent.com/a",
            false,
            false
        ));
        for url in [
            "http://argos-net.com/a",
            "https://argos-net.com.evil.test/a",
            "https://127.0.0.1/a",
            "https://localhost/a",
            "https://argos-net.com:444/a",
            "https://u:p@argos-net.com/a",
            "https://github.com/evil/a",
            "file:///tmp/a",
            "https://[::1]/a",
        ] {
            assert!(!allowed(url, false, true), "{url}");
        }
        for ip in [
            "127.0.0.1",
            "10.1.1.1",
            "169.254.169.254",
            "172.16.0.1",
            "192.168.1.1",
            "100.64.0.1",
            "::1",
            "fc00::1",
            "::ffff:127.0.0.1",
        ] {
            assert!(!public_ip(ip.parse().unwrap()));
        }
        assert!(public_ip("8.8.8.8".parse().unwrap()));
    }

    #[test]
    fn progress_is_throttled_but_reports_completion() {
        let throttle = Throttle::new(Duration::from_millis(200));
        let start = Instant::now();
        assert!(throttle.ready(start, false));
        assert!(!throttle.ready(start + Duration::from_millis(50), false));
        assert!(!throttle.ready(start + Duration::from_millis(199), false));
        assert!(throttle.ready(start + Duration::from_millis(200), false));
        assert!(throttle.ready(start + Duration::from_millis(210), true));
        assert!(!throttle.ready(start + Duration::from_millis(220), false));

        let seen = std::cell::RefCell::new(Vec::new());
        let report = |read: u64, total: Option<u64>| seen.borrow_mut().push((read, total));
        let mut reader = Counting {
            inner: Cursor::new(vec![0; 100 * 1024]),
            read: 0,
            total: Some(100 * 1024),
            report: &report,
        };
        let mut output = Vec::new();
        copy(&mut reader, &mut output, MODEL_LIMIT, &|| Ok(())).unwrap();
        let seen = seen.into_inner();
        assert_eq!(seen.first(), Some(&(64 * 1024, Some(100 * 1024))));
        assert_eq!(
            seen.iter().rev().nth(1),
            Some(&(100 * 1024, Some(100 * 1024)))
        );
    }

    #[test]
    fn streaming_limits_and_generation_cancellation() {
        let mut output = Vec::new();
        assert_eq!(
            copy(&mut Cursor::new(b"1234"), &mut output, 4, &|| Ok(())).unwrap(),
            4
        );
        assert_eq!(
            copy(&mut Cursor::new(b"12345"), &mut Vec::new(), 4, &|| Ok(())).unwrap_err(),
            "download_too_large"
        );
        let checks = std::cell::Cell::new(0);
        let mut output = Vec::new();
        let error = copy(
            &mut Cursor::new(vec![0; 128 * 1024]),
            &mut output,
            MODEL_LIMIT,
            &|| {
                checks.set(checks.get() + 1);
                if checks.get() >= 3 {
                    Err("request_cancelled".into())
                } else {
                    Ok(())
                }
            },
        )
        .unwrap_err();
        assert_eq!(error, "request_cancelled");
        assert_eq!(output.len(), 64 * 1024);
        assert_eq!(CATALOG_LIMIT, 8 * 1024 * 1024);
        assert_eq!(MODEL_LIMIT, 512 * 1024 * 1024);
    }

    #[test]
    fn classification_and_retry_policy_are_bounded_and_private() {
        assert_eq!(http_error(429), "network_rate_limited");
        assert_eq!(http_error(503), "network_server");
        assert_eq!(http_error(403), "network_http_forbidden");
        assert!(retryable(http_error(503)));
        assert!(!retryable(http_error(403)));
        assert!(!retryable("network_tls_certificate"));
        assert!(!retryable("download_too_large"));
        let error =
            std::io::Error::new(std::io::ErrorKind::ConnectionReset, "secret text and token");
        assert_eq!(io_error(&error), "network_interrupted");
        assert_eq!(network_error(&ureq::Error::HostNotFound), "network_dns");
        assert_eq!(network_error(&ureq::Error::Tls("secret")), "network_tls");
    }

    #[test]
    fn temporary_files_are_removed_on_error_and_success() {
        let root =
            std::env::temp_dir().join(format!("zapive-download-test-{}", std::process::id()));
        let models = root.join("models");
        std::fs::create_dir_all(&models).unwrap();
        let (temporary, file) = temporary(&models).unwrap();
        let path = temporary.0.clone();
        assert!(path.is_absolute());
        assert_eq!(
            path.parent().unwrap(),
            std::fs::canonicalize(root.join("downloads")).unwrap()
        );
        drop(file);
        drop(temporary);
        assert!(!path.exists());
        let _ = std::fs::remove_dir_all(root);
    }
}
