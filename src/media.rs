// Media pipeline: encrypted cache, downloads and pixel decoding. Port of
// the receive half of src/media.ts on master. Everything here runs on the
// tokio side; decoded pixels cross to the UI as plain RGBA buffers.
use crate::paths::media_cache;
use crate::vault::KeyHandle;
use image::AnimationDecoder as _;
use image::imageops;
use std::path::PathBuf;
use whatsapp_rust::waproto::whatsapp as wa;

// A decoded bitmap ready to become a slint::Image on the UI thread.
#[derive(Clone)]
pub struct Decoded {
    pub w: u32,
    pub h: u32,
    pub rgba: Vec<u8>,
}

pub fn sanitize(id: &str) -> String {
    id.chars().map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '_' }).collect()
}

// Cached media carries the app's own extensions, one per kind, since
// every file is a sealed blob only Zapive reads: `.photo`, `.sticker`,
// `.video`, `.audio`, `.doc`, `.link` (link-card pictures) and `.avatar`.
pub fn kind_ext(mimetype: &str) -> &'static str {
    let mime = mimetype.split(';').next().unwrap_or("").trim();
    if mime.starts_with("image/") {
        "photo"
    } else if mime.starts_with("video/") {
        "video"
    } else if mime.starts_with("audio/") {
        "audio"
    } else {
        "doc"
    }
}

// The plain extension a decrypted copy gets, so whatever opens it (the
// OS, a decoder that sniffs names) knows what it is.
fn plain_ext(kind: &str) -> &'static str {
    match kind {
        "photo" | "link" | "avatar" => "jpg",
        "sticker" => "webp",
        "video" => "mp4",
        "audio" => "ogg",
        _ => "bin",
    }
}

pub fn cache_path(id: &str, mimetype: &str) -> PathBuf {
    media_cache().join(format!("{}.{}", sanitize(id), kind_ext(mimetype)))
}

// Files written by earlier builds carried the media's real extension
// (and a prefix for stickers and link pictures); they are renamed into
// the current scheme once, at boot.
pub fn migrate_cache() {
    let dir = media_cache();
    let Ok(entries) = std::fs::read_dir(&dir) else { return };
    let mut moved = 0;
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else { continue };
        let Some(ext) = path.extension().and_then(|e| e.to_str()) else { continue };
        let (new_stem, kind) = match ext {
            "jpg" if stem.starts_with("lnk_") => (stem["lnk_".len()..].to_string(), "link"),
            "webp" if stem.starts_with("stk_") => (stem["stk_".len()..].to_string(), "sticker"),
            "jpg" | "png" | "gif" | "webp" => (stem.to_string(), "photo"),
            "mp4" | "3gp" => (stem.to_string(), "video"),
            "ogg" | "mp3" | "m4a" | "wav" => (stem.to_string(), "audio"),
            "pdf" | "bin" => (stem.to_string(), "doc"),
            _ => continue,
        };
        let target = dir.join(format!("{new_stem}.{kind}"));
        if target.exists() {
            let _ = std::fs::remove_file(&path);
        } else if std::fs::rename(&path, &target).is_ok() {
            moved += 1;
        }
    }
    if moved > 0 {
        println!("[media] renamed {moved} cached file(s) into the current scheme");
    }
}

// The downloadable part of a message, if any.
fn downloadable(msg: &wa::Message) -> Option<&dyn whatsapp_rust::wacore::download::Downloadable> {
    use whatsapp_rust::proto_helpers::MessageExt as _;
    let inner = msg.get_base_message();
    if let Some(m) = inner.image_message.as_option() {
        return Some(m);
    }
    if let Some(m) = inner.sticker_message.as_option() {
        return Some(m);
    }
    if let Some(m) = inner.video_message.as_option() {
        return Some(m);
    }
    if let Some(m) = inner.audio_message.as_option() {
        return Some(m);
    }
    if let Some(m) = inner.document_message.as_option() {
        return Some(m);
    }
    None
}

// Whether the message carries media Zapive can fetch from the server.
// Outgoing messages never need it: the file came from disk, so the
// original copy is opened instead of downloading the round trip.
pub fn is_downloadable(msg: &wa::Message) -> bool {
    downloadable(msg).is_some()
}

// How many plaintext bytes the download will write, as declared by the
// message (encrypted size on the CDN, minus padding/IV/tag).
pub fn expected_bytes(msg: &wa::Message) -> Option<u64> {
    downloadable(msg)?.file_length()
}

