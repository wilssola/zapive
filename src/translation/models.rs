// Argos model store: listing installed packages, planning routes between
// language pairs and installing a downloaded `.argosmodel` archive.
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{BufReader, Read, Write};
use std::path::{Path, PathBuf};
use zip::ZipArchive;

pub(super) type Pair = (String, String);
pub(super) type Catalog = BTreeMap<Pair, Vec<String>>;
pub(super) type Check<'a> = &'a dyn Fn() -> Result<(), String>;

const MAX_ENTRIES: usize = 10_000;
const MAX_UNPACKED: u64 = 2 * 1024 * 1024 * 1024;
const MAX_METADATA: u64 = 1024 * 1024;

pub(super) fn code(s: &str) -> bool {
    let mut parts = s.split('-');
    let first = parts.next().unwrap_or_default();
    (2..=3).contains(&first.len())
        && first.bytes().all(|b| b.is_ascii_lowercase())
        && parts
            .next()
            .is_none_or(|p| (2..=8).contains(&p.len()) && p.bytes().all(|b| b.is_ascii_lowercase()))
        && parts.next().is_none()
}

pub(super) fn random_hex() -> String {
    format!("{:032x}", rand::random::<u128>())
}

pub(super) fn plan_route(
    source: &str,
    target: &str,
    catalog: &Catalog,
    installed: &BTreeSet<Pair>,
) -> Option<Vec<Pair>> {
    let edges: BTreeSet<_> = catalog.keys().chain(installed.iter()).cloned().collect();
    let direct = (source.into(), target.into());
    if edges.contains(&direct) {
        return Some(vec![direct]);
    }
    edges
        .iter()
        .filter(|(a, b)| a == source && b != source && b != target)
        .filter_map(|first| {
            let second = (first.1.clone(), target.into());
            edges.contains(&second).then(|| vec![first.clone(), second])
        })
        .min_by_key(|r| {
            (
                r.iter().filter(|p| !installed.contains(*p)).count(),
                r[0].1 != "en",
                r[0].1.clone(),
            )
        })
}

pub(super) struct Meta {
    pub from: String,
    pub to: String,
    pub target_prefix: String,
}

pub(super) fn read_meta(dir: &Path) -> Result<Meta, String> {
    let file = fs::File::open(dir.join("metadata.json")).map_err(|_| "invalid_model_archive")?;
    let mut bytes = Vec::new();
    file.take(MAX_METADATA + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "invalid_model_archive")?;
    if bytes.len() as u64 > MAX_METADATA {
        return Err("invalid_model_archive".into());
    }
    parse_meta(&bytes)
}

fn parse_meta(bytes: &[u8]) -> Result<Meta, String> {
    let value: Value = serde_json::from_slice(bytes).map_err(|_| "invalid_model_archive")?;
    if value
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("translate")
        != "translate"
    {
        return Err("invalid_model_archive".into());
    }
    let from = value["from_code"].as_str().filter(|s| code(s));
    let to = value["to_code"].as_str().filter(|s| code(s));
    let (Some(from), Some(to)) = (from, to) else {
        return Err("invalid_model_archive".into());
    };
    Ok(Meta {
        from: from.into(),
        to: to.into(),
        target_prefix: value["target_prefix"].as_str().unwrap_or_default().into(),
    })
}

fn usable(dir: &Path) -> bool {
    dir.join("model").join("model.bin").is_file() && dir.join("sentencepiece.model").is_file()
}

/// Installed translation packages that the in-process engine can run, keyed by
/// language pair. When a pair was installed more than once the newest wins.
pub(super) fn installed(models: &Path) -> BTreeMap<Pair, PathBuf> {
    let mut found: BTreeMap<Pair, (std::time::SystemTime, PathBuf)> = BTreeMap::new();
    let Ok(entries) = fs::read_dir(models) else {
        return BTreeMap::new();
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(meta) = fs::symlink_metadata(&path) else {
            continue;
        };
        if !meta.is_dir() || meta.file_type().is_symlink() || !usable(&path) {
            continue;
        }
        let Ok(info) = read_meta(&path) else {
            continue;
        };
        let modified = meta.modified().unwrap_or(std::time::UNIX_EPOCH);
        let key = (info.from, info.to);
        if found.get(&key).is_none_or(|(old, _)| modified >= *old) {
            found.insert(key, (modified, path));
        }
    }
    found.into_iter().map(|(k, (_, p))| (k, p)).collect()
}

pub(super) fn pairs_label<'a>(pairs: impl IntoIterator<Item = &'a Pair>) -> String {
    pairs
        .into_iter()
        .map(|(a, b)| format!("{a}:{b}"))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>()
        .join(",")
}

