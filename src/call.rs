// Live-call audio: the microphone and speaker halves of a WhatsApp voice
// call. whatsapp-rust wants 16 kHz mono i16 in 60 ms (960-sample) frames
// on both sides, so this module owns the cpal streams, the rate
// conversion to and from the device rate, and the ringtones.
//
// cpal streams are !Send, so the call streams live on a dedicated thread
// that parks until the call ends; the tokio side only ever holds the
// async_channel endpoints, which is exactly what the voip facade takes.
use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
// The voip facade takes async_channel endpoints directly, so the crate's
// re-export is what keeps both sides on the same version.
use whatsapp_rust::async_channel;

// What the voip engine speaks: 16 kHz mono, 60 ms per frame.
const CALL_RATE: u32 = 16_000;
const FRAME: usize = 960;
// Playout past this much backlog is stale; drop the oldest instead of
// letting the delay grow for the rest of the call.
const MAX_PLAYOUT: usize = CALL_RATE as usize / 2;

// Linear resampler that keeps its phase across callbacks: samples go in
// at `from`, come out at `to`. Both directions of the call use it.
struct Resampler {
    queue: VecDeque<f32>,
    prev: f32,
    next: f32,
    frac: f64,
    step: f64,
}

impl Resampler {
    fn new(from: u32, to: u32) -> Self {
        Self {
            queue: VecDeque::new(),
            prev: 0.0,
            next: 0.0,
            // Starts past the first sample so the first pull primes.
            frac: 1.0,
            step: from as f64 / to.max(1) as f64,
        }
    }

    fn push(&mut self, sample: f32) {
        self.queue.push_back(sample);
    }

    fn drop_backlog(&mut self, keep: usize) {
        while self.queue.len() > keep {
            self.queue.pop_front();
        }
    }

    // None when the input ran dry; the phase is left untouched so the
    // next call picks up where this one stopped.
    fn pull(&mut self) -> Option<f32> {
        while self.frac >= 1.0 {
            let sample = self.queue.pop_front()?;
            self.prev = self.next;
            self.next = sample;
            self.frac -= 1.0;
        }
        let out = self.prev + (self.next - self.prev) * self.frac as f32;
        self.frac += self.step;
        Some(out)
    }
}

// The tokio-side handle on a call's audio. The channels go straight to
// the voip facade (async_channel endpoints implement AudioSource and
// AudioSink); dropping this stops the device thread.
pub struct CallAudio {
    mic: async_channel::Receiver<Vec<i16>>,
    speaker: async_channel::Sender<Vec<i16>>,
    muted: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
}

impl CallAudio {
    // Opens the devices on a thread of its own and returns once they are
    // running. None when neither capture nor playout could start.
    pub fn start() -> Option<CallAudio> {
        // Three frames of slack each way: enough to ride out a scheduling
        // hiccup, short enough that the delay stays inaudible.
        let (mic_tx, mic_rx) = async_channel::bounded::<Vec<i16>>(3);
        let (spk_tx, spk_rx) = async_channel::bounded::<Vec<i16>>(3);
        let muted = Arc::new(AtomicBool::new(false));
        let stop = Arc::new(AtomicBool::new(false));
        let (ready_tx, ready_rx) = std::sync::mpsc::channel::<bool>();
        {
            let muted = muted.clone();
            let stop = stop.clone();
            std::thread::Builder::new()
                .name("zapive-call-audio".into())
                .spawn(move || run_devices(mic_tx, spk_rx, muted, stop, ready_tx))
                .ok()?;
        }
        // The thread reports whether at least one stream came up.
        if !ready_rx.recv().unwrap_or(false) {
            stop.store(true, Ordering::SeqCst);
            return None;
        }
        Some(CallAudio { mic: mic_rx, speaker: spk_tx, muted, stop })
    }

    pub fn mic(&self) -> async_channel::Receiver<Vec<i16>> {
        self.mic.clone()
    }

    pub fn speaker(&self) -> async_channel::Sender<Vec<i16>> {
        self.speaker.clone()
    }

    pub fn muted_flag(&self) -> Arc<AtomicBool> {
        self.muted.clone()
    }
}

impl Drop for CallAudio {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        self.mic.close();
        self.speaker.close();
    }
}

