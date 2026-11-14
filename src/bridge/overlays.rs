// Lightbox and the video overlay (GIF zoom, click to play).
use super::*;

impl Bridge {
    // ---- lightbox ----

    // The bubble holds a small decode; the lightbox wants the real thing.
    pub(super) fn open_lightbox(&mut self, id: &str) {
        if !self.media_path.contains_key(id) {
            self.request_manual_media(id);
        }
        let Some(row) = (0..self.messages_model.row_count())
            .filter_map(|i| self.messages_model.row_data(i))
            .find(|r| r.id == id)
        else {
            return;
        };
        self.ui.set_lightbox_image(row.picture.clone());
        self.ui.set_lightbox_id(id.into());
        self.ui.set_lightbox_open(true);
        if let Some(path) = self.media_path.get(id) {
            crate::jobs::decode_full(id.to_string(), std::path::PathBuf::from(path));
        }
    }

    pub fn on_full_image(&mut self, id: &str, img: crate::media::Decoded) {
        if self.ui.get_lightbox_open() && self.ui.get_lightbox_id() == id {
            self.ui.set_lightbox_image(image_of(&img));
        }
    }

    // ---- video overlay (GIF zoom and click-to-play) ----

    pub(super) fn open_video(&mut self, id: &str) {
        let Some(m) = self.find_message(id).cloned() else {
            return;
        };
        let Some(path) = self.media_path.get(id).map(std::path::PathBuf::from) else {
            return;
        };
        self.close_video();
        self.video_id = Some(id.to_string());
        // The bubble's frames show instantly while the sharper set loads.
        if m.gif {
            if let Some(anim) = self.animated.get(id) {
                self.zoom_frames = anim.frames.clone();
            } else if let Some((img, _, _)) = self.decoded.get(id) {
                self.zoom_frames = vec![img.clone()];
            }
            self.start_zoom_loop();
            self.ui.set_video_is_clip(false);
            self.ui.set_video_loading(false);
            self.ui.set_video_w(m.media_w.max(320) as i32);
            self.ui.set_video_h(m.media_h.max(240) as i32);
            self.ui.set_video_open(true);
            crate::jobs::zoom_frames(id.to_string(), path);
            return;
        }
        let poster = self.decoded.get(id).map(|(img, _, _)| img.clone());
        self.play_clip(path, m.media_w, m.media_h, m.duration_sec, poster);
    }

    // Opens the player on a cached clip. `poster` stands in until the
    // first picture is decoded.
    pub(super) fn play_clip(
        &mut self,
        path: std::path::PathBuf,
        w: u32,
        h: u32,
        seconds: u32,
        poster: Option<slint::Image>,
    ) {
        self.video_gen += 1;
        let generation = self.video_gen;
        self.video_playing = true;
        self.video_pos = 0.0;
        self.video_duration = seconds as f64;
        self.ui.set_video_frame(poster.unwrap_or_else(empty_image));
        self.ui.set_video_w(w.max(1) as i32);
        self.ui.set_video_h(h.max(1) as i32);
        self.ui.set_video_is_clip(true);
        self.ui.set_video_loading(true);
        self.ui.set_video_playing(true);
        self.ui.set_video_ended(false);
        self.ui.set_video_progress(0.0);
        self.ui.set_video_pos_label("0:00".into());
        self.ui
            .set_video_duration_label(crate::store::format_duration(seconds).into());
        self.ui.set_video_volume(self.video_volume);
        self.ui.set_video_muted(self.video_muted);
        self.ui.set_video_open(true);
        self.video = Some(crate::video::Playback::start(
            self.media_key.clone(),
            path,
            move |event| {
                ui_apply(move |b| b.on_video_event(generation, event));
            },
        ));
    }

    pub(super) fn close_video(&mut self) {
        self.zoom_timer.stop();
        self.zoom_frames.clear();
        self.zoom_idx = 0;
        self.video_gen += 1;
        self.video = None;
        self.video_audio = None;
        self.video_id = None;
        self.ui.set_video_open(false);
        self.ui.set_video_frame(empty_image());
    }

    pub(super) fn start_zoom_loop(&mut self) {
        if let Some(first) = self.zoom_frames.first() {
            self.ui.set_video_frame(first.clone());
        }
        if self.zoom_frames.len() < 2 {
            return;
        }
        if self.window_idle {
            return;
        }
        self.zoom_timer.start(
            slint::TimerMode::Repeated,
            Duration::from_millis(66),
            || {
                apply_now(|b| {
                    if b.zoom_frames.is_empty() {
                        b.zoom_timer.stop();
                        return;
                    }
                    b.zoom_idx = (b.zoom_idx + 1) % b.zoom_frames.len();
                    b.ui.set_video_frame(b.zoom_frames[b.zoom_idx].clone());
                });
            },
        );
    }

    pub fn on_zoom_frames(&mut self, id: &str, frames: Vec<crate::media::Decoded>) {
        if self.video_id.as_deref() != Some(id) || frames.is_empty() {
            return;
        }
        let (w, h) = (frames[0].w as i32, frames[0].h as i32);
        self.zoom_frames = frames.iter().map(image_of).collect();
        self.zoom_idx = 0;
        self.ui.set_video_w(w);
        self.ui.set_video_h(h);
        self.start_zoom_loop();
    }

