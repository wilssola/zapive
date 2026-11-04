// Files dropped on the window from the file manager. They join a queue that
// the image review shows one by one (staged-count / staged-index); images
// open in the editor, anything else is sent as a document once confirmed.
use super::*;
use std::path::{Path, PathBuf};

pub(super) const MAX_DROPPED: usize = 10;
const MAX_DROPPED_BYTES: u64 = 100 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum DroppedKind {
    Image,
    Document,
}

pub(super) fn dropped_kind(path: &Path) -> DroppedKind {
    let ext = path
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "jpg" | "jpeg" | "png" | "webp" | "bmp" | "gif" => DroppedKind::Image,
        _ => DroppedKind::Document,
    }
}

// What may join the queue: regular files of a sane size, at most
// MAX_DROPPED, no duplicates. Directories, links to nowhere and empty or
// huge files are skipped silently.
pub(super) fn accept_dropped(
    paths: &[PathBuf],
    metadata: impl Fn(&Path) -> Option<u64>,
) -> Vec<PathBuf> {
    let mut seen = HashSet::new();
    paths
        .iter()
        .filter(|path| seen.insert((*path).clone()))
        .filter(|path| metadata(path).is_some_and(|size| size > 0 && size <= MAX_DROPPED_BYTES))
        .take(MAX_DROPPED)
        .cloned()
        .collect()
}

// A neutral card standing in for a non-image file in the review.
fn document_card(label: &str) -> crate::media::Decoded {
    let mut card = image::RgbaImage::from_pixel(480, 320, image::Rgba([42, 57, 66, 255]));
    let _ = crate::media::burn_preview_text(&mut card, label, 0.4, 0.45);
    crate::media::Decoded {
        w: 480,
        h: 320,
        rgba: card.into_raw(),
    }
}

fn file_size(path: &Path) -> Option<u64> {
    let meta = std::fs::metadata(path).ok()?;
    meta.is_file().then_some(meta.len())
}

thread_local! {
    static COLLECTED: std::cell::RefCell<Vec<PathBuf>> = const { std::cell::RefCell::new(Vec::new()) };
    static BATCH_TIMER: slint::Timer = slint::Timer::default();
    // While files are dragged over the window: follows the pointer so the
    // chat row under it can light up (winit sends no motion during an OS
    // drag). Reads the position only; nothing ever moves the cursor.
    static TRACK_TIMER: slint::Timer = slint::Timer::default();
    static DROP_POINT: std::cell::Cell<Option<(f32, f32)>> = const { std::cell::Cell::new(None) };
}

// The pointer in the window's logical coordinates, read from the OS.
#[cfg(windows)]
fn pointer_in(window: &slint::winit_030::winit::window::Window) -> Option<(f32, f32)> {
    use windows::Win32::Foundation::POINT;
    use windows::Win32::UI::WindowsAndMessaging::GetCursorPos;
    let mut screen = POINT::default();
    unsafe { GetCursorPos(&mut screen).ok()? };
    let origin = window.inner_position().ok()?;
    let scale = window.scale_factor();
    Some((
        ((f64::from(screen.x) - f64::from(origin.x)) / scale) as f32,
        ((f64::from(screen.y) - f64::from(origin.y)) / scale) as f32,
    ))
}

#[cfg(not(windows))]
fn pointer_in(_window: &slint::winit_030::winit::window::Window) -> Option<(f32, f32)> {
    None
}

// Fed by the main window's winit event filter (Slint keeps a single filter
// per window, so main.rs forwards each event here instead of this module
// registering its own). winit reports one HoveredFile and one DroppedFile
// per file; drops are collected briefly and handed over as one batch.
pub fn handle_file_drop_event(event: &slint::winit_030::winit::event::WindowEvent) {
    use slint::winit_030::winit::event::WindowEvent;
    match event {
        WindowEvent::HoveredFile(_) => {
            apply_now(|b| b.on_file_hover(true));
            start_tracking();
        }
        WindowEvent::HoveredFileCancelled => {
            stop_tracking();
            apply_now(|b| b.on_file_hover(false));
        }
        WindowEvent::DroppedFile(path) => {
            // The last tracked point; refreshed once more on the next tick.
            apply_now(|b| {
                b.refresh_drop_point();
            });
            stop_tracking();
            COLLECTED.with(|list| list.borrow_mut().push(path.clone()));
            BATCH_TIMER.with(|timer| {
                timer.start(
                    slint::TimerMode::SingleShot,
                    Duration::from_millis(80),
                    || {
                        let paths = COLLECTED.with(|list| std::mem::take(&mut *list.borrow_mut()));
                        let point = DROP_POINT.with(|point| point.take());
                        apply_now(move |b| b.on_files_dropped_at(paths, point));
                    },
                );
            });
        }
        _ => {}
    }
}

