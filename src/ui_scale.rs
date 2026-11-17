// User interface scale: the choice in Settings > Appearance, its validation
// and the scale factor handed to the toolkit at startup.

pub const DEFAULT_PERCENT: u16 = 100;
pub const CHOICES: [u16; 5] = [100, 125, 150, 175, 200];

// The stored value is free text in the vault; only the listed steps are
// honoured, anything else falls back to the default.
pub fn parse(text: &str) -> u16 {
    text.trim()
        .parse::<u16>()
        .ok()
        .filter(|percent| CHOICES.contains(percent))
        .unwrap_or(DEFAULT_PERCENT)
}

static STARTED_WITH: std::sync::atomic::AtomicU16 =
    std::sync::atomic::AtomicU16::new(DEFAULT_PERCENT);

// The scale the window was created with; a different saved value needs a
// restart to take effect.
pub fn remember_started_with(percent: u16) {
    STARTED_WITH.store(percent, std::sync::atomic::Ordering::Relaxed);
}

pub fn started_with() -> u16 {
    STARTED_WITH.load(std::sync::atomic::Ordering::Relaxed)
}

pub fn index_of(percent: u16) -> usize {
    CHOICES
        .iter()
        .position(|candidate| *candidate == percent)
        .unwrap_or(0)
}

pub fn percent_at(index: usize) -> Option<u16> {
    CHOICES.get(index).copied()
}

// Windows reports 96 DPI for 100%. Anything that is not a positive number
// is treated as 96 so a broken query cannot shrink the window to nothing.
pub fn system_dpi_or_default(dpi: u32) -> u32 {
    if dpi == 0 { 96 } else { dpi }
}

// The factor the window is created with: the system DPI scale times the
// chosen percentage. `None` at 100% keeps the toolkit's own default.
pub fn env_factor(system_dpi: u32, percent: u16) -> Option<String> {
    if percent == DEFAULT_PERCENT {
        return None;
    }
    let factor = f64::from(system_dpi_or_default(system_dpi)) / 96.0 * f64::from(percent) / 100.0;
    Some(format!("{factor:.4}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_every_listed_step() {
        for percent in CHOICES {
            assert_eq!(parse(&percent.to_string()), percent);
        }
        assert_eq!(parse(" 150 "), 150);
    }

    #[test]
    fn unknown_or_malformed_values_fall_back_to_the_default() {
        for bad in [
            "",
            "0",
            "90",
            "101",
            "250",
            "-125",
            "1.5",
            "150%",
            "abc",
            "9999999999",
        ] {
            assert_eq!(parse(bad), DEFAULT_PERCENT, "{bad:?}");
        }
    }

    #[test]
    fn index_round_trips_and_out_of_range_is_none() {
        for (index, percent) in CHOICES.iter().enumerate() {
            assert_eq!(index_of(*percent), index);
            assert_eq!(percent_at(index), Some(*percent));
        }
        assert_eq!(index_of(133), 0);
        assert_eq!(percent_at(CHOICES.len()), None);
    }

    #[test]
    fn default_scale_sets_no_factor_on_any_dpi() {
        assert_eq!(env_factor(96, 100), None);
        assert_eq!(env_factor(144, 100), None);
    }

    #[test]
    fn factor_multiplies_the_system_scale() {
        assert_eq!(env_factor(96, 150).as_deref(), Some("1.5000"));
        assert_eq!(env_factor(144, 150).as_deref(), Some("2.2500"));
        assert_eq!(env_factor(192, 200).as_deref(), Some("4.0000"));
    }

    #[test]
    fn zero_dpi_is_treated_as_the_baseline() {
        assert_eq!(system_dpi_or_default(0), 96);
        assert_eq!(env_factor(0, 125).as_deref(), Some("1.2500"));
    }
}
