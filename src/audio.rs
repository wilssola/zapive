// Voice-note audio, fully in-process: FFmpeg decodes (with the atempo
// chain for the 1x-3x speeds, pitch preserved) and cpal plays. Replaces
// the external ffplay of the Node build.
use crate::media::Decoded;
use ffmpeg_next as ff;
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

// ffplay capped atempo at 2x, so higher speeds chain filters; same here.
fn tempo_chain(rate: f64) -> String {
    let mut parts = Vec::new();
    let mut rest = rate;
    while rest > 2.0 {
        parts.push("atempo=2.0".to_string());
        rest /= 2.0;
    }
    if (rest - 1.0).abs() > 1e-3 {
        parts.push(format!("atempo={rest:.4}"));
    }
    if parts.is_empty() { "anull".to_string() } else { parts.join(",") }
}

fn decode_filtered(path: &Path, filter: &str, out_rate: u32) -> Option<Vec<f32>> {
    let mut input = ff::format::input(path).ok()?;
    let stream = input.streams().best(ff::media::Type::Audio)?;
    let stream_index = stream.index();
    let time_base = stream.time_base();
    let context = ff::codec::context::Context::from_parameters(stream.parameters()).ok()?;
    let mut decoder = context.decoder().audio().ok()?;

    let mut graph = ff::filter::Graph::new();
    let layout = if decoder.channel_layout().is_empty() {
        ff::channel_layout::ChannelLayout::MONO
    } else {
        decoder.channel_layout()
    };
    let args = format!(
        "time_base={}/{}:sample_rate={}:sample_fmt={}:channel_layout=0x{:x}",
        time_base.numerator(),
        time_base.denominator(),
        decoder.rate(),
        decoder.format().name(),
        layout.bits()
    );
    graph.add(&ff::filter::find("abuffer")?, "in", &args).ok()?;
    graph.add(&ff::filter::find("abuffersink")?, "out", "").ok()?;
    let spec = format!("{filter},aformat=sample_fmts=flt:sample_rates={out_rate}:channel_layouts=mono");
    graph.output("in", 0).ok()?.input("out", 0).ok()?.parse(&spec).ok()?;
    graph.validate().ok()?;

    let mut samples = Vec::new();
    let mut drain = |graph: &mut ff::filter::Graph, samples: &mut Vec<f32>| {
        let mut filtered = ff::frame::Audio::empty();
        while graph.get("out").unwrap().sink().frame(&mut filtered).is_ok() {
            let count = filtered.samples();
            let data = filtered.data(0);
            let floats: &[f32] = bytemuck_cast(&data[..count * 4]);
            samples.extend_from_slice(floats);
        }
    };
    let mut receive = |decoder: &mut ff::decoder::Audio,
                       graph: &mut ff::filter::Graph,
                       samples: &mut Vec<f32>| {
        let mut frame = ff::frame::Audio::empty();
        while decoder.receive_frame(&mut frame).is_ok() {
            if graph.get("in").unwrap().source().add(&frame).is_ok() {
                drain(graph, samples);
            }
        }
    };
    for (stream, packet) in input.packets() {
        if stream.index() != stream_index {
            continue;
        }
        if decoder.send_packet(&packet).is_ok() {
            receive(&mut decoder, &mut graph, &mut samples);
        }
    }
    let _ = decoder.send_eof();
    receive(&mut decoder, &mut graph, &mut samples);
    let _ = graph.get("in").unwrap().source().flush();
    drain(&mut graph, &mut samples);
    Some(samples)
}

fn bytemuck_cast(bytes: &[u8]) -> &[f32] {
    // The FFmpeg frame buffer is properly aligned for its sample format.
    unsafe { std::slice::from_raw_parts(bytes.as_ptr() as *const f32, bytes.len() / 4) }
}

