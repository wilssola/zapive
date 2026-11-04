// The incoming-message chime. Synthesised here (two soft bell-like notes, a
// rising major third) so there is no audio asset to ship or license, and
// played through cpal so it works whatever the OS notification settings are.
use std::sync::Mutex;
use std::time::{Duration, Instant};

const PEAK_GAIN: f32 = 0.22;
// Bursts of messages chime once, not once per message.
const MIN_GAP: Duration = Duration::from_millis(1500);

static LAST_PLAYED: Mutex<Option<Instant>> = Mutex::new(None);

// (start seconds, frequency Hz, length seconds)
const NOTES: [(f32, f32, f32); 2] = [(0.0, 880.0, 0.32), (0.11, 1108.73, 0.42)];

fn note_tail(note: (f32, f32, f32)) -> f32 {
    note.0 + note.2
}

// Mono samples at `rate`: a fundamental plus a quiet octave partial, a
// 6 ms attack and an exponential decay so it never clicks.
pub fn chime_samples(rate: u32) -> Vec<f32> {
    let total = NOTES.iter().map(|n| note_tail(*n)).fold(0.0, f32::max);
    let len = (total * rate as f32).ceil() as usize + 1;
    let mut out = vec![0.0f32; len];
    for (start, freq, length) in NOTES {
        let first = (start * rate as f32) as usize;
        let count = (length * rate as f32) as usize;
        for i in 0..count {
            let Some(slot) = out.get_mut(first + i) else {
                break;
            };
            let t = i as f32 / rate as f32;
            let attack = (t / 0.006).min(1.0);
            let release = ((length - t) / 0.03).clamp(0.0, 1.0);
            let envelope = attack * release * (-t * 9.0).exp();
            let phase = std::f32::consts::TAU * freq * t;
            *slot += (phase.sin() + 0.25 * (phase * 2.0).sin()) * envelope;
        }
    }
    for sample in &mut out {
        *sample = (*sample * PEAK_GAIN).clamp(-1.0, 1.0);
    }
    out
}

// Messages older than this arrived through the offline backlog or a
// history sync, not live.
pub const MAX_LIVE_AGE_SECS: i64 = 30;

// Everything the chime decision looks at.
#[derive(Clone, Copy, Debug)]
pub struct ChimeContext {
    pub setting_on: bool,
    pub from_me: bool,
    pub system_message: bool,
    pub vault_locked: bool,
    pub chat_muted: bool,
    // Archived chats are quiet like muted ones: no chime, no banner.
    pub chat_archived: bool,
    pub mentions_me: bool,
    // Seconds between the message timestamp and now; None when the
    // message carries no usable timestamp.
    pub age_secs: Option<i64>,
    pub chat_open: bool,
    pub window_focused: bool,
    // Time since the last chime, None if there was none yet.
    pub since_last: Option<Duration>,
}

// A muted or archived chat stays quiet unless someone tagged me, the same
// rule for the chime and for the desktop banner.
pub fn is_quiet_chat(muted: bool, archived: bool, mentions_me: bool) -> bool {
    (muted || archived) && !mentions_me
}

pub fn should_chime(ctx: &ChimeContext) -> bool {
    if !ctx.setting_on || ctx.from_me || ctx.system_message || ctx.vault_locked {
        return false;
    }
    if is_quiet_chat(ctx.chat_muted, ctx.chat_archived, ctx.mentions_me) {
        return false;
    }
    if ctx.age_secs.is_some_and(|age| age > MAX_LIVE_AGE_SECS) {
        return false;
    }
    if ctx.chat_open && ctx.window_focused {
        return false;
    }
    ctx.since_last.is_none_or(|gap| gap >= MIN_GAP)
}

// Checks the burst gap and, when clear, arms it for the next call.
fn throttled() -> bool {
    let Ok(mut last) = LAST_PLAYED.lock() else {
        return true;
    };
    let now = Instant::now();
    if last.is_some_and(|at| now.duration_since(at) < MIN_GAP) {
        return true;
    }
    *last = Some(now);
    false
}

pub fn since_last_chime() -> Option<Duration> {
    LAST_PLAYED
        .lock()
        .ok()
        .and_then(|last| last.map(|at| at.elapsed()))
}

