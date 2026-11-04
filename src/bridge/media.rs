// Avatars, thumbnails, downloads and decoded-image caches.
use super::*;

pub(super) struct Anim {
    pub(super) frames: Vec<slint::Image>,
    pub(super) idx: usize,
    pub(super) looping: bool,
}

pub(super) const DECODE_CACHE_MAX: usize = 50;

// The count cap alone is not a memory bound: photos are decoded at up to
// 1280px, so one entry can be 6 MB of RGBA and fifty of them 300 MB. These
// budgets are what actually keeps the caches honest; the counts above stay
// as a ceiling on bookkeeping.
pub(super) const DECODE_CACHE_BYTES: usize = 32 * 1024 * 1024;

pub(super) const ANIM_CACHE_BYTES: usize = 16 * 1024 * 1024;

pub(super) const MAX_ANIMATIONS: usize = 6;

// How many chats keep their message lists in RAM at once.
pub(super) const WARM_MAX: usize = 8;

// Avatars are decoded at 112px already; this keeps a long session from
// accumulating one per contact ever seen.
pub(super) const AVATAR_CACHE_MAX: usize = 300;

// Records `key` as the newest entry of an LRU order and returns the keys
// that no longer fit within `max`.
pub(super) fn touch_lru(
    order: &mut std::collections::VecDeque<String>,
    key: &str,
    max: usize,
) -> Vec<String> {
    order.retain(|known| known != key);
    order.push_back(key.to_string());
    let mut evicted = Vec::new();
    while order.len() > max {
        if let Some(old) = order.pop_front() {
            evicted.push(old);
        }
    }
    evicted
}

impl Bridge {
    // ---- media (results arrive from the tokio side) ----

    pub(super) fn avatar_for(&self, jid: &str) -> Option<slint::Image> {
        self.avatars.get(jid).cloned().flatten()
    }

    pub(super) fn queue_avatar(&mut self, jid: &str) {
        if jid.is_empty() || self.requested_avatars.contains(jid) {
            return;
        }
        self.requested_avatars.insert(jid.to_string());
        self.wa.send(Cmd::FetchAvatar(jid.to_string()));
    }

    pub(super) fn ensure_avatars(&mut self) {
        let jids: Vec<String> = self
            .store
            .sorted_chats()
            .into_iter()
            .map(|meta| meta.jid.clone())
            .filter(|jid| !self.requested_avatars.contains(jid))
            .collect();
        for jid in jids {
            self.queue_avatar(&jid);
        }
    }

    pub fn on_avatar(&mut self, jid: &str, img: Option<crate::media::Decoded>, resolved: bool) {
        match (img, resolved) {
            (Some(decoded), _) => {
                let image = image_of(&decoded);
                self.avatars.insert(jid.to_string(), Some(image.clone()));
                // An evicted avatar is fetched again the next time a row asks.
                for old in touch_lru(&mut self.avatar_order, jid, AVATAR_CACHE_MAX) {
                    self.avatars.remove(&old);
                    self.requested_avatars.remove(&old);
                }
                self.patch_avatar_everywhere(jid, &image);
                self.refresh_message_details();
                self.refresh_typing_members();
                if jid == self.self_jid {
                    self.refresh_accounts();
                }
            }
            (None, true) => {
                // Confirmed: no picture. The initial stays.
                self.avatars.insert(jid.to_string(), None);
            }
            (None, false) => {
                // Transient failure — retry a few times, spaced out.
                let tries = self.avatar_tries.entry(jid.to_string()).or_insert(0);
                *tries += 1;
                if *tries < 3 {
                    self.requested_avatars.remove(jid);
                    let jid = jid.to_string();
                    self.once(1500, move |b| b.queue_avatar(&jid));
                }
            }
        }
    }

    // Which jid a search row's picture belongs to.
    pub(super) fn hit_face(&self, row: &SearchHit) -> String {
        row.face.to_string()
    }

    pub fn on_avatar_large(&mut self, jid: &str, img: crate::media::Decoded) {
        if self.ui.get_info_open() && self.current_jid.as_deref() == Some(jid) {
            self.ui.set_info_avatar(image_of(&img));
            self.ui.set_info_has_avatar(true);
        }
    }