fn start_tracking() {
    apply_now(|b| b.track_drop_target());
    TRACK_TIMER.with(|timer| {
        timer.start(
            slint::TimerMode::Repeated,
            Duration::from_millis(60),
            || {
                apply_now(|b| b.track_drop_target());
            },
        );
    });
}

fn stop_tracking() {
    TRACK_TIMER.with(|timer| timer.stop());
    apply_now(|b| b.ui.set_drop_target_jid("".into()));
}

impl Bridge {
    pub fn on_file_hover(&mut self, hovering: bool) {
        let ready =
            self.current_jid.is_some() && self.vault.as_ref().is_some_and(|vault| !vault.locked());
        self.ui.set_file_drag_hover(hovering && ready);
    }

    // Files dropped on the window. Only a conversation that is open takes
    // them; the first image goes to the editor, the others wait their turn.
    // Follows the pointer during a drag and lights up the chat row under it.
    fn refresh_drop_point(&mut self) -> Option<(f32, f32)> {
        use slint::winit_030::WinitWindowAccessor;
        let point = self.ui.window().with_winit_window(pointer_in).flatten();
        if point.is_some() {
            DROP_POINT.with(|cell| cell.set(point));
        }
        DROP_POINT.with(|cell| cell.get())
    }

    pub(super) fn track_drop_target(&mut self) {
        let point = self.refresh_drop_point();
        let jid = point
            .map(|(x, y)| self.ui.invoke_chat_at(x, y).to_string())
            .unwrap_or_default();
        self.ui.set_drop_target_jid(jid.as_str().into());
        let over_chat = !jid.is_empty();
        let ready = self.vault.as_ref().is_some_and(|vault| !vault.locked());
        // Over a row the row itself is the cue; the conversation overlay is
        // only for dropping into the open chat.
        self.ui
            .set_file_drag_hover(ready && !over_chat && self.current_jid.is_some());
    }

    // A drop on a chat-list row goes to that chat (opened first); anywhere
    // else to the open conversation. Either way it only reaches the review.
    pub(super) fn on_files_dropped_at(&mut self, paths: Vec<PathBuf>, point: Option<(f32, f32)>) {
        self.ui.set_drop_target_jid("".into());
        let target = point
            .map(|(x, y)| self.ui.invoke_chat_at(x, y).to_string())
            .filter(|jid| !jid.is_empty());
        if let Some(jid) = target
            && self.current_jid.as_deref() != Some(jid.as_str())
            && self.vault.as_ref().is_some_and(|vault| !vault.locked())
        {
            self.clear_staged();
            self.open_dm(&jid, None);
        }
        self.on_files_dropped(paths);
    }

    pub fn on_files_dropped(&mut self, paths: Vec<PathBuf>) {
        self.ui.set_file_drag_hover(false);
        if self.current_jid.is_none() || self.vault.as_ref().is_none_or(|vault| vault.locked()) {
            return;
        }
        // Nothing is sent from here: every file only joins the review queue
        // and goes out when the user presses Send on it.
        let accepted = accept_dropped(&paths, file_size);
        if accepted.is_empty() {
            return;
        }
        if self.ui.get_preview_open() && self.current_jid == self.preview_jid {
            let room = MAX_DROPPED.saturating_sub(self.staged_total);
            let extra: Vec<PathBuf> = accepted.into_iter().take(room).collect();
            self.staged_total += extra.len();
            self.staged_files.extend(extra);
            self.sync_staged_ui();
            return;
        }
        let mut queue = accepted.into_iter();
        let Some(first) = queue.next() else { return };
        self.staged_files = queue.collect();
        self.staged_total = self.staged_files.len() + 1;
        self.staged_position = 0;
        self.sync_staged_ui();
        self.start_staged(first);
    }

