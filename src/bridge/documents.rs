// Documents and the PDF viewer.
use super::*;

impl Bridge {
    // ---- documents and the PDF viewer ----

    // Decrypts a downloaded document off the UI thread, then runs `done` with
    // the message and its copy under the real file name. Nothing runs when
    // the document is not here.
    pub(super) fn plain_document(
        &self,
        id: &str,
        done: impl FnOnce(&mut Bridge, StoredMessage, std::path::PathBuf) + Send + 'static,
    ) {
        let Some(m) = self.find_message(id).cloned() else {
            return;
        };
        let Some(cached) = self.media_path.get(id).map(std::path::PathBuf::from) else {
            return;
        };
        let name = sanitize_file_name(&m.text);
        crate::jobs::plain_copy(cached, Some(name), move |b, path| {
            if let Some(path) = path {
                done(b, m, path);
            }
        });
    }

    pub(super) fn open_document(&mut self, id: &str) {
        if !self.media_path.contains_key(id) {
            self.request_manual_media(id);
            return;
        }
        let id = id.to_string();
        self.plain_document(&id.clone(), move |b, m, path| {
            if crate::pdf::is_pdf(&m.mimetype, &m.text) {
                b.open_pdf(&id, &m.text, path);
            } else {
                crate::platform::open_path(&path.to_string_lossy());
            }
        });
    }

    pub(super) fn save_document(&mut self, id: &str) {
        self.plain_document(id, |_, m, path| {
            let name = sanitize_file_name(&m.text);
            std::thread::spawn(move || {
                let picked = rfd::FileDialog::new().set_file_name(&name).save_file();
                if let Some(target) = picked
                    && let Err(e) = std::fs::copy(&path, &target)
                {
                    eprintln!("[doc] save failed: {e}");
                }
            });
        });
    }

    pub(super) fn open_pdf(&mut self, id: &str, title: &str, path: std::path::PathBuf) {
        self.pdf_gen += 1;
        self.pdf_id = id.to_string();
        self.pdf_path = Some(path.clone());
        self.pdf_sizes.clear();
        self.pdf_rendered.clear();
        self.pdf_pending.clear();
        self.pdf_zoom = 1.0;
        self.pdf_model.set_vec(Vec::new());
        self.ui.set_pdf_title(title.into());
        self.ui.set_pdf_page(0);
        self.ui.set_pdf_count(0);
        self.ui.set_pdf_total_h(0.0);
        self.ui.set_pdf_total_w(0.0);
        self.ui.set_pdf_engine_missing(false);
        self.ui.set_pdf_installing(false);
        self.ui.set_pdf_status(t("pdf.opening").into());
        self.ui.set_pdf_zoom_label("100%".into());
        self.ui.set_pdf_open(true);
        self.pdf.send(crate::pdf::PdfReq::Open {
            path,
            generation: self.pdf_gen,
        });
    }

    pub(super) fn close_pdf(&mut self) {
        self.ui.set_pdf_open(false);
        self.pdf_gen += 1;
        self.pdf_model.set_vec(Vec::new());
        self.pdf_sizes.clear();
        self.pdf_rendered.clear();
        self.pdf_pending.clear();
        self.pdf.send(crate::pdf::PdfReq::Close);
    }

    pub fn on_pdf_engine_missing(&mut self, generation: u64) {
        if generation != self.pdf_gen {
            return;
        }
        self.ui.set_pdf_engine_missing(true);
        self.ui.set_pdf_status(t("pdf.engine_missing").into());
    }

    pub fn on_pdf_engine_installed(&mut self, result: Result<(), String>) {
        self.ui.set_pdf_installing(false);
        match result {
            Ok(()) => {
                self.pdf.send(crate::pdf::PdfReq::Rebind);
                self.ui.set_pdf_engine_missing(false);
                if let (Some(path), false) = (self.pdf_path.clone(), self.pdf_id.is_empty()) {
                    let (id, title) = (self.pdf_id.clone(), self.ui.get_pdf_title().to_string());
                    self.open_pdf(&id, &title, path);
                }
            }
            Err(e) => self
                .ui
                .set_pdf_status(ta("pdf.install_failed", &[&e]).into()),
        }
    }

    pub fn on_pdf_failed(&mut self, generation: u64, reason: &str) {
        if generation != self.pdf_gen {
            return;
        }
        self.ui.set_pdf_status(ta("pdf.failed", &[reason]).into());
    }

    pub fn on_pdf_opened(&mut self, generation: u64, sizes: Vec<(f32, f32)>) {
        if generation != self.pdf_gen {
            return;
        }
        self.ui.set_pdf_status("".into());
        self.ui.set_pdf_count(sizes.len() as i32);
        self.pdf_sizes = sizes;
        self.pdf_layout(false);
        self.ui.invoke_pdf_set_scroll(0.0);
        self.pdf_scrolled(0.0);
    }