// Human size: 640 B, 18.4 KB, 3.2 MB. Empty when the size is unknown.
pub fn format_size(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    const GB: f64 = MB * 1024.0;
    let b = bytes as f64;
    if b >= GB {
        format!("{:.1} GB", b / GB)
    } else if b >= MB {
        format!("{:.1} MB", b / MB)
    } else if b >= KB {
        format!("{:.1} KB", b / KB)
    } else {
        format!("{bytes} B")
    }
}

// Stickers are keyed by their content hash rather than the message id:
// the same sticker sent ten times is one file and one decode.
pub fn sticker_key(msg: &wa::Message) -> Option<String> {
    use whatsapp_rust::proto_helpers::MessageExt as _;
    let sha = msg.get_base_message().sticker_message.as_option()?.file_sha256.as_ref()?;
    if sha.is_empty() {
        return None;
    }
    Some(sha.iter().map(|b| format!("{b:02x}")).collect())
}

// Where a message's media lives in the cache, and whether it is there.
// Stickers cached per message by earlier builds are moved under their
// content hash the first time they are asked for.
pub fn cached_path(id: &str, mimetype: &str, msg: &wa::Message) -> (PathBuf, bool) {
    let path = match sticker_key(msg) {
        Some(sha) => {
            let by_hash = media_cache().join(format!("{sha}.sticker"));
            if !by_hash.exists() {
                let legacy = cache_path(id, "image/webp");
                if legacy.exists() {
                    let _ = std::fs::rename(&legacy, &by_hash);
                }
            }
            by_hash
        }
        None => cache_path(id, mimetype),
    };
    let exists = path.exists();
    (path, exists)
}

pub async fn cache_sent(
    key: &KeyHandle,
    id: &str,
    mimetype: &str,
    msg: &wa::Message,
    bytes: &[u8],
) -> Option<PathBuf> {
    if !key.unlocked() {
        return None;
    }
    let (path, _) = cached_path(id, mimetype, msg);
    if let Err(e) = tokio::fs::write(&path, key.encrypt_bytes(bytes)).await {
        eprintln!("[media] cache write failed for {id}: {e}");
        return None;
    }
    Some(path)
}

// Downloads (if missing) and returns the cached, encrypted file's path.
// The permit is only held while bytes actually move: a task waiting out
// a rate limit must not sit on it, or cached media queues behind it.
pub async fn ensure_cached(
    client: &std::sync::Arc<whatsapp_rust::client::Client>,
    key: &KeyHandle,
    id: &str,
    mimetype: &str,
    msg: &wa::Message,
    sem: &std::sync::Arc<tokio::sync::Semaphore>,
) -> Option<PathBuf> {
    ensure_cached_with_progress(client, Some(key), id, mimetype, msg, sem, |_| {}).await
}

struct ProgressWriter<F> {
    bytes: std::io::Cursor<Vec<u8>>,
    progress: std::sync::Arc<F>,
    // Last reported byte count: the socket hands over ~16 KB chunks, which
    // on a fast CDN would repaint the bar hundreds of times a second.
    reported: std::sync::atomic::AtomicU64,
}

impl<F: Fn(u64)> ProgressWriter<F> {
    fn report(&self) {
        let written = self.bytes.position();
        let last = self.reported.load(std::sync::atomic::Ordering::Relaxed);
        if written.saturating_sub(last) >= PROGRESS_STEP_BYTES {
            self.reported.store(written, std::sync::atomic::Ordering::Relaxed);
            (self.progress)(written);
        }
    }
}

const PROGRESS_STEP_BYTES: u64 = 32 * 1024;

impl<F: Fn(u64)> std::io::Write for ProgressWriter<F> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let written = std::io::Write::write(&mut self.bytes, buf)?;
        self.report();
        Ok(written)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        std::io::Write::flush(&mut self.bytes)
    }
}

impl<F: Fn(u64)> std::io::Seek for ProgressWriter<F> {
    fn seek(&mut self, pos: std::io::SeekFrom) -> std::io::Result<u64> {
        std::io::Seek::seek(&mut self.bytes, pos)
    }
}

impl<F: Fn(u64)> whatsapp_rust::wacore::download::DownloadWriter for ProgressWriter<F> {
    fn truncate(&mut self, len: u64) -> std::io::Result<()> {
        self.bytes.get_mut().truncate(usize::try_from(len).unwrap_or(usize::MAX));
        self.reported.store(0, std::sync::atomic::Ordering::Relaxed);
        (self.progress)(self.bytes.get_ref().len() as u64);
        Ok(())
    }
}

/**
 * Reports plaintext bytes written, resetting progress when a download is
 * retried. A cache hit does not invoke the callback.
 *
 * The result carries the persisted path; when `key` is None the plaintext is
 * written in place of the sealed blob (never after a fresh download: the
 * encryption keeps decrypted media off disk).
 */
