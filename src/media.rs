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
