// Status/stories viewer.
use super::*;

impl Bridge {
    // ---- status/stories ----

    pub(super) fn refresh_statuses(&mut self) {
        let authors = self.store.status_authors();
        self.ui.set_statuses_unseen(self.store.statuses.values().flatten()
            .any(|item| !item.from_me && !item.read_sent));
        let rows: Vec<ChatItem> = authors
            .iter()
            .map(|(jid, latest, _count)| {
                // Authors are keyed by the identity the update arrived
                // with; the picture belongs to the canonical one.
                let canon = self.store.canon_owned(jid);
                let mut row = self.to_chat_row(&canon, "", latest.timestamp, 0, false);
                row.jid = jid.clone().into();
                row
            })
            .collect();
        for (jid, _, _) in &authors {
            let canon = self.store.canon_owned(jid);
            self.queue_avatar(&canon);
        }
        self.status_model.set_vec(rows);
    }

    pub(super) fn open_status_viewer(&mut self, jid: &str) {
        let Some(items) = self.store.statuses.get(jid).cloned() else {
            return;
        };
        if items.is_empty() {
            return;
        }
        self.viewer = Some((items, 0));
        self.show_status();
    }

    pub(super) fn step_status(&mut self, delta: i32) {
        let Some((items, idx)) = &mut self.viewer else {
            return;
        };
        let next = *idx as i32 + delta;
        if next < 0 || next >= items.len() as i32 {
            self.viewer = None;
            self.ui.set_sv_open(false);
            return;
        }
        *idx = next as usize;
        self.show_status();
    }

    pub(super) fn show_status(&mut self) {
        let Some((items, idx)) = &self.viewer else {
            return;
        };
        let (count, idx_now) = (items.len(), *idx);
        let item = items[idx_now].clone();
        if let Some(stored) = self.store.statuses.get_mut(&item.jid)
            .and_then(|items| items.iter_mut().find(|stored| stored.id == item.id))
            && !stored.read_sent
        {
            stored.read_sent = true;
            self.store.statuses_dirty = true;
            self.refresh_statuses();
            self.schedule_save();
        }
        self.ui.set_sv_name(self.store.chat_name(&item.jid).into());
        self.ui.set_sv_time(format_time(item.timestamp).into());
        self.ui.set_sv_text(item.text.clone().into());
        self.ui.set_sv_index(idx_now as i32 + 1);
        self.ui.set_sv_count(count as i32);
        let has_image = item.kind == MessageKind::Image;
        self.ui.set_sv_has_image(false);
        let is_video = item
            .raw
            .as_deref()
            .map(|raw| {
                use whatsapp_rust::proto_helpers::MessageExt as _;
                raw.get_base_message().video_message.is_set()
            })
            .unwrap_or(false);
        self.ui.set_sv_is_video(is_video);
        self.ui
            .set_sv_video_ready(is_video && self.media_path.contains_key(&item.id));
        if has_image {
            if let Some((img, _, _)) = self.decoded.get(&item.id) {
                self.ui.set_sv_image(img.clone());
                self.ui.set_sv_has_image(true);
            } else if let Some(raw) = item.raw.clone() {
                // Instant thumbnail while the full image downloads.
                use whatsapp_rust::proto_helpers::MessageExt as _;
                let inner = raw.get_base_message();
                let thumb = inner
                    .image_message
                    .as_option()
                    .and_then(|m| m.jpeg_thumbnail.as_ref())
                    .or_else(|| {
                        inner
                            .video_message
                            .as_option()
                            .and_then(|m| m.jpeg_thumbnail.as_ref())
                    })
                    .filter(|t| !t.is_empty());
                if let Some(thumb) = thumb {
                    self.wa.send(Cmd::DecodeThumb {
                        id: item.id.clone(),
                        bytes: thumb.to_vec(),
                        link: false,
                    });
                }
                if inner.image_message.is_set() && !self.media_inflight.contains(&item.id) {
                    self.media_inflight.insert(item.id.clone());
                    self.wa.send(Cmd::Media {
                        id: item.id.clone(),
                        mimetype: item.mimetype.clone(),
                        message: raw,
                        want: MediaWant::Image,
                    });
                } else if inner.video_message.is_set() && !self.media_inflight.contains(&item.id) {
                    // A video update: the clip itself, for a sharp poster
                    // frame now and playback on click.
                    match self.media_path.get(&item.id).cloned() {
                        Some(path) => self.wa.send(Cmd::StatusPoster {
                            id: item.id.clone(),
                            path: std::path::PathBuf::from(path),
                        }),
                        None => {
                            self.media_inflight.insert(item.id.clone());
                            self.wa.send(Cmd::Media {
                                id: item.id.clone(),
                                mimetype: if item.mimetype.is_empty() {
                                    "video/mp4".into()
                                } else {
                                    item.mimetype.clone()
                                },
                                message: raw,
                                want: MediaWant::File,
                            });
                        }
                    }
                }
            }
        }
        self.ui.set_sv_open(true);
    }

    // The clip of a video update landed: draw its first frame.
    pub fn on_status_poster(&mut self, id: &str, img: crate::media::Decoded) {
        let image = image_of(&img);
        self.feed_viewer(id, &image);
    }

    pub(super) fn play_status_video(&mut self) {
        let Some((items, idx)) = &self.viewer else {
            return;
        };
        let item = items[*idx].clone();
        let Some(path) = self.media_path.get(&item.id).map(std::path::PathBuf::from) else {
            return;
        };
        self.close_video();
        self.video_id = Some(item.id.clone());
        self.play_clip(path, item.media_w, item.media_h, item.duration_sec, None);
    }

    // Puts a freshly decoded bitmap on the open viewer when it matches.
    pub(super) fn feed_viewer(&mut self, id: &str, image: &slint::Image) {
        if let Some((items, idx)) = &self.viewer
            && items[*idx].id == id
        {
            self.ui.set_sv_image(image.clone());
            self.ui.set_sv_has_image(true);
        }
    }
}