    pub(super) fn patch_avatar_everywhere(&mut self, jid: &str, image: &slint::Image) {
        // Chat list row.
        for i in 0..self.chats_model.row_count() {
            if let Some(mut row) = self.chats_model.row_data(i)
                && row.jid == jid
            {
                row.avatar = image.clone();
                row.hasAvatar = true;
                self.chats_model.set_row_data(i, row);
                break;
            }
        }
        // Conversation header.
        if Some(&jid.to_string()) == self.current_jid.as_ref() {
            self.ui.set_current_avatar(image.clone());
            self.ui.set_current_avatar_has(true);
        }
        // Calls and status lists.
        for i in 0..self.calls_model.row_count() {
            if let Some(mut row) = self.calls_model.row_data(i)
                && row.from == jid
                && !row.hasAvatar
            {
                row.avatar = image.clone();
                row.hasAvatar = true;
                self.calls_model.set_row_data(i, row);
            }
        }
        for i in 0..self.status_model.row_count() {
            if let Some(mut row) = self.status_model.row_data(i)
                && (row.jid == jid || self.store.canon(row.jid.as_str()) == jid)
                && !row.hasAvatar
            {
                row.avatar = image.clone();
                row.hasAvatar = true;
                self.status_model.set_row_data(i, row);
            }
        }
        for model in [
            &self.member_model,
            &self.info_member_model,
            &self.mention_model,
        ] {
            for i in 0..model.row_count() {
                if let Some(mut row) = model.row_data(i)
                    && row.jid == jid
                    && !row.hasAvatar
                {
                    row.avatar = image.clone();
                    row.hasAvatar = true;
                    model.set_row_data(i, row);
                }
            }
        }
        // Search hits (sidebar and in-chat) that show this face.
        for model in [&self.search_model, &self.chat_search_model] {
            for i in 0..model.row_count() {
                if let Some(mut row) = model.row_data(i)
                    && !row.hasAvatar
                    && self.hit_face(&row) == jid
                {
                    row.avatar = image.clone();
                    row.hasAvatar = true;
                    model.set_row_data(i, row);
                }
            }
        }
        // Sender and voice-note avatars inside message rows.
        for i in 0..self.messages_model.row_count() {
            let Some(mut row) = self.messages_model.row_data(i) else {
                continue;
            };
            let mut changed = false;
            if row.senderJid == jid && !row.senderHasAvatar {
                row.senderAvatar = image.clone();
                row.senderHasAvatar = true;
                changed = true;
            }
            if row.voiceJid == jid && !row.voiceHasAvatar {
                row.voiceAvatar = image.clone();
                row.voiceHasAvatar = true;
                changed = true;
            }
            if changed {
                self.messages_model.set_row_data(i, row);
            }
        }
    }

    // Media is fetched and decoded for the rows on screen, as they report
    // their position; a whole chat's pictures never sit in RAM at once.
    pub(super) fn load_media_for_chat(&mut self, jid: &str) {
        let ids: Vec<String> = self.row_geom.keys().cloned().collect();
        let known: HashSet<String> = ids.into_iter().collect();
        let pending: Vec<StoredMessage> = self
            .store
            .messages_for(jid)
            .iter()
            .filter(|m| known.contains(&m.id))
            .filter(|m| needs_media(m))
            .cloned()
            .collect();
        for m in pending {
            self.request_automatic_media(&m);
        }
    }

    // A row just reported where it is: if it shows media that is not
    // loaded (never was, or was released while off screen), ask for it.
    pub(super) fn media_for_visible_row(&mut self, id: &str) {
        let Some(jid) = self.current_jid.clone() else {
            return;
        };
        if self.media_inflight.contains(id) || self.media_failed.contains(id) {
            return;
        }
        let Some(m) = self
            .store
            .messages_for(&jid)
            .iter()
            .find(|m| m.id == id)
            .cloned()
        else {
            return;
        };
        if !needs_media(&m) {
            return;
        }
        let loaded = (0..self.messages_model.row_count())
            .filter_map(|i| self.messages_model.row_data(i))
            .find(|row| row.id == id)
            .map(|row| {
                let visual = matches!(m.kind, MessageKind::Image | MessageKind::Video);
                (visual
                    && row.mediaReady
                    && (row.picture.size().width > 0 || m.kind == MessageKind::Video))
                    || (!visual && m.kind != MessageKind::Text && row.mediaReady)
                    || (m.kind == MessageKind::Text && row.hasLinkThumb)
            })
            .unwrap_or(true);
        if !loaded {
            self.request_automatic_media(&m);
        }
    }

    pub(super) fn schedule_row_gc(&mut self) {
        if self.row_gc_queued {
            return;
        }
        self.row_gc_queued = true;
        self.once(500, |b| {
            b.row_gc_queued = false;
            b.gc_row_images();
        });
    }

