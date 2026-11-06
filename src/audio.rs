// Voice-note audio, fully in-process and FFmpeg-free: opus-pure (a Rust
// port of libopus) handles the ogg/opus voice notes, Symphonia decodes
// everything else (AAC video soundtracks, mp3/wav/flac documents), a
// small WSOLA pass gives the 1x-3x speeds with pitch preserved, and
// cpal plays.
use crate::media::Decoded;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

pub const OUT_RATE: u32 = 48_000;

// A fully decoded mono track at OUT_RATE, already time-stretched.
#[derive(Clone)]
pub struct AudioBuffer {
    pub samples: Arc<Vec<f32>>,
}

impl AudioBuffer {
    pub fn duration_secs(&self) -> f64 {
        self.samples.len() as f64 / OUT_RATE as f64
    }
}

// ---- decoding (opus for voice notes, Symphonia for the rest) ----

// Mono samples at the source rate, whatever the container.
fn decode_any(path: &Path) -> Option<(Vec<f32>, u32)> {
    let mut head = [0u8; 64];
    {
        use std::io::Read;
        let mut f = std::fs::File::open(path).ok()?;
        let _ = f.read(&mut head);
    }
    if head.starts_with(b"OggS") && head.windows(8).any(|w| w == b"OpusHead") {
        return decode_opus_ogg(path);
    }
    decode_symphonia(path)
}

fn decode_opus_ogg(path: &Path) -> Option<(Vec<f32>, u32)> {
    let file = std::fs::File::open(path).ok()?;
    let mut reader = ogg::PacketReader::new(std::io::BufReader::new(file));
    let mut decoder: Option<opus_pure::OpusDecoder> = None;
    let mut channels = 1usize;
    let mut pre_skip = 0usize;
    let mut seen_tags = false;
    let mut pcm = Vec::new();
    // One packet decodes at most 120ms: 5760 samples per channel at 48k.
    let mut scratch = vec![0f32; 5760 * 2];
    while let Ok(Some(packet)) = reader.read_packet() {
        let Some(dec) = decoder.as_mut() else {
            if packet.data.starts_with(b"OpusHead") && packet.data.len() >= 19 {
                channels = (packet.data[9] as usize).clamp(1, 2);
                pre_skip = u16::from_le_bytes([packet.data[10], packet.data[11]]) as usize;
                decoder = opus_pure::OpusDecoder::new(OUT_RATE as i32, channels).ok();
            }
            continue;
        };
        if !seen_tags {
            seen_tags = true; // OpusTags
            continue;
        }
        let Ok(n) = dec.decode(&packet.data, 5760, &mut scratch) else {
            continue;
        };
        // The decoder can ring slightly past full scale; keep the player
        // fed with clamped samples.
        for frame in scratch[..n * channels].chunks(channels) {
            let s = frame.iter().sum::<f32>() / channels as f32;
            pcm.push(s.clamp(-1.0, 1.0));
        }
    }
    if pre_skip < pcm.len() {
        pcm.drain(..pre_skip);
    }
    if pcm.is_empty() {
        None
    } else {
        Some((pcm, OUT_RATE))
    }
}

pub fn probe_duration(path: &Path) -> Option<u32> {
    use symphonia::core::{io::MediaSourceStream, probe::Hint};
    let file = std::fs::File::open(path).ok()?;
    let stream = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|ext| ext.to_str()) {
        hint.with_extension(ext);
    }
    let probed = symphonia::default::get_probe()
        .format(&hint, stream, &Default::default(), &Default::default())
        .ok()?;
    let params = &probed.format.default_track()?.codec_params;
    let frames = params.n_frames?;
    let time = params.time_base?.calc_time(frames);
    Some(
        (time.seconds as f64 + time.frac)
            .round()
            .clamp(0.0, u32::MAX as f64) as u32,
    )
}

