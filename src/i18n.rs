// Shared catalogs: i18n/{en_US,pt}/LC_MESSAGES/zapive.po. Slint bundles the same
// files (build.rs, DefaultTranslationContext::None), so an entry is keyed by
// `msgid` alone -- the semantic id -- with no `msgctxt`.
//
// Plurals: `msgid = "<base>.one"`, `msgid_plural = "<base>.other"`. Slint reads
// them with `@tr("<base>.one" | "<base>.other" % n)`; Rust asks for the base id
// with `tp("<base>", n, ..)`. `{n}` is the count.
use rspolib::pofile;
use std::collections::HashMap;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU8, Ordering};

static CURRENT_LOCALE: AtomicU8 = AtomicU8::new(0);
static CATALOGS: OnceLock<Catalogs> = OnceLock::new();
#[cfg(test)]
pub(crate) static LOCALE_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
struct Catalogs { en: HashMap<&'static str, Entry>, pt: HashMap<&'static str, Entry> }
struct Entry { forms: Vec<&'static str>, plural: bool, placeholders: Vec<&'static str> }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Locale { En, Pt }
pub fn set_locale(locale: Locale) { CURRENT_LOCALE.store((locale == Locale::Pt) as u8, Ordering::Relaxed); }
pub fn current_locale() -> Locale { if CURRENT_LOCALE.load(Ordering::Relaxed) == 1 { Locale::Pt } else { Locale::En } }

pub fn t(id: &str) -> String { ta(id, &[]) }

pub fn is_translation(id: &str, value: &str) -> bool {
    let catalogs = catalogs();
    [catalogs.en.get(id), catalogs.pt.get(id)]
        .into_iter()
        .flatten()
        .any(|entry| entry.forms.iter().any(|form| *form == value))
}

// Positional `{0}`, `{1}` arguments.
pub fn ta(id: &str, args: &[&str]) -> String {
    let Some(entry) = localized_entry(id) else { return missing(id) };
    validate_args(id, &entry.placeholders, args.len());
    format_message(entry.forms[0], args, None, &[])
}

// Plural message: `{n}` is `count`; any other placeholder is a named argument.
pub fn tp(id: &str, count: i64, args: &[(&str, &str)]) -> String {
    let Some(entry) = localized_entry(id) else { return missing(id) };
    let expected = entry.placeholders.iter().copied().filter(|name| *name != "n").collect::<std::collections::HashSet<_>>();
    let actual = args.iter().map(|(name, _)| *name).collect::<std::collections::HashSet<_>>();
    assert_eq!(actual, expected, "wrong named arguments for {id}");
    let template = entry.forms.get(usize::from(count != 1)).or(entry.forms.first()).copied().unwrap_or_default();
    format_message(template, &[], Some(count), args)
}

fn missing(id: &str) -> String {
    debug_assert!(false, "missing i18n id: {id}");
    String::new()
}

fn validate_args(id: &str, placeholders: &[&str], actual: usize) {
    assert!(placeholders.iter().all(|name| name.parse::<usize>().is_ok()), "{id} has named placeholders; use tp");
    let expected = placeholders.iter().filter_map(|name| name.parse::<usize>().ok()).max().map_or(0, |max| max + 1);
    assert_eq!(actual, expected, "wrong argument count for {id}");
}

// The selected catalog first, English when the selected one lacks the id.
fn localized_entry(id: &str) -> Option<&'static Entry> {
    let catalogs = catalogs();
    let selected = if current_locale() == Locale::Pt { &catalogs.pt } else { &catalogs.en };
    selected.get(id).or_else(|| catalogs.en.get(id))
}

fn catalogs() -> &'static Catalogs {
    CATALOGS.get_or_init(|| Catalogs {
        en: load_catalog(include_str!("../i18n/en_US/LC_MESSAGES/zapive.po")),
        pt: load_catalog(include_str!("../i18n/pt/LC_MESSAGES/zapive.po")),
    })
}

fn load_catalog(source: &str) -> HashMap<&'static str, Entry> {
    parse_catalog(source).into_iter().flat_map(|(msgid, entry)| {
        let alias = entry.plural.then(|| msgid.strip_suffix(".one")).flatten();
        let alias = alias.map(|base| (base, Entry { forms: entry.forms.clone(), plural: true, placeholders: entry.placeholders.clone() }));
        [Some((msgid, entry)), alias]
    }).flatten().collect()
}