    // Rows that have not reported a position since the last two scroll
    // steps are off screen: their bitmaps go, the row keeps its size.
    pub(super) fn gc_row_images(&mut self) {
        let epoch = self.geom_epoch;
        let mut freed = 0usize;
        for i in 0..self.messages_model.row_count() {
            let Some(mut row) = self.messages_model.row_data(i) else {
                continue;
            };
            let near = self
                .row_geom
                .get(row.id.as_str())
                .is_some_and(|(_, _, e)| epoch.wrapping_sub(*e) <= 2);
            if near {
                continue;
            }
            let has_pic = row.picture.size().width > 0;
            if !has_pic && !row.hasLinkThumb {
                continue;
            }
            let id = row.id.to_string();
            if has_pic {
                freed += image_bytes(&row.picture);
                row.picture = empty_image();
                // Stickers and GIFs animate from their own caches; a photo
                // or poster is simply asked for again when it returns.
                if !row.sticker {
                    row.mediaReady = false;
                }
                self.drop_animation(&id);
            }
            if row.hasLinkThumb {
                freed += image_bytes(&row.linkThumb);
                row.linkThumb = empty_image();
                row.hasLinkThumb = false;
            }
            self.messages_model.set_row_data(i, row);
            if let Some((old, _, _)) = self.decoded.remove(&id) {
                self.decoded_bytes = self.decoded_bytes.saturating_sub(image_bytes(&old));
                self.decoded_order.retain(|k| k != &id);
            }
        }
        if freed > 4 * 1024 * 1024 {
            println!(
                "[media] released {} MB of off-screen pictures",
                freed / (1024 * 1024)
            );
        }
    }

    pub(super) fn request_automatic_media(&mut self, m: &StoredMessage) {
        use whatsapp_rust::proto_helpers::MessageExt as _;
        if !m.deleted
            && matches!(m.kind, MessageKind::Image | MessageKind::Video)
            && !self.decoded.contains_key(&m.id)
            && self.preview_requested.insert(m.id.clone())
        {
            let bytes = m.raw.as_ref().and_then(|raw| {
                let inner = raw.get_base_message();
                inner
                    .image_message
                    .as_option()
                    .and_then(|v| v.jpeg_thumbnail.clone())
                    .or_else(|| {
                        inner
                            .video_message
                            .as_option()
                            .and_then(|v| v.jpeg_thumbnail.clone())
                    })
            });
            if let Some(bytes) = bytes.filter(|bytes| !bytes.is_empty()) {
                self.wa.send(Cmd::DecodePreview {
                    id: m.id.clone(),
                    bytes,
                });
            }
        }
        let voice = m.raw.as_ref().is_some_and(|raw| {
            raw.get_base_message()
                .audio_message
                .as_option()
                .is_some_and(|a| a.ptt == Some(true))
        });
        let enabled = if m.sticker {
            self.ui.get_auto_download_stickers()
        } else {
            match m.kind {
                MessageKind::Image => self.ui.get_auto_download_photos(),
                MessageKind::Audio => self.ui.get_auto_download_audio(),
                MessageKind::Video => self.ui.get_auto_download_video(),
                MessageKind::Doc => self.ui.get_auto_download_documents(),
                _ => false,
            }
        };
        let cached = m
            .raw
            .as_ref()
            .is_some_and(|raw| crate::media::cached_path(&m.id, &m.mimetype, raw).1);
        if enabled || voice || cached || m.kind == MessageKind::Text {
            self.request_media(m);
        }
    }

    pub(super) fn request_manual_media(&mut self, id: &str) {
        let message = self.current_jid.as_ref().and_then(|jid| {
            self.store
                .messages_for(jid)
                .iter()
                .find(|m| m.id == id)
                .cloned()
        });
        if let Some(message) = message {
            self.media_failed.remove(id);
            self.request_media(&message);
            let loading = self.media_inflight.contains(id);
            self.patch_row(id, |row| row.mediaLoading = loading);
        }
    }