pub async fn ensure_cached_with_progress<F>(
    client: &std::sync::Arc<whatsapp_rust::client::Client>,
    key: Option<&KeyHandle>,
    id: &str,
    mimetype: &str,
    msg: &wa::Message,
    sem: &std::sync::Arc<tokio::sync::Semaphore>,
    progress: F,
) -> Option<PathBuf>
where
    F: Fn(u64) + Send + Sync + 'static,
{
    let (path, exists) = cached_path(id, mimetype, msg);
    if exists {
        return Some(path);
    }
    let key = key?;
    let target = downloadable(msg)?;
    let total = target.file_length().unwrap_or(0);
    let progress = std::sync::Arc::new(progress);
    let writer = || {
        progress(0);
        ProgressWriter {
            bytes: std::io::Cursor::new(Vec::new()),
            progress: progress.clone(),
            reported: std::sync::atomic::AtomicU64::new(0),
        }
    };
    wait_for_backoff().await;
    let mut attempt = {
        let _permit = sem.acquire().await;
        client.download_to_writer(target, writer()).await
    };
    if attempt.as_ref().is_err_and(|e| rate_limited(&e.to_string())) {
        // The server is throttling media: back everything off, then try
        // this one once more.
        note_rate_limit();
        wait_for_backoff().await;
        let _permit = sem.acquire().await;
        attempt = client.download_to_writer(target, writer()).await;
    }
    match attempt {
        Ok(bytes) => {
            let len = bytes.bytes.get_ref().len() as u64;
            // The last chunk may be smaller than a step: land on the real end.
            // A declared total smaller than what arrived must not push the
            // bar past 100%.
            progress(if total == 0 { len } else { len.min(total) });
            let sealed = key.encrypt_bytes(&bytes.bytes.into_inner());
            if let Err(e) = tokio::fs::write(&path, sealed).await {
                eprintln!("[media] cache write failed for {id}: {e}");
                return None;
            }
            Some(path)
        }
        Err(e) => {
            eprintln!("[media] download failed for {id}: {e}");
            None
        }
    }
}

// Sealed avatar cache: one file per jid, so the chat list fills from
// disk instead of asking the server for every picture on every boot.
// An empty file remembers "this jid has no picture".
pub fn avatar_cache_path(jid: &str) -> PathBuf {
    let dir = media_cache().join("avatars");
    let _ = std::fs::create_dir_all(&dir);
    dir.join(format!("{}.avatar", sanitize(jid)))
}

// Decode sizes: a bubble is at most 330x380 logical, a link card 360
// wide, so these cover a 2x display without keeping a screen-sized
// bitmap per message. The lightbox re-decodes at FULL_PX on demand.
pub const BUBBLE_PX: u32 = 720;
pub const CARD_PX: u32 = 480;
pub const FULL_PX: u32 = 2048;

// Avatars are decoded once at a size that stays crisp in the list on
// a HiDPI screen; the info panel asks for a larger cut on demand.
pub const AVATAR_PX: u32 = 112;
pub const AVATAR_LARGE_PX: u32 = 320;

// Whether cached avatar bytes hold the full picture. Earlier builds
// stored the server's 96px preview; anything that small is refetched
// and overwritten in place.
pub fn avatar_is_full(bytes: &[u8]) -> bool {
    image::load_from_memory(bytes)
        .map(|img| img.width().min(img.height()) >= 200)
        .unwrap_or(false)
}

// A link preview's high-resolution thumbnail, cached like other media.
pub fn link_thumb_path(id: &str) -> PathBuf {
    media_cache().join(format!("{}.link", sanitize(id)))
}

// WhatsApp answers bursts of media downloads with 429 rate-overlimit.
// One shared "not before" instant holds every download back for a while
// after that, instead of each task hammering on.
static BACKOFF_UNTIL_MS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

pub fn rate_limited(err: &str) -> bool {
    err.contains("rate-overlimit") || err.contains("429")
}

pub fn note_rate_limit() {
    BACKOFF_UNTIL_MS.store(now_ms() + 30_000, std::sync::atomic::Ordering::SeqCst);
}

pub async fn wait_for_backoff() {
    let until = BACKOFF_UNTIL_MS.load(std::sync::atomic::Ordering::SeqCst);
    let now = now_ms();
    if until > now {
        tokio::time::sleep(std::time::Duration::from_millis(until - now)).await;
    }
}

pub fn read_cached(key: &KeyHandle, path: &PathBuf) -> Option<Vec<u8>> {
    let data = std::fs::read(path).ok()?;
    key.decrypt_bytes(&data).ok()
}

