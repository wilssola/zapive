// Camera capture and H.264 encoding for video calls. whatsapp-rust never
// touches pixels: it wants complete Annex-B access units in, and hands
// reassembled peer access units back out, so both the encoder and the
// decoder live here.
//
// nokhwa's own MJPEG decoding is switched off (it links C mozjpeg); the
// `image` crate already does JPEG in pure Rust, and the other webcam
// formats convert with a few lines of arithmetic.
use crate::media::Decoded;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use whatsapp_rust::async_channel;

use nokhwa::pixel_format::RgbFormat;
use nokhwa::utils::{
    CameraFormat, CameraIndex, FrameFormat, RequestedFormat, RequestedFormatType, Resolution,
};

// WhatsApp's compatibility cadence. The bare-channel video source assumes
// 15 fps too, so the RTP stride matches without extra plumbing.
pub const FPS: u32 = 15;
// Well under WhatsApp's 720p ceiling: the peer sees a desktop webcam, and
// the encoder keeps up on one thread.
const CAPTURE_W: u32 = 640;
const CAPTURE_H: u32 = 480;
const BITRATE_BPS: u32 = 600_000;
// A fresh IDR every two seconds, so a peer joining or recovering from loss
// gets a decodable picture quickly.
const KEYFRAME_EVERY: u32 = FPS * 2;
// The self-view is decoration; keep it small so it costs little memory.
const PREVIEW_W: u32 = 240;

// One picture in flight per direction. A decoded frame is megabytes of
// RGBA, so a UI thread that falls behind must drop frames rather than let
// them queue. The call screen clears these once it has shown the frame.
pub static PREVIEW_BUSY: AtomicBool = AtomicBool::new(false);
pub static REMOTE_BUSY: AtomicBool = AtomicBool::new(false);

// One camera, listed for the settings picker.
pub struct CameraOption {
    pub name: String,
    pub index: CameraIndex,
}

// Cameras the system reports, in the order the backend lists them.
pub fn cameras() -> Vec<CameraOption> {
    match nokhwa::query(nokhwa::utils::ApiBackend::Auto) {
        Ok(found) => found
            .into_iter()
            .map(|info| CameraOption { name: info.human_name(), index: info.index().clone() })
            .collect(),
        Err(e) => {
            log::warn!("[camera] cannot list devices: {e}");
            Vec::new()
        }
    }
}

fn index_for(name: Option<&str>) -> CameraIndex {
    let wanted = name.unwrap_or_default();
    if !wanted.is_empty()
        && let Some(found) = cameras().into_iter().find(|c| c.name == wanted)
    {
        return found.index;
    }
    CameraIndex::Index(0)
}

// The call side of the camera: encoded access units plus the switch that
// stops the capture thread.
pub struct CameraFeed {
    frames: async_channel::Receiver<Vec<u8>>,
    stop: Arc<AtomicBool>,
    // Bumped by the call when the peer asks for a keyframe.
    force_key: Arc<AtomicBool>,
}

impl CameraFeed {
    // Opens the camera on a thread of its own and encodes every frame.
    // `preview` receives a small copy for the local self-view.
    pub fn start(
        device: Option<&str>,
        preview: impl Fn(Decoded) + Send + 'static,
    ) -> Option<CameraFeed> {
        // Two frames of slack: video is loss tolerant and a deep queue only
        // buys latency.
        let (tx, rx) = async_channel::bounded::<Vec<u8>>(2);
        let stop = Arc::new(AtomicBool::new(false));
        let force_key = Arc::new(AtomicBool::new(false));
        let index = index_for(device);
        let (ready_tx, ready_rx) = std::sync::mpsc::channel::<bool>();
        {
            let stop = stop.clone();
            let force_key = force_key.clone();
            std::thread::Builder::new()
                .name("zapive-camera".into())
                .spawn(move || capture(index, tx, stop, force_key, preview, ready_tx))
                .ok()?;
        }
        if !ready_rx.recv().unwrap_or(false) {
            stop.store(true, Ordering::SeqCst);
            return None;
        }
        Some(CameraFeed { frames: rx, stop, force_key })
    }

    pub fn source(&self) -> async_channel::Receiver<Vec<u8>> {
        self.frames.clone()
    }

    // The peer lost the picture and wants a fresh IDR.
    pub fn request_keyframe(&self) {
        self.force_key.store(true, Ordering::Relaxed);
    }
}

impl Drop for CameraFeed {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        self.frames.close();
    }
}