    pub(super) fn request_media(&mut self, m: &StoredMessage) {
        if m.deleted || self.media_inflight.contains(&m.id) || self.media_failed.contains(&m.id) {
            return;
        }
        let raw = match m.raw.clone() {
            Some(raw) => raw,
            None if m.kind == MessageKind::Text => std::sync::Arc::new(wa::Message::default()),
            None => return,
        };
        use whatsapp_rust::proto_helpers::MessageExt as _;
        let inner = raw.get_base_message();
        match m.kind {
            MessageKind::System => {}
            MessageKind::Text => {
                let ext = inner
                    .extended_text_message
                    .as_option()
                    .cloned()
                    .unwrap_or_default();
                let inline = ext
                    .jpeg_thumbnail
                    .clone()
                    .filter(|t| !t.is_empty())
                    .unwrap_or_default();
                // The card's real picture lives on the CDN; the inline
                // jpeg is a placeholder-sized copy, kept as the fallback.
                let hi_res = ext
                    .thumbnail_direct_path
                    .clone()
                    .zip(ext.media_key.clone())
                    .zip(
                        ext.thumbnail_sha256
                            .clone()
                            .zip(ext.thumbnail_enc_sha256.clone()),
                    );
                if let Some(((direct_path, media_key), (sha256, enc_sha256))) = hi_res
                    && !direct_path.is_empty()
                    && !media_key.is_empty()
                {
                    self.media_inflight.insert(m.id.clone());
                    self.wa.send(Cmd::LinkThumb {
                        id: m.id.clone(),
                        direct_path,
                        media_key,
                        sha256,
                        enc_sha256,
                        fallback: inline,
                    });
                } else if !inline.is_empty() {
                    self.media_inflight.insert(m.id.clone());
                    self.wa.send(Cmd::DecodeThumb {
                        id: m.id.clone(),
                        bytes: inline,
                        link: true,
                    });
                } else {
                    let url = message_link(m);
                    if !url.is_empty() {
                        self.media_inflight.insert(m.id.clone());
                        self.wa.send(Cmd::LinkPreview {
                            jid: m.jid.clone(),
                            id: m.id.clone(),
                            text: m.text.clone(),
                            url,
                        });
                    }
                }
                return;
            }
            MessageKind::Image => {
                // A sticker seen before (in any chat) is served from the
                // frame cache: no download, no decode.
                if m.sticker
                    && let Some(key) = sticker_key(m)
                    && let Some(frames) = self.stickers.get(&key).cloned()
                {
                    self.touch_sticker(&key);
                    self.show_sticker_frames(&m.id, &frames);
                    return;
                }
                self.media_inflight.insert(m.id.clone());
                self.wa.send(Cmd::Media {
                    id: m.id.clone(),
                    mimetype: m.mimetype.clone(),
                    message: raw,
                    want: if m.sticker {
                        MediaWant::Sticker
                    } else {
                        MediaWant::Image
                    },
                });
            }
            MessageKind::Video => {
                self.media_inflight.insert(m.id.clone());
                if m.gif {
                    self.wa.send(Cmd::Media {
                        id: m.id.clone(),
                        mimetype: m.mimetype.clone(),
                        message: raw,
                        want: MediaWant::Gif,
                    });
                    return;
                }
                // Poster from the embedded thumbnail; the clip itself is
                // cached for the click-to-play path.
                if let Some(thumb) = inner
                    .video_message
                    .as_option()
                    .and_then(|v| v.jpeg_thumbnail.as_ref())
                    .filter(|t| !t.is_empty())
                {
                    self.wa.send(Cmd::DecodeThumb {
                        id: m.id.clone(),
                        bytes: thumb.to_vec(),
                        link: false,
                    });
                }
                self.wa.send(Cmd::Media {
                    id: m.id.clone(),
                    mimetype: m.mimetype.clone(),
                    message: raw,
                    want: MediaWant::Video,
                });
            }
            MessageKind::Audio | MessageKind::Doc => {
                self.media_inflight.insert(m.id.clone());
                self.wa.send(Cmd::Media {
                    id: m.id.clone(),
                    mimetype: m.mimetype.clone(),
                    message: raw,
                    want: MediaWant::File,
                });
            }
        }
        let loading = self.media_inflight.contains(&m.id);
        self.patch_row(&m.id, |row| row.mediaLoading = loading);
    }

    pub(super) fn remember_decoded(&mut self, id: &str, image: slint::Image, w: i32, h: i32) {
        // Re-decoding the same message replaces its entry, so drop the old
        // one from the running total first.
        if let Some((old, _, _)) = self.decoded.remove(id) {
            self.decoded_bytes = self.decoded_bytes.saturating_sub(image_bytes(&old));
            self.decoded_order.retain(|known| known != id);
        }
        self.decoded_bytes += image_bytes(&image);
        self.decoded_order.push_back(id.to_string());
        self.decoded.insert(id.to_string(), (image, w, h));
        self.refresh_message_details();
        // Evicting is cheap: the rows already on screen hold their own
        // reference to the image, so this only costs a re-decode the next
        // time the conversation is rebuilt.
        while self.decoded_order.len() > DECODE_CACHE_MAX
            || (self.decoded_bytes > DECODE_CACHE_BYTES && self.decoded_order.len() > 1)
        {
            let Some(evicted) = self.decoded_order.pop_front() else {
                break;
            };
            if let Some((old, _, _)) = self.decoded.remove(&evicted) {
                self.decoded_bytes = self.decoded_bytes.saturating_sub(image_bytes(&old));
            }
        }
    }

