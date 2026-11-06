// Sticker and GIF pickers and the sticker cache.
use super::*;

pub(super) const STICKER_CACHE_MAX: usize = 64;

pub(super) const STICKER_CACHE_BYTES: usize = 24 * 1024 * 1024;

impl Bridge {
    // ---- sticker & GIF pickers ----

    pub(super) fn sticker_grid(&self, items: Vec<&StoredMessage>) -> Vec<ModelRc<StickerCell>> {
        let cells: Vec<StickerCell> = items
            .iter()
            .map(|m| {
                let cached = sticker_key(m)
                    .and_then(|k| self.stickers.get(&k))
                    .and_then(|frames| frames.first().cloned())
                    .or_else(|| self.decoded.get(&m.id).map(|(img, _, _)| img.clone()));
                StickerCell {
                    id: m.id.clone().into(),
                    ready: cached.is_some(),
                    pic: cached.unwrap_or_else(empty_image),
                }
            })
            .collect();
        cells
            .chunks(4)
            .map(|c| ModelRc::from(Rc::new(VecModel::from(c.to_vec()))))
            .collect()
    }

    // A sticker's picker cell is filled from the frame cache the moment
    // its frames land.
    pub(super) fn patch_sticker_cells(&self, id: &str, image: &slint::Image) {
        for model in [&self.sticker_model, &self.fav_model] {
            for r in 0..model.row_count() {
                let Some(row) = model.row_data(r) else {
                    continue;
                };
                for c in 0..row.row_count() {
                    if let Some(mut cell) = row.row_data(c)
                        && cell.id == id
                        && !cell.ready
                    {
                        cell.pic = image.clone();
                        cell.ready = true;
                        row.set_row_data(c, cell);
                    }
                }
            }
        }
    }

    // Fills the sticker tab with recent/starred stickers (lazy decode).
    pub(super) fn load_sticker_panel(&mut self) {
        // Sticker/GIF history scans hydrated chats only; wake the most
        // recent ones without disturbing the warm LRU.
        if let Some(vault) = self.vault.as_ref() {
            let recent: Vec<String> = {
                let sorted = self.store.sorted_chats();
                sorted.iter().take(15).map(|m| m.jid.clone()).collect()
            };
            for jid in recent {
                self.store.hydrate(vault, &jid);
            }
        }
        crate::jobs::gif_search(String::new());
        let favs = self.store.starred_stickers(32);
        self.ui.set_fav_hint(if favs.is_empty() {
            t("fav.empty").into()
        } else {
            "".into()
        });
        let fav_pending: Vec<StoredMessage> = favs
            .iter()
            .filter(|m| !self.decoded.contains_key(&m.id))
            .map(|m| (*m).clone())
            .collect();
        let fav_grid = self.sticker_grid(favs);
        self.fav_model.set_vec(fav_grid);
        let recents = self.store.recent_stickers(32);
        let recent_pending: Vec<StoredMessage> = recents
            .iter()
            .filter(|m| !self.decoded.contains_key(&m.id))
            .map(|m| (*m).clone())
            .collect();
        let recent_grid = self.sticker_grid(recents);
        self.sticker_model.set_vec(recent_grid);
        // Cached stickers fill their cells right away; the rest download.
        for m in fav_pending.into_iter().chain(recent_pending) {
            if let Some(frames) = sticker_key(&m).and_then(|k| self.stickers.get(&k)).cloned()
                && let Some(first) = frames.first()
            {
                self.patch_sticker_cells(&m.id, first);
                continue;
            }
            self.request_automatic_media(&m);
        }
    }

    pub(super) fn send_sticker_by_id(&mut self, id: &str) {
        let Some(jid) = self.current_jid.clone() else {
            return;
        };
        // Panel stickers are past messages: reusing their CDN copy skips a
        // re-upload, but this is a fresh send, not a forward (WhatsApp
        // itself never tags a re-sent sticker as forwarded).
        let raw = self
            .store
            .messages
            .values()
            .flatten()
            .find(|m| m.id == id && m.sticker)
            .and_then(|m| m.raw.clone());
        if let Some(message) = raw {
            self.ui.set_picker_open(false);
            self.wa.send(Cmd::ResendMedia { jid, message });
        }
    }

    pub fn on_gif_results(&mut self, results: Vec<(String, String, crate::media::Decoded)>) {
        self.gif_url_by_id.clear();
        if results.is_empty() {
            self.ui.set_gif_hint(t("gif.no_results").into());
            self.gif_model.set_vec(Vec::new());
            return;
        }
        let cells: Vec<StickerCell> = results
            .iter()
            .map(|(id, url, img)| {
                self.gif_url_by_id.insert(id.clone(), url.clone());
                StickerCell {
                    id: id.clone().into(),
                    pic: image_of(img),
                    ready: true,
                }
            })
            .collect();
        let grid: Vec<ModelRc<StickerCell>> = cells
            .chunks(4)
            .map(|c| ModelRc::from(Rc::new(VecModel::from(c.to_vec()))))
            .collect();
        self.gif_model.set_vec(grid);
        self.ui.set_gif_hint("".into());
    }

    pub(super) fn send_gif_by_id(&mut self, id: &str) {
        let Some(jid) = self.current_jid.clone() else {
            return;
        };
        self.ui.set_picker_open(false);
        if let Some(url) = self.gif_url_by_id.get(id).cloned() {
            self.wa.send(Cmd::SendGifUrl { jid, url });
            return;
        }
        // A history GIF: resend the original message's CDN copy, not a
        // forward (see send_sticker_by_id).
        let raw = self
            .store
            .messages
            .values()
            .flatten()
            .find(|m| m.id == id && m.gif)
            .and_then(|m| m.raw.clone());
        if let Some(message) = raw {
            self.wa.send(Cmd::ResendMedia { jid, message });
        }
    }
}
