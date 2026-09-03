// Video without FFmpeg: the `mp4` crate demuxes, openh264 (compiled from
// source by cargo) decodes and encodes H.264, and GIFs go through the
// pure-Rust `image` decoder. WhatsApp media is H.264/MP4 throughout, so
// this covers the real traffic; exotic containers simply get no preview.
use crate::media::Decoded;
use std::io::{BufReader, Read};
use std::path::Path;

fn is_gif(path: &Path) -> bool {
    let mut magic = [0u8; 4];
    std::fs::File::open(path)
        .and_then(|mut f| f.read_exact(&mut magic).map(|_| ()))
        .map(|_| &magic[..3] == b"GIF")
        .unwrap_or(false)
}

fn open_mp4(path: &Path) -> Option<mp4::Mp4Reader<BufReader<std::fs::File>>> {
    let file = std::fs::File::open(path).ok()?;
    let size = file.metadata().ok()?.len();
    mp4::Mp4Reader::read_header(BufReader::new(file), size).ok()
}

fn h264_track(reader: &mp4::Mp4Reader<BufReader<std::fs::File>>) -> Option<u32> {
    reader
        .tracks()
        .iter()
        .find(|(_, t)| matches!(t.media_type(), Ok(mp4::MediaType::H264)))
        .map(|(id, _)| *id)
}

pub fn probe_size(path: &Path) -> (u32, u32) {
    let fallback = (640, 360);
    if is_gif(path) {
        use image::ImageDecoder;
        return std::fs::File::open(path)
            .ok()
            .and_then(|f| image::codecs::gif::GifDecoder::new(BufReader::new(f)).ok())
            .map(|d| d.dimensions())
            .filter(|&(w, h)| w > 0 && h > 0)
            .unwrap_or(fallback);
    }
    let Some(reader) = open_mp4(path) else { return fallback };
    h264_track(&reader)
        .and_then(|id| reader.tracks().get(&id))
        .map(|t| (t.width() as u32, t.height() as u32))
        .filter(|&(w, h)| w > 0 && h > 0)
        .unwrap_or(fallback)
}

// AVCC samples carry length-prefixed NALs; the decoder wants Annex-B.
fn avcc_to_annexb(sample: &[u8], out: &mut Vec<u8>) {
    let mut i = 0usize;
    while i + 4 <= sample.len() {
        let len = u32::from_be_bytes([sample[i], sample[i + 1], sample[i + 2], sample[i + 3]])
            as usize;
        i += 4;
        if len == 0 || i + len > sample.len() {
            break;
        }
        out.extend_from_slice(&[0, 0, 0, 1]);
        out.extend_from_slice(&sample[i..i + len]);
        i += len;
    }
}

fn rgb_to_decoded(rgb: &[u8], w: u32, h: u32, target_w: u32) -> Decoded {
    let mut rgba = Vec::with_capacity((w * h * 4) as usize);
    for px in rgb.chunks(3) {
        rgba.extend_from_slice(&[px[0], px[1], px[2], 255]);
    }
    let img = image::RgbaImage::from_raw(w, h, rgba)
        .unwrap_or_else(|| image::RgbaImage::new(1, 1));
    if target_w >= w {
        return Decoded { w, h, rgba: img.into_raw() };
    }
    let tw = (target_w / 2) * 2;
    let th = (((h as u64 * tw as u64 / w as u64) as u32 / 2) * 2).max(2);
    let scaled =
        image::imageops::resize(&img, tw.max(2), th, image::imageops::FilterType::Triangle);
    Decoded { w: scaled.width(), h: scaled.height(), rgba: scaled.into_raw() }
}