// Decrypted copy for consumers that need a real file (FFmpeg, the OS
// opener). Lives under .tmp, which is wiped at boot.
pub fn temp_plain(key: &KeyHandle, path: &PathBuf) -> Option<PathBuf> {
    let dir = media_cache().join(".tmp");
    let _ = std::fs::create_dir_all(&dir);
    let kind = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    let out = dir.join(format!("{}.{}", path.file_stem()?.to_str()?, plain_ext(kind)));
    if !out.exists() {
        let plain = read_cached(key, path)?;
        std::fs::write(&out, plain).ok()?;
    }
    Some(out)
}

pub fn clean_tmp() {
    let _ = std::fs::remove_dir_all(media_cache().join(".tmp"));
    clean_notification_thumbnails();
}

pub fn clean_notification_thumbnails() {
    let _ = std::fs::remove_dir_all(crate::paths::cache_dir().join(".toast"));
}

pub fn notification_thumbnail(bytes: &[u8]) -> Option<PathBuf> {
    const MAX_INPUT: usize = 64 * 1024;
    const MAX_DIMENSION: u32 = 1024;
    if bytes.is_empty() || bytes.len() > MAX_INPUT {
        return None;
    }
    let image = image::load_from_memory(bytes).ok()?;
    if image.width() > MAX_DIMENSION || image.height() > MAX_DIMENSION {
        return None;
    }
    let mut encoded = std::io::Cursor::new(Vec::new());
    image.write_to(&mut encoded, image::ImageFormat::Jpeg).ok()?;
    if encoded.get_ref().len() > MAX_INPUT {
        return None;
    }
    let dir = crate::paths::cache_dir().join(".toast");
    std::fs::create_dir_all(&dir).ok()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).ok()?;
    }
    use rand::RngCore as _;
    let mut random = [0u8; 16];
    rand::rng().fill_bytes(&mut random);
    let name = random.iter().map(|b| format!("{b:02x}")).collect::<String>();
    let path = dir.join(format!("{name}.jpg"));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    use std::io::Write as _;
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt as _;
        options.custom_flags(0x100000);
    }
    let mut file = options.open(&path).ok()?;
    file.write_all(encoded.get_ref()).ok()?;
    Some(path)
}

pub enum PreviewEdit {
    Crop(f32, f32, f32, f32),
    Text(String, f32, f32),
    Draw(String, [u8; 4], f32),
}

pub fn crop_preview(image: &image::RgbaImage, x: f32, y: f32, w: f32, h: f32) -> Option<image::RgbaImage> {
    if ![x, y, w, h].iter().all(|v| v.is_finite()) || w <= 0.0 || h <= 0.0 {
        return None;
    }
    let (iw, ih) = image.dimensions();
    let left = (x.clamp(0.0, 1.0) * iw as f32).floor() as u32;
    let top = (y.clamp(0.0, 1.0) * ih as f32).floor() as u32;
    let right = ((x + w).clamp(0.0, 1.0) * iw as f32).ceil() as u32;
    let bottom = ((y + h).clamp(0.0, 1.0) * ih as f32).ceil() as u32;
    if right <= left || bottom <= top {
        return None;
    }
    Some(imageops::crop_imm(image, left, top, right - left, bottom - top).to_image())
}

fn preview_font() -> Option<ab_glyph::FontArc> {
    static FONT: std::sync::OnceLock<Option<ab_glyph::FontArc>> = std::sync::OnceLock::new();
    FONT.get_or_init(|| {
        let mut db = fontdb::Database::new();
        db.load_system_fonts();
        let query = fontdb::Query {
            families: &[fontdb::Family::Name("Arial"), fontdb::Family::Name("DejaVu Sans"), fontdb::Family::SansSerif],
            weight: fontdb::Weight::BOLD,
            ..Default::default()
        };
        let preferred = db.query(&query);
        preferred.into_iter().chain(db.faces().map(|face| face.id)).find_map(|id| {
            db.with_face_data(id, |data, index| {
                ab_glyph::FontVec::try_from_vec_and_index(data.to_vec(), index).ok().map(ab_glyph::FontArc::new)
            }).flatten()
        })
    }).clone()
}

