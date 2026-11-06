// Background work that only borrows the shared runtime, not the WhatsApp
// session: decoding, link previews, GIF search, updates, PDF engine.
use crate::bridge::wa_apply as ui_apply;
use crate::wa::SESSION_EPOCH;
use std::sync::atomic::Ordering;
use std::sync::{Arc, LazyLock, Mutex, RwLock};

// The vault key for the encrypted media cache; set at boot, empty before.
static MEDIA_KEY: LazyLock<RwLock<crate::vault::KeyHandle>> = LazyLock::new(Default::default);
static LINK_CACHE: LazyLock<Arc<Mutex<crate::link_preview::Cache>>> =
    LazyLock::new(|| Arc::new(Mutex::new(crate::link_preview::Cache::default())));
static LINK_SEM: LazyLock<Arc<tokio::sync::Semaphore>> =
    LazyLock::new(|| Arc::new(tokio::sync::Semaphore::new(2)));

pub fn set_media_key(key: crate::vault::KeyHandle) {
    if let Ok(mut slot) = MEDIA_KEY.write() {
        *slot = key;
    }
}

fn media_key() -> crate::vault::KeyHandle {
    MEDIA_KEY.read().map(|k| k.clone()).unwrap_or_default()
}

pub fn decode_preview(id: String, bytes: Vec<u8>) {
    let Some(rt) = crate::RT.get().map(|rt| rt.handle()) else {
        return;
    };
    rt.spawn(async move {
        let img = tokio::task::spawn_blocking(move || {
            let image = image::load_from_memory(&bytes).ok()?.blur(8.0).to_rgba8();
            Some(crate::media::Decoded {
                w: image.width(),
                h: image.height(),
                rgba: image.into_raw(),
            })
        })
        .await
        .ok()
        .flatten();
        ui_apply(move |b| b.on_media_preview(&id, img));
    });
}

pub fn link_preview(jid: String, id: String, text: String, url: String) {
    // Read now, as the executor did on receipt; see wa::SESSION_EPOCH.
    let epoch = SESSION_EPOCH.load(Ordering::SeqCst);
    let Some(rt) = crate::RT.get().map(|rt| rt.handle()) else {
        return;
    };
    let cache = LINK_CACHE.clone();
    let sem = LINK_SEM.clone();
    rt.spawn(async move {
        let permit =
            tokio::time::timeout(std::time::Duration::from_secs(20), sem.acquire_owned()).await;
        let preview = if let Ok(Ok(permit)) = permit {
            tokio::task::spawn_blocking(move || {
                let _permit = permit;
                if SESSION_EPOCH.load(Ordering::SeqCst) != epoch {
                    return None;
                }
                cache.lock().ok()?.get(&url, epoch)
            })
            .await
            .ok()
            .flatten()
        } else {
            None
        };
        crate::bridge::wa_apply_at(epoch, move |b| b.on_link_preview(&jid, &id, &text, preview));
    });
}

// Decodes an embedded thumbnail (link previews, video posters).
pub fn decode_thumb(id: String, bytes: Vec<u8>, link: bool) {
    // Read now, as the executor did on receipt; see wa::SESSION_EPOCH.
    let epoch = SESSION_EPOCH.load(Ordering::SeqCst);
    let Some(rt) = crate::RT.get().map(|rt| rt.handle()) else {
        return;
    };
    rt.spawn(async move {
        if let Some(img) = tokio::task::spawn_blocking(move || {
            crate::media::decode_bytes(&bytes, crate::media::BUBBLE_PX)
        })
        .await
        .ok()
        .flatten()
        {
            crate::bridge::wa_apply_at(epoch, move |b| b.on_thumb(&id, link, img));
        } else if link {
            crate::bridge::wa_apply_at(epoch, move |b| b.on_media_missing(&id));
        }
    });
}

// Stickers and GIFs (the video player runs its own threads).
pub fn gif_search(query: String) {
    let Some(rt) = crate::RT.get().map(|rt| rt.handle()) else {
        return;
    };
    rt.spawn(async move {
        let results = tokio::task::spawn_blocking(move || {
            let q = if query.trim().is_empty() {
                "funny"
            } else {
                query.trim()
            };
            let url = format!(
                "https://api.openverse.org/v1/images/?extension=gif&page_size=20&q={}",
                urlencode(q)
            );
            let mut res = ureq::get(&url).header("User-Agent", "Zapive").call().ok()?;
            let text = res.body_mut().read_to_string().ok()?;
            let json: serde_json::Value = serde_json::from_str(&text).ok()?;
            let mut out = Vec::new();
            for item in json.get("results")?.as_array()?.iter().take(12) {
                let id = item.get("id")?.as_str()?.to_string();
                let gif_url = item.get("url")?.as_str()?.to_string();
                let thumb_url = item
                    .get("thumbnail")
                    .and_then(|t| t.as_str())
                    .unwrap_or(&gif_url)
                    .to_string();
                let Some(mut thumb) = ureq::get(&thumb_url)
                    .header("User-Agent", "Zapive")
                    .call()
                    .ok()
                else {
                    continue;
                };
                let Some(bytes) = thumb.body_mut().read_to_vec().ok() else {
                    continue;
                };
                if let Some(img) = crate::media::decode_cover(&bytes, 84) {
                    out.push((id, gif_url, img));
                }
            }
            Some(out)
        })
        .await
        .ok()
        .flatten()
        .unwrap_or_default();
        ui_apply(move |b| b.on_gif_results(results));
    });
}

