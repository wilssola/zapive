// Video without FFmpeg: the `mp4` crate demuxes, openh264 (compiled from
// source by cargo) decodes and encodes H.264, and GIFs go through the
// pure-Rust `image` decoder. WhatsApp media is H.264/MP4 throughout, so
// this covers the real traffic; exotic containers simply get no preview.
use crate::media::Decoded;
use std::io::{BufReader, Read};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

fn is_gif(path: &Path) -> bool {
    let mut magic = [0u8; 4];
    std::fs::File::open(path)
        .and_then(|mut f| f.read_exact(&mut magic).map(|_| ()))
        .map(|_| &magic[..3] == b"GIF")
        .unwrap_or(false)
}

// OpenH264 with the crate's default "flush after every decode" pulls
// pictures out of the reorder buffer early. On a clip with B-frames
// (anything a phone's camera or a screen recorder writes) that works
// for a hundred-odd pictures and then the decoder errors out for the
// rest of the file. Without it the decoder hands pictures back when
// they are due, in display order.
fn new_decoder() -> Option<openh264::decoder::Decoder> {
    let config = openh264::decoder::DecoderConfig::new()
        .flush_after_decode(openh264::decoder::Flush::NoFlush);
    openh264::decoder::Decoder::with_api_config(openh264::OpenH264API::from_source(), config).ok()
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

// The clip's first picture, upright, for the bubble and the player's
// backdrop while it loads.
pub fn poster(path: &Path, target_w: u32) -> Option<Decoded> {
    let turns = open_mp4(path)
        .and_then(|reader| h264_track(&reader).and_then(|id| reader.tracks().get(&id).map(quarter_turns)))
        .unwrap_or(0);
    let first = frames(path, target_w, 0.0, 1).into_iter().next()?;
    if turns == 0 {
        return Some(first);
    }
    let img = image::RgbaImage::from_raw(first.w, first.h, first.rgba)?;
    let img = match turns {
        1 => image::imageops::rotate90(&img),
        2 => image::imageops::rotate180(&img),
        _ => image::imageops::rotate270(&img),
    };
    Some(Decoded { w: img.width(), h: img.height(), rgba: img.into_raw() })
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
    let Some(mut decoder) = new_decoder() else { return out };
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

// ---- the video player ----
//
// Streams a clip instead of decoding it whole: a thread walks the samples,
// decodes each one as its time comes up against a clock, and hands the
// picture to the UI. That is what makes pause, seek and clips of any
// length possible -- the player this replaces decoded up to 900 frames
// of RGBA before showing the first one, at 15fps and 560px.
//
// The soundtrack is decoded whole on a second thread (it is small next to
// the video) and played by the UI side's audio::Player; the `resync` flag
// on a frame is where the two are lined up again after a start or a seek.

pub enum PlayerEvent {
    // The clip is open: picture size as displayed and length in seconds.
    Ready { w: u32, h: u32, duration: f64 },
    Frame { frame: Decoded, pos: f64, resync: bool },
    Audio(crate::audio::AudioBuffer),
    Ended,
    Failed,
}

enum Ctl {
    Pause,
    Resume,
    Seek(f64),
}

// Dropping this stops the threads.
pub struct Playback {
    tx: std::sync::mpsc::Sender<Ctl>,
    busy: Arc<AtomicBool>,
}

impl Playback {
    // `sealed` is the encrypted cache file; it is opened into a plain
    // temp file on the player's own thread, off the UI.
    pub fn start(
        key: crate::vault::KeyHandle,
        sealed: std::path::PathBuf,
        emit: impl Fn(PlayerEvent) + Send + Sync + 'static,
    ) -> Playback {
        let (tx, rx) = std::sync::mpsc::channel();
        let busy = Arc::new(AtomicBool::new(false));
        let shown = busy.clone();
        let emit = Arc::new(emit);
        let _ = std::thread::Builder::new().name("zapive-video-player".into()).spawn(move || {
            let Some(plain) = crate::media::temp_plain(&key, &sealed) else {
                emit(PlayerEvent::Failed);
                return;
            };
            {
                let (plain, emit) = (plain.clone(), emit.clone());
                let _ = std::thread::Builder::new().name("zapive-video-audio".into()).spawn(
                    move || {
                        if let Some(buffer) = crate::audio::decode_with_tempo(&plain, 1.0) {
                            emit(PlayerEvent::Audio(buffer));
                        }
                    },
                );
            }
            if play_loop(&plain, &rx, &shown, &*emit).is_none() {
                emit(PlayerEvent::Failed);
            }
        });
        Playback { tx, busy }
    }

    pub fn pause(&self) {
        let _ = self.tx.send(Ctl::Pause);
    }

    pub fn resume(&self) {
        let _ = self.tx.send(Ctl::Resume);
    }

    pub fn seek(&self, secs: f64) {
        let _ = self.tx.send(Ctl::Seek(secs));
    }

    // The UI drew the last frame it was handed and can take another. One
    // in flight at a time: a UI that falls behind skips frames instead of
    // queueing megabytes of them.
    pub fn frame_shown(&self) {
        self.busy.store(false, Ordering::Release);
    }
}

// When each sample is shown and whether decoding can start at it.
struct SampleTable {
    // Presentation time per sample, in decode order (seconds).
    pts: Vec<f64>,
    sync: Vec<bool>,
    // The same times in display order: the decoder hands pictures back in
    // that order, so the n-th picture out is shown at the n-th of these.
    shown_at: Vec<f64>,
    duration: f64,
}

impl SampleTable {
    fn read(track: &mp4::Mp4Track) -> Option<SampleTable> {
        let stbl = &track.trak.mdia.minf.stbl;
        let scale = track.timescale().max(1) as f64;
        let count = track.sample_count() as usize;
        let mut dts = Vec::with_capacity(count);
        let mut at = 0u64;
        for entry in &stbl.stts.entries {
            for _ in 0..entry.sample_count {
                dts.push(at);
                at += entry.sample_delta as u64;
            }
        }
        // A fragmented file keeps its timing in the fragments; nothing
        // WhatsApp sends is one, and without a table there is no seeking.
        if dts.len() < count || count == 0 {
            return None;
        }
        dts.truncate(count);
        let mut offsets = Vec::with_capacity(count);
        if let Some(ctts) = &stbl.ctts {
            for entry in &ctts.entries {
                for _ in 0..entry.sample_count {
                    offsets.push(entry.sample_offset as i64);
                }
            }
        }
        offsets.resize(count, 0);
        let raw: Vec<i64> = dts.iter().zip(&offsets).map(|(d, o)| *d as i64 + o).collect();
        // Encoders with B-frames shift everything by the reorder delay;
        // the first picture still belongs at zero.
        let first = raw.iter().copied().min().unwrap_or(0);
        let pts: Vec<f64> = raw.iter().map(|p| (p - first) as f64 / scale).collect();
        let sync = match &stbl.stss {
            Some(stss) => {
                let mut sync = vec![false; count];
                for &n in &stss.entries {
                    if let Some(slot) = sync.get_mut((n as usize).wrapping_sub(1)) {
                        *slot = true;
                    }
                }
                sync[0] = true;
                sync
            }
            None => vec![true; count],
        };
        let mut shown_at = pts.clone();
        shown_at.sort_by(|a, b| a.total_cmp(b));
        Some(SampleTable { pts, sync, shown_at, duration: at as f64 / scale })
    }

    // The last sync sample at or before `secs` (0-based).
    fn sync_before(&self, secs: f64) -> usize {
        let mut best = 0;
        for i in 0..self.pts.len() {
            if self.sync[i] {
                if self.pts[i] <= secs + 1e-6 {
                    best = i;
                } else {
                    break;
                }
            }
        }
        best
    }
}

// Quarter turns a phone recorded the clip with: the pixels are stored as
// the sensor saw them and the track's matrix says how to stand them up.
fn quarter_turns(track: &mp4::Mp4Track) -> u8 {
    let m = &track.trak.tkhd.matrix;
    // 16.16 fixed point; only the signs matter for a right-angle turn.
    match (m.a.signum(), m.b.signum(), m.c.signum(), m.d.signum()) {
        (0, 1, -1, 0) => 1,
        (-1, 0, 0, -1) => 2,
        (0, -1, 1, 0) => 3,
        _ => 0,
    }
}

// Pictures wider than this are scaled down on the way to the UI: past it
// the upload per frame costs more than the eye gets back in a window.
const PLAYER_MAX_W: u32 = 1920;

fn to_picture(yuv: &openh264::decoder::DecodedYUV<'_>, turns: u8) -> Decoded {
    use openh264::formats::YUVSource;
    let (w, h) = yuv.dimensions();
    let mut rgba = vec![0u8; w * h * 4];
    yuv.write_rgba8(&mut rgba);
    let mut img = image::RgbaImage::from_raw(w as u32, h as u32, rgba)
        .unwrap_or_else(|| image::RgbaImage::new(1, 1));
    if img.width() > PLAYER_MAX_W {
        let th = (img.height() as u64 * PLAYER_MAX_W as u64 / img.width() as u64).max(1) as u32;
        img = image::imageops::resize(&img, PLAYER_MAX_W, th, image::imageops::FilterType::Triangle);
    }
    let img = match turns {
        1 => image::imageops::rotate90(&img),
        2 => image::imageops::rotate180(&img),
        3 => image::imageops::rotate270(&img),
        _ => img,
    };
    Decoded { w: img.width(), h: img.height(), rgba: img.into_raw() }
}

// None when the clip cannot be played at all.
fn play_loop(
    plain: &Path,
    rx: &std::sync::mpsc::Receiver<Ctl>,
    busy: &AtomicBool,
    emit: &(dyn Fn(PlayerEvent) + Send + Sync),
) -> Option<()> {
    use std::sync::mpsc::{RecvTimeoutError, TryRecvError};
    use std::time::{Duration, Instant};

    // The clock and what the commands do to it. `origin` is the instant
    // position zero would have been at, so "now" is origin.elapsed().
    struct Transport {
        playing: bool,
        ended: bool,
        position: f64,
        origin: Instant,
        seek: Option<f64>,
        resync: bool,
    }
    impl Transport {
        fn apply(&mut self, ctl: Ctl) {
            match ctl {
                Ctl::Pause => {
                    if self.playing {
                        self.position = self.origin.elapsed().as_secs_f64();
                        self.playing = false;
                    }
                }
                Ctl::Resume => {
                    if !self.playing {
                        // Play at the end means play again.
                        if self.ended {
                            self.seek = Some(0.0);
                        }
                        self.origin = Instant::now() - Duration::from_secs_f64(self.position);
                        self.playing = true;
                        self.resync = true;
                    }
                }
                Ctl::Seek(to) => self.seek = Some(to),
            }
        }
    }

    let mut reader = open_mp4(plain)?;
    let track_id = h264_track(&reader)?;
    let (table, sps, pps, turns, size) = {
        let track = reader.tracks().get(&track_id)?;
        let table = SampleTable::read(track)?;
        let sps = track.sequence_parameter_set().ok()?.to_vec();
        let pps = track.picture_parameter_set().ok()?.to_vec();
        (table, sps, pps, quarter_turns(track), (track.width() as u32, track.height() as u32))
    };
    let (w, h) = if turns % 2 == 1 { (size.1, size.0) } else { size };
    emit(PlayerEvent::Ready { w, h, duration: table.duration });

    let count = table.pts.len();
    let mut decoder = new_decoder()?;
    let mut annexb = Vec::new();
    // Next sample to feed, and which display slot the next picture fills.
    let mut next = 0usize;
    let mut slot = 0usize;
    // Pictures before this are decoded for the ones after them to build
    // on, and thrown away: a seek lands mid-GOP.
    let mut skip_until = 0.0f64;
    // A seek while paused still has to show where it landed.
    let mut show_one = false;
    let mut dropped = 0u32;
    // Samples in a row the decoder refused.
    let mut broken = 0u32;
    let mut t = Transport {
        playing: true,
        ended: false,
        position: 0.0,
        origin: Instant::now(),
        seek: None,
        resync: true,
    };

    loop {
        // Commands first, so a seek never waits behind a frame's sleep.
        loop {
            match rx.try_recv() {
                Ok(ctl) => t.apply(ctl),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return Some(()),
            }
        }
        if let Some(to) = t.seek.take() {
            // No further than the last picture, so a seek to the very end
            // still has something to show.
            let to = to.clamp(0.0, table.shown_at.last().copied().unwrap_or(0.0));
            let key = table.sync_before(to);
            // A fresh decoder: the old one holds reference pictures from
            // somewhere else in the clip.
            decoder = new_decoder()?;
            next = key;
            slot = table.shown_at.partition_point(|&at| at + 1e-6 < table.pts[key]);
            skip_until = to;
            t.position = to;
            t.origin = Instant::now() - Duration::from_secs_f64(to);
            t.resync = true;
            t.ended = false;
            show_one = !t.playing;
        }
        if t.ended || (!t.playing && !show_one) {
            // Parked: nothing to do until a command arrives.
            match rx.recv_timeout(Duration::from_millis(250)) {
                Ok(ctl) => t.apply(ctl),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return Some(()),
            }
            continue;
        }
        if next >= count {
            // Whatever the decoder still held back for reordering is a
            // few pictures at the very end; the clip is over.
            log::info!("[video] end of clip: sample {next} of {count}, picture {slot}");
            t.ended = true;
            t.playing = false;
            t.position = table.duration;
            show_one = false;
            emit(PlayerEvent::Ended);
            continue;
        }

        let sample = match reader.read_sample(track_id, next as u32 + 1) {
            Ok(Some(sample)) => sample,
            other => {
                log::warn!(
                    "[video] sample {} of {count} cannot be read ({:?}); ending the clip there",
                    next + 1,
                    other.err()
                );
                next = count;
                continue;
            }
        };
        annexb.clear();
        if table.sync[next] {
            annexb.extend_from_slice(&[0, 0, 0, 1]);
            annexb.extend_from_slice(&sps);
            annexb.extend_from_slice(&[0, 0, 0, 1]);
            annexb.extend_from_slice(&pps);
        }
        avcc_to_annexb(&sample.bytes, &mut annexb);
        next += 1;
        let yuv = match decoder.decode(&annexb) {
            Ok(Some(yuv)) => yuv,
            // Held back for reordering; it comes out with a later sample.
            Ok(None) => continue,
            Err(e) => {
                // Without this the loop would race through a damaged
                // stretch unpaced and "end" the clip in an instant.
                broken += 1;
                if broken == 1 {
                    log::warn!("[video] sample {next} of {count} does not decode: {e}");
                }
                if broken > 90 {
                    return None;
                }
                slot += 1;
                continue;
            }
        };
        broken = 0;
        let at = table.shown_at.get(slot).copied().unwrap_or(table.duration);
        slot += 1;
        if at + 1e-6 < skip_until {
            continue;
        }

        if show_one {
            // Paused: this is the picture the seek landed on.
            show_one = false;
            t.position = at;
            t.resync = false;
            busy.store(true, Ordering::Release);
            emit(PlayerEvent::Frame { frame: to_picture(&yuv, turns), pos: at, resync: true });
            continue;
        }
        if t.resync {
            // Decoding up to a seek target takes a moment the clock
            // should not count: start it where the picture is.
            t.origin = Instant::now() - Duration::from_secs_f64(at);
        }
        // Wait for the picture's time, in slices short enough that a
        // command is never more than a frame away.
        let mut interrupted = false;
        loop {
            let now = t.origin.elapsed().as_secs_f64();
            if now + 0.002 >= at {
                break;
            }
            match rx.recv_timeout(Duration::from_secs_f64((at - now).min(0.02))) {
                Ok(ctl) => {
                    t.apply(ctl);
                    interrupted = true;
                    break;
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return Some(()),
            }
        }
        if interrupted {
            // The command decides what comes next; this one picture is
            // simply not shown.
            continue;
        }
        // Running late (a slow machine, a huge clip): drop pictures
        // rather than the pace, so the sound stays in step. A machine
        // that cannot keep up at all gets a picture anyway every few
        // frames, with the clock moved to it and the sound brought along.
        let late = t.origin.elapsed().as_secs_f64() - at;
        if !t.resync && (late > 0.12 || busy.load(Ordering::Acquire)) {
            dropped += 1;
            if dropped < 8 {
                continue;
            }
            if late > 0.12 {
                t.origin = Instant::now() - Duration::from_secs_f64(at);
                t.resync = true;
            }
        }
        dropped = 0;
        busy.store(true, Ordering::Release);
        emit(PlayerEvent::Frame { frame: to_picture(&yuv, turns), pos: at, resync: t.resync });
        t.resync = false;
    }
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