// Runs on the audio thread: builds both streams, then sleeps until the
// call ends. The streams are dropped on the way out, which is what
// actually closes the devices.
fn run_devices(
    mic_tx: async_channel::Sender<Vec<i16>>,
    spk_rx: async_channel::Receiver<Vec<i16>>,
    muted: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    ready: std::sync::mpsc::Sender<bool>,
) {
    let input = build_input(mic_tx, muted);
    let output = build_output(spk_rx);
    if input.is_none() {
        eprintln!("[call] no microphone; the peer will hear silence");
    }
    if output.is_none() {
        eprintln!("[call] no speaker; the peer cannot be heard");
    }
    let _ = ready.send(input.is_some() || output.is_some());
    if input.is_none() && output.is_none() {
        return;
    }
    while !stop.load(Ordering::SeqCst) {
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

// Capture: mixdown to mono, resample to 16 kHz, cut into 60 ms frames.
fn build_input(
    mic_tx: async_channel::Sender<Vec<i16>>,
    muted: Arc<AtomicBool>,
) -> Option<cpal::Stream> {
    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
    let device = cpal::default_host().default_input_device()?;
    let config = device.default_input_config().ok()?;
    let channels = config.channels() as usize;
    let mut resampler = Resampler::new(config.sample_rate().0, CALL_RATE);
    let mut frame: Vec<i16> = Vec::with_capacity(FRAME);
    let stream = device
        .build_input_stream(
            &config.config(),
            move |data: &[f32], _| {
                for block in data.chunks(channels) {
                    resampler.push(block.iter().sum::<f32>() / channels.max(1) as f32);
                }
                // A wedged consumer must not grow the queue without end.
                resampler.drop_backlog(MAX_PLAYOUT);
                let silent = muted.load(Ordering::Relaxed);
                while let Some(sample) = resampler.pull() {
                    let value = if silent { 0 } else { (sample.clamp(-1.0, 1.0) * 32767.0) as i16 };
                    frame.push(value);
                    if frame.len() == FRAME {
                        // Voice is loss tolerant: a full channel means the
                        // engine is behind, so drop rather than block the
                        // audio callback.
                        let _ = mic_tx.try_send(std::mem::take(&mut frame));
                        frame = Vec::with_capacity(FRAME);
                    }
                }
            },
            |e| eprintln!("[call] microphone stream error: {e}"),
            None,
        )
        .ok()?;
    stream.play().ok()?;
    Some(stream)
}

// Playout: pull decoded 16 kHz frames off the channel and stretch them
// to whatever rate the output device runs at.
fn build_output(spk_rx: async_channel::Receiver<Vec<i16>>) -> Option<cpal::Stream> {
    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
    let device = cpal::default_host().default_output_device()?;
    let config = device.default_output_config().ok()?;
    let channels = config.channels() as usize;
    let mut resampler = Resampler::new(CALL_RATE, config.sample_rate().0);
    let stream = device
        .build_output_stream(
            &config.config(),
            move |out: &mut [f32], _| {
                while let Ok(frame) = spk_rx.try_recv() {
                    for sample in frame {
                        resampler.push(sample as f32 / 32768.0);
                    }
                }
                resampler.drop_backlog(MAX_PLAYOUT);
                for block in out.chunks_mut(channels) {
                    // Underrun plays silence; the phase stays put so the
                    // next frame resumes cleanly.
                    let sample = resampler.pull().unwrap_or(0.0);
                    for slot in block {
                        *slot = sample;
                    }
                }
            },
            |e| eprintln!("[call] speaker stream error: {e}"),
            None,
        )
        .ok()?;
    stream.play().ok()?;
    Some(stream)
}

// ---- ringtones ----

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Ring {
    // The caller's side: one short tone every few seconds.
    Back,
    // The callee's side: the ringtone, repeating until someone picks up.
    In,
}

impl Ring {
    // How long one turn of the pattern lasts before it starts over.
    fn period(self) -> f32 {
        match self {
            // Close to the phone network's cadence: a burst, then a rest.
            Ring::Back => 4.0,
            Ring::In => 3.0,
        }
    }

    // The ringtone: a four-note arpeggio, each note plucked and left to
    // decay, then silence for the rest of the turn.
    const NOTES: [(f32, f32); 4] =
        [(0.0, 659.26), (0.18, 830.61), (0.36, 987.77), (0.54, 1318.51)];

    // One sample of the pattern at `t` seconds into it.
    fn sample(self, t: f32) -> f32 {
        match self {
            Ring::Back => {
                // The two tones a ringback is made of, on for 1.2s.
                let level = window(t, 0.0, 1.2);
                if level <= 0.0 {
                    return 0.0;
                }
                (sine(t, 440.0) + sine(t, 480.0)) * 0.5 * level
            }
            Ring::In => Self::NOTES
                .iter()
                .map(|&(start, freq)| {
                    let local = t - start;
                    if !(0.0..=0.9).contains(&local) {
                        return 0.0;
                    }
                    // Plucked: full at the attack, gone within a second.
                    let decay = (-4.0 * local).exp();
                    // A touch of the octave above gives the note some bite.
                    let voice = sine(local, freq) * 0.8 + sine(local, freq * 2.0) * 0.2;
                    voice * decay * window(local, 0.0, 0.9)
                })
                .sum::<f32>(),
        }
    }
}

// A synthesized ring on the default output device. Lives on the UI
// thread next to the voice-note player; dropping it stops the sound.
pub struct Ringer {
    _stream: cpal::Stream,
}

impl Ringer {
    pub fn start(kind: Ring) -> Option<Ringer> {
        use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
        let device = cpal::default_host().default_output_device()?;
        let config = device.default_output_config().ok()?;
        let rate = config.sample_rate().0 as f32;
        let channels = config.channels() as usize;
        let period = kind.period();
        // Loud enough to be heard across the room, quiet enough not to
        // startle anyone wearing headphones.
        let gain = if kind == Ring::In { 0.22 } else { 0.16 };
        let mut t = 0f32;
        let stream = device
            .build_output_stream(
                &config.config(),
                move |out: &mut [f32], _| {
                    for block in out.chunks_mut(channels) {
                        let sample = (kind.sample(t) * gain).clamp(-1.0, 1.0);
                        for slot in block {
                            *slot = sample;
                        }
                        t += 1.0 / rate;
                        if t >= period {
                            t -= period;
                        }
                    }
                },
                |e| eprintln!("[call] ringtone stream error: {e}"),
                None,
            )
            .ok()?;
        stream.play().ok()?;
        Some(Ringer { _stream: stream })
    }
}

fn sine(t: f32, freq: f32) -> f32 {
    (t * std::f32::consts::TAU * freq).sin()
}

// A tone window with 20 ms fades, so the ring has no clicks at its edges.
fn window(t: f32, start: f32, len: f32) -> f32 {
    let local = t - start;
    if local < 0.0 || local > len {
        return 0.0;
    }
    const FADE: f32 = 0.02;
    (local / FADE).min((len - local) / FADE).clamp(0.0, 1.0)
}