pub fn burn_preview_text(image: &mut image::RgbaImage, text: &str, x: f32, y: f32) -> Option<()> {
    use ab_glyph::{Font, ScaleFont};
    if !x.is_finite() || !y.is_finite() || text.trim().is_empty() || image.width() == 0 || image.height() == 0 {
        return None;
    }
    let font = preview_font()?;
    let size = (image.width().min(image.height()) as f32 * 0.055).clamp(16.0, 96.0);
    let scaled = font.as_scaled(size);
    let outline = (size / 16.0).ceil() as i32;
    let margin = outline as f32 + 1.0;
    let start_x = (x.clamp(0.0, 1.0) * image.width() as f32).clamp(margin.min(image.width() as f32), (image.width() as f32 - margin).max(margin));
    let mut pen_x = start_x;
    let mut pen_y = (y.clamp(0.0, 1.0) * image.height() as f32).min((image.height() as f32 - size - margin).max(0.0)) + scaled.ascent();
    let mut previous = None;
    let mut glyphs = Vec::new();
    for ch in text.chars().take(4096) {
        if ch == '\n' {
            pen_x = margin;
            pen_y += scaled.height() + scaled.line_gap();
            previous = None;
            continue;
        }
        if ch.is_control() {
            continue;
        }
        let id = scaled.glyph_id(ch);
        if let Some(prev) = previous {
            pen_x += scaled.kern(prev, id);
        }
        let advance = scaled.h_advance(id);
        if pen_x + advance > image.width() as f32 - margin && pen_x > margin {
            pen_x = margin;
            pen_y += scaled.height() + scaled.line_gap();
        }
        if pen_y - scaled.ascent() >= image.height() as f32 {
            break;
        }
        if let Some(glyph) = font.outline_glyph(id.with_scale_and_position(size, ab_glyph::point(pen_x, pen_y))) {
            glyphs.push(glyph);
        }
        pen_x += advance;
        previous = Some(id);
    }
    for dark in [true, false] {
        for glyph in &glyphs {
            let bounds = glyph.px_bounds();
            glyph.draw(|gx, gy, coverage| {
                let radius = if dark { outline } else { 0 };
                for dy in -radius..=radius {
                    for dx in -radius..=radius {
                        if dx * dx + dy * dy > radius * radius { continue; }
                        let px = bounds.min.x as i32 + gx as i32 + dx;
                        let py = bounds.min.y as i32 + gy as i32 + dy;
                        if px < 0 || py < 0 || px >= image.width() as i32 || py >= image.height() as i32 { continue; }
                        let pixel = image.get_pixel_mut(px as u32, py as u32);
                        let alpha = coverage + pixel[3] as f32 / 255.0 * (1.0 - coverage);
                        for channel in 0..3 {
                            let value = if dark { 16.0 } else { 255.0 };
                            pixel[channel] = ((value * coverage + pixel[channel] as f32 * pixel[3] as f32 / 255.0 * (1.0 - coverage)) / alpha.max(f32::EPSILON)).round() as u8;
                        }
                        pixel[3] = (alpha * 255.0).round() as u8;
                    }
                }
            });
        }
    }
    Some(())
}

fn preview_draw_points(points: &str) -> Option<Vec<(f32, f32)>> {
    let mut parsed = Vec::new();
    for point in points.trim().strip_suffix(';').unwrap_or(points.trim()).split(';') {
        if parsed.len() == 20_000 {
            return None;
        }
        let (x, y) = point.split_once(',')?;
        let x: f32 = x.trim().parse().ok()?;
        let y: f32 = y.trim().parse().ok()?;
        if ![x, y].iter().all(|v| v.is_finite() && (0.0..=1.0).contains(v)) {
            return None;
        }
        parsed.push((x, y));
    }
    Some(parsed)
}

pub fn burn_preview_draw(image: &mut image::RgbaImage, points: &str, color: [u8; 4], thickness: f32) -> Option<()> {
    if !thickness.is_finite() || thickness <= 0.0 || thickness > 1.0 || image.width() == 0 || image.height() == 0 {
        return None;
    }
    let points = preview_draw_points(points)?;
    let (w, h) = image.dimensions();
    let radius = thickness * w.min(h) as f32 * 0.5;
    let mut coverage = vec![0u8; (w as usize).checked_mul(h as usize)?];
    let scaled: Vec<_> = points.iter().map(|&(x, y)| (x * w as f32, y * h as f32)).collect();
    for i in 0..scaled.len() {
        let (ax, ay) = scaled[i.saturating_sub(1)];
        let (bx, by) = scaled[i];
        let left = (ax.min(bx) - radius - 1.0).max(0.0) as u32;
        let top = (ay.min(by) - radius - 1.0).max(0.0) as u32;
        let right = (ax.max(bx) + radius + 1.0).ceil().min(w as f32) as u32;
        let bottom = (ay.max(by) + radius + 1.0).ceil().min(h as f32) as u32;
        let dx = bx - ax;
        let dy = by - ay;
        let length_squared = dx * dx + dy * dy;
        for y in top..bottom {
            for x in left..right {
                let px = x as f32 + 0.5 - ax;
                let py = y as f32 + 0.5 - ay;
                let t = if length_squared > 0.0 { ((px * dx + py * dy) / length_squared).clamp(0.0, 1.0) } else { 0.0 };
                let distance = (px - t * dx).hypot(py - t * dy);
                let alpha = ((radius + 0.5 - distance).clamp(0.0, 1.0) * 255.0).round() as u8;
                let cell = &mut coverage[y as usize * w as usize + x as usize];
                *cell = (*cell).max(alpha);
            }
        }
    }
    for (pixel, coverage) in image.pixels_mut().zip(coverage) {
        let source_alpha = coverage as f32 / 255.0 * color[3] as f32 / 255.0;
        if source_alpha == 0.0 { continue; }
        let destination_alpha = pixel[3] as f32 / 255.0;
        let alpha = source_alpha + destination_alpha * (1.0 - source_alpha);
        for channel in 0..3 {
            pixel[channel] = ((color[channel] as f32 * source_alpha + pixel[channel] as f32 * destination_alpha * (1.0 - source_alpha)) / alpha).round() as u8;
        }
        pixel[3] = (alpha * 255.0).round() as u8;
    }
    Some(())
}