fn decode_symphonia(path: &Path) -> Option<(Vec<f32>, u32)> {
    use symphonia::core::audio::SampleBuffer;
    use symphonia::core::codecs::CODEC_TYPE_NULL;
    use symphonia::core::io::MediaSourceStream;
    use symphonia::core::probe::Hint;
    let file = std::fs::File::open(path).ok()?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }
    let probed = symphonia::default::get_probe()
        .format(&hint, mss, &Default::default(), &Default::default())
        .ok()?;
    let mut format = probed.format;
    let track = format
        .tracks()
        .iter()
        .find(|t| t.codec_params.codec != CODEC_TYPE_NULL && t.codec_params.sample_rate.is_some())?
        .clone();
    let mut decoder = symphonia::default::get_codecs()
        .make(&track.codec_params, &Default::default())
        .ok()?;
    let rate = track.codec_params.sample_rate?;
    let mut mono = Vec::new();
    let mut sbuf: Option<SampleBuffer<f32>> = None;
    while let Ok(packet) = format.next_packet() {
        if packet.track_id() != track.id {
            continue;
        }
        let Ok(decoded) = decoder.decode(&packet) else {
            continue;
        };
        let spec = *decoded.spec();
        let buf = sbuf.get_or_insert_with(|| SampleBuffer::new(decoded.capacity() as u64, spec));
        buf.copy_interleaved_ref(decoded);
        let ch = spec.channels.count().max(1);
        for frame in buf.samples().chunks(ch) {
            mono.push(frame.iter().sum::<f32>() / ch as f32);
        }
    }
    if mono.is_empty() {
        None
    } else {
        Some((mono, rate))
    }
}

// Plain linear resample; opus itself sets the quality floor at these rates.
fn resample(samples: &[f32], from: u32, to: u32) -> Vec<f32> {
    if from == to || samples.is_empty() {
        return samples.to_vec();
    }
    let ratio = to as f64 / from as f64;
    let out_len = (samples.len() as f64 * ratio) as usize;
    let mut out = Vec::with_capacity(out_len);
    for i in 0..out_len {
        let src = i as f64 / ratio;
        let lo = src.floor() as usize;
        let hi = (lo + 1).min(samples.len() - 1);
        let t = (src - lo as f64) as f32;
        out.push(samples[lo] * (1.0 - t) + samples[hi] * t);
    }
    out
}

// WSOLA time-stretch: replaces FFmpeg's atempo. Overlapping Hann windows
// are taken from the input at `factor` times the output pace, each shifted
// within a small search range to line up with its natural continuation,
// then overlap-added — speed changes, pitch does not.
fn time_stretch(input: &[f32], factor: f64) -> Vec<f32> {
    const WIN: usize = 1024; // ~21ms at 48k
    const HOP: usize = WIN / 2;
    const SEARCH: usize = 320; // ~6.7ms each way
    if (factor - 1.0).abs() < 1e-3 || input.len() < WIN * 2 {
        return input.to_vec();
    }
    let window: Vec<f32> = (0..WIN)
        .map(|i| {
            let x = std::f32::consts::PI * i as f32 / (WIN - 1) as f32;
            x.sin() * x.sin()
        })
        .collect();
    let out_len = (input.len() as f64 / factor) as usize;
    let mut out = vec![0f32; out_len + WIN];
    let mut norm = vec![0f32; out_len + WIN];
    let mut prev: usize = 0; // input position of the previous segment
    let mut pos_out = 0usize;
    while pos_out < out_len {
        let ideal = (pos_out as f64 * factor) as usize;
        let best = if pos_out == 0 {
            0
        } else {
            // The natural continuation of the last segment is `prev + HOP`;
            // pick the candidate near `ideal` that best matches it.
            let target = (prev + HOP).min(input.len().saturating_sub(WIN));
            let lo = ideal.saturating_sub(SEARCH);
            let hi = (ideal + SEARCH).min(input.len().saturating_sub(WIN));
            let mut best = lo;
            let mut best_score = f32::NEG_INFINITY;
            let reference = &input[target..target + WIN.min(256)];
            let mut cand = lo;
            while cand <= hi {
                let probe = &input[cand..cand + reference.len()];
                let score: f32 = reference.iter().zip(probe).map(|(a, b)| a * b).sum();
                if score > best_score {
                    best_score = score;
                    best = cand;
                }
                cand += 4;
            }
            best
        };
        if best + WIN > input.len() {
            break;
        }
        for i in 0..WIN {
            out[pos_out + i] += input[best + i] * window[i];
            norm[pos_out + i] += window[i];
        }
        prev = best;
        pos_out += HOP;
    }
    out.truncate(out_len);
    for (sample, w) in out.iter_mut().zip(norm.iter()) {
        if *w > 1e-3 {
            *sample /= w;
        }
    }
    out
}

// Decodes the whole (small) voice note with the speed applied, so seeking
// and pausing are just cursor moves.
pub fn decode_with_tempo(path: &Path, rate: f64) -> Option<AudioBuffer> {
    let (samples, in_rate) = decode_any(path)?;
    let samples = resample(&samples, in_rate, OUT_RATE);
    let samples = time_stretch(&samples, rate);
    if samples.is_empty() {
        return None;
    }
    Some(AudioBuffer {
        samples: Arc::new(samples),
    })
}