fn gif_frames(path: &Path, target_w: u32, fps: f64, max_frames: usize) -> Vec<Decoded> {
    use image::AnimationDecoder;
    let mut out = Vec::new();
    let Ok(file) = std::fs::File::open(path) else { return out };
    let Ok(decoder) = image::codecs::gif::GifDecoder::new(BufReader::new(file)) else {
        return out;
    };
    let min_gap = if fps > 0.0 { 1.0 / fps } else { 0.0 };
    let mut ts = 0.0f64;
    let mut next_at = 0.0f64;
    for frame in decoder.into_frames() {
        let Ok(frame) = frame else { break };
        if out.len() >= max_frames {
            break;
        }
        let (num, den) = frame.delay().numer_denom_ms();
        let delay = num as f64 / den.max(1) as f64 / 1000.0;
        let keep = ts + 1e-6 >= next_at;
        ts += delay.max(0.01);
        if !keep {
            continue;
        }
        next_at = ts + min_gap;
        let img = frame.into_buffer();
        let (w, h) = (img.width(), img.height());
        if target_w < w {
            let tw = (target_w / 2) * 2;
            let th = (((h as u64 * tw as u64 / w as u64) as u32 / 2) * 2).max(2);
            let scaled = image::imageops::resize(
                &img,
                tw.max(2),
                th,
                image::imageops::FilterType::Triangle,
            );
            out.push(Decoded { w: scaled.width(), h: scaled.height(), rgba: scaled.into_raw() });
        } else {
            out.push(Decoded { w, h, rgba: img.into_raw() });
        }
    }
    out
}

// Decodes up to `max_frames` frames at roughly `fps`, scaled so the width
// is `target_w` (even, keeping aspect), as RGBA bitmaps.
pub fn frames(path: &Path, target_w: u32, fps: f64, max_frames: usize) -> Vec<Decoded> {
    if is_gif(path) {
        return gif_frames(path, target_w, fps, max_frames);
    }
    let mut out = Vec::new();
    let Some(mut reader) = open_mp4(path) else { return out };
    let Some(track_id) = h264_track(&reader) else { return out };
    let (sample_count, timescale, sps, pps) = {
        let Some(track) = reader.tracks().get(&track_id) else { return out };
        let (Ok(sps), Ok(pps)) = (track.sequence_parameter_set(), track.picture_parameter_set())
        else {
            return out;
        };
        (track.sample_count(), track.timescale().max(1), sps.to_vec(), pps.to_vec())
    };
    let Ok(mut decoder) = openh264::decoder::Decoder::new() else { return out };
    let min_gap = if fps > 0.0 { 1.0 / fps } else { 0.0 };
    let mut next_at = 0.0f64;
    let mut annexb = Vec::new();
    for i in 1..=sample_count {
        if out.len() >= max_frames {
            break;
        }
        let Ok(Some(sample)) = reader.read_sample(track_id, i) else { break };
        annexb.clear();
        if i == 1 {
            annexb.extend_from_slice(&[0, 0, 0, 1]);
            annexb.extend_from_slice(&sps);
            annexb.extend_from_slice(&[0, 0, 0, 1]);
            annexb.extend_from_slice(&pps);
        }
        avcc_to_annexb(&sample.bytes, &mut annexb);
        // Every frame feeds the decoder (references), but only the gated
        // ones are converted and kept.
        let Ok(Some(yuv)) = decoder.decode(&annexb) else { continue };
        let ts = sample.start_time as f64 / timescale as f64;
        if ts + 1e-6 < next_at {
            continue;
        }
        next_at = ts + min_gap;
        use openh264::formats::YUVSource;
        let (w, h) = yuv.dimensions();
        let mut rgb = vec![0u8; w * h * 3];
        yuv.write_rgb8(&mut rgb);
        out.push(rgb_to_decoded(&rgb, w as u32, h as u32, target_w));
    }
    out
}

// ---- gif -> gif-playback mp4 (what WhatsApp expects on the wire) ----