    // A late-loading image must not push the newest message out of view;
    // debounced so a burst of images re-anchors only once.
    pub(super) fn restick(&mut self) {
        if !self.ui.get_stick_bottom() || self.stick_requeued {
            return;
        }
        self.stick_requeued = true;
        self.once(120, |b| {
            b.stick_requeued = false;
            if b.ui.get_stick_bottom() {
                b.ui.invoke_scroll_conversation_end();
            }
        });
    }

    pub fn on_media_preview(&mut self, id: &str, img: Option<crate::media::Decoded>) {
        let Some(img) = img else { return };
        if self.decoded.contains_key(id) {
            return;
        }
        let image = image_of(&img);
        self.media_previews.insert(id.to_string(), image.clone());
        self.patch_row(id, |row| row.picture = image.clone());
    }

    pub fn on_media_missing(&mut self, id: &str) {
        self.media_inflight.remove(id);
        self.patch_row(id, |row| row.mediaLoading = false);
        self.media_failed.insert(id.to_string());
    }

    pub fn on_media_file(&mut self, id: &str, path: &std::path::Path) {
        self.media_inflight.remove(id);
        self.patch_row(id, |row| row.mediaLoading = false);
        let path_str = path.to_string_lossy().into_owned();
        self.media_path.insert(id.to_string(), path_str.clone());
        self.patch_row(id, |row| {
            row.mediaPath = path_str.into();
            row.mediaReady = true;
        });
        // Voice notes draw their amplitude bars once the file is here.
        let is_audio = self
            .current_jid
            .as_ref()
            .and_then(|jid| self.store.messages_for(jid).iter().find(|m| m.id == id))
            .map(|m| m.kind == MessageKind::Audio)
            .unwrap_or(false);
        if is_audio && !self.waves.contains_key(id) {
            self.wa.send(Cmd::Waveform {
                id: id.to_string(),
                path: path.to_path_buf(),
            });
        }
        // A video status waiting on its clip gets the poster frame now.
        if let Some((items, idx)) = &self.viewer
            && items[*idx].id == id
        {
            self.ui.set_sv_video_ready(true);
            self.wa.send(Cmd::StatusPoster {
                id: id.to_string(),
                path: path.to_path_buf(),
            });
        }
        self.restick();
    }

    pub fn on_media_image(&mut self, id: &str, path: &std::path::Path, img: crate::media::Decoded) {
        self.media_inflight.remove(id);
        self.patch_row(id, |row| row.mediaLoading = false);
        let image = image_of(&img);
        let (pic_w, pic_h) = bubble_fit(img.w as i32, img.h as i32);
        self.remember_decoded(id, image.clone(), pic_w, pic_h);
        let path_str = path.to_string_lossy().into_owned();
        self.media_path.insert(id.to_string(), path_str.clone());
        self.patch_row(id, |row| {
            row.picture = image.clone();
            row.picW = pic_w;
            row.picH = pic_h;
            row.mediaPath = path_str.into();
            row.mediaReady = true;
        });
        self.feed_viewer(id, &image);
        self.restick();
    }

    pub fn on_media_sticker(
        &mut self,
        id: &str,
        path: &std::path::Path,
        frames: Vec<crate::media::Decoded>,
    ) {
        self.media_inflight.remove(id);
        self.patch_row(id, |row| row.mediaLoading = false);
        if frames.is_empty() {
            self.on_media_missing(id);
            return;
        }
        let images: Rc<Vec<slint::Image>> = Rc::new(frames.iter().map(image_of).collect());
        let path_str = path.to_string_lossy().into_owned();
        self.media_path.insert(id.to_string(), path_str);
        // Cached under the content hash, so the same sticker in another
        // chat (or the picker) never touches the network again.
        let key = self
            .current_jid
            .as_ref()
            .and_then(|jid| self.store.messages_for(jid).iter().find(|m| m.id == id))
            .and_then(sticker_key)
            .or_else(|| {
                self.store
                    .messages
                    .values()
                    .flatten()
                    .find(|m| m.id == id)
                    .and_then(sticker_key)
            });
        if let Some(key) = key {
            self.remember_sticker(&key, images.clone());
        }
        self.show_sticker_frames(id, &images);
        self.restick();
    }

