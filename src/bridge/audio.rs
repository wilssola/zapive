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
        let Some(cached) = self.media_path.get(&id).map(std::path::PathBuf::from) else {
            self.audio = None;
            return;
        };
        // The decrypt runs off the UI thread; the decode starts when it lands.
        crate::jobs::plain_copy(cached, None, move |b, plain| {
            let still_wanted = b.audio.as_ref().is_some_and(|a| a.id == id);
            match plain {
                Some(plain) if still_wanted => crate::jobs::audio_decode(id, plain, rate_idx, rate),
                Some(_) => {}
                None if still_wanted => b.audio = None,
                None => {}
            }
        });
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

    fn set_rec_levels(&self, levels: Vec<f32>, filled: usize, head: usize) {
        self.ui.set_rec_levels(ModelRc::new(VecModel::from(levels)));
        self.ui.set_rec_filled(filled as i32);
        self.ui.set_rec_head(head as i32);
    }

    pub(super) fn start_recording(&mut self) {
        if self.recorder.is_some() || self.current_jid.is_none() {
            return;
        }
        match crate::audio::Recorder::start() {
            Some(recorder) => {
                self.recorder = Some(recorder);
                self.rec_preview = None;
                self.ui.set_rec_paused(false);
                self.ui.set_rec_previewing(false);
                self.ui.set_rec_elapsed("0:00".into());
                self.set_rec_levels(vec![0.0; crate::audio::REC_BARS], 0, 0);
                self.ui.set_rec_active(true);
                self.rec_timer.start(
                    slint::TimerMode::Repeated,
                    Duration::from_millis(50),
                    || apply_now(|b| b.tick_recording()),
                );
            }
            None => self.ui.set_status_text(t("rec.no_mic").into()),
        }
    }

    // Live: newest levels and the clock. Paused: only the preview moves.
    fn tick_recording(&mut self) {
        let bars = crate::audio::REC_BARS;
        let Some(recorder) = &self.recorder else {
            self.rec_timer.stop();
            return;
        };
        if recorder.is_paused() {
            let Some((player, total)) = &self.rec_preview else {
                return;
            };
            if player.finished() {
                self.stop_rec_preview();
                return;
            }
            let pos = player.position_secs();
            let head = ((pos / total).clamp(0.0, 1.0) * bars as f64).round() as usize;
            let label = format!(
                "{} / {}",
                crate::store::format_duration(pos as u32),
                crate::store::format_duration(total.round() as u32)
            );
            self.ui.set_rec_head(head as i32);
            self.ui.set_rec_elapsed(label.into());
            return;
        }
        let label = crate::store::format_duration(recorder.elapsed_secs() as u32);
        let (levels, filled) = recorder.recent_levels(bars);
        self.ui.set_rec_elapsed(label.into());
        self.set_rec_levels(levels, filled, filled);
    }

    // Stops capturing without ending the recording; the waveform becomes
    // an overview of everything said so far.
    pub(super) fn pause_recording(&mut self) {
        let bars = crate::audio::REC_BARS;
        let Some(recorder) = &self.recorder else {
            return;
        };
        recorder.pause();
        let overview = recorder.overview_levels(bars);
        let label = crate::store::format_duration(recorder.elapsed_secs() as u32);
        self.ui.set_rec_paused(true);
        self.ui.set_rec_elapsed(label.into());
        self.set_rec_levels(overview, bars, bars);
    }

    pub(super) fn resume_recording(&mut self) {
        self.rec_preview = None;
        self.ui.set_rec_previewing(false);
        let Some(recorder) = &self.recorder else {
            return;
        };
        recorder.resume();
        self.ui.set_rec_paused(false);
    }

    // Plays the paused recording from the start, or stops that preview.
    pub(super) fn toggle_rec_preview(&mut self) {
        if self.rec_preview.is_some() {
            self.stop_rec_preview();
            return;
        }
        let Some(recorder) = &self.recorder else {
            return;
        };
        if !recorder.is_paused() {
            return;
        }
        let (samples, rate) = recorder.snapshot();
        let Some(buffer) = crate::audio::recording_buffer(&samples, rate) else {
            return;
        };
        let Some(player) = crate::audio::Player::start(&buffer, 0.0) else {
            return;
        };
        self.rec_preview = Some((player, buffer.duration_secs()));
        self.ui.set_rec_head(0);
        self.ui.set_rec_previewing(true);
    }

    fn stop_rec_preview(&mut self) {
        self.rec_preview = None;
        self.ui.set_rec_previewing(false);
        self.ui.set_rec_head(crate::audio::REC_BARS as i32);
        if let Some(recorder) = &self.recorder {
            let label = crate::store::format_duration(recorder.elapsed_secs() as u32);
            self.ui.set_rec_elapsed(label.into());
        }
    }

    pub(super) fn stop_recording(&mut self, send: bool) {
        self.rec_timer.stop();
        self.rec_preview = None;
        self.ui.set_rec_active(false);
        self.ui.set_rec_paused(false);
        self.ui.set_rec_previewing(false);
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