// ---- waveform (the 44-bar bitmap under the play button) ----

const BARS: usize = 44;
const BAR_W: usize = 3;
const BAR_GAP: usize = 2;
const WAVE_H: usize = 30;

pub fn waveform(path: &Path) -> Option<Decoded> {
    let (samples, _) = decode_any(path)?;
    if samples.is_empty() {
        return None;
    }
    let mut peaks = [0f32; BARS];
    let bucket = (samples.len() / BARS).max(1);
    for (i, peak) in peaks.iter_mut().enumerate() {
        let start = i * bucket;
        let end = ((i + 1) * bucket).min(samples.len());
        *peak = samples[start..end]
            .iter()
            .map(|s| s.abs())
            .fold(0.0, f32::max);
    }
    let max = peaks.iter().fold(0.0f32, |a, &b| a.max(b)).max(1e-6);
    // White opaque bars on transparency; the .slint side colorizes them.
    let width = BARS * (BAR_W + BAR_GAP);
    let mut rgba = vec![0u8; width * WAVE_H * 4];
    for (i, &peak) in peaks.iter().enumerate() {
        let h = ((peak / max) * WAVE_H as f32).round().max(2.0) as usize;
        let top = (WAVE_H - h) / 2;
        for y in top..top + h {
            for x in 0..BAR_W {
                let px = (y * width + i * (BAR_W + BAR_GAP) + x) * 4;
                rgba[px..px + 4].copy_from_slice(&[255, 255, 255, 255]);
            }
        }
    }
    Some(Decoded {
        w: width as u32,
        h: WAVE_H as u32,
        rgba,
    })
}

// The 64-value amplitude strip WhatsApp embeds in voice notes.
pub fn message_waveform(samples: &[f32]) -> Vec<u8> {
    const POINTS: usize = 64;
    if samples.is_empty() {
        return vec![0; POINTS];
    }
    let bucket = (samples.len() / POINTS).max(1);
    let mut out = Vec::with_capacity(POINTS);
    for i in 0..POINTS {
        let start = (i * bucket).min(samples.len() - 1);
        let end = ((i + 1) * bucket).min(samples.len());
        let peak = samples[start..end]
            .iter()
            .map(|s| s.abs())
            .fold(0.0, f32::max);
        out.push((peak * 100.0).min(100.0) as u8);
    }
    out
}

// ---- playback (cpal pulls from the decoded buffer) ----

struct PlayShared {
    buffer: Arc<Vec<f32>>,
    cursor: AtomicUsize,
    finished: AtomicBool,
    // f32 bits; 1.0 unless the video player's volume says otherwise.
    gain: std::sync::atomic::AtomicU32,
}

pub struct Player {
    _stream: cpal::Stream,
    shared: Arc<PlayShared>,
}

impl Player {
    // Starts at `offset_secs` in OUTPUT time (already stretched).
    pub fn start(buffer: &AudioBuffer, offset_secs: f64) -> Option<Player> {
        use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
        let device = cpal::default_host().default_output_device()?;
        let config = device.default_output_config().ok()?;
        let out_rate = config.sample_rate();
        let channels = config.channels() as usize;
        // The buffer is mono at OUT_RATE; a mismatched device rate plays
        // with nearest-sample scaling.
        let step = OUT_RATE as f64 / out_rate as f64;
        let start = ((offset_secs * OUT_RATE as f64) as usize).min(buffer.samples.len());
        let shared = Arc::new(PlayShared {
            buffer: buffer.samples.clone(),
            cursor: AtomicUsize::new(start),
            finished: AtomicBool::new(false),
            gain: std::sync::atomic::AtomicU32::new(1.0f32.to_bits()),
        });
        let cb = shared.clone();
        let mut frac = 0.0f64;
        let stream = device
            .build_output_stream(
                config.config(),
                move |out: &mut [f32], _| {
                    let mut cursor = cb.cursor.load(Ordering::Relaxed);
                    let gain = f32::from_bits(cb.gain.load(Ordering::Relaxed));
                    for frame in out.chunks_mut(channels) {
                        let sample = if cursor < cb.buffer.len() {
                            let s = cb.buffer[cursor] * gain;
                            frac += step;
                            while frac >= 1.0 {
                                cursor += 1;
                                frac -= 1.0;
                            }
                            s
                        } else {
                            cb.finished.store(true, Ordering::Relaxed);
                            0.0
                        };
                        for slot in frame {
                            *slot = sample;
                        }
                    }
                    cb.cursor.store(cursor, Ordering::Relaxed);
                },
                |e| eprintln!("[audio] output stream error: {e}"),
                None,
            )
            .ok()?;
        stream.play().ok()?;
        Some(Player {
            _stream: stream,
            shared,
        })
    }