// (msgid, entry) in file order; the header entry (empty msgid) is skipped.
fn parse_catalog(source: &str) -> Vec<(&'static str, Entry)> {
    pofile(source).expect("invalid embedded PO catalog").entries.into_iter()
        .filter(|entry| !entry.msgid.is_empty())
        .map(|entry| {
            let plural = entry.msgid_plural.is_some();
            let forms: Vec<&'static str> = if plural {
                entry.msgstr_plural.into_iter().map(|s| &*Box::leak(s.into_boxed_str())).collect()
            } else {
                vec![&*Box::leak(entry.msgstr.unwrap_or_default().into_boxed_str())]
            };
            let placeholders = extract_placeholders(&forms);
            (&*Box::leak(entry.msgid.into_boxed_str()), Entry { forms, plural, placeholders })
        }).collect()
}

fn format_message(message: &str, args: &[&str], count: Option<i64>, named_args: &[(&str, &str)]) -> String {
    let mut output = String::with_capacity(message.len());
    let mut remaining = message;
    while let Some(start) = remaining.find('{') {
        output.push_str(&remaining[..start]);
        let Some(end) = remaining[start + 1..].find('}') else {
            output.push_str(&remaining[start..]);
            return output;
        };
        let name = &remaining[start + 1..start + 1 + end];
        let replacement = if name == "n" {
            count.map(|value| value.to_string())
        } else if let Some((_, value)) = named_args.iter().find(|(key, _)| *key == name) {
            Some((*value).to_string())
        } else {
            name.parse::<usize>().ok().and_then(|index| args.get(index).map(|value| (*value).to_string()))
        };
        if let Some(value) = replacement { output.push_str(&value); }
        else { output.push_str(&remaining[start..start + end + 2]); }
        remaining = &remaining[start + end + 2..];
    }
    output.push_str(remaining);
    output
}