    // Puts cached frames on a message's row (and the picker cell) and
    // starts the animation when there is more than one.
    pub(super) fn show_sticker_frames(&mut self, id: &str, frames: &Rc<Vec<slint::Image>>) {
        let Some(first) = frames.first().cloned() else {
            return;
        };
        let size = first.size();
        let (w, h) = (size.width as i32, size.height as i32);
        self.remember_decoded(id, first.clone(), w, h);
        let ready_path = self.media_path.get(id).cloned().unwrap_or_default();
        self.patch_sticker_cells(id, &first);
        self.patch_row(id, |row| {
            row.picture = first;
            row.picW = w;
            row.picH = h;
            row.mediaPath = ready_path.into();
            row.mediaReady = true;
        });
        if frames.len() > 1 {
            self.start_animation(id, frames.as_ref().clone(), true);
        }
    }

    pub(super) fn remember_sticker(&mut self, key: &str, frames: Rc<Vec<slint::Image>>) {
        if let Some(old) = self.stickers.remove(key) {
            self.sticker_bytes = self
                .sticker_bytes
                .saturating_sub(old.iter().map(image_bytes).sum::<usize>());
            self.sticker_order.retain(|k| k != key);
        }
        self.sticker_bytes += frames.iter().map(image_bytes).sum::<usize>();
        self.sticker_order.push_back(key.to_string());
        self.stickers.insert(key.to_string(), frames);
        while self.sticker_order.len() > STICKER_CACHE_MAX
            || (self.sticker_bytes > STICKER_CACHE_BYTES && self.sticker_order.len() > 1)
        {
            let Some(evicted) = self.sticker_order.pop_front() else {
                break;
            };
            if let Some(old) = self.stickers.remove(&evicted) {
                self.sticker_bytes = self
                    .sticker_bytes
                    .saturating_sub(old.iter().map(image_bytes).sum::<usize>());
            }
        }
    }

    // Marks a cached sticker as recently used.
    pub(super) fn touch_sticker(&mut self, key: &str) {
        self.sticker_order.retain(|k| k != key);
        self.sticker_order.push_back(key.to_string());
    }

    // After a conversation is (re)built, stickers already in the cache
    // show at once instead of waiting for the media pipeline.
    pub(super) fn apply_cached_stickers(&mut self, jid: &str) {
        let hits: Vec<(String, String)> = self
            .store
            .messages_for(jid)
            .iter()
            .filter(|m| m.sticker && !m.deleted)
            .filter_map(|m| sticker_key(m).map(|k| (m.id.clone(), k)))
            .filter(|(_, k)| self.stickers.contains_key(k))
            .collect();
        for (id, key) in hits {
            if let Some(frames) = self.stickers.get(&key).cloned() {
                self.touch_sticker(&key);
                self.show_sticker_frames(&id, &frames);
            }
        }
    }

    pub fn on_media_gif(
        &mut self,
        id: &str,
        path: &std::path::Path,
        frames: Vec<crate::media::Decoded>,
    ) {
        self.media_inflight.remove(id);
        self.patch_row(id, |row| row.mediaLoading = false);
        if frames.is_empty() {
            self.on_media_missing(id);
            return;
        }
        let images: Vec<slint::Image> = frames.iter().map(image_of).collect();
        let (pic_w, pic_h) = bubble_fit(frames[0].w as i32, frames[0].h as i32);
        self.remember_decoded(id, images[0].clone(), pic_w, pic_h);
        let path_str = path.to_string_lossy().into_owned();
        self.media_path.insert(id.to_string(), path_str.clone());
        let first = images[0].clone();
        self.patch_row(id, |row| {
            row.picture = first;
            row.picW = pic_w;
            row.picH = pic_h;
            row.mediaPath = path_str.into();
            row.mediaReady = true;
        });
        if images.len() > 1 {
            // Like WhatsApp, a GIF plays once and rests on its first frame.
            self.start_animation(id, images, false);
        }
        self.restick();
    }

