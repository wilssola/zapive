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

pub fn ext_for(mimetype: &str) -> &'static str {
    match mimetype.split(';').next().unwrap_or("").trim() {
        "image/jpeg" => "jpg",
        "image/png" => "png",
        "image/webp" => "webp",
        "image/gif" => "gif",
        "video/mp4" => "mp4",
        "video/3gpp" => "3gp",
        "audio/ogg" => "ogg",
        "audio/mpeg" => "mp3",
        "audio/mp4" => "m4a",
        "audio/wav" => "wav",
        "application/pdf" => "pdf",
        _ => "bin",
    }
}

pub fn cache_path(id: &str, mimetype: &str) -> PathBuf {
    media_cache().join(format!("{}.{}", sanitize(id), ext_for(mimetype)))
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

// Downloads (if missing) and returns the cached, encrypted file's path.
pub async fn ensure_cached(
    client: &std::sync::Arc<whatsapp_rust::client::Client>,
    key: &KeyHandle,
    id: &str,
    mimetype: &str,
    msg: &wa::Message,
) -> Option<PathBuf> {
    let path = cache_path(id, mimetype);
    if tokio::fs::try_exists(&path).await.unwrap_or(false) {
        return Some(path);
    }
    let target = downloadable(msg)?;
    match client.download(target).await {
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

pub fn read_cached(key: &KeyHandle, path: &PathBuf) -> Option<Vec<u8>> {
    let data = std::fs::read(path).ok()?;
    key.decrypt_bytes(&data).ok()
}

// Decrypted copy for consumers that need a real file (FFmpeg, the OS
// opener). Lives under .tmp, which is wiped at boot.
pub fn temp_plain(key: &KeyHandle, path: &PathBuf) -> Option<PathBuf> {
    let dir = media_cache().join(".tmp");
    let _ = std::fs::create_dir_all(&dir);
    let out = dir.join(path.file_name()?);
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
    let resized = img.resize_to_fill(size, size, imageops::FilterType::Triangle);
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