    // Shows one queued file in the review: images open in the editor, any
    // other file shows a plain card and is sent as a document on confirm.
    pub(super) fn start_staged(&mut self, path: PathBuf) {
        if dropped_kind(&path) == DroppedKind::Image {
            self.start_preview(path);
            return;
        }
        self.cancel_preview();
        self.preview_jid = self.current_jid.clone();
        self.staged_document = Some(path.clone());
        let label = path
            .extension()
            .and_then(|ext| ext.to_str())
            .unwrap_or("FILE")
            .to_ascii_uppercase();
        self.ui.set_preview_image(image_of(&document_card(&label)));
        self.ui.set_preview_open(true);
        self.ui.invoke_focus_preview_caption();
    }

    // Confirm on a document card: this is the only place a dropped
    // document is sent.
    pub(super) fn confirm_staged_document(&mut self) -> bool {
        let Some(path) = self.staged_document.take() else {
            return false;
        };
        let jid = self
            .current_jid
            .clone()
            .filter(|jid| Some(jid) == self.preview_jid.as_ref());
        self.cancel_preview();
        if let Some(jid) = jid {
            let origin = Some(path.to_string_lossy().into_owned());
            self.wa.send(Cmd::SendDocument { jid, origin, path });
        }
        self.next_staged();
        true
    }

    pub(super) fn sync_staged_ui(&mut self) {
        let count = if self.staged_total > 1 {
            self.staged_total
        } else {
            0
        };
        self.ui
            .set_staged_count(count.min(i32::MAX as usize) as i32);
        self.ui
            .set_staged_index(self.staged_position.min(i32::MAX as usize) as i32);
    }

    // The next queued image after one was sent or discarded.
    pub(super) fn next_staged(&mut self) {
        if self.staged_files.is_empty() {
            self.staged_total = 0;
            self.staged_position = 0;
            self.sync_staged_ui();
            return;
        }
        let next = self.staged_files.remove(0);
        self.staged_position += 1;
        self.sync_staged_ui();
        self.start_staged(next);
    }

    // The review's thumbnail strip: leaving the current picture unsent
    // moves on to the next one in the queue.
    pub(super) fn skip_staged(&mut self) {
        if self.staged_files.is_empty() {
            return;
        }
        self.next_staged();
    }

    pub(super) fn clear_staged(&mut self) {
        self.staged_files.clear();
        self.staged_document = None;
        self.staged_total = 0;
        self.staged_position = 0;
        self.ui.set_staged_count(0);
        self.ui.set_staged_index(0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn size_of(path: &Path) -> Option<u64> {
        match path.to_str()? {
            "empty.png" => Some(0),
            "huge.png" => Some(MAX_DROPPED_BYTES + 1),
            "dir" | "missing.png" => None,
            _ => Some(10),
        }
    }

    fn paths(list: &[&str]) -> Vec<PathBuf> {
        list.iter().map(PathBuf::from).collect()
    }

    #[test]
    fn kind_follows_the_extension_case_insensitively() {
        assert_eq!(dropped_kind(Path::new("a.PNG")), DroppedKind::Image);
        assert_eq!(dropped_kind(Path::new("a.jpeg")), DroppedKind::Image);
        assert_eq!(dropped_kind(Path::new("a.pdf")), DroppedKind::Document);
        assert_eq!(dropped_kind(Path::new("noext")), DroppedKind::Document);
    }

    #[test]
    fn accept_skips_empty_huge_missing_and_duplicates() {
        let got = accept_dropped(
            &paths(&[
                "a.png",
                "empty.png",
                "huge.png",
                "dir",
                "missing.png",
                "a.png",
                "b.pdf",
            ]),
            size_of,
        );
        assert_eq!(got, paths(&["a.png", "b.pdf"]));
    }

    #[test]
    fn accept_keeps_drop_order_and_mixes_kinds_without_sending() {
        let got = accept_dropped(&paths(&["z.pdf", "a.png", "m.txt"]), size_of);
        assert_eq!(got, paths(&["z.pdf", "a.png", "m.txt"]));
        let kinds: Vec<DroppedKind> = got.iter().map(|path| dropped_kind(path)).collect();
        assert_eq!(
            kinds,
            vec![
                DroppedKind::Document,
                DroppedKind::Image,
                DroppedKind::Document
            ]
        );
    }

    #[test]
    fn accept_caps_the_queue() {
        let many: Vec<PathBuf> = (0..30)
            .map(|i| PathBuf::from(format!("f{i}.png")))
            .collect();
        assert_eq!(accept_dropped(&many, size_of).len(), MAX_DROPPED);
        assert!(accept_dropped(&[], size_of).is_empty());
    }
}