/// Bytes used by a directory tree; symbolic links are not followed.
pub(super) fn disk_size(dir: &Path) -> u64 {
    let mut total = 0;
    let mut pending = vec![dir.to_path_buf()];
    while let Some(dir) = pending.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(meta) = fs::symlink_metadata(entry.path()) else {
                continue;
            };
            if meta.file_type().is_symlink() {
                continue;
            } else if meta.is_dir() {
                pending.push(entry.path());
            } else {
                total += meta.len();
            }
        }
    }
    total
}

pub(super) fn status(models: &Path, catalog: &Catalog) -> Value {
    let installed = installed(models);
    let list: Vec<Value> = installed
        .iter()
        .map(|((source, target), dir)| {
            json!({ "source": source, "target": target, "size": disk_size(dir) })
        })
        .collect();
    json!({
        "models": pairs_label(installed.keys()),
        "pairs": pairs_label(catalog.keys()),
        "installed": list,
    })
}

/// True when `path` resolves to a direct child of the canonical `root`.
fn inside(root: &Path, path: &Path) -> bool {
    fs::canonicalize(path).is_ok_and(|path| path.parent() == Some(root))
}

/// Deletes every installed copy of `source -> target`. Only real directories
/// directly inside the models root are touched; links are never followed.
pub(super) fn remove(models: &Path, source: &str, target: &str) -> Result<(), String> {
    if !code(source) || !code(target) {
        return Err("invalid_language".into());
    }
    let root = fs::canonicalize(models).map_err(|_| "model_not_installed")?;
    let entries = fs::read_dir(&root).map_err(|_| "model_remove_failed")?;
    let mut removed = 0;
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(meta) = fs::symlink_metadata(&path) else {
            continue;
        };
        if !meta.is_dir() || meta.file_type().is_symlink() {
            continue;
        }
        let Ok(info) = read_meta(&path) else {
            continue;
        };
        if info.from != source || info.to != target {
            continue;
        }
        if !inside(&root, &path) {
            return Err("model_remove_failed".into());
        }
        fs::remove_dir_all(&path).map_err(|_| "model_remove_failed")?;
        removed += 1;
    }
    if removed == 0 {
        return Err("model_not_installed".into());
    }
    Ok(())
}

fn archive_error<E>(_: E) -> String {
    "invalid_model_archive".into()
}

fn install_error<E>(_: E) -> String {
    "model_install_failed".into()
}

/// Normalised relative path components of a zip entry name, or an error when
/// the name could escape the staging directory.
pub(super) fn safe_parts(name: &str) -> Result<Vec<String>, String> {
    if name.is_empty() || name.contains(['\0', '\\', ':']) || name.starts_with('/') {
        return Err("invalid_model_archive".into());
    }
    let mut parts = Vec::new();
    for part in name.split('/') {
        match part {
            "" | "." => {}
            ".." => return Err("invalid_model_archive".into()),
            p if p.ends_with(['.', ' ']) => return Err("invalid_model_archive".into()),
            p => parts.push(p.to_string()),
        }
    }
    if parts.is_empty() {
        return Err("invalid_model_archive".into());
    }
    Ok(parts)
}

struct Staging(PathBuf);
impl Drop for Staging {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn extract(archive: &Path, staging: &Path, check: Check<'_>) -> Result<PathBuf, String> {
    let file = fs::File::open(archive).map_err(archive_error)?;
    let mut zip = ZipArchive::new(BufReader::new(file)).map_err(archive_error)?;
    if zip.len() > MAX_ENTRIES {
        return Err("invalid_model_archive".into());
    }
    let mut entries = Vec::with_capacity(zip.len());
    let mut roots = BTreeSet::new();
    let mut declared = 0u64;
    for index in 0..zip.len() {
        let entry = zip.by_index_raw(index).map_err(archive_error)?;
        let name = entry.name().map_err(archive_error)?.into_owned();
        let parts = safe_parts(&name)?;
        if entry
            .unix_mode()
            .is_some_and(|mode| mode & 0o170000 == 0o120000)
        {
            return Err("invalid_model_archive".into());
        }
        declared = declared.saturating_add(entry.size());
        if declared > MAX_UNPACKED {
            return Err("invalid_model_archive".into());
        }
        roots.insert(parts[0].clone());
        entries.push((index, parts, entry.is_dir()));
    }
    if roots.len() != 1 {
        return Err("invalid_model_archive".into());
    }
    let mut total = 0u64;
    let mut buffer = vec![0u8; 64 * 1024];
    for (index, parts, directory) in entries {
        check()?;
        let destination = parts.iter().fold(staging.to_path_buf(), |p, c| p.join(c));
        if directory {
            fs::create_dir_all(&destination).map_err(install_error)?;
            continue;
        }
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent).map_err(install_error)?;
        }
        let mut entry = zip.by_index(index).map_err(archive_error)?;
        let mut out = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&destination)
            .map_err(archive_error)?;
        loop {
            check()?;
            let n = entry.read(&mut buffer).map_err(archive_error)?;
            if n == 0 {
                break;
            }
            total += n as u64;
            if total > MAX_UNPACKED {
                return Err("invalid_model_archive".into());
            }
            out.write_all(&buffer[..n]).map_err(install_error)?;
        }
        out.flush().map_err(install_error)?;
    }
    let root = staging.join(roots.into_iter().next().unwrap_or_default());
    let meta = fs::symlink_metadata(&root).map_err(archive_error)?;
    if !meta.is_dir() || meta.file_type().is_symlink() {
        return Err("invalid_model_archive".into());
    }
    Ok(root)
}