pub fn edit_preview(path: &std::path::Path, edit: PreviewEdit) -> Option<(PathBuf, Decoded)> {
    let bytes = std::fs::read(path).ok()?;
    let mut image = apply_orientation(image::load_from_memory(&bytes).ok()?, exif_rotation(&bytes)).to_rgba8();
    match edit {
        PreviewEdit::Crop(x, y, w, h) => image = crop_preview(&image, x, y, w, h)?,
        PreviewEdit::Text(text, x, y) => burn_preview_text(&mut image, &text, x, y)?,
        PreviewEdit::Draw(points, color, thickness) => burn_preview_draw(&mut image, &points, color, thickness)?,
    }
    let dir = media_cache().join(".tmp");
    std::fs::create_dir_all(&dir).ok()?;
    let path = dir.join(format!("edit-{:032x}.png", rand::random::<u128>()));
    image.save(&path).ok()?;
    let preview = image::DynamicImage::ImageRgba8(image).thumbnail(1280, 1280).to_rgba8();
    Some((path, Decoded { w: preview.width(), h: preview.height(), rgba: preview.into_raw() }))
}

// A 512x512 webp sticker with transparent padding, like WhatsApp makes.
pub fn to_webp_sticker(data: &[u8]) -> Option<Vec<u8>> {
    let img = image::load_from_memory(data).ok()?;
    let fitted = img.resize(512, 512, imageops::FilterType::Triangle).to_rgba8();
    let mut canvas = image::RgbaImage::new(512, 512);
    let (x, y) = ((512 - fitted.width()) / 2, (512 - fitted.height()) / 2);
    imageops::overlay(&mut canvas, &fitted, x as i64, y as i64);
    let encoder = webp::Encoder::from_rgba(&canvas, 512, 512);
    Some(encoder.encode(90.0).to_vec())
}

fn exif_rotation(data: &[u8]) -> u32 {
    let mut cursor = std::io::Cursor::new(data);
    exif::Reader::new()
        .read_from_container(&mut cursor)
        .ok()
        .and_then(|meta| {
            meta.get_field(exif::Tag::Orientation, exif::In::PRIMARY)
                .and_then(|f| f.value.get_uint(0))
        })
        .unwrap_or(1)
}

fn apply_orientation(img: image::DynamicImage, orientation: u32) -> image::DynamicImage {
    match orientation {
        2 => img.fliph(),
        3 => img.rotate180(),
        4 => img.flipv(),
        5 => img.rotate90().fliph(),
        6 => img.rotate90(),
        7 => img.rotate270().fliph(),
        8 => img.rotate270(),
        _ => img,
    }
}

// Decode + EXIF rotate + fit into max_dim (never upscaling) + RGBA8.
pub fn decode_bytes(data: &[u8], max_dim: u32) -> Option<Decoded> {
    let orientation = exif_rotation(data);
    let img = image::load_from_memory(data).ok()?;
    let img = apply_orientation(img, orientation);
    let (w, h) = (img.width(), img.height());
    let scale = (max_dim as f64 / w as f64).min(max_dim as f64 / h as f64).min(1.0);
    let (tw, th) = (((w as f64 * scale) as u32).max(1), ((h as f64 * scale) as u32).max(1));
    let resized =
        if scale < 1.0 { img.resize(tw, th, imageops::FilterType::Triangle) } else { img };
    let rgba = resized.to_rgba8();
    Some(Decoded { w: rgba.width(), h: rgba.height(), rgba: rgba.into_raw() })
}