    // The clip's own first picture, replacing the blurry thumbnail.
    pub fn on_video_poster(&mut self, id: &str, img: crate::media::Decoded) {
        let image = image_of(&img);
        let (pic_w, pic_h) = poster_box(img.w as i32, img.h as i32);
        self.remember_decoded(id, image.clone(), pic_w, pic_h);
        self.patch_row(id, |row| {
            row.picture = image.clone();
            row.picW = pic_w;
            row.picH = pic_h;
        });
        self.restick();
    }

    pub fn on_link_preview(
        &mut self,
        jid: &str,
        id: &str,
        text: &str,
        preview: Option<crate::link_preview::Preview>,
    ) {
        let jid = self.store.canon_owned(jid);
        let Some(m) = self
            .store
            .messages
            .get_mut(&jid)
            .and_then(|messages| messages.iter_mut().find(|m| m.id == id))
        else {
            self.media_inflight.remove(id);
            return;
        };
        if m.deleted || m.kind != MessageKind::Text || m.text != text {
            return;
        }
        self.media_inflight.remove(id);
        let Some(preview) = preview else {
            self.on_media_missing(id);
            return;
        };
        if m.link_title.is_empty() {
            m.link_title = preview.title;
        }
        if m.link_desc.is_empty() {
            m.link_desc = preview.description;
        }
        if m.link_url.is_empty() {
            m.link_url = crate::link_preview::first_url(text);
        }
        let (title, desc, url) = (
            m.link_title.clone(),
            m.link_desc.clone(),
            m.link_url.clone(),
        );
        self.store.dirty_jids.insert(jid);
        self.schedule_save();
        let image = preview.image.as_ref().map(image_of);
        if image.is_none() {
            self.media_failed.insert(id.to_owned());
        }
        self.patch_row(id, |row| {
            row.linkTitle = title.into();
            row.linkDesc = desc.into();
            row.linkHost = host_of(&url).into();
            row.linkUrl = url.into();
            row.hasLink = true;
            row.mediaLoading = false;
            row.hasLinkOg = preview.og;
            if let (Some(image), Some(decoded)) = (image, preview.image) {
                row.linkThumb = image;
                row.hasLinkThumb = true;
                row.linkThumbW = decoded.w as i32;
                row.linkThumbH = decoded.h as i32;
            }
        });
        self.restick();
    }

    pub fn on_thumb(&mut self, id: &str, link: bool, img: crate::media::Decoded) {
        let image = image_of(&img);
        let (w, h) = (img.w as i32, img.h as i32);
        if link {
            let valid = self.find_message(id).is_some_and(|m| {
                !m.deleted
                    && m.kind == MessageKind::Text
                    && m.raw.as_ref().is_some_and(|raw| {
                        raw.get_base_message()
                            .extended_text_message
                            .as_option()
                            .is_some_and(|ext| {
                                ext.jpeg_thumbnail
                                    .as_ref()
                                    .is_some_and(|bytes| !bytes.is_empty())
                                    || ext
                                        .thumbnail_direct_path
                                        .as_ref()
                                        .is_some_and(|path| !path.is_empty())
                            })
                    })
            });
            if !valid {
                return;
            }
            self.media_inflight.remove(id);
            self.patch_row(id, |row| row.mediaLoading = false);
            self.patch_row(id, |row| {
                row.linkThumb = image;
                row.hasLinkThumb = true;
                row.hasLinkOg = true;
                row.linkThumbW = w;
                row.linkThumbH = h;
            });
        } else {
            // Video poster. The embedded thumbnail is around a hundred
            // pixels wide, and sizing the bubble to it is what made video
            // previews tiny: the box comes from the clip's own size, and
            // the thumbnail only lends its shape when that is unknown. A
            // sharp poster replaces it once the clip is here.
            if self.decoded.contains_key(id) {
                return;
            }
            let sized = self
                .find_message(id)
                .is_some_and(|m| m.media_w > 0 && m.media_h > 0);
            let (pic_w, pic_h) = poster_box(w, h);
            self.patch_row(id, |row| {
                row.picture = image.clone();
                if !sized {
                    row.picW = pic_w;
                    row.picH = pic_h;
                }
            });
            self.feed_viewer(id, &image);
        }
        self.restick();
    }

    pub(super) fn start_animation(&mut self, id: &str, frames: Vec<slint::Image>, looping: bool) {
        self.drop_animation(id);
        self.anim_bytes += frames.iter().map(image_bytes).sum::<usize>();
        self.anim_order.push_back(id.to_string());
        self.animated.insert(
            id.to_string(),
            Anim {
                frames,
                idx: 0,
                looping,
            },
        );
        // A 320px GIF can be 45 frames of RGBA, so the count cap needs a
        // byte budget beside it.
        while self.anim_order.len() > MAX_ANIMATIONS
            || (self.anim_bytes > ANIM_CACHE_BYTES && self.anim_order.len() > 1)
        {
            let Some(oldest) = self.anim_order.pop_front() else {
                break;
            };
            self.forget_animation(&oldest);
        }
        self.start_animation_timer();
    }

