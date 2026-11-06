// Audio player, mini player and voice recording.
use super::*;

// The note being played (or paused); the mini player takes over when the
// user leaves its conversation.
pub(super) struct AudioState {
    pub(super) id: String,
    pub(super) jid: String,
    pub(super) duration: f64,
    pub(super) paused: bool,
    // Waiting for the decoded buffer; holds where to start, in source secs.
    pub(super) pending_offset: Option<f64>,
    pub(super) player: Option<crate::audio::Player>,
}

pub(super) const RATES: [f64; 5] = [1.0, 1.5, 2.0, 2.5, 3.0];

pub(super) const RATE_LABELS: [&str; 5] = ["1x", "1.5x", "2x", "2.5x", "3x"];

impl Bridge {
    // ---- audio (player, mini player, recording) ----

    pub(super) fn plain_audio_path(&self, id: &str) -> Option<std::path::PathBuf> {
        let cached = std::path::PathBuf::from(self.media_path.get(id)?);
        crate::media::temp_plain(&self.media_key, &cached)
    }

    pub(super) fn toggle_audio(&mut self, id: &str) {
        if id.is_empty() {
            return;
        }
        if let Some(a) = &mut self.audio
            && a.id == id
        {
            if let Some(player) = &a.player {
                if a.paused {
                    player.resume();
                    a.paused = false;
                } else {
                    player.pause();
                    a.paused = true;
                }
                let (aid, ajid, paused) = (a.id.clone(), a.jid.clone(), a.paused);
                if Some(&ajid) == self.current_jid.as_ref() {
                    self.patch_row(&aid, |row| row.playing = !paused);
                }
                self.ui.set_mini_audio_playing(!paused);
            }
            return;
        }
        if !self.media_path.contains_key(id) {
            self.request_manual_media(id);
            return;
        }
        self.start_audio(id, 0.0);
    }

    pub(super) fn start_audio(&mut self, id: &str, offset_secs: f64) {
        let Some(jid) = self.current_jid.clone() else {
            return;
        };
        let Some(m) = self.store.messages_for(&jid).iter().find(|m| m.id == id) else {
            return;
        };
        let duration = (m.duration_sec.max(1)) as f64;
        self.stop_audio();
        self.audio = Some(AudioState {
            id: id.to_string(),
            jid,
            duration,
            paused: false,
            pending_offset: Some(offset_secs),
            player: None,
        });
        self.spin_audio_at(offset_secs);
    }

    // Starts (or requests the decode for) the current note at a source
    // position, honoring the selected speed.
    pub(super) fn spin_audio_at(&mut self, offset_secs: f64) {
        let rate_idx = self.audio_rate_idx;
        let rate = RATES[rate_idx];
        let Some(a) = &mut self.audio else { return };
        let id = a.id.clone();
        if let Some(buffer) = self.audio_buffers.get(&(id.clone(), rate_idx)) {
            let player = crate::audio::Player::start(buffer, offset_secs / rate);
            if let Some(a) = &mut self.audio {
                a.player = player;
                a.paused = false;
                a.pending_offset = None;
            }
            self.ensure_audio_timer();
            let jid = self
                .audio
                .as_ref()
                .map(|a| a.jid.clone())
                .unwrap_or_default();
            if Some(&jid) == self.current_jid.as_ref() {
                self.patch_row(&id, |row| row.playing = true);
            }
            self.ui.set_mini_audio_playing(true);
            self.send_played_receipt(&jid, &id);
            return;
        }
        a.pending_offset = Some(offset_secs);
        let Some(plain) = self.plain_audio_path(&id) else {
            self.audio = None;
            return;
        };
        crate::jobs::audio_decode(id, plain, rate_idx, rate);
    }

    pub fn on_audio_ready(&mut self, id: &str, rate_idx: usize, buffer: crate::audio::AudioBuffer) {
        if self.audio_buffers.len() > 8 {
            self.audio_buffers.clear();
        }
        self.audio_buffers
            .insert((id.to_string(), rate_idx), buffer);
        if let Some(a) = &self.audio
            && a.id == id
            && rate_idx == self.audio_rate_idx
            && let Some(offset) = a.pending_offset
        {
            self.spin_audio_at(offset);
        }
    }

    pub(super) fn seek_audio(&mut self, id: &str, frac: f32) {
        let matches = self.audio.as_ref().map(|a| a.id == id).unwrap_or(false);
        if matches {
            let duration = self.audio.as_ref().map(|a| a.duration).unwrap_or(1.0);
            self.spin_audio_at((frac as f64).clamp(0.0, 1.0) * duration);
        } else {
            let duration = self
                .current_jid
                .as_ref()
                .and_then(|jid| self.store.messages_for(jid).iter().find(|m| m.id == id))
                .map(|m| m.duration_sec.max(1) as f64)
                .unwrap_or(0.0);
            self.start_audio(id, (frac as f64).clamp(0.0, 1.0) * duration);
        }
    }