    pub fn pause(&self) {
        use cpal::traits::StreamTrait;
        let _ = self._stream.pause();
    }

    pub fn resume(&self) {
        use cpal::traits::StreamTrait;
        let _ = self._stream.play();
    }

    // Jumps to `secs` (OUTPUT time). The callback picks the new cursor up
    // on its next buffer, a few milliseconds away.
    pub fn seek(&self, secs: f64) {
        let at = ((secs.max(0.0) * OUT_RATE as f64) as usize).min(self.shared.buffer.len());
        self.shared.cursor.store(at, Ordering::Relaxed);
        self.shared.finished.store(false, Ordering::Relaxed);
    }

    pub fn set_gain(&self, gain: f32) {
        self.shared
            .gain
            .store(gain.clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
    }

    // Position in OUTPUT seconds (multiply by the rate for source time).
    pub fn position_secs(&self) -> f64 {
        self.shared.cursor.load(Ordering::Relaxed) as f64 / OUT_RATE as f64
    }

    pub fn finished(&self) -> bool {
        self.shared.finished.load(Ordering::Relaxed)
    }
}

// ---- recording (cpal capture; libopus encodes the voice note) ----

// Bars shown by the recording bar's waveform.
pub const REC_BARS: usize = 64;
// One level bar per this much captured audio.
const LEVEL_WINDOW_MS: usize = 50;

// Peak level per 50 ms of captured audio, for the live waveform.
struct LevelMeter {
    bucket: usize,
    peak: f32,
    seen: usize,
    bars: Vec<f32>,
}

impl LevelMeter {
    fn new(rate: u32, channels: usize) -> Self {
        LevelMeter {
            bucket: (rate as usize * LEVEL_WINDOW_MS / 1000 * channels).max(1),
            peak: 0.0,
            seen: 0,
            bars: Vec::new(),
        }
    }

    // Speech peaks well below full scale; lift it so the bars read.
    fn scale(peak: f32) -> f32 {
        (peak * 3.0).min(1.0)
    }

    fn push(&mut self, data: &[f32]) {
        for sample in data {
            self.peak = self.peak.max(sample.abs());
            self.seen += 1;
            if self.seen >= self.bucket {
                self.bars.push(Self::scale(self.peak));
                self.peak = 0.0;
                self.seen = 0;
            }
        }
    }

    // The latest `n` bars (zero padded at the end) and how many are real.
    fn recent(&self, n: usize) -> (Vec<f32>, usize) {
        let start = self.bars.len().saturating_sub(n);
        let mut out: Vec<f32> = self.bars[start..].to_vec();
        let filled = out.len();
        out.resize(n, 0.0);
        (out, filled)
    }

    // The whole recording squeezed into `n` bars (the peak of each span).
    fn overview(&self, n: usize) -> Vec<f32> {
        let len = self.bars.len();
        if len == 0 || n == 0 {
            return vec![0.0; n];
        }
        (0..n)
            .map(|i| {
                let lo = (i * len / n).min(len - 1);
                let hi = ((i + 1) * len / n).max(lo + 1).min(len);
                self.bars[lo..hi].iter().copied().fold(0.0, f32::max)
            })
            .collect()
    }
}

struct RecShared {
    samples: Vec<f32>,
    meter: LevelMeter,
}

pub struct Recorder {
    _stream: cpal::Stream,
    shared: Arc<std::sync::Mutex<RecShared>>,
    paused: Arc<AtomicBool>,
    rate: u32,
    channels: usize,
}

fn to_mono(raw: Vec<f32>, channels: usize) -> Vec<f32> {
    if channels <= 1 {
        return raw;
    }
    raw.chunks(channels)
        .map(|frame| frame.iter().sum::<f32>() / frame.len() as f32)
        .collect()
}

impl Recorder {
    pub fn start() -> Option<Recorder> {
        use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
        let device = cpal::default_host().default_input_device()?;
        let config = device.default_input_config().ok()?;
        let rate = config.sample_rate();
        let channels = config.channels() as usize;
        let shared = Arc::new(std::sync::Mutex::new(RecShared {
            samples: Vec::new(),
            meter: LevelMeter::new(rate, channels),
        }));
        let paused = Arc::new(AtomicBool::new(false));
        let sink = shared.clone();
        let gate = paused.clone();
        let stream = device
            .build_input_stream(
                config.config(),
                move |data: &[f32], _| {
                    // A paused recording keeps the stream open but drops
                    // what the microphone hears.
                    if gate.load(Ordering::Relaxed) {
                        return;
                    }
                    if let Ok(mut shared) = sink.lock() {
                        shared.samples.extend_from_slice(data);
                        shared.meter.push(data);
                    }
                },
                |e| eprintln!("[audio] input stream error: {e}"),
                None,
            )
            .ok()?;
        stream.play().ok()?;
        Some(Recorder {
            _stream: stream,
            shared,
            paused,
            rate,
            channels,
        })
    }