pub fn zoom_frames(id: String, path: std::path::PathBuf) {
    let Some(rt) = crate::RT.get().map(|rt| rt.handle()) else {
        return;
    };
    let key = media_key();
    rt.spawn(async move {
        let frames = tokio::task::spawn_blocking(move || {
            crate::media::temp_plain(&key, &path).map(|p| crate::video::frames(&p, 720, 15.0, 90))
        })
        .await
        .ok()
        .flatten()
        .unwrap_or_default();
        if !frames.is_empty() {
            ui_apply(move |b| b.on_zoom_frames(&id, frames));
        }
    });
}

// Self-update.
pub fn check_update() {
    let Some(rt) = crate::RT.get().map(|rt| rt.handle()) else {
        return;
    };
    rt.spawn(async move {
        let result = tokio::task::spawn_blocking(crate::update::check)
            .await
            .unwrap_or_else(|_| Err("update check task failed".into()));
        // Direct apply: the banner must show on the lock
        // and login screens, not wait in the backlog.
        crate::bridge::ui_apply(move |b| b.on_update_checked(result));
    });
}

pub fn apply_update() {
    let Some(rt) = crate::RT.get().map(|rt| rt.handle()) else {
        return;
    };
    rt.spawn(async move {
        let result = tokio::task::spawn_blocking(crate::update::apply)
            .await
            .unwrap_or_else(|e| Err(e.to_string()));
        crate::bridge::ui_apply(move |b| b.on_update_applied(result));
    });
}

// Voice notes: full decode at a speed, waveform bars.
pub fn audio_decode(id: String, plain: std::path::PathBuf, rate_idx: usize, rate: f64) {
    let Some(rt) = crate::RT.get().map(|rt| rt.handle()) else {
        return;
    };
    rt.spawn(async move {
        let buffer =
            tokio::task::spawn_blocking(move || crate::audio::decode_with_tempo(&plain, rate))
                .await
                .ok()
                .flatten();
        match buffer {
            Some(buffer) => ui_apply(move |b| b.on_audio_ready(&id, rate_idx, buffer)),
            None => eprintln!("[audio] decode failed for {id}"),
        }
    });
}

pub fn waveform(id: String, path: std::path::PathBuf) {
    let Some(rt) = crate::RT.get().map(|rt| rt.handle()) else {
        return;
    };
    let key = media_key();
    rt.spawn(async move {
        let wave = tokio::task::spawn_blocking(move || {
            crate::media::temp_plain(&key, &path).and_then(|p| crate::audio::waveform(&p))
        })
        .await
        .ok()
        .flatten();
        if let Some(img) = wave {
            ui_apply(move |b| b.on_wave(&id, img));
        }
    });
}

// Downloads pdfium into the data directory for the PDF viewer.
pub fn pdf_engine_install() {
    let Some(rt) = crate::RT.get().map(|rt| rt.handle()) else {
        return;
    };
    rt.spawn(async move {
        let result = tokio::task::spawn_blocking(crate::pdf::install_engine)
            .await
            .unwrap_or_else(|e| Err(e.to_string()));
        ui_apply(move |b| b.on_pdf_engine_installed(result));
    });
}

// The full-resolution decode of a cached photo, for the lightbox.
pub fn decode_full(id: String, path: std::path::PathBuf) {
    let Some(rt) = crate::RT.get().map(|rt| rt.handle()) else {
        return;
    };
    let key = media_key();
    rt.spawn(async move {
        let img = tokio::task::spawn_blocking(move || {
            crate::media::read_cached(&key, &path)
                .and_then(|d| crate::media::decode_bytes(&d, crate::media::FULL_PX))
        })
        .await
        .ok()
        .flatten();
        if let Some(img) = img {
            ui_apply(move |b| b.on_full_image(&id, img));
        }
    });
}

fn urlencode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_encoding_uses_form_spaces_and_utf8_bytes() {
        assert_eq!(urlencode(""), "");
        assert_eq!(urlencode("Az09-_.~"), "Az09-_.~");
        assert_eq!(urlencode("a b+c/&"), "a+b%2Bc%2F%26");
        assert_eq!(urlencode("é"), "%C3%A9");
    }
}