    pub(super) fn cycle_audio_rate(&mut self) {
        // The old player ran at the previous rate: its source position is
        // output position times that rate; capture it before restarting.
        let prev_rate = RATES[self.audio_rate_idx];
        self.audio_rate_idx = (self.audio_rate_idx + 1) % RATES.len();
        self.ui
            .set_audio_rate_label(RATE_LABELS[self.audio_rate_idx].into());
        let offset = self.audio.as_mut().map(|a| {
            let pos = a
                .player
                .as_ref()
                .map(|p| p.position_secs() * prev_rate)
                .unwrap_or(0.0);
            a.player = None;
            pos.min(a.duration)
        });
        if let Some(offset) = offset {
            // The running note restarts at its position, new speed.
            self.spin_audio_at(offset);
        }
    }

    pub(super) fn ensure_audio_timer(&mut self) {
        if self.audio_timer.running() || self.audio.is_none() {
            return;
        }
        self.audio_timer.start(
            slint::TimerMode::Repeated,
            Duration::from_millis(250),
            || {
                apply_now(|b| b.tick_audio());
            },
        );
    }

    pub(super) fn tick_audio(&mut self) {
        let rate = RATES[self.audio_rate_idx];
        let Some(a) = &self.audio else {
            self.audio_timer.stop();
            return;
        };
        let Some(player) = &a.player else { return };
        if player.finished() {
            self.stop_audio();
            return;
        }
        let pos = (player.position_secs() * rate).clamp(0.0, a.duration);
        let progress = (pos / a.duration).clamp(0.0, 1.0) as f32;
        let label = crate::store::format_duration(pos as u32);
        let (id, jid, paused) = (a.id.clone(), a.jid.clone(), a.paused);
        if Some(&jid) == self.current_jid.as_ref() {
            self.patch_row(&id, |row| {
                row.playing = !paused;
                row.progress = progress;
                row.posLabel = label.into();
            });
        }
        self.ui.set_mini_audio_progress(progress);
    }

    pub(super) fn stop_audio(&mut self) {
        if let Some(a) = self.audio.take()
            && Some(&a.jid) == self.current_jid.as_ref()
        {
            self.patch_row(&a.id, |row| {
                row.playing = false;
                row.progress = 0.0;
                row.posLabel = "".into();
            });
        }
        self.ui.set_mini_audio(false);
        self.ui.set_mini_audio_playing(false);
        self.audio_timer.stop();
    }

    // Shows or hides the mini bar depending on where the note lives.
    pub(super) fn sync_mini_player(&mut self) {
        let show = match (&self.audio, &self.current_jid) {
            (Some(a), Some(current)) => &a.jid != current,
            (Some(_), None) => true,
            _ => false,
        };
        if !show {
            self.ui.set_mini_audio(false);
            return;
        }
        let Some(a) = &self.audio else { return };
        let name = self.store.chat_name(&a.jid);
        let avatar = self.avatar_for(&a.jid);
        self.ui.set_mini_audio_name(name.clone().into());
        self.ui.set_mini_audio_avatar_has(avatar.is_some());
        self.ui
            .set_mini_audio_avatar(avatar.unwrap_or_else(empty_image));
        self.ui.set_mini_audio_initial(initial_of(&name).into());
        self.ui.set_mini_audio_color_idx(color_idx_of(&a.jid));
        self.ui.set_mini_audio_playing(!a.paused);
        self.ui.set_mini_audio(true);
    }

    pub fn on_wave(&mut self, id: &str, img: crate::media::Decoded) {
        let image = image_of(&img);
        self.waves.insert(id.to_string(), image.clone());
        self.patch_row(id, |row| {
            row.wave = image;
            row.hasWave = true;
        });
    }

    pub(super) fn start_recording(&mut self) {
        if self.recorder.is_some() || self.current_jid.is_none() {
            return;
        }
        match crate::audio::Recorder::start() {
            Some(recorder) => {
                self.recorder = Some(recorder);
                self.rec_started = Some(Instant::now());
                self.ui.set_rec_elapsed("0:00".into());
                self.ui.set_rec_active(true);
                self.rec_timer.start(
                    slint::TimerMode::Repeated,
                    Duration::from_millis(500),
                    || {
                        apply_now(|b| {
                            if let Some(started) = b.rec_started {
                                let secs = started.elapsed().as_secs() as u32;
                                b.ui.set_rec_elapsed(crate::store::format_duration(secs).into());
                            }
                        });
                    },
                );
            }
            None => self.ui.set_status_text(t("rec.no_mic").into()),
        }
    }

    pub(super) fn stop_recording(&mut self, send: bool) {
        self.rec_timer.stop();
        self.rec_started = None;
        self.ui.set_rec_active(false);
        let view_once = self.ui.get_rec_view_once();
        self.ui.set_rec_view_once(false);
        let Some(recorder) = self.recorder.take() else {
            return;
        };
        if !send {
            return;
        }
        let (samples, in_rate) = recorder.stop();
        // Anything shorter than half a second is a misclick.
        if (samples.len() as f64) < in_rate as f64 / 2.0 {
            return;
        }
        let Some(jid) = self.current_jid.clone() else {
            return;
        };
        self.wa.send(Cmd::SendVoice {
            jid,
            samples,
            in_rate,
            view_once,
        });
    }
}