    // Runs only while something animates and the window is visible.
    pub(super) fn start_animation_timer(&mut self) {
        if self.window_idle || self.anim_timer.running() {
            return;
        }
        self.anim_timer.start(
            slint::TimerMode::Repeated,
            Duration::from_millis(66),
            || apply_now(|b| b.tick_animations()),
        );
    }

    // Removes an animation and its bytes, keeping the order queue in step.
    pub(super) fn drop_animation(&mut self, id: &str) {
        if self.forget_animation(id) {
            self.anim_order.retain(|known| known != id);
        }
    }

    pub(super) fn forget_animation(&mut self, id: &str) -> bool {
        let Some(anim) = self.animated.remove(id) else {
            return false;
        };
        self.anim_bytes = self
            .anim_bytes
            .saturating_sub(anim.frames.iter().map(image_bytes).sum::<usize>());
        true
    }

    pub(super) fn tick_animations(&mut self) {
        if self.animated.is_empty() {
            self.anim_timer.stop();
            return;
        }
        let mut finished = Vec::new();
        let mut patches: Vec<(String, slint::Image)> = Vec::new();
        for (id, anim) in self.animated.iter_mut() {
            if anim.looping {
                anim.idx = (anim.idx + 1) % anim.frames.len();
            } else if anim.idx + 1 < anim.frames.len() {
                anim.idx += 1;
            } else {
                finished.push(id.clone());
                patches.push((id.clone(), anim.frames[0].clone()));
                continue;
            }
            patches.push((id.clone(), anim.frames[anim.idx].clone()));
        }
        for id in finished {
            self.drop_animation(&id);
        }
        for (id, frame) in patches {
            self.patch_row(&id, |row| row.picture = frame);
        }
    }

    // Group senders and voice notes want their pictures; collected after
    // the rows are built (to_row itself is read-only).
    pub(super) fn queue_row_avatars(&mut self, jid: &str) {
        let mut wanted: Vec<String> = Vec::new();
        for m in self.store.messages_for(jid) {
            if is_group(jid) && !m.from_me && !m.sender_jid.is_empty() {
                wanted.push(self.store.canon_owned(&m.sender_jid));
            }
            if m.kind == MessageKind::Audio {
                wanted.push(if m.from_me {
                    self.self_jid.clone()
                } else if m.sender_jid.is_empty() {
                    jid.to_string()
                } else {
                    self.store.canon_owned(&m.sender_jid)
                });
            }
        }
        wanted.sort();
        wanted.dedup();
        for jid in wanted {
            self.queue_avatar(&jid);
        }
    }
}

#[cfg(test)]
mod lru_tests {
    use super::*;

    #[test]
    fn lru_evicts_the_oldest_and_refreshes_on_touch() {
        let mut order = std::collections::VecDeque::new();
        assert!(touch_lru(&mut order, "a", 3).is_empty());
        assert!(touch_lru(&mut order, "b", 3).is_empty());
        assert!(touch_lru(&mut order, "c", 3).is_empty());
        assert!(touch_lru(&mut order, "a", 3).is_empty());
        assert_eq!(touch_lru(&mut order, "d", 3), vec!["b".to_string()]);
        assert_eq!(
            order.iter().cloned().collect::<Vec<_>>(),
            vec!["c", "a", "d"]
        );
    }

    #[test]
    fn lru_never_exceeds_the_cap_and_has_no_duplicates() {
        let mut order = std::collections::VecDeque::new();
        for i in 0..1000 {
            touch_lru(&mut order, &format!("k{}", i % 400), AVATAR_CACHE_MAX);
            assert!(order.len() <= AVATAR_CACHE_MAX);
        }
        let unique: std::collections::HashSet<_> = order.iter().collect();
        assert_eq!(unique.len(), order.len());
    }

    #[test]
    fn byte_budget_is_a_hard_ceiling_below_the_old_worst_case() {
        assert!(DECODE_CACHE_BYTES <= 64 * 1024 * 1024);
        assert!(DECODE_CACHE_BYTES + ANIM_CACHE_BYTES < 330 * 1024 * 1024);
    }
}