    // What the player threads report. `generation` is the clip they were
    // started for: a closed or replaced player keeps talking for a moment.
    pub fn on_video_event(&mut self, generation: u64, event: crate::video::PlayerEvent) {
        use crate::video::PlayerEvent;
        if generation != self.video_gen {
            return;
        }
        match event {
            PlayerEvent::Ready { w, h, duration } => {
                self.video_duration = duration;
                self.ui.set_video_w(w.max(1) as i32);
                self.ui.set_video_h(h.max(1) as i32);
                self.ui.set_video_duration_label(
                    crate::store::format_duration(duration.round() as u32).into(),
                );
            }
            PlayerEvent::Frame { frame, pos, resync } => {
                self.ui.set_video_w(frame.w as i32);
                self.ui.set_video_h(frame.h as i32);
                self.ui.set_video_frame(image_of(&frame));
                self.ui.set_video_loading(false);
                self.set_video_pos(pos);
                // The first picture after a start, a resume or a seek is
                // where the sound is lined up with the picture again.
                if resync && let Some(audio) = &self.video_audio {
                    audio.seek(pos);
                    if self.video_playing {
                        audio.resume()
                    } else {
                        audio.pause()
                    }
                }
                if let Some(video) = &self.video {
                    video.frame_shown();
                }
            }
            PlayerEvent::Audio(buffer) => {
                // Decoded whole while the picture was already running.
                let audio = crate::audio::Player::start(&buffer, self.video_pos);
                if let Some(audio) = &audio {
                    audio.set_gain(if self.video_muted {
                        0.0
                    } else {
                        self.video_volume
                    });
                    if !self.video_playing {
                        audio.pause();
                    }
                }
                self.video_audio = audio;
            }
            PlayerEvent::Ended => {
                self.video_playing = false;
                self.ui.set_video_playing(false);
                self.ui.set_video_ended(true);
                self.set_video_pos(self.video_duration);
                if let Some(audio) = &self.video_audio {
                    audio.pause();
                }
            }
            // Not H.264 in an MP4 (HEVC from an iPhone sent as a file,
            // say): whatever the system plays videos with gets it.
            PlayerEvent::Failed => {
                let cached = self
                    .video_id
                    .as_ref()
                    .and_then(|id| self.media_path.get(id))
                    .cloned();
                self.close_video();
                if let Some(cached) = cached {
                    crate::jobs::plain_copy(std::path::PathBuf::from(cached), None, |_, plain| {
                        if let Some(plain) = plain {
                            crate::platform::open_path(&plain.to_string_lossy());
                        }
                    });
                }
            }
        }
    }

    pub(super) fn set_video_pos(&mut self, pos: f64) {
        self.video_pos = pos;
        let total = self.video_duration.max(0.001);
        self.ui
            .set_video_progress((pos / total).clamp(0.0, 1.0) as f32);
        self.ui
            .set_video_pos_label(crate::store::format_duration(pos as u32).into());
    }

    pub(super) fn video_toggle(&mut self) {
        let Some(video) = &self.video else { return };
        if self.video_playing {
            video.pause();
            if let Some(audio) = &self.video_audio {
                audio.pause();
            }
        } else {
            // The sound comes back with the next picture (see `resync`).
            video.resume();
            self.ui.set_video_ended(false);
        }
        self.video_playing = !self.video_playing;
        self.ui.set_video_playing(self.video_playing);
    }

    pub(super) fn video_seek(&mut self, secs: f64) {
        let Some(video) = &self.video else { return };
        let secs = secs.clamp(0.0, self.video_duration);
        // Quiet until the picture lands where it was asked to go.
        if let Some(audio) = &self.video_audio {
            audio.pause();
        }
        video.seek(secs);
        self.ui.set_video_ended(false);
        self.set_video_pos(secs);
    }

    pub(super) fn video_set_volume(&mut self, volume: f32, muted: bool) {
        self.video_volume = volume.clamp(0.0, 1.0);
        self.video_muted = muted;
        self.ui.set_video_volume(self.video_volume);
        self.ui.set_video_muted(muted);
        if let Some(audio) = &self.video_audio {
            audio.set_gain(if muted { 0.0 } else { self.video_volume });
        }
        if let Some(vault) = &self.vault {
            vault.setting_set("video_volume", &format!("{:.2}", self.video_volume));
        }
    }

    pub(super) fn video_save(&mut self) {
        let Some(id) = self.video_id.clone() else {
            return;
        };
        let Some(cached) = self.media_path.get(&id).map(std::path::PathBuf::from) else {
            return;
        };
        crate::jobs::plain_copy(cached, None, |_, plain| {
            let Some(plain) = plain else {
                return;
            };
            let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
            let name = format!("VID-{stamp}.mp4");
            std::thread::spawn(move || {
                let picked = rfd::FileDialog::new().set_file_name(&name).save_file();
                if let Some(target) = picked
                    && let Err(e) = std::fs::copy(&plain, &target)
                {
                    eprintln!("[video] save failed: {e}");
                }
            });
        });
    }
}