// Square cover crop (avatars in the UI).
pub fn decode_cover(data: &[u8], size: u32) -> Option<Decoded> {
    let img = image::load_from_memory(data).ok()?;
    let resized = img.resize_to_fill(size, size, imageops::FilterType::Lanczos3);
    let rgba = resized.to_rgba8();
    Some(Decoded { w: rgba.width(), h: rgba.height(), rgba: rgba.into_raw() })
}

// Sticker frames: animated webp/gif capped, each fit into a square box.
pub fn sticker_frames(data: &[u8], box_dim: u32, cap: usize) -> Vec<Decoded> {
    let fit = |frame: image::RgbaImage| -> Decoded {
        let img = image::DynamicImage::ImageRgba8(frame);
        let (w, h) = (img.width(), img.height());
        let scale = (box_dim as f64 / w as f64).min(box_dim as f64 / h as f64).min(1.0);
        let resized = if scale < 1.0 {
            img.resize(
                ((w as f64 * scale) as u32).max(1),
                ((h as f64 * scale) as u32).max(1),
                imageops::FilterType::Triangle,
            )
        } else {
            img
        };
        let rgba = resized.to_rgba8();
        Decoded { w: rgba.width(), h: rgba.height(), rgba: rgba.into_raw() }
    };
    // Try the animated decoders first; a still image is the fallback.
    let animated: Option<Vec<Decoded>> = (|| {
        let frames: Vec<Decoded> = image::codecs::webp::WebPDecoder::new(std::io::Cursor::new(data))
            .ok()
            .filter(|d| d.has_animation())
            .map(|d| {
                d.into_frames()
                    .take(cap)
                    .filter_map(|f| f.ok())
                    .map(|f| fit(f.into_buffer()))
                    .collect()
            })
            .or_else(|| {
                image::codecs::gif::GifDecoder::new(std::io::Cursor::new(data)).ok().map(|d| {
                    d.into_frames()
                        .take(cap)
                        .filter_map(|f| f.ok())
                        .map(|f| fit(f.into_buffer()))
                        .collect()
                })
            })?;
        if frames.is_empty() { None } else { Some(frames) }
    })();
    if let Some(frames) = animated {
        return frames;
    }
    decode_bytes(data, box_dim).map(|d| vec![d]).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preview_crop_uses_normalized_current_image_coordinates() {
        let image = image::RgbaImage::from_fn(100, 80, |x, y| image::Rgba([x as u8, y as u8, 0, 255]));
        let cropped = crop_preview(&image, 0.25, 0.25, 0.5, 0.5).unwrap();
        assert_eq!(cropped.dimensions(), (50, 40));
        assert_eq!(cropped.get_pixel(0, 0), image.get_pixel(25, 20));
        assert_eq!(crop_preview(&cropped, 0.0, 0.0, 0.5, 0.5).unwrap().dimensions(), (25, 20));
        assert!(crop_preview(&image, f32::NAN, 0.0, 1.0, 1.0).is_none());
        assert!(crop_preview(&image, 1.0, 0.0, 0.1, 1.0).is_none());
        assert!(crop_preview(&image, 0.0, 0.0, 0.0, 1.0).is_none());
    }

    #[test]
    fn preview_draw_dot_is_round_and_antialiased() {
        let mut image = image::RgbaImage::new(100, 80);
        burn_preview_draw(&mut image, "0.5,0.5;", [20, 80, 160, 255], 0.125).unwrap();
        assert_eq!(image.get_pixel(50, 40).0, [20, 80, 160, 255]);
        assert_eq!(image.get_pixel(45, 35)[3], 0);
        assert!(image.pixels().any(|p| p[3] > 0 && p[3] < 255));
        assert_eq!(image.get_pixel(0, 0)[3], 0);
    }

    #[test]
    fn preview_draw_segments_are_continuous_and_blended_once() {
        let mut image = image::RgbaImage::new(100, 100);
        burn_preview_draw(&mut image, "0.1,0.2;0.8,0.2;0.8,0.9;0.8,0.9;", [12, 34, 56, 128], 0.06).unwrap();
        for x in 10..81 {
            assert_eq!(image.get_pixel(x, 20).0, [12, 34, 56, 128]);
        }
        for y in 20..90 {
            assert_eq!(image.get_pixel(80, y).0, [12, 34, 56, 128]);
        }
    }

    #[test]
    fn preview_draw_respects_rgba_and_image_alpha() {
        let mut image = image::RgbaImage::from_pixel(40, 80, image::Rgba([0, 0, 255, 255]));
        burn_preview_draw(&mut image, "0.5,0.5", [255, 0, 0, 128], 0.2).unwrap();
        assert_eq!(image.get_pixel(20, 40).0, [128, 0, 127, 255]);
        let original = image.clone();
        burn_preview_draw(&mut image, "0,0;1,1;", [0, 255, 0, 0], 1.0).unwrap();
        assert_eq!(image, original);
    }

    #[test]
    fn preview_draw_rejects_invalid_input_without_changes() {
        let mut image = image::RgbaImage::new(20, 20);
        let original = image.clone();
        for points in ["", ";", "0.5", "0.5,0.5,0.5", "NaN,0", "0,inf", "-0.1,0", "0,1.1", "0,0;;", "0,0;bad"] {
            assert!(burn_preview_draw(&mut image, points, [255; 4], 0.1).is_none(), "{points}");
            assert_eq!(image, original);
        }
        for thickness in [0.0, -0.1, 1.1, f32::NAN, f32::INFINITY] {
            assert!(burn_preview_draw(&mut image, "0.5,0.5", [255; 4], thickness).is_none());
        }
        assert!(preview_draw_points(&"0,0;".repeat(20_000)).is_some());
        assert!(preview_draw_points(&"0,0;".repeat(20_001)).is_none());
        assert!(burn_preview_draw(&mut image::RgbaImage::new(0, 20), "0,0", [255; 4], 0.1).is_none());
        assert_eq!(image, original);
    }

    #[test]
    fn preview_text_burns_white_letters_and_dark_outline() {
        let mut image = image::RgbaImage::from_pixel(640, 480, image::Rgba([120, 120, 120, 255]));
        burn_preview_text(&mut image, "Readable text", 0.2, 0.3).expect("a system font is required");
        assert!(image.pixels().any(|p| p[0] > 240 && p[1] > 240 && p[2] > 240));
        assert!(image.pixels().any(|p| p[0] < 30 && p[1] < 30 && p[2] < 30));
        assert_eq!(image.get_pixel(0, 0), &image::Rgba([120, 120, 120, 255]));
        assert!(burn_preview_text(&mut image, "", 0.5, 0.5).is_none());
        assert!(burn_preview_text(&mut image, "text", f32::NAN, 0.5).is_none());
    }

    #[tokio::test]
    async fn sent_image_is_cached_encrypted_and_decodable() {
        let id = format!("sent_image_test_{}", std::process::id());
        let vault_path = std::env::temp_dir().join(format!("{id}.db"));
        let mut vault = crate::vault::Vault::open_at(&vault_path.to_string_lossy()).unwrap();
        vault.open().unwrap();
        let key = vault.key_handle();
        let message = wa::Message::default();
        let image = image::DynamicImage::new_rgb8(2, 3);
        let mut bytes = std::io::Cursor::new(Vec::new());
        image.write_to(&mut bytes, image::ImageFormat::Png).unwrap();
        let bytes = bytes.into_inner();
        std::fs::create_dir_all(media_cache()).unwrap();

        let path = cache_sent(&key, &id, "image/png", &message, &bytes).await.unwrap();

        assert!(cached_path(&id, "image/png", &message).1);
        assert_ne!(std::fs::read(&path).unwrap(), bytes);
        let cached = read_cached(&key, &path).unwrap();
        assert_eq!(cached, bytes);
        let decoded = decode_bytes(&cached, BUBBLE_PX).unwrap();
        assert_eq!((decoded.w, decoded.h), (2, 3));

        std::fs::remove_file(path).unwrap();
        drop(vault);
        crate::paths::remove_db(&vault_path);
    }

    #[tokio::test]
    async fn sent_image_is_not_cached_without_an_unlocked_key() {
        let path = cache_sent(
            &KeyHandle::default(),
            "sent_image_locked_test",
            "image/png",
            &wa::Message::default(),
            b"image bytes",
        ).await;
        assert!(path.is_none());
    }
}


#[cfg(test)]
mod notification_tests {
    use super::notification_thumbnail;

    #[test]
    fn rejects_empty_thumbnail() {
        assert!(notification_thumbnail(&[]).is_none());
    }

    #[test]
    fn rejects_thumbnail_above_input_limit() {
        assert!(notification_thumbnail(&vec![0; 64 * 1024 + 1]).is_none());
    }

    #[test]
    fn rejects_invalid_image_at_input_limit() {
        assert!(notification_thumbnail(&vec![0; 64 * 1024]).is_none());
    }

    #[test]
    fn rejects_corrupt_image_headers() {
        for bytes in [&b"\xff\xd8\xff"[..], &b"\x89PNG\r\n\x1a\n"[..], &b"not an image"[..]] {
            assert!(notification_thumbnail(bytes).is_none());
        }
    }
}