// Decodes the whole (small) voice note with the speed applied, so seeking
// and pausing are just cursor moves.
pub fn decode_with_tempo(path: &Path, rate: f64) -> Option<AudioBuffer> {
    let samples = decode_filtered(path, &tempo_chain(rate), OUT_RATE)?;
    if samples.is_empty() {
        return None;
    }
    Some(AudioBuffer { samples: Arc::new(samples) })
}

// ---- waveform (the 44-bar bitmap under the play button) ----

const BARS: usize = 44;
const BAR_W: usize = 3;
const BAR_GAP: usize = 2;
const WAVE_H: usize = 30;

pub fn waveform(path: &Path) -> Option<Decoded> {
    let samples = decode_filtered(path, "anull", 8000)?;
    if samples.is_empty() {
        return None;
    }
    let mut peaks = [0f32; BARS];
    let bucket = (samples.len() / BARS).max(1);
    for (i, peak) in peaks.iter_mut().enumerate() {
        let start = i * bucket;
        let end = ((i + 1) * bucket).min(samples.len());
        *peak = samples[start..end].iter().map(|s| s.abs()).fold(0.0, f32::max);
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
    Some(Decoded { w: width as u32, h: WAVE_H as u32, rgba })
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
        let peak = samples[start..end].iter().map(|s| s.abs()).fold(0.0, f32::max);
        out.push((peak * 100.0).min(100.0) as u8);
    }
    out
}

// ---- playback (cpal pulls from the decoded buffer) ----

