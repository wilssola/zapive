use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GfxChoice {
    Auto,
    Wgpu,
    Skia,
    OpenGl,
    Software,
}

impl GfxChoice {
    pub const ALL: [GfxChoice; 5] = [
        GfxChoice::Auto,
        GfxChoice::Wgpu,
        GfxChoice::Skia,
        GfxChoice::OpenGl,
        GfxChoice::Software,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Wgpu => "wgpu",
            Self::Skia => "skia",
            Self::OpenGl => "opengl",
            Self::Software => "software",
        }
    }

    pub fn parse(text: &str) -> Option<GfxChoice> {
        match text.trim().to_ascii_lowercase().as_str() {
            "auto" => Some(Self::Auto),
            "wgpu" => Some(Self::Wgpu),
            "skia" => Some(Self::Skia),
            "opengl" => Some(Self::OpenGl),
            "software" => Some(Self::Software),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Compiled {
    pub skia: bool,
    pub skia_opengl: bool,
    pub skia_wgpu: bool,
    pub skia_software: bool,
    pub software: bool,
}

impl Compiled {
    pub fn from_build() -> Self {
        Self::everything()
    }

    pub fn everything() -> Self {
        Compiled {
            skia: true,
            skia_opengl: true,
            skia_wgpu: true,
            skia_software: true,
            software: true,
        }
    }

    #[cfg(test)]
    pub fn skia_software_only() -> Self {
        Compiled {
            skia: false,
            skia_opengl: false,
            skia_wgpu: false,
            skia_software: true,
            software: false,
        }
    }
}

#[cfg(test)]
pub const VALID_RENDERER_NAMES: [&str; 5] = [
    "skia",
    "skia-opengl",
    "skia-wgpu",
    "skia-software",
    "software",
];

pub fn available_choices(compiled: &Compiled) -> Vec<GfxChoice> {
    GfxChoice::ALL
        .into_iter()
        .filter(|choice| match choice {
            GfxChoice::Auto => true,
            GfxChoice::Wgpu => compiled.skia_wgpu,
            GfxChoice::Skia => compiled.skia,
            GfxChoice::OpenGl => compiled.skia_opengl,
            GfxChoice::Software => compiled.software,
        })
        .collect()
}

pub fn effective_choice(choice: GfxChoice, compiled: &Compiled) -> GfxChoice {
    if available_choices(compiled).contains(&choice) {
        choice
    } else {
        GfxChoice::Auto
    }
}

pub type Attempt = Option<&'static str>;

pub fn attempts(choice: GfxChoice, compiled: &Compiled) -> Vec<Attempt> {
    let choice = effective_choice(choice, compiled);
    let first = match choice {
        GfxChoice::Auto => None,
        GfxChoice::Wgpu => Some("skia-wgpu"),
        GfxChoice::Skia => Some("skia"),
        GfxChoice::OpenGl => Some("skia-opengl"),
        GfxChoice::Software => Some("software"),
    };
    let mut chain = vec![first];
    for candidate in [Some("skia-software"), Some("software")] {
        if candidate == Some("skia-software") && !compiled.skia_software {
            continue;
        }
        if candidate == Some("software") && !compiled.software {
            continue;
        }
        if !chain.contains(&candidate) {
            chain.push(candidate);
        }
    }
    chain
}

pub fn parse_file(text: &str) -> GfxChoice {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(text) else {
        return GfxChoice::Auto;
    };
    value
        .get("renderer")
        .and_then(serde_json::Value::as_str)
        .and_then(GfxChoice::parse)
        .unwrap_or(GfxChoice::Auto)
}

pub fn to_file(choice: GfxChoice) -> String {
    serde_json::json!({ "renderer": choice.as_str() }).to_string()
}

pub fn resolve(file: Option<&str>) -> GfxChoice {
    file.map(parse_file).unwrap_or(GfxChoice::Auto)
}

pub fn config_path(dir: &Path) -> PathBuf {
    dir.join("graphics.json")
}

pub fn read_choice(dir: &Path) -> GfxChoice {
    resolve(std::fs::read_to_string(config_path(dir)).ok().as_deref())
}

pub fn save_choice(dir: &Path, choice: GfxChoice) -> std::io::Result<()> {
    let path = config_path(dir);
    let temp = dir.join("graphics.json.tmp");
    std::fs::write(&temp, to_file(choice))?;
    std::fs::rename(temp, path)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartState {
    Clean,
    Crashed,
}

pub fn marker_path(dir: &Path) -> PathBuf {
    dir.join("graphics.starting")
}

pub fn begin_start(dir: &Path) -> std::io::Result<StartState> {
    let path = marker_path(dir);
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
    {
        Ok(file) => {
            let synced = file.sync_all();
            finish_marker_write(file, &path, synced).map(|()| StartState::Clean)
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(StartState::Crashed),
        Err(error) => Err(error),
    }
}

fn finish_marker_write(
    file: std::fs::File,
    path: &Path,
    synced: std::io::Result<()>,
) -> std::io::Result<()> {
    drop(file);
    if synced.is_err() {
        let _ = std::fs::remove_file(path);
    }
    synced
}

pub fn should_clear_on_exit(marked: bool, panicking: bool) -> bool {
    marked && !panicking
}

pub fn finish_start(dir: &Path) {
    let _ = std::fs::remove_file(marker_path(dir));
}

pub fn plan_after_marker(
    choice: GfxChoice,
    compiled: &Compiled,
    state: StartState,
) -> Vec<Attempt> {
    match state {
        StartState::Clean => attempts(choice, compiled),
        StartState::Crashed => {
            let mut chain = Vec::new();
            if compiled.skia_software {
                chain.push(Some("skia-software"));
            }
            if compiled.software {
                chain.push(Some("software"));
            }
            if chain.is_empty() {
                chain.push(None);
            }
            chain
        }
    }
}

pub fn label_key(choice: GfxChoice) -> &'static str {
    match choice {
        GfxChoice::Auto => "settings.graphics.auto",
        GfxChoice::Wgpu => "settings.graphics.wgpu",
        GfxChoice::Skia => "settings.graphics.skia",
        GfxChoice::OpenGl => "settings.graphics.opengl",
        GfxChoice::Software => "settings.graphics.software",
    }
}

static STARTUP_CHOICE: std::sync::OnceLock<GfxChoice> = std::sync::OnceLock::new();

pub fn remember_startup_choice(choice: GfxChoice) {
    let _ = STARTUP_CHOICE.set(choice);
}

pub fn restart_needed(chosen: GfxChoice, compiled: &Compiled) -> bool {
    needs_restart(chosen, STARTUP_CHOICE.get().copied(), compiled)
}

pub fn needs_restart(chosen: GfxChoice, startup: Option<GfxChoice>, compiled: &Compiled) -> bool {
    let startup = effective_choice(startup.unwrap_or(GfxChoice::Auto), compiled);
    effective_choice(chosen, compiled) != startup
}

pub fn choice_for_attempt(attempt: Attempt) -> GfxChoice {
    match attempt {
        None => GfxChoice::Auto,
        Some("skia-wgpu") => GfxChoice::Wgpu,
        Some("skia") => GfxChoice::Skia,
        Some("skia-opengl") => GfxChoice::OpenGl,
        Some(_) => GfxChoice::Software,
    }
}

pub fn select_first<F>(chain: &[Attempt], mut select: F) -> Result<Attempt, Vec<(Attempt, String)>>
where
    F: FnMut(Attempt) -> Result<(), String>,
{
    let mut errors = Vec::new();
    for attempt in chain {
        match select(*attempt) {
            Ok(()) => return Ok(*attempt),
            Err(error) => errors.push((*attempt, error)),
        }
    }
    Err(errors)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_clears_only_an_existing_marker_without_a_panic() {
        assert!(should_clear_on_exit(true, false));
        assert!(!should_clear_on_exit(false, false));
        assert!(!should_clear_on_exit(true, true));
        assert!(!should_clear_on_exit(false, true));
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("zapive-gfx-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        dir
    }

    fn is_software(a: &Attempt) -> bool {
        matches!(a, Some("software") | Some("skia-software"))
    }

    #[test]
    fn parse_is_case_insensitive_and_trims() {
        assert_eq!(GfxChoice::parse("skia"), Some(GfxChoice::Skia));
        assert_eq!(GfxChoice::parse("  WGPU "), Some(GfxChoice::Wgpu));
        assert_eq!(GfxChoice::parse("OpenGL"), Some(GfxChoice::OpenGl));
        assert_eq!(GfxChoice::parse("Software"), Some(GfxChoice::Software));
        assert_eq!(GfxChoice::parse("auto"), Some(GfxChoice::Auto));
    }

    #[test]
    fn parse_rejects_unknown_values_without_panicking() {
        for bad in [
            "",
            " ",
            "directx",
            "skia-wgpu",
            "vello",
            "femtovg",
            "1",
            "skia\0",
            "\u{1F4A5}",
        ] {
            assert_eq!(GfxChoice::parse(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn as_str_round_trips_for_every_choice() {
        let mut seen = std::collections::HashSet::new();
        for choice in GfxChoice::ALL {
            assert!(
                seen.insert(choice.as_str()),
                "duplicate id {}",
                choice.as_str()
            );
            assert_eq!(GfxChoice::parse(choice.as_str()), Some(choice));
        }
    }

    #[test]
    fn file_round_trips_every_choice() {
        for choice in GfxChoice::ALL {
            assert_eq!(parse_file(&to_file(choice)), choice, "{choice:?}");
        }
    }

    #[test]
    fn file_that_is_broken_or_unknown_means_auto() {
        for bad in [
            "",
            "   ",
            "not json",
            "{",
            r#"{"renderer":"#,
            r#"{"renderer":"directx"}"#,
            r#"{"renderer":"vello"}"#,
            r#"{"renderer":42}"#,
            r#"{"other":"skia"}"#,
            "[]",
            "null",
            "\u{feff}",
        ] {
            assert_eq!(parse_file(bad), GfxChoice::Auto, "{bad:?}");
        }
    }

    #[test]
    fn file_ignores_unknown_extra_keys() {
        assert_eq!(
            parse_file(r#"{"renderer":"software","future":true}"#),
            GfxChoice::Software
        );
    }

    #[test]
    fn every_compiled_option_is_offered_in_order() {
        assert_eq!(
            available_choices(&Compiled::everything()),
            vec![
                GfxChoice::Auto,
                GfxChoice::Wgpu,
                GfxChoice::Skia,
                GfxChoice::OpenGl,
                GfxChoice::Software
            ]
        );
    }

    #[test]
    fn options_that_are_not_compiled_are_hidden_but_auto_stays() {
        assert_eq!(
            available_choices(&Compiled::skia_software_only()),
            vec![GfxChoice::Auto]
        );
        let mut no_gpu = Compiled::everything();
        no_gpu.skia_wgpu = false;
        no_gpu.skia_opengl = false;
        assert_eq!(
            available_choices(&no_gpu),
            vec![GfxChoice::Auto, GfxChoice::Skia, GfxChoice::Software]
        );
    }

    #[test]
    fn saved_choice_that_is_not_compiled_behaves_as_auto() {
        let sw = Compiled::skia_software_only();
        for choice in [
            GfxChoice::Wgpu,
            GfxChoice::Skia,
            GfxChoice::OpenGl,
            GfxChoice::Software,
        ] {
            assert_eq!(effective_choice(choice, &sw), GfxChoice::Auto, "{choice:?}");
        }
        assert_eq!(
            effective_choice(GfxChoice::Wgpu, &Compiled::everything()),
            GfxChoice::Wgpu
        );
    }

    #[test]
    fn auto_never_names_a_renderer_first_so_slint_and_slint_backend_decide() {
        let all = Compiled::everything();
        assert_eq!(attempts(GfxChoice::Auto, &all)[0], None);
        assert_eq!(
            attempts(GfxChoice::Auto, &all),
            vec![None, Some("skia-software"), Some("software")]
        );
        assert_eq!(
            attempts(GfxChoice::Auto, &Compiled::skia_software_only()),
            vec![None, Some("skia-software")]
        );
    }

    #[test]
    fn explicit_choices_name_their_renderer_first() {
        let all = Compiled::everything();
        assert_eq!(attempts(GfxChoice::Wgpu, &all)[0], Some("skia-wgpu"));
        assert_eq!(attempts(GfxChoice::Skia, &all)[0], Some("skia"));
        assert_eq!(attempts(GfxChoice::OpenGl, &all)[0], Some("skia-opengl"));
        assert_eq!(attempts(GfxChoice::Software, &all)[0], Some("software"));
    }

    #[test]
    fn explicit_choice_is_followed_by_the_software_safety_net() {
        let all = Compiled::everything();
        assert_eq!(
            attempts(GfxChoice::Wgpu, &all),
            vec![Some("skia-wgpu"), Some("skia-software"), Some("software")]
        );
        assert_eq!(
            attempts(GfxChoice::Software, &all),
            vec![Some("software"), Some("skia-software")]
        );
    }

    #[test]
    fn every_chain_ends_in_software_without_duplicates_or_foreign_names() {
        for compiled in [Compiled::everything(), Compiled::skia_software_only()] {
            for choice in GfxChoice::ALL {
                let chain = attempts(choice, &compiled);
                assert!(!chain.is_empty(), "{choice:?} {compiled:?}");
                assert!(is_software(chain.last().unwrap()), "{choice:?}: {chain:?}");
                let unique: std::collections::HashSet<_> = chain.iter().collect();
                assert_eq!(unique.len(), chain.len(), "{choice:?}: {chain:?}");
                for name in chain.iter().flatten() {
                    assert!(VALID_RENDERER_NAMES.contains(name), "{name}");
                }
            }
        }
    }

    #[test]
    fn renderer_that_is_not_compiled_is_never_attempted() {
        let sw = Compiled::skia_software_only();
        for choice in GfxChoice::ALL {
            let chain = attempts(choice, &sw);
            for missing in ["skia", "skia-opengl", "skia-wgpu", "software"] {
                assert!(
                    !chain.contains(&Some(missing)),
                    "{choice:?} offers {missing}"
                );
            }
        }
    }

    #[test]
    fn resolve_reads_only_the_saved_file_and_defaults_to_auto() {
        assert_eq!(resolve(None), GfxChoice::Auto);
        assert_eq!(
            resolve(Some(&to_file(GfxChoice::OpenGl))),
            GfxChoice::OpenGl
        );
        assert_eq!(resolve(Some("garbage")), GfxChoice::Auto);
    }

    #[test]
    fn saving_a_choice_does_not_change_one_already_resolved() {
        let before = resolve(Some(&to_file(GfxChoice::Skia)));
        let _saved_later = to_file(GfxChoice::Software);
        assert_eq!(before, GfxChoice::Skia);
    }

    #[test]
    fn first_start_is_clean_and_leaves_a_marker_until_finished() {
        let dir = temp_dir("clean");
        assert_eq!(begin_start(&dir).unwrap(), StartState::Clean);
        assert!(marker_path(&dir).exists());
        finish_start(&dir);
        assert!(!marker_path(&dir).exists());
        assert_eq!(begin_start(&dir).unwrap(), StartState::Clean);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn start_that_never_finished_is_reported_as_crashed() {
        let dir = temp_dir("crashed");
        assert_eq!(begin_start(&dir).unwrap(), StartState::Clean);
        assert_eq!(begin_start(&dir).unwrap(), StartState::Crashed);
        assert_eq!(begin_start(&dir).unwrap(), StartState::Crashed);
        finish_start(&dir);
        assert_eq!(begin_start(&dir).unwrap(), StartState::Clean);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn begin_start_on_a_missing_directory_is_an_error_not_a_panic() {
        let dir =
            std::env::temp_dir().join(format!("zapive-gfx-{}-absent/x/y", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        assert!(begin_start(&dir).is_err());
    }

    #[test]
    fn crashed_start_uses_software_only_whatever_was_chosen() {
        for compiled in [Compiled::everything(), Compiled::skia_software_only()] {
            for choice in GfxChoice::ALL {
                let chain = plan_after_marker(choice, &compiled, StartState::Crashed);
                assert!(!chain.is_empty());
                assert!(chain.iter().all(is_software), "{choice:?}: {chain:?}");
            }
        }
        assert_eq!(
            plan_after_marker(
                GfxChoice::Wgpu,
                &Compiled::everything(),
                StartState::Crashed
            ),
            vec![Some("skia-software"), Some("software")]
        );
    }

    #[test]
    fn clean_start_keeps_the_normal_chain() {
        let all = Compiled::everything();
        for choice in GfxChoice::ALL {
            assert_eq!(
                plan_after_marker(choice, &all, StartState::Clean),
                attempts(choice, &all)
            );
        }
    }
    #[test]
    fn crashed_start_does_not_overwrite_the_saved_choice() {
        let dir = temp_dir("keep");
        save_choice(&dir, GfxChoice::Wgpu).unwrap();
        assert_eq!(begin_start(&dir).unwrap(), StartState::Clean);
        let state = begin_start(&dir).unwrap();
        assert_eq!(state, StartState::Crashed);
        let chain = plan_after_marker(read_choice(&dir), &Compiled::everything(), state);
        assert_eq!(chain[0], Some("skia-software"));
        assert_eq!(read_choice(&dir), GfxChoice::Wgpu);
        finish_start(&dir);
        assert_eq!(begin_start(&dir).unwrap(), StartState::Clean);
        assert_eq!(
            plan_after_marker(
                read_choice(&dir),
                &Compiled::everything(),
                StartState::Clean
            )[0],
            Some("skia-wgpu")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn crashed_start_prefers_skia_software_and_keeps_what_is_compiled() {
        let mut only_sw = Compiled::skia_software_only();
        only_sw.skia_software = false;
        only_sw.software = true;
        assert_eq!(
            plan_after_marker(GfxChoice::Skia, &only_sw, StartState::Crashed),
            vec![Some("software")]
        );
        assert_eq!(
            plan_after_marker(
                GfxChoice::Auto,
                &Compiled::skia_software_only(),
                StartState::Crashed
            ),
            vec![Some("skia-software")]
        );
    }

    #[test]
    fn failed_marker_sync_removes_the_marker() {
        let dir = temp_dir("sync");
        let path = marker_path(&dir);
        let file = std::fs::File::create(&path).unwrap();
        let error = finish_marker_write(file, &path, Err(std::io::Error::other("sync")));
        assert_eq!(error.unwrap_err().to_string(), "sync");
        assert!(!path.exists());
        assert_eq!(begin_start(&dir).unwrap(), StartState::Clean);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn successful_marker_sync_keeps_the_marker() {
        let dir = temp_dir("synced");
        let path = marker_path(&dir);
        let file = std::fs::File::create(&path).unwrap();
        assert!(finish_marker_write(file, &path, Ok(())).is_ok());
        assert!(path.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn restart_is_needed_only_when_the_choice_differs_from_this_start() {
        let all = Compiled::everything();
        assert!(!needs_restart(GfxChoice::Skia, Some(GfxChoice::Skia), &all));
        assert!(needs_restart(GfxChoice::Wgpu, Some(GfxChoice::Skia), &all));
        assert!(!needs_restart(GfxChoice::Auto, None, &all));
        assert!(needs_restart(GfxChoice::Software, None, &all));
        let sw = Compiled::skia_software_only();
        assert!(!needs_restart(GfxChoice::Auto, Some(GfxChoice::Wgpu), &sw));
    }

    #[test]
    fn repeated_crashes_stay_on_software_instead_of_looping() {
        let dir = temp_dir("loop");
        let all = Compiled::everything();
        assert_eq!(begin_start(&dir).unwrap(), StartState::Clean);
        for _ in 0..3 {
            let state = begin_start(&dir).unwrap();
            assert_eq!(state, StartState::Crashed);
            let chain = plan_after_marker(GfxChoice::Wgpu, &all, state);
            assert_eq!(chain[0], Some("skia-software"));
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn select_error_advances_to_the_next_attempt() {
        let mut tried = Vec::new();
        let chosen = select_first(
            &[Some("skia-wgpu"), Some("skia-software"), Some("software")],
            |a| {
                tried.push(a);
                if a == Some("software") {
                    Ok(())
                } else {
                    Err("unavailable".to_string())
                }
            },
        );
        assert_eq!(chosen, Ok(Some("software")));
        assert_eq!(
            tried,
            [Some("skia-wgpu"), Some("skia-software"), Some("software")]
        );
    }

    #[test]
    fn select_stops_at_the_first_success_and_auto_may_succeed_unnamed() {
        let mut tried = 0;
        let chosen = select_first(&[None, Some("skia-software")], |_| {
            tried += 1;
            Ok(())
        });
        assert_eq!(chosen, Ok(None));
        assert_eq!(tried, 1);
    }

    #[test]
    fn select_reports_every_failure_when_nothing_works() {
        let result = select_first(&[None, Some("software")], |a| {
            Err(format!("{} broke", a.unwrap_or("auto")))
        });
        assert_eq!(
            result.unwrap_err(),
            vec![
                (None, "auto broke".to_string()),
                (Some("software"), "software broke".to_string())
            ]
        );
        assert_eq!(select_first(&[], |_| Ok(())), Err(Vec::new()));
    }
}