// Fire and forget: a failure (no output device, busy device) is silent.
// The caller has already decided with should_chime.
pub fn play_message() {
    if throttled() {
        return;
    }
    let _ = std::thread::Builder::new()
        .name("message-sound".into())
        .spawn(|| {
            use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
            let Some(device) = cpal::default_host().default_output_device() else {
                return;
            };
            let Ok(config) = device.default_output_config() else {
                return;
            };
            let rate = config.sample_rate();
            let channels = config.channels() as usize;
            let samples = chime_samples(rate.max(8_000));
            let length = samples.len();
            let mut cursor = 0usize;
            let Ok(stream) = device.build_output_stream(
                config.config(),
                move |out: &mut [f32], _| {
                    for frame in out.chunks_mut(channels) {
                        let value = samples.get(cursor).copied().unwrap_or(0.0);
                        cursor += 1;
                        frame.fill(value);
                    }
                },
                |error| eprintln!("[sound] output stream error: {error}"),
                None,
            ) else {
                return;
            };
            if stream.play().is_err() {
                return;
            }
            std::thread::sleep(Duration::from_secs_f32(
                length as f32 / rate.max(8_000) as f32 + 0.25,
            ));
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn live() -> ChimeContext {
        ChimeContext {
            setting_on: true,
            from_me: false,
            system_message: false,
            vault_locked: false,
            chat_muted: false,
            chat_archived: false,
            mentions_me: false,
            age_secs: Some(1),
            chat_open: false,
            window_focused: false,
            since_last: None,
        }
    }

    #[test]
    fn chimes_for_a_plain_live_message() {
        assert!(should_chime(&live()));
    }

    #[test]
    fn respects_the_setting() {
        assert!(!should_chime(&ChimeContext {
            setting_on: false,
            ..live()
        }));
    }

    #[test]
    fn never_chimes_for_own_system_or_locked() {
        assert!(!should_chime(&ChimeContext {
            from_me: true,
            ..live()
        }));
        assert!(!should_chime(&ChimeContext {
            system_message: true,
            ..live()
        }));
        assert!(!should_chime(&ChimeContext {
            vault_locked: true,
            ..live()
        }));
    }

    #[test]
    fn muted_chats_are_quiet_unless_mentioned() {
        assert!(!should_chime(&ChimeContext {
            chat_muted: true,
            ..live()
        }));
        assert!(should_chime(&ChimeContext {
            chat_muted: true,
            mentions_me: true,
            ..live()
        }));
    }

    #[test]
    fn archived_chats_are_quiet_unless_mentioned() {
        assert!(!should_chime(&ChimeContext {
            chat_archived: true,
            ..live()
        }));
        assert!(should_chime(&ChimeContext {
            chat_archived: true,
            mentions_me: true,
            ..live()
        }));
        assert!(!should_chime(&ChimeContext {
            chat_archived: true,
            chat_muted: true,
            ..live()
        }));
    }

    #[test]
    fn quiet_chat_rule_is_shared_by_chime_and_banner() {
        assert!(is_quiet_chat(false, true, false));
        assert!(is_quiet_chat(true, false, false));
        assert!(!is_quiet_chat(false, false, false));
        assert!(!is_quiet_chat(false, true, true));
        assert!(!is_quiet_chat(true, true, true));
    }

    #[test]
    fn backlog_and_history_are_quiet() {
        assert!(should_chime(&ChimeContext {
            age_secs: Some(MAX_LIVE_AGE_SECS),
            ..live()
        }));
        assert!(!should_chime(&ChimeContext {
            age_secs: Some(MAX_LIVE_AGE_SECS + 1),
            ..live()
        }));
        assert!(!should_chime(&ChimeContext {
            age_secs: Some(86_400),
            ..live()
        }));
        assert!(should_chime(&ChimeContext {
            age_secs: None,
            ..live()
        }));
        assert!(should_chime(&ChimeContext {
            age_secs: Some(-5),
            ..live()
        }));
    }

    #[test]
    fn open_chat_is_quiet_only_while_the_window_is_focused() {
        assert!(!should_chime(&ChimeContext {
            chat_open: true,
            window_focused: true,
            ..live()
        }));
        assert!(should_chime(&ChimeContext {
            chat_open: true,
            window_focused: false,
            ..live()
        }));
        assert!(should_chime(&ChimeContext {
            chat_open: false,
            window_focused: true,
            ..live()
        }));
    }

    #[test]
    fn bursts_are_throttled() {
        let just_played = Some(MIN_GAP - Duration::from_millis(1));
        assert!(!should_chime(&ChimeContext {
            since_last: just_played,
            ..live()
        }));
        assert!(should_chime(&ChimeContext {
            since_last: Some(MIN_GAP),
            ..live()
        }));
        assert!(should_chime(&ChimeContext {
            since_last: Some(Duration::from_secs(60)),
            ..live()
        }));
    }
    const SYNTH_RATE: u32 = 48_000;

    #[test]
    fn chime_is_short_audible_and_clip_free() {
        let samples = chime_samples(SYNTH_RATE);
        let seconds = samples.len() as f32 / SYNTH_RATE as f32;
        assert!((0.4..1.0).contains(&seconds));
        let peak = samples.iter().fold(0.0f32, |peak, s| peak.max(s.abs()));
        assert!(peak > 0.05 && peak < 0.9, "peak {peak}");
        assert!(samples.iter().all(|s| s.is_finite()));
        assert!(samples.last().is_some_and(|s| s.abs() < 1e-3));
    }

    #[test]
    fn chime_scales_with_the_device_rate() {
        let low = chime_samples(24_000).len();
        let high = chime_samples(48_000).len();
        assert!(high > low * 19 / 10 && high < low * 21 / 10);
    }

    #[test]
    fn burst_is_throttled_until_the_gap_elapses() {
        let reset = |at: Option<Instant>| *LAST_PLAYED.lock().unwrap() = at;
        reset(None);
        assert!(!throttled(), "first chime must play");
        assert!(throttled(), "immediate second chime is part of the burst");
        assert!(throttled(), "still inside the gap");
        let before_gap = Instant::now().checked_sub(MIN_GAP - Duration::from_millis(200));
        reset(before_gap);
        assert!(throttled(), "just under the gap stays throttled");
        let after_gap = Instant::now().checked_sub(MIN_GAP + Duration::from_millis(50));
        reset(after_gap);
        assert!(!throttled(), "a chime after the gap plays again");
        assert!(throttled(), "and re-arms the throttle");
        reset(None);
    }
}