    // Lays the pages out top to bottom at the current zoom (1.0 = page
    // width fills the viewer) and asks for whatever is on screen.
    pub(super) fn pdf_layout(&mut self, keep_scroll: bool) {
        if self.pdf_sizes.is_empty() {
            return;
        }
        const MARGIN: f32 = 24.0;
        const GAP: f32 = 16.0;
        let view_w = self.ui.get_pdf_view_w().max(200.0);
        let old_page_w = self.pdf_page_w;
        let page_w = ((view_w - 2.0 * MARGIN) * self.pdf_zoom).max(120.0);
        self.pdf_page_w = page_w;
        let mut y = GAP;
        let mut rows: Vec<PdfPage> = Vec::with_capacity(self.pdf_sizes.len());
        for (i, (pw, ph)) in self.pdf_sizes.iter().enumerate() {
            let h = page_w * (ph / pw.max(1.0));
            let old = self.pdf_model.row_data(i);
            let rendered_w = self.pdf_rendered.get(&i).copied().unwrap_or(0);
            let keep = old.as_ref().filter(|_| rendered_w > 0);
            rows.push(PdfPage {
                index: i as i32,
                y,
                w: page_w,
                h,
                pic: keep.map(|o| o.pic.clone()).unwrap_or_else(empty_image),
                ready: keep.is_some_and(|o| o.ready),
            });
            y += h + GAP;
        }
        self.ui.set_pdf_total_h(y);
        self.ui.set_pdf_total_w((page_w + 2.0 * MARGIN).max(view_w));
        self.pdf_model.set_vec(rows);
        self.ui
            .set_pdf_zoom_label(format!("{}%", (self.pdf_zoom * 100.0).round() as i32).into());
        if keep_scroll && old_page_w > 0.0 {
            let ratio = page_w / old_page_w;
            let scroll = self.ui.get_pdf_scroll() * ratio;
            self.ui.invoke_pdf_set_scroll(scroll);
            self.pdf_scrolled(scroll);
        }
    }

    // Renders the pages around the viewport at the exact width they are
    // drawn at, and lets pages far away go.
    pub(super) fn pdf_scrolled(&mut self, scroll: f32) {
        if self.pdf_sizes.is_empty() {
            return;
        }
        let view_h = self.ui.get_pdf_view_h().max(200.0);
        let (top, bottom) = (scroll - view_h * 0.5, scroll + view_h * 1.5);
        let width = self.pdf_page_w.round() as u32;
        let mut current = 0;
        for i in 0..self.pdf_model.row_count() {
            let Some(row) = self.pdf_model.row_data(i) else {
                continue;
            };
            if row.y <= scroll + view_h * 0.35 {
                current = i;
            }
            let visible = row.y + row.h >= top && row.y <= bottom;
            if visible {
                if self.pdf_rendered.get(&i) != Some(&width)
                    && !self.pdf_pending.contains(&(i, width))
                {
                    self.pdf_pending.insert((i, width));
                    self.pdf.send(crate::pdf::PdfReq::Render {
                        page: i,
                        width,
                        generation: self.pdf_gen,
                    });
                }
            } else if row.ready
                && (row.y + row.h < top - view_h * 2.0 || row.y > bottom + view_h * 2.0)
            {
                // Out of reach: drop the pixels, keep the slot.
                let mut cleared = row;
                cleared.pic = empty_image();
                cleared.ready = false;
                self.pdf_model.set_row_data(i, cleared);
                self.pdf_rendered.remove(&i);
            }
        }
        self.ui.set_pdf_page(current as i32 + 1);
    }

    pub fn on_pdf_page(&mut self, generation: u64, page: usize, img: crate::media::Decoded) {
        if generation != self.pdf_gen {
            return;
        }
        self.pdf_pending.retain(|(p, _)| *p != page);
        let width = self.pdf_page_w.round() as u32;
        // A page rendered for an older zoom is still better than blank;
        // the current width was requested again if it differs.
        if let Some(mut row) = self.pdf_model.row_data(page) {
            row.pic = image_of(&img);
            row.ready = true;
            self.pdf_model.set_row_data(page, row);
        }
        self.pdf_rendered
            .insert(page, if img.w == width { width } else { img.w });
    }

    pub(super) fn pdf_zoom_step(&mut self, step: i32) {
        let next = match step {
            0 => 1.0,
            s if s > 0 => (self.pdf_zoom * 1.25).min(4.0),
            _ => (self.pdf_zoom / 1.25).max(0.4),
        };
        if (next - self.pdf_zoom).abs() < 0.001 {
            return;
        }
        self.pdf_zoom = next;
        self.pdf_layout(true);
    }

    pub(super) fn pdf_goto(&mut self, delta: i32) {
        let count = self.pdf_model.row_count() as i32;
        if count == 0 {
            return;
        }
        let target = (self.ui.get_pdf_page() - 1 + delta).clamp(0, count - 1);
        if let Some(row) = self.pdf_model.row_data(target as usize) {
            let y = (row.y - 16.0).max(0.0);
            self.ui.invoke_pdf_set_scroll(y);
            self.pdf_scrolled(y);
        }
    }
}