    pub fn pause(&self) {
        self.paused.store(true, Ordering::Relaxed);
    }

    pub fn resume(&self) {
        self.paused.store(false, Ordering::Relaxed);
    }

    pub fn is_paused(&self) -> bool {
        self.paused.load(Ordering::Relaxed)
    }

    // Seconds captured so far (pauses excluded).
    pub fn elapsed_secs(&self) -> f64 {
        let frames = self
            .shared
            .lock()
            .map(|s| s.samples.len() / self.channels.max(1))
            .unwrap_or(0);
        frames as f64 / self.rate.max(1) as f64
    }

    pub fn recent_levels(&self, n: usize) -> (Vec<f32>, usize) {
        self.shared
            .lock()
            .map(|s| s.meter.recent(n))
            .unwrap_or_else(|_| (vec![0.0; n], 0))
    }

    pub fn overview_levels(&self, n: usize) -> Vec<f32> {
        self.shared
            .lock()
            .map(|s| s.meter.overview(n))
            .unwrap_or_else(|_| vec![0.0; n])
    }

    // What was captured so far, without ending the recording: mono samples
    // at the device rate.
    pub fn snapshot(&self) -> (Vec<f32>, u32) {
        let raw = self
            .shared
            .lock()
            .map(|s| s.samples.clone())
            .unwrap_or_default();
        (to_mono(raw, self.channels), self.rate)
    }