fn annexb_nals(bitstream: &[u8]) -> Vec<&[u8]> {
    let mut nals = Vec::new();
    let mut start = None;
    let mut i = 0usize;
    while i + 3 <= bitstream.len() {
        let (code, skip) = if i + 4 <= bitstream.len() && bitstream[i..i + 4] == [0, 0, 0, 1] {
            (true, 4)
        } else if bitstream[i..i + 3] == [0, 0, 1] {
            (true, 3)
        } else {
            (false, 1)
        };
        if code {
            if let Some(s) = start {
                nals.push(&bitstream[s..i]);
            }
            start = Some(i + skip);
            i += skip;
        } else {
            i += 1;
        }
    }
    if let Some(s) = start {
        nals.push(&bitstream[s..]);
    }
    nals
}

pub fn gif_to_mp4(src: &Path, dst: &Path) -> Option<()> {
    const FPS: u32 = 15;
    let frames = gif_frames(src, 640, FPS as f64, 450);
    let first = frames.first()?;
    let (w, h) = ((first.w / 2) * 2, (first.h / 2) * 2);
    if w == 0 || h == 0 {
        return None;
    }
    let mut encoder = openh264::encoder::Encoder::new().ok()?;

    let file = std::fs::File::create(dst).ok()?;
    let config = mp4::Mp4Config {
        major_brand: str::parse("isom").ok()?,
        minor_version: 512,
        compatible_brands: vec![
            str::parse("isom").ok()?,
            str::parse("iso2").ok()?,
            str::parse("avc1").ok()?,
            str::parse("mp41").ok()?,
        ],
        timescale: 1000,
    };
    let mut writer = mp4::Mp4Writer::write_start(std::io::BufWriter::new(file), &config).ok()?;
    let mut track_added = false;
    let mut pending: Vec<(Vec<u8>, bool)> = Vec::new();

    for (index, frame) in frames.iter().enumerate() {
        // Crop to even dimensions, drop alpha.
        let mut rgb = Vec::with_capacity((w * h * 3) as usize);
        for y in 0..h {
            for x in 0..w {
                let px = ((y * frame.w + x) * 4) as usize;
                rgb.extend_from_slice(&frame.rgba[px..px + 3]);
            }
        }
        let source = openh264::formats::RgbSliceU8::new(&rgb, (w as usize, h as usize));
        let yuv = openh264::formats::YUVBuffer::from_rgb_source(source);
        let Ok(bitstream) = encoder.encode(&yuv) else { continue };
        let raw = bitstream.to_vec();
        let mut sps: Option<Vec<u8>> = None;
        let mut pps: Option<Vec<u8>> = None;
        let mut sample = Vec::new();
        let mut is_sync = false;
        for nal in annexb_nals(&raw) {
            if nal.is_empty() {
                continue;
            }
            match nal[0] & 0x1F {
                7 => sps = Some(nal.to_vec()),
                8 => pps = Some(nal.to_vec()),
                kind => {
                    if kind == 5 {
                        is_sync = true;
                    }
                    sample.extend_from_slice(&(nal.len() as u32).to_be_bytes());
                    sample.extend_from_slice(nal);
                }
            }
        }
        if !track_added {
            let (Some(sps), Some(pps)) = (sps, pps) else { continue };
            writer
                .add_track(&mp4::TrackConfig {
                    track_type: mp4::TrackType::Video,
                    timescale: FPS,
                    language: "und".to_string(),
                    media_conf: mp4::MediaConfig::AvcConfig(mp4::AvcConfig {
                        width: w as u16,
                        height: h as u16,
                        seq_param_set: sps,
                        pic_param_set: pps,
                    }),
                })
                .ok()?;
            track_added = true;
        }
        if !sample.is_empty() {
            pending.push((sample, is_sync));
        }
        let _ = index;
    }
    if !track_added || pending.is_empty() {
        return None;
    }
    for (i, (sample, is_sync)) in pending.into_iter().enumerate() {
        writer
            .write_sample(
                1,
                &mp4::Mp4Sample {
                    start_time: i as u64,
                    duration: 1,
                    rendering_offset: 0,
                    is_sync,
                    bytes: bytes::Bytes::from(sample),
                },
            )
            .ok()?;
    }
    writer.write_end().ok()?;
    Some(())
}
