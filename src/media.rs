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
    let (path, exists) = cached_path(id, mimetype, msg);
    if exists {
        return Some(path);
    }
    let target = downloadable(msg)?;
    wait_for_backoff().await;
    let mut attempt = {
        let _permit = sem.acquire().await;
        client.download(target).await
    };
    if attempt.as_ref().is_err_and(|e| rate_limited(&e.to_string())) {
        // The server is throttling media: back everything off, then try
        // this one once more.
        note_rate_limit();
        wait_for_backoff().await;
        let _permit = sem.acquire().await;
        attempt = client.download(target).await;
    }
    match attempt {
        Ok(bytes) => {
            let sealed = key.encrypt_bytes(&bytes);
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