    // Mono samples at the device rate.
    pub fn stop(self) -> (Vec<f32>, u32) {
        self.snapshot()
    }
}

// A recording as a playable buffer, for listening before sending.
pub fn recording_buffer(samples: &[f32], in_rate: u32) -> Option<AudioBuffer> {
    if samples.is_empty() {
        return None;
    }
    Some(AudioBuffer {
        samples: Arc::new(resample(samples, in_rate, OUT_RATE)),
    })
}

// Encodes captured samples as WhatsApp's voice-note format
// (ogg, libopus 32kbps, mono, 48kHz). Returns the seconds recorded.
pub fn encode_voice_ogg(samples: &[f32], in_rate: u32, out_path: &Path) -> Option<u32> {
    if samples.is_empty() {
        return None;
    }
    let pcm = resample(samples, in_rate, OUT_RATE);
    let seconds = (pcm.len() as f64 / OUT_RATE as f64).round() as u32;

    let mut encoder =
        opus_pure::OpusEncoder::new(OUT_RATE as i32, 1, opus_pure::Application::Voip).ok()?;
    encoder.bitrate_bps = 32_000;
    // libopus reports a 312-sample lookahead at 48k; the port matches it.
    let pre_skip: u64 = 312;

    let file = std::fs::File::create(out_path).ok()?;
    let mut writer = ogg::PacketWriter::new(std::io::BufWriter::new(file));
    let serial: u32 = 0x5a41_5049; // "ZAPI"
    use ogg::writing::PacketWriteEndInfo;
    let mut head = Vec::with_capacity(19);
    head.extend_from_slice(b"OpusHead");
    head.push(1); // version
    head.push(1); // channels
    head.extend_from_slice(&(pre_skip as u16).to_le_bytes());
    head.extend_from_slice(&OUT_RATE.to_le_bytes());
    head.extend_from_slice(&0u16.to_le_bytes()); // output gain
    head.push(0); // channel mapping family
    writer
        .write_packet(head, serial, PacketWriteEndInfo::EndPage, 0)
        .ok()?;
    let mut tags = Vec::new();
    tags.extend_from_slice(b"OpusTags");
    tags.extend_from_slice(&(6u32).to_le_bytes());
    tags.extend_from_slice(b"zapive");
    tags.extend_from_slice(&0u32.to_le_bytes());
    writer
        .write_packet(tags, serial, PacketWriteEndInfo::EndPage, 0)
        .ok()?;

    const FRAME: usize = 960; // 20ms at 48k
    let mut granule = pre_skip;
    let chunks: Vec<&[f32]> = pcm.chunks(FRAME).collect();
    let mut padded = [0f32; FRAME];
    for (i, chunk) in chunks.iter().enumerate() {
        let frame: &[f32] = if chunk.len() == FRAME {
            chunk
        } else {
            padded[..chunk.len()].copy_from_slice(chunk);
            padded[chunk.len()..].fill(0.0);
            &padded
        };
        let mut packet = vec![0u8; 4000];
        let Ok(n) = encoder.encode(frame, FRAME, &mut packet) else {
            continue;
        };
        packet.truncate(n);
        let bytes = packet;
        granule += FRAME as u64;
        let end = if i + 1 == chunks.len() {
            PacketWriteEndInfo::EndStream
        } else {
            PacketWriteEndInfo::NormalPacket
        };
        writer.write_packet(bytes, serial, end, granule).ok()?;
    }
    Some(seconds.max(1))
}

#[cfg(test)]
mod duration_tests {
    #[test]
    fn audio_duration_probe_reads_wav_metadata() {
        let path =
            std::env::temp_dir().join(format!("zapive-audio-duration-{}.wav", std::process::id()));
        let data_size = 16_000u32;
        let mut wav = Vec::new();
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&(36 + data_size).to_le_bytes());
        wav.extend_from_slice(b"WAVEfmt ");
        wav.extend_from_slice(&16u32.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&8_000u32.to_le_bytes());
        wav.extend_from_slice(&16_000u32.to_le_bytes());
        wav.extend_from_slice(&2u16.to_le_bytes());
        wav.extend_from_slice(&16u16.to_le_bytes());
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&data_size.to_le_bytes());
        wav.resize(44 + data_size as usize, 0);
        std::fs::write(&path, wav).unwrap();
        let duration = super::probe_duration(&path);
        std::fs::remove_file(&path).unwrap();
        assert_eq!(duration, Some(1));
        assert_eq!(super::probe_duration(&path), None);
    }
}

#[cfg(test)]
mod level_tests {
    use super::LevelMeter;

    // 1 kHz mono: 50 samples per 50 ms bar.
    fn meter() -> LevelMeter {
        LevelMeter::new(1_000, 1)
    }

    #[test]
    fn each_window_becomes_one_scaled_bar() {
        let mut m = meter();
        m.push(&[0.1; 50]);
        m.push(&[0.5; 50]);
        assert_eq!(m.bars.len(), 2);
        assert!((m.bars[0] - 0.3).abs() < 1e-6);
        // Louder than full scale after the lift is capped at 1.
        assert_eq!(m.bars[1], 1.0);
    }

    #[test]
    fn a_partial_window_carries_over_between_pushes() {
        let mut m = meter();
        m.push(&[0.1; 30]);
        assert!(m.bars.is_empty());
        m.push(&[0.2; 20]);
        assert_eq!(m.bars.len(), 1);
        assert!((m.bars[0] - 0.6).abs() < 1e-6);
    }

    #[test]
    fn recent_pads_and_reports_how_many_are_real() {
        let mut m = meter();
        m.push(&[0.1; 100]);
        let (bars, filled) = m.recent(4);
        assert_eq!((bars.len(), filled), (4, 2));
        assert_eq!(bars[2], 0.0);
        // Longer than the window: only the newest bars remain.
        m.push(&[0.2; 50 * 5]);
        let (bars, filled) = m.recent(3);
        assert_eq!((bars.len(), filled), (3, 3));
        assert!(bars.iter().all(|b| (*b - 0.6).abs() < 1e-6));
    }

    #[test]
    fn overview_keeps_the_peak_of_each_span() {
        let mut m = meter();
        for level in [0.1f32, 0.1, 0.3, 0.1] {
            m.push(&[level; 50]);
        }
        let view = m.overview(2);
        assert_eq!(view.len(), 2);
        assert!((view[0] - 0.3).abs() < 1e-6);
        assert!((view[1] - 0.9).abs() < 1e-6);
        assert_eq!(meter().overview(3), vec![0.0; 3]);
    }
}