/// Validates and installs a `.argosmodel` archive. The package is unpacked in a
/// staging directory next to `models` and renamed in place only once complete.
pub(super) fn install_archive(
    models: &Path,
    archive: &Path,
    source: &str,
    target: &str,
    check: Check<'_>,
) -> Result<(), String> {
    let base = models.parent().ok_or("models_directory_required")?;
    fs::create_dir_all(models).map_err(install_error)?;
    let staging = base.join(format!(".translation-{}", random_hex()));
    fs::create_dir(&staging).map_err(install_error)?;
    let staging = Staging(staging);
    let root = extract(archive, &staging.0, check)?;
    let meta = read_meta(&root)?;
    if meta.from != source || meta.to != target {
        return Err("invalid_model_archive".into());
    }
    if !root.join("model").join("model.bin").is_file() {
        return Err("invalid_model_archive".into());
    }
    if !root.join("sentencepiece.model").is_file() {
        return Err("model_unsupported".into());
    }
    check()?;
    let prefix = format!("{source}_{target}_");
    let destination = models.join(format!("{prefix}{}", random_hex()));
    fs::rename(&root, &destination).map_err(install_error)?;
    remove_replaced(models, &prefix, &destination);
    Ok(())
}

fn remove_replaced(models: &Path, prefix: &str, keep: &Path) {
    let Ok(entries) = fs::read_dir(models) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let replaced = name.to_str().is_some_and(|n| {
            n.strip_prefix(prefix)
                .is_some_and(|rest| rest.len() == 32 && rest.bytes().all(|b| b.is_ascii_hexdigit()))
        });
        if replaced
            && path != keep
            && fs::symlink_metadata(&path).is_ok_and(|m| m.is_dir() && !m.file_type().is_symlink())
        {
            let _ = fs::remove_dir_all(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn edges(items: &[(&str, &str)]) -> Catalog {
        items
            .iter()
            .map(|(a, b)| ((a.to_string(), b.to_string()), vec![]))
            .collect()
    }
    fn have(items: &[(&str, &str)]) -> BTreeSet<Pair> {
        edges(items).into_keys().collect()
    }

    #[test]
    fn plan_route_prefers_direct_then_installed_then_english() {
        let mut catalog = edges(&[
            ("pt", "fr"),
            ("pt", "en"),
            ("en", "fr"),
            ("pt", "es"),
            ("es", "fr"),
        ]);
        let installed = have(&[("pt", "es"), ("es", "fr")]);
        assert_eq!(
            plan_route("pt", "fr", &catalog, &installed).unwrap(),
            vec![("pt".into(), "fr".into())]
        );
        catalog.remove(&("pt".into(), "fr".into()));
        assert_eq!(
            plan_route("pt", "fr", &catalog, &installed).unwrap()[0].1,
            "es"
        );
        assert_eq!(
            plan_route("pt", "fr", &catalog, &BTreeSet::new()).unwrap()[0].1,
            "en"
        );
        assert!(plan_route("zz", "fr", &catalog, &installed).is_none());
        assert_eq!(
            plan_route("pt", "fr", &Catalog::new(), &installed)
                .unwrap()
                .len(),
            2
        );
        assert!(plan_route("pt", "fr", &Catalog::new(), &have(&[("pt", "en")])).is_none());
    }

    #[test]
    fn language_codes_follow_the_catalog_pattern() {
        for ok in ["en", "pt", "zh", "fil", "zh-hans", "pt-brazil"] {
            assert!(code(ok), "{ok}");
        }
        for bad in [
            "", "e", "EN", "en_US", "en-", "a1", "en-x", "abcd", "en-us-x", "../x",
        ] {
            assert!(!code(bad), "{bad}");
        }
    }

    #[test]
    fn zip_entry_names_cannot_escape_the_staging_directory() {
        assert_eq!(
            safe_parts("translate-en_pt/model/model.bin").unwrap(),
            ["translate-en_pt", "model", "model.bin"]
        );
        assert_eq!(safe_parts("./a//b/").unwrap(), ["a", "b"]);
        for bad in [
            "",
            "/etc/passwd",
            "../x",
            "a/../../x",
            "a/..",
            "a\\b",
            "C:/x",
            "a:b",
            "a\0b",
            "a./b",
            "a /b",
            "./",
        ] {
            assert!(safe_parts(bad).is_err(), "{bad:?}");
        }
    }

    fn scratch(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "zapive-models-{name}-{}-{}",
            std::process::id(),
            random_hex()
        ));
        fs::create_dir_all(root.join("models")).unwrap();
        root
    }

    fn archive(path: &Path, files: &[(&str, &[u8])], symlink: Option<&str>) {
        use zip::write::SimpleFileOptions;
        let mut writer = zip::ZipWriter::new(fs::File::create(path).unwrap());
        let options = SimpleFileOptions::default();
        for (name, data) in files {
            writer.start_file(*name, options).unwrap();
            writer.write_all(data).unwrap();
        }
        if let Some(name) = symlink {
            writer.add_symlink(name, "/etc/passwd", options).unwrap();
        }
        writer.finish().unwrap();
    }

    const META: &[u8] = br#"{"from_code":"en","to_code":"pt","package_version":"1"}"#;

    fn good() -> Vec<(&'static str, &'static [u8])> {
        vec![
            ("translate-en_pt/metadata.json", META),
            ("translate-en_pt/model/model.bin", b"bin"),
            ("translate-en_pt/sentencepiece.model", b"spm"),
        ]
    }

    #[test]
    fn installs_a_valid_archive_atomically_and_lists_it() {
        let root = scratch("ok");
        let models = root.join("models");
        let file = root.join("a.argosmodel");
        archive(&file, &good(), None);
        install_archive(&models, &file, "en", "pt", &|| Ok(())).unwrap();
        let listed = installed(&models);
        assert_eq!(listed.len(), 1);
        let path = &listed[&("en".to_string(), "pt".to_string())];
        assert!(path.starts_with(&models));
        assert!(
            path.file_name()
                .unwrap()
                .to_str()
                .unwrap()
                .starts_with("en_pt_")
        );
        assert_eq!(status(&models, &Catalog::new())["models"], "en:pt");
        assert_eq!(
            fs::read_dir(&root).unwrap().count(),
            2,
            "staging left behind"
        );
        install_archive(&models, &file, "en", "pt", &|| Ok(())).unwrap();
        assert_eq!(fs::read_dir(&models).unwrap().count(), 1, "old copy kept");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn rejects_archives_that_do_not_match_or_are_unsafe() {
        let root = scratch("bad");
        let models = root.join("models");
        let file = root.join("a.argosmodel");
        let fail = |files: &[(&str, &[u8])], symlink: Option<&str>, s: &str, t: &str| {
            archive(&file, files, symlink);
            install_archive(&models, &file, s, t, &|| Ok(())).unwrap_err()
        };
        assert_eq!(fail(&good(), None, "en", "es"), "invalid_model_archive");
        assert_eq!(
            fail(&[("../evil", b"x")], None, "en", "pt"),
            "invalid_model_archive"
        );
        assert_eq!(
            fail(&[("/abs", b"x")], None, "en", "pt"),
            "invalid_model_archive"
        );
        let mut two_roots = good();
        two_roots.push(("other/file", b"x"));
        assert_eq!(fail(&two_roots, None, "en", "pt"), "invalid_model_archive");
        let mut linked = good();
        linked.truncate(3);
        assert_eq!(
            fail(&linked, Some("translate-en_pt/link"), "en", "pt"),
            "invalid_model_archive"
        );
        let no_bin = vec![
            ("translate-en_pt/metadata.json", META),
            ("translate-en_pt/sentencepiece.model", b"spm"),
        ];
        assert_eq!(fail(&no_bin, None, "en", "pt"), "invalid_model_archive");
        let no_spm = vec![
            ("translate-en_pt/metadata.json", META),
            ("translate-en_pt/model/model.bin", b"bin"),
        ];
        assert_eq!(fail(&no_spm, None, "en", "pt"), "model_unsupported");
        let bad_meta = vec![
            ("translate-en_pt/metadata.json", &b"{}"[..]),
            ("translate-en_pt/model/model.bin", b"bin"),
            ("translate-en_pt/sentencepiece.model", b"spm"),
        ];
        assert_eq!(fail(&bad_meta, None, "en", "pt"), "invalid_model_archive");
        fs::write(&file, b"not a zip").unwrap();
        assert_eq!(
            install_archive(&models, &file, "en", "pt", &|| Ok(())).unwrap_err(),
            "invalid_model_archive"
        );
        assert!(installed(&models).is_empty());
        assert_eq!(fs::read_dir(&models).unwrap().count(), 0);
        assert_eq!(
            fs::read_dir(&root)
                .unwrap()
                .filter(|e| e
                    .as_ref()
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .starts_with('.'))
                .count(),
            0,
            "staging left behind"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn archive_with_too_many_entries_is_rejected() {
        let root = scratch("many");
        let file = root.join("a.argosmodel");
        let names: Vec<String> = (0..=MAX_ENTRIES).map(|i| format!("r/{i}")).collect();
        let files: Vec<(&str, &[u8])> = names.iter().map(|n| (n.as_str(), &b""[..])).collect();
        archive(&file, &files, None);
        assert_eq!(
            install_archive(&root.join("models"), &file, "en", "pt", &|| Ok(())).unwrap_err(),
            "invalid_model_archive"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn cancellation_stops_installation_without_leftovers() {
        let root = scratch("cancel");
        let models = root.join("models");
        let file = root.join("a.argosmodel");
        archive(&file, &good(), None);
        assert_eq!(
            install_archive(&models, &file, "en", "pt", &|| Err(
                "request_cancelled".into()
            ))
            .unwrap_err(),
            "request_cancelled"
        );
        assert!(installed(&models).is_empty());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn packages_without_a_tokenizer_are_not_listed() {
        let root = scratch("spm");
        let models = root.join("models");
        let dir = models.join("en_pt_x");
        fs::create_dir_all(dir.join("model")).unwrap();
        fs::write(dir.join("metadata.json"), META).unwrap();
        fs::write(dir.join("model").join("model.bin"), b"bin").unwrap();
        assert!(installed(&models).is_empty());
        fs::write(dir.join("sentencepiece.model"), b"spm").unwrap();
        assert_eq!(installed(&models).len(), 1);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn metadata_is_validated() {
        let meta =
            parse_meta(br#"{"from_code":"en","to_code":"pt","target_prefix":"__pt__"}"#).unwrap();
        assert_eq!((meta.from.as_str(), meta.to.as_str()), ("en", "pt"));
        assert_eq!(meta.target_prefix, "__pt__");
        assert_eq!(parse_meta(META).unwrap().target_prefix, "");
        for bad in [
            &b"[]"[..],
            br#"{"from_code":"EN","to_code":"pt"}"#,
            br#"{"from_code":"en"}"#,
            br#"{"from_code":"en","to_code":"pt","type":"stanza"}"#,
            b"nope",
        ] {
            assert!(parse_meta(bad).is_err());
        }
    }

    #[test]
    fn remove_deletes_only_matching_models_inside_the_root() {
        let root = scratch("remove");
        let models = root.join("models");
        let file = root.join("a.argosmodel");
        archive(&file, &good(), None);
        install_archive(&models, &file, "en", "pt", &|| Ok(())).unwrap();
        let outside = root.join("outside");
        fs::create_dir_all(outside.join("model")).unwrap();
        fs::write(outside.join("metadata.json"), META).unwrap();
        let dir = installed(&models)[&("en".to_string(), "pt".to_string())].clone();
        assert!(disk_size(&dir) >= 9);
        assert_eq!(
            status(&models, &Catalog::new())["installed"][0]["source"],
            "en"
        );
        assert_eq!(
            remove(&models, "../x", "pt").unwrap_err(),
            "invalid_language"
        );
        assert_eq!(
            remove(&models, "pt", "en").unwrap_err(),
            "model_not_installed"
        );
        remove(&models, "en", "pt").unwrap();
        assert!(installed(&models).is_empty());
        assert!(models.is_dir() && outside.join("metadata.json").is_file());
        assert_eq!(
            remove(&models, "en", "pt").unwrap_err(),
            "model_not_installed"
        );
        assert_eq!(
            remove(&root.join("missing"), "en", "pt").unwrap_err(),
            "model_not_installed"
        );
        let _ = fs::remove_dir_all(root);
    }
}