// Names between braces across every form, sorted and unique. `{}` (Slint's
// anonymous argument) has no name and is skipped.
fn extract_placeholders(forms: &[&str]) -> Vec<&'static str> {
    let mut names = Vec::new();
    for form in forms {
        let mut remaining = *form;
        while let Some(start) = remaining.find('{') {
            let Some(end) = remaining[start + 1..].find('}') else { break };
            let name = &remaining[start + 1..start + 1 + end];
            if !name.is_empty() { names.push(&*Box::leak(name.to_string().into_boxed_str())); }
            remaining = &remaining[start + end + 2..];
        }
    }
    names.sort_unstable();
    names.dedup();
    names
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{BTreeMap, BTreeSet};
    use std::path::{Path, PathBuf};
    // domain.area.item: two or more dot-separated segments, each starting with a
    // lowercase letter (a digit is allowed after the first segment) and then letters,
    // digits or '_': status.scanQr, account.backup.erase_action.
    fn is_id(text: &str) -> bool {
        let mut parts = 0;
        for part in text.split('.') {
            let mut chars = part.chars();
            let ok = chars.next().is_some_and(|c| c.is_ascii_lowercase() || (parts > 0 && c.is_ascii_digit()))
                && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
            if !ok { return false; }
            parts += 1;
        }
        parts >= 2
    }

    fn files(dir: &Path, ext: &str, out: &mut Vec<PathBuf>) {
        let mut entries = std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().path()).collect::<Vec<_>>();
        entries.sort();
        for path in entries {
            if path.is_dir() { files(&path, ext, out); }
            else if path.extension().is_some_and(|e| e == ext) { out.push(path); }
        }
    }

    // (file:line, first string literal) of every `name("literal"` call.
    fn calls(dir: &str, ext: &str, pattern: &str, skip: &str) -> Vec<(String, String)> {
        let re = regex::Regex::new(pattern).unwrap();
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut paths = Vec::new();
        files(&root.join(dir), ext, &mut paths);
        let mut found = Vec::new();
        for path in paths {
            if path.file_name().is_some_and(|n| n == skip) { continue; }
            let text = std::fs::read_to_string(&path).unwrap();
            for (index, line) in text.lines().enumerate() {
                for caps in re.captures_iter(line) {
                    let rel = path.strip_prefix(root).unwrap().display().to_string().replace('\\', "/");
                    found.push((format!("{rel}:{}", index + 1), caps[1].to_string()));
                }
            }
        }
        found
    }

    fn rust_literal_ids(catalogs: &Catalogs) -> BTreeSet<String> {
        let mut paths = Vec::new();
        files(&Path::new(env!("CARGO_MANIFEST_DIR")).join("src"), "rs", &mut paths);
        let re = regex::Regex::new(r#""([^"\\]*(?:\\.[^"\\]*)*)""#).unwrap();
        let mut literals = Vec::new();
        for path in paths.into_iter().filter(|path| path.file_name().is_none_or(|name| name != "i18n.rs")) {
            let text = std::fs::read_to_string(path).unwrap();
            for line in text.lines() {
                literals.extend(re.captures_iter(line).map(|caps| caps[1].to_string()));
            }
        }
        literals.into_iter()
            .filter(|literal| is_id(literal) && catalogs.en.contains_key(literal.as_str()))
            .collect()
    }

    fn rust_calls() -> Vec<(String, String, &'static str)> {
        let mut found = Vec::new();
        for (kind, pattern) in [("t", r#"\bt\(\s*"([^"]*)""#), ("ta", r#"\bta\(\s*"([^"]*)""#), ("tp", r#"\btp\(\s*"([^"]*)""#)] {
            for (at, id) in calls("src", "rs", pattern, "i18n.rs") { found.push((at, id, kind)); }
        }
        found
    }

    fn slint_calls() -> Vec<(String, String)> {
        calls("ui", "slint", r#"@tr\(\s*"((?:[^"\\]|\\.)*)""#, "")
    }

    // Plural usage in Slint: @tr("a.one" | "a.other" % n). The first literal is matched by
    // slint_calls; this returns the second one (file:line, "a.other").
    fn slint_plural_others() -> Vec<(String, String)> {
        calls("ui", "slint", r#"@tr\(\s*"(?:[^"\\]|\\.)*"\s*\|\s*"((?:[^"\\]|\\.)*)""#, "")
    }

    fn catalog(path: &str) -> (Vec<(&'static str, Entry)>, String) {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let text = std::fs::read_to_string(root.join(path)).unwrap();
        (parse_catalog(&text), text)
    }

    #[test]
    fn catalogs_are_complete_context_free_and_in_step() {
        let (en, en_text) = catalog("i18n/en_US/LC_MESSAGES/zapive.po");
        let (pt, pt_text) = catalog("i18n/pt/LC_MESSAGES/zapive.po");
        for (name, text) in [("en_US", &en_text), ("pt", &pt_text)] {
            assert!(!text.lines().any(|l| l.starts_with("msgctxt")), "{name}: msgctxt is not allowed");
            assert!(text.contains("Plural-Forms: nplurals=2; plural=(n != 1);"), "{name}: plural rule");
        }
        let ids = |entries: &[(&'static str, Entry)]| entries.iter().map(|(id, _)| *id).collect::<Vec<_>>();
        for (name, entries) in [("en_US", &en), ("pt", &pt)] {
            let all = ids(entries);
            let unique = all.iter().collect::<BTreeSet<_>>();
            assert_eq!(all.len(), unique.len(), "{name}: duplicate msgid");
            for (id, entry) in entries {
                assert!(entry.forms.iter().all(|form| !form.is_empty()), "{name}: empty msgstr for {id}");
                if entry.plural {
                    assert!(id.ends_with(".one") && is_id(id), "{name}: plural msgid must be <id>.one: {id}");
                    assert_eq!(entry.forms.len(), 2, "{name}: {id} needs msgstr[0] and msgstr[1]");
                }
            }
        }
        for (id, entry) in &en {
            if entry.plural {
                let base = id.trim_end_matches(".one");
                assert!(en_text.contains(&format!("msgid_plural \"{base}.other\"")), "en_US: {id} needs msgid_plural {base}.other");
            }
        }
        let by_id = |entries: Vec<(&'static str, Entry)>| entries.into_iter().collect::<BTreeMap<_, _>>();
        let (en, pt) = (by_id(en), by_id(pt));
        assert_eq!(en.keys().collect::<Vec<_>>(), pt.keys().collect::<Vec<_>>(), "en_US and pt must hold the same ids");
        for (id, en_entry) in &en {
            let pt_entry = &pt[id];
            assert_eq!(en_entry.plural, pt_entry.plural, "plural mismatch: {id}");
            assert_eq!(en_entry.placeholders, pt_entry.placeholders, "placeholder mismatch: {id}");
        }
    }

    #[test]
    fn rust_calls_resolve_to_the_right_kind_of_entry_in_both_locales() {
        let catalogs = catalogs();
        let calls = rust_calls();
        assert!(calls.len() > 100, "scanner found only {} calls", calls.len());
        for (at, id, kind) in &calls {
            assert!(is_id(id), "{at}: {kind}(\"{id}\") is not a semantic id");
            for (name, map) in [("en_US", &catalogs.en), ("pt", &catalogs.pt)] {
                let entry = map.get(id.as_str()).unwrap_or_else(|| panic!("{at}: {id} missing in {name}"));
                assert_eq!(entry.plural, *kind == "tp", "{at}: {kind}(\"{id}\") against a {} entry", if entry.plural { "plural" } else { "singular" });
                let numbered = entry.placeholders.iter().all(|p| p.parse::<usize>().is_ok());
                assert!(*kind == "tp" || numbered, "{at}: {id} has named placeholders, use tp");
            }
        }
    }

    // Every id the code or UI can ask for. A Slint plural names two ids, "<id>.one" and
    // "<id>.other"; the catalog stores them as one entry keyed by "<id>.one".
    fn referenced_ids() -> BTreeSet<String> {
        let mut used = BTreeSet::new();
        for (_, id, _) in rust_calls() { used.insert(id); }
        used.extend(rust_literal_ids(catalogs()));
        used.insert("message.forwarded.label".to_string());
        for (_, id) in slint_calls() { used.insert(id); }
        for (_, id) in slint_plural_others() { used.insert(id); }
        used
    }

    #[test]
    fn no_catalog_entry_is_orphaned_or_outside_the_id_pattern() {
        let catalogs = catalogs();
        let used = referenced_ids();
        let (en, _) = catalog("i18n/en_US/LC_MESSAGES/zapive.po");
        for (id, entry) in &en {
            assert!(is_id(id), "catalog msgid is not a semantic id (transitional literal?): {id:?}");
            let base = id.strip_suffix(".one").filter(|_| entry.plural);
            let referenced = used.contains(*id) || base.is_some_and(|base| used.contains(base) || used.contains(&format!("{base}.other")));
            assert!(referenced, "orphan id: {id} is not referenced by src/**/*.rs or ui/**/*.slint");
        }
        let pt_only = catalogs.pt.keys().filter(|id| !catalogs.en.contains_key(*id)).collect::<Vec<_>>();
        assert!(pt_only.is_empty(), "ids only in pt: {pt_only:?}");
    }

    #[test]
    fn every_slint_translation_is_an_id_present_in_both_catalogs() {
        let catalogs = catalogs();
        let calls = slint_calls();
        assert!(calls.len() > 200, "scanner found only {} @tr calls", calls.len());
        for (at, arg) in &calls {
            assert!(is_id(arg), "{at}: @tr(\"{arg}\") is not a semantic id");
            for (name, map) in [("en_US", &catalogs.en), ("pt", &catalogs.pt)] {
                let entry = map.get(arg.as_str()).unwrap_or_else(|| panic!("{at}: @tr(\"{arg}\") missing in {name}"));
                assert!(entry.forms.iter().all(|form| !form.is_empty()), "{at}: empty msgstr for {arg} in {name}");
            }
        }
        for (at, other) in slint_plural_others() {
            assert!(is_id(&other) && other.ends_with(".other"), "{at}: plural second form must be <id>.other: {other}");
            let one = format!("{}.one", other.trim_end_matches(".other"));
            assert!(calls.iter().any(|(site, id)| site == &at && id == &one), "{at}: {other} has no matching {one} on the same line");
            for (name, map) in [("en_US", &catalogs.en), ("pt", &catalogs.pt)] {
                let entry = map.get(one.as_str()).unwrap_or_else(|| panic!("{at}: {one} missing in {name}"));
                assert!(entry.plural, "{at}: {one} must be a plural entry in {name}");
            }
        }
    }

    #[test]
    fn bundled_translation_runtime_selects_both_locales() {
        let _lock = LOCALE_TEST_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let _ui = crate::AppWindow::new().expect("create Slint component");
        for locale in ["en_US", "pt"] {
            slint::select_bundled_translation(locale).expect("bundled locale exists");
        }
        slint::select_bundled_translation("en_US").expect("restore English bundle");
    }

    #[test]
    fn interpolation_preserves_argument_placeholders_literally() {
        assert_eq!(format_message("{0} {1}", &["{1}", "second"], None, &[]), "{1} second");
        assert_eq!(format_message("{n}: {name}", &[], Some(3), &[]), "3: {name}");

        let plural_template = catalogs().en.get("selection.messages.count").unwrap().forms[1];
        assert_eq!(format_message(plural_template, &["{1}"], Some(2), &[]), "2 selected");
        assert_eq!(format_message("{n} {0}", &["{n}"], Some(2), &[]), "2 {n}");
        assert_eq!(format_message("{name} {n}", &[], Some(4), &[("name", "{n}")]), "{n} 4");
    }

    #[test]
    fn plural_forms_follow_the_count_in_both_locales() {
        let _lock = LOCALE_TEST_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let previous = current_locale();
        struct Restore(Locale);
        impl Drop for Restore { fn drop(&mut self) { set_locale(self.0); } }
        let _restore = Restore(previous);
        set_locale(Locale::En);
        assert_eq!(tp("ephemeral.minutes", 1, &[]), "1 minute");
        assert_eq!(tp("ephemeral.minutes", 5, &[]), "5 minutes");
        assert_eq!(tp("ephemeral.days", 0, &[]), "0 days");
        set_locale(Locale::Pt);
        assert_eq!(tp("ephemeral.hours", 1, &[]), "1 hora");
        assert_eq!(tp("ephemeral.hours", 3, &[]), "3 horas");
        assert_eq!(ta("status.connect_failed", &["rede"]), "Falha ao conectar: rede");
        set_locale(Locale::En);
        assert_eq!(ta("status.connect_failed", &["net"]), "Failed to connect: net");
    }
}