struct PlayShared {
    buffer: Arc<Vec<f32>>,
    cursor: AtomicUsize,
    finished: AtomicBool,
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
        let out_rate = config.sample_rate().0;
        let channels = config.channels() as usize;
        // The buffer is mono at OUT_RATE; a mismatched device rate plays
        // with nearest-sample scaling.
        let step = OUT_RATE as f64 / out_rate as f64;
        let start = ((offset_secs * OUT_RATE as f64) as usize).min(buffer.samples.len());
        let shared = Arc::new(PlayShared {
            buffer: buffer.samples.clone(),
            cursor: AtomicUsize::new(start),
            finished: AtomicBool::new(false),
        });
        let cb = shared.clone();
        let mut frac = 0.0f64;
        let stream = device
            .build_output_stream(
                &config.config(),
                move |out: &mut [f32], _| {
                    let mut cursor = cb.cursor.load(Ordering::Relaxed);
                    for frame in out.chunks_mut(channels) {
                        let sample = if cursor < cb.buffer.len() {
                            let s = cb.buffer[cursor];
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
        Some(Player { _stream: stream, shared })
    }

    pub fn pause(&self) {
        use cpal::traits::StreamTrait;
        let _ = self._stream.pause();
    }

    pub fn resume(&self) {
        use cpal::traits::StreamTrait;
        let _ = self._stream.play();
    }

    // Position in OUTPUT seconds (multiply by the rate for source time).
    pub fn position_secs(&self) -> f64 {
        self.shared.cursor.load(Ordering::Relaxed) as f64 / OUT_RATE as f64
    }

    pub fn finished(&self) -> bool {
        self.shared.finished.load(Ordering::Relaxed)
    }
}

// ---- recording (cpal capture; FFmpeg encodes the opus voice note) ----

pub struct Recorder {
    _stream: cpal::Stream,
    samples: Arc<std::sync::Mutex<Vec<f32>>>,
    rate: u32,
    channels: usize,
}

impl Recorder {
    pub fn start() -> Option<Recorder> {
        use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
        let device = cpal::default_host().default_input_device()?;
        let config = device.default_input_config().ok()?;
        let rate = config.sample_rate().0;
        let channels = config.channels() as usize;
        let samples = Arc::new(std::sync::Mutex::new(Vec::new()));
        let sink = samples.clone();
        let stream = device
            .build_input_stream(
                &config.config(),
                move |data: &[f32], _| {
                    if let Ok(mut buf) = sink.lock() {
                        buf.extend_from_slice(data);
                    }
                },
                |e| eprintln!("[audio] input stream error: {e}"),
                None,
            )
            .ok()?;
        stream.play().ok()?;
        Some(Recorder { _stream: stream, samples, rate, channels })
    }

    // Mono samples at the device rate.
    pub fn stop(self) -> (Vec<f32>, u32) {
        let raw = self.samples.lock().map(|b| b.clone()).unwrap_or_default();
        if self.channels <= 1 {
            return (raw, self.rate);
        }
        let mono: Vec<f32> = raw
            .chunks(self.channels)
            .map(|frame| frame.iter().sum::<f32>() / frame.len() as f32)
            .collect();
        (mono, self.rate)
    }
}

// Encodes captured samples as WhatsApp's voice-note format
// (ogg, libopus 32kbps, mono, 48kHz). Returns the seconds recorded.
pub fn encode_voice_ogg(samples: &[f32], in_rate: u32, out_path: &Path) -> Option<u32> {
    if samples.is_empty() {
        return None;
    }
    // Resample to 48kHz mono with a plain linear pass (voice quality is
    // set by opus itself at these rates).
    let ratio = OUT_RATE as f64 / in_rate as f64;
    let out_len = (samples.len() as f64 * ratio) as usize;
    let mut pcm = Vec::with_capacity(out_len);
    for i in 0..out_len {
        let src = i as f64 / ratio;
        let lo = src.floor() as usize;
        let hi = (lo + 1).min(samples.len() - 1);
        let t = src - lo as f64;
        pcm.push(samples[lo] as f64 * (1.0 - t) + samples[hi] as f64 * t);
    }
    let seconds = (pcm.len() as f64 / OUT_RATE as f64).round() as u32;

    let codec = ff::encoder::find_by_name("libopus")?;
    let mut output = ff::format::output(out_path).ok()?;
    let mut encoder = ff::codec::context::Context::new_with_codec(codec).encoder().audio().ok()?;
    encoder.set_rate(OUT_RATE as i32);
    encoder.set_channel_layout(ff::channel_layout::ChannelLayout::MONO);
    encoder.set_format(ff::format::Sample::F32(ff::format::sample::Type::Planar));
    encoder.set_bit_rate(32_000);
    encoder.set_time_base(ff::Rational::new(1, OUT_RATE as i32));
    let mut encoder = encoder.open_as(codec).ok()?;
    {
        let mut stream = output.add_stream(codec).ok()?;
        stream.set_parameters(&encoder);
        stream.set_time_base(ff::Rational::new(1, OUT_RATE as i32));
    }
    output.write_header().ok()?;

    let frame_size = encoder.frame_size().max(960) as usize;
    let mut pts: i64 = 0;
    let mut write_packets = |encoder: &mut ff::encoder::Audio, output: &mut ff::format::context::Output| {
        let mut packet = ff::Packet::empty();
        while encoder.receive_packet(&mut packet).is_ok() {
            packet.set_stream(0);
            let _ = packet.write_interleaved(output);
        }
    };
    for chunk in pcm.chunks(frame_size) {
        let mut frame = ff::frame::Audio::new(
            ff::format::Sample::F32(ff::format::sample::Type::Planar),
            frame_size,
            ff::channel_layout::ChannelLayout::MONO,
        );
        frame.set_rate(OUT_RATE);
        frame.set_pts(Some(pts));
        pts += chunk.len() as i64;
        let plane = frame.plane_mut::<f32>(0);
        for (slot, &s) in plane.iter_mut().zip(chunk.iter()) {
            *slot = s as f32;
        }
        for slot in plane.iter_mut().skip(chunk.len()) {
            *slot = 0.0;
        }
        if encoder.send_frame(&frame).is_ok() {
            write_packets(&mut encoder, &mut output);
        }
    }
    let _ = encoder.send_eof();
    write_packets(&mut encoder, &mut output);
    output.write_trailer().ok()?;
    Some(seconds.max(1))
}