fn capture(
    index: CameraIndex,
    tx: async_channel::Sender<Vec<u8>>,
    stop: Arc<AtomicBool>,
    force_key: Arc<AtomicBool>,
    preview: impl Fn(Decoded),
    ready: std::sync::mpsc::Sender<bool>,
) {
    let wanted = RequestedFormat::new::<RgbFormat>(RequestedFormatType::Closest(CameraFormat::new(
        Resolution::new(CAPTURE_W, CAPTURE_H),
        FrameFormat::NV12,
        FPS,
    )));
    let mut camera = match nokhwa::Camera::new(index, wanted) {
        Ok(camera) => camera,
        Err(e) => {
            log::error!("[camera] cannot open: {e}");
            let _ = ready.send(false);
            return;
        }
    };
    if let Err(e) = camera.open_stream() {
        log::error!("[camera] cannot start the stream: {e}");
        let _ = ready.send(false);
        return;
    }
    // H.264 needs even dimensions; the camera may not have given us the
    // resolution we asked for.
    let resolution = camera.resolution();
    let (w, h) = ((resolution.width() / 2) * 2, (resolution.height() / 2) * 2);
    if w == 0 || h == 0 {
        let _ = ready.send(false);
        return;
    }
    let config = openh264::encoder::EncoderConfig::new()
        .bitrate(openh264::encoder::BitRate::from_bps(BITRATE_BPS))
        .max_frame_rate(openh264::encoder::FrameRate::from_hz(FPS as f32))
        .usage_type(openh264::encoder::UsageType::CameraVideoRealTime)
        // WhatsApp decodes Constrained Baseline; anything richer risks a
        // peer that cannot play it.
        .profile(openh264::encoder::Profile::Baseline)
        .skip_frames(true);
    let mut encoder = match openh264::encoder::Encoder::with_api_config(
        openh264::OpenH264API::from_source(),
        config,
    ) {
        Ok(encoder) => encoder,
        Err(e) => {
            log::error!("[camera] cannot start the encoder: {e}");
            let _ = ready.send(false);
            return;
        }
    };
    let _ = ready.send(true);

    let interval = std::time::Duration::from_micros(1_000_000 / FPS as u64);
    let mut since_keyframe = 0u32;
    let mut rgb: Vec<u8> = Vec::new();
    while !stop.load(Ordering::SeqCst) {
        let started = std::time::Instant::now();
        let Ok(buffer) = camera.frame() else { continue };
        let source = buffer.resolution();
        if !to_rgb(buffer.buffer(), buffer.source_frame_format(), source, w, h, &mut rgb) {
            continue;
        }
        if since_keyframe >= KEYFRAME_EVERY || force_key.swap(false, Ordering::Relaxed) {
            encoder.force_intra_frame();
            since_keyframe = 0;
        }
        let slice = openh264::formats::RgbSliceU8::new(&rgb, (w as usize, h as usize));
        let yuv = openh264::formats::YUVBuffer::from_rgb_source(slice);
        if let Ok(bitstream) = encoder.encode(&yuv) {
            let au = bitstream.to_vec();
            if !au.is_empty() {
                // A full channel means the relay is behind; dropping the
                // frame is what video is supposed to do.
                let _ = tx.try_send(au);
            }
        }
        since_keyframe += 1;
        preview(shrink(&rgb, w, h));
        if let Some(rest) = interval.checked_sub(started.elapsed()) {
            std::thread::sleep(rest);
        }
    }
}

// Whatever the webcam speaks, cropped to `w`x`h` and written as RGB8.
// Returns false when the frame cannot be read as the format claims.
fn to_rgb(
    data: &[u8],
    format: FrameFormat,
    source: Resolution,
    w: u32,
    h: u32,
    out: &mut Vec<u8>,
) -> bool {
    let (sw, sh) = (source.width(), source.height());
    if sw < w || sh < h {
        return false;
    }
    out.clear();
    out.resize((w * h * 3) as usize, 0);
    // Reads one source pixel; every branch below crops from the top-left,
    // which is all the resizing a fixed-size capture needs.
    match format {
        FrameFormat::MJPEG => {
            let Ok(decoded) = image::load_from_memory_with_format(data, image::ImageFormat::Jpeg)
            else {
                return false;
            };
            let rgb = decoded.to_rgb8();
            if rgb.width() < w || rgb.height() < h {
                return false;
            }
            for y in 0..h {
                let src = (y * rgb.width() * 3) as usize;
                let dst = (y * w * 3) as usize;
                out[dst..dst + (w * 3) as usize]
                    .copy_from_slice(&rgb.as_raw()[src..src + (w * 3) as usize]);
            }
            true
        }
        FrameFormat::RAWRGB => copy_packed(data, sw, w, h, out, [0, 1, 2]),
        FrameFormat::RAWBGR => copy_packed(data, sw, w, h, out, [2, 1, 0]),
        FrameFormat::GRAY => {
            if data.len() < (sw * sh) as usize {
                return false;
            }
            for y in 0..h {
                for x in 0..w {
                    let value = data[(y * sw + x) as usize];
                    let dst = ((y * w + x) * 3) as usize;
                    out[dst..dst + 3].copy_from_slice(&[value, value, value]);
                }
            }
            true
        }
        FrameFormat::YUYV => {
            if data.len() < (sw * sh * 2) as usize {
                return false;
            }
            for y in 0..h {
                for x in 0..w {
                    let pair = ((y * sw + (x & !1)) * 2) as usize;
                    let luma = data[pair + if x % 2 == 0 { 0 } else { 2 }];
                    let (u, v) = (data[pair + 1], data[pair + 3]);
                    write_yuv(out, (y * w + x) as usize, luma, u, v);
                }
            }
            true
        }
        FrameFormat::NV12 => {
            let plane = (sw * sh) as usize;
            if data.len() < plane + plane / 2 {
                return false;
            }
            for y in 0..h {
                for x in 0..w {
                    let luma = data[(y * sw + x) as usize];
                    let chroma = plane + ((y / 2) * sw + (x & !1)) as usize;
                    write_yuv(out, (y * w + x) as usize, luma, data[chroma], data[chroma + 1]);
                }
            }
            true
        }
    }
}

fn copy_packed(data: &[u8], sw: u32, w: u32, h: u32, out: &mut [u8], order: [usize; 3]) -> bool {
    if data.len() < (sw * h * 3) as usize {
        return false;
    }
    for y in 0..h {
        for x in 0..w {
            let src = ((y * sw + x) * 3) as usize;
            let dst = ((y * w + x) * 3) as usize;
            for (channel, pick) in order.iter().enumerate() {
                out[dst + channel] = data[src + pick];
            }
        }
    }
    true
}

// BT.601, the range webcams and H.264 both assume.
fn write_yuv(out: &mut [u8], pixel: usize, y: u8, u: u8, v: u8) {
    let (y, u, v) = (y as f32, u as f32 - 128.0, v as f32 - 128.0);
    let dst = pixel * 3;
    out[dst] = (y + 1.402 * v).clamp(0.0, 255.0) as u8;
    out[dst + 1] = (y - 0.344_136 * u - 0.714_136 * v).clamp(0.0, 255.0) as u8;
    out[dst + 2] = (y + 1.772 * u).clamp(0.0, 255.0) as u8;
}

// Nearest-neighbour shrink for the self-view: it is drawn thumbnail-sized,
// so filtering would cost more than it shows.
fn shrink(rgb: &[u8], w: u32, h: u32) -> Decoded {
    let scale = (PREVIEW_W as f32 / w as f32).min(1.0);
    let (tw, th) = (((w as f32 * scale) as u32).max(1), ((h as f32 * scale) as u32).max(1));
    let mut rgba = vec![0u8; (tw * th * 4) as usize];
    for y in 0..th {
        let sy = y * h / th;
        for x in 0..tw {
            let sx = x * w / tw;
            let src = ((sy * w + sx) * 3) as usize;
            let dst = ((y * tw + x) * 4) as usize;
            rgba[dst..dst + 3].copy_from_slice(&rgb[src..src + 3]);
            rgba[dst + 3] = 255;
        }
    }
    Decoded { w: tw, h: th, rgba }
}

// ---- the receiving half ----

// Decodes the peer's access units. One decoder per call: H.264 is a
// stateful stream, so it cannot be rebuilt per frame.
pub struct RemoteVideo {
    decoder: openh264::decoder::Decoder,
    // Skips everything until the first keyframe; feeding a decoder
    // mid-GOP only produces garbage.
    started: bool,
    rgb: Vec<u8>,
}

impl RemoteVideo {
    pub fn new() -> Option<RemoteVideo> {
        let decoder = openh264::decoder::Decoder::new().ok()?;
        Some(RemoteVideo { decoder, started: false, rgb: Vec::new() })
    }

    // None while the stream has yet to produce a displayable picture.
    pub fn decode(&mut self, frame: &whatsapp_rust::voip::VideoFrame) -> Option<Decoded> {
        if !self.started {
            if !frame.keyframe {
                return None;
            }
            self.started = true;
        }
        use openh264::formats::YUVSource;
        let yuv = self.decoder.decode(&frame.data).ok()??;
        let (w, h) = yuv.dimensions();
        self.rgb.resize(w * h * 3, 0);
        yuv.write_rgb8(&mut self.rgb);
        let mut rgba = vec![0u8; w * h * 4];
        for (pixel, src) in self.rgb.chunks_exact(3).enumerate() {
            let dst = pixel * 4;
            rgba[dst..dst + 3].copy_from_slice(src);
            rgba[dst + 3] = 255;
        }
        Some(Decoded { w: w as u32, h: h as u32, rgba })
    }
}

