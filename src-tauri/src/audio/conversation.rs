//! Two-channel audio capture for the Conversations feature: microphone
//! ("me") plus WASAPI loopback on the default output device ("them").
//!
//! Loopback works by building an *input* stream on a *render* (output)
//! device — cpal's WASAPI backend transparently sets
//! `AUDCLNT_STREAMFLAGS_LOOPBACK` when it detects this
//! (cpal 0.15.3 `src/host/wasapi/device.rs:571`). The one trap: a render
//! device's `default_input_config()` returns `StreamTypeNotSupported` and
//! `supported_input_configs()` is empty by design — the mix format has to
//! be read via `default_output_config()` instead, which is what
//! `negotiate_loopback_config` below does.
//!
//! This is Windows-only in practice (WASAPI loopback has no cpal equivalent
//! on Linux/macOS), but nothing here is behind `cfg(windows)` — it's built
//! entirely on cpal's cross-platform `Device` trait, so it still type-checks
//! and unit-tests on any platform. On non-Windows hosts, opening an input
//! stream on the default output device will simply fail at `build_input_stream`
//! time and surface as `AudioError::StreamError`.
//!
//! Each channel runs its own VAD-gated accumulator on a dedicated thread and
//! finalizes a `ConversationChunk` (WAV-encoded, 16kHz mono) whenever the
//! speaker pauses. Chunks from both channels land on one `mpsc` queue; the
//! command layer transcribes each with Groq and persists/emits it tagged
//! with its own wall-clock `started_at_ms`. The two channels are merged
//! into one ordered transcript purely by sorting on that timestamp at read
//! time (`Database::get_conversation`) — no cross-channel sequencing
//! happens in the capture layer itself.

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{SampleFormat, StreamConfig};
use serde::Serialize;
use std::panic;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{SystemTime, UNIX_EPOCH};

use super::recorder::{AudioError, TARGET_SAMPLE_RATE, encode_wav, is_speechless, negotiate_config, resample};

/// Silence needed after speech before a chunk is finalized and sent for
/// transcription. Short enough to feel responsive turn-to-turn.
const SILENCE_HANGOVER_MS: u64 = 700;
/// Hard cap so a long monologue still yields chunks instead of buffering
/// (and delaying transcription of) an entire unbroken ramble.
const MAX_CHUNK_MS: u64 = 20_000;
/// Chunks shorter than this are dropped rather than sent to Groq — almost
/// always a stray noise blip, not speech.
const MIN_CHUNK_MS: u64 = 300;
/// Per-callback-frame RMS floor for the speech/silence decision. Frames run
/// only a few milliseconds, far under `is_speechless`'s 250ms minimum, so
/// this is a separate, simpler check; the finalized chunk is re-validated
/// with `is_speechless` before it's sent anywhere.
const FRAME_SILENCE_RMS_FLOOR: f32 = 0.0015;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Channel {
    Me,
    Them,
}

impl Channel {
    pub fn as_str(&self) -> &'static str {
        match self {
            Channel::Me => "me",
            Channel::Them => "them",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ConversationChunk {
    pub channel: Channel,
    /// Wall-clock ms since UNIX epoch when this chunk's audio started.
    pub started_at_ms: i64,
    pub wav: Vec<u8>,
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Accumulates mono f32 samples at `sample_rate`, splitting into chunks on
/// a silence hangover or a max-duration cutoff.
struct VadAccumulator {
    sample_rate: u32,
    buf: Vec<f32>,
    /// Samples classified as speech, tracked separately from `buf.len()`
    /// because `buf` also carries the trailing hangover silence (kept so a
    /// mid-sentence pause isn't chopped out of the encoded audio). Gating
    /// MIN_CHUNK_MS on `buf`'s total length would be a no-op: any
    /// hangover-triggered finalize already has >= SILENCE_HANGOVER_MS of
    /// trailing silence in `buf`, which alone exceeds MIN_CHUNK_MS.
    speech_samples: usize,
    silence_run_samples: usize,
    started_at_ms: Option<i64>,
}

impl VadAccumulator {
    fn new(sample_rate: u32) -> Self {
        Self {
            sample_rate,
            buf: Vec::new(),
            speech_samples: 0,
            silence_run_samples: 0,
            started_at_ms: None,
        }
    }

    fn speech_duration_ms(&self) -> u64 {
        (self.speech_samples as u64 * 1000) / self.sample_rate.max(1) as u64
    }

    fn duration_ms(&self) -> u64 {
        (self.buf.len() as u64 * 1000) / self.sample_rate.max(1) as u64
    }

    /// Feed one callback's worth of mono samples. Returns a finished chunk
    /// (start timestamp + raw mono samples at `sample_rate`) when a pause
    /// or the max duration is hit.
    fn push(&mut self, frame: &[f32]) -> Option<(i64, Vec<f32>)> {
        if frame.is_empty() {
            return None;
        }
        let rms = (frame.iter().map(|s| s * s).sum::<f32>() / frame.len() as f32).sqrt();
        let is_speech = rms >= FRAME_SILENCE_RMS_FLOOR;

        if is_speech {
            if self.buf.is_empty() {
                self.started_at_ms = Some(now_ms());
            }
            self.buf.extend_from_slice(frame);
            self.speech_samples += frame.len();
            self.silence_run_samples = 0;
        } else if !self.buf.is_empty() {
            // Keep buffering silence inside an utterance (a mid-sentence
            // pause shouldn't chop it in half) but count it toward the
            // hangover that eventually ends the utterance. Does NOT count
            // toward speech_samples — see the field doc above.
            self.buf.extend_from_slice(frame);
            self.silence_run_samples += frame.len();
        } else {
            return None;
        }

        let silence_ms = (self.silence_run_samples as u64 * 1000) / self.sample_rate.max(1) as u64;
        if silence_ms >= SILENCE_HANGOVER_MS || self.duration_ms() >= MAX_CHUNK_MS {
            return self.finalize();
        }
        None
    }

    /// Force-finalize whatever is buffered — used internally on
    /// hangover/max-duration, and externally to flush on stop. Returns
    /// `None` for an empty buffer, or one with too little actual speech in
    /// it (a stray noise blip followed by silence, not a real utterance).
    fn finalize(&mut self) -> Option<(i64, Vec<f32>)> {
        if self.buf.is_empty() || self.speech_duration_ms() < MIN_CHUNK_MS {
            self.buf.clear();
            self.speech_samples = 0;
            self.silence_run_samples = 0;
            self.started_at_ms = None;
            return None;
        }
        let started_at_ms = self.started_at_ms.take().unwrap_or_else(now_ms);
        let samples = std::mem::take(&mut self.buf);
        self.speech_samples = 0;
        self.silence_run_samples = 0;
        Some((started_at_ms, samples))
    }
}

fn set_error(error: &Mutex<Option<String>>, msg: String) {
    if let Ok(mut e) = error.lock() {
        *e = Some(msg);
    }
}

/// Turn a finalized native-rate buffer into a `ConversationChunk`, or `None`
/// if it's still effectively silence (belt-and-braces on top of the VAD).
fn to_chunk(channel: Channel, started_at_ms: i64, samples: Vec<f32>, native_rate: u32) -> Option<ConversationChunk> {
    if is_speechless(&samples, native_rate) {
        return None;
    }
    let mono_16k = if native_rate != TARGET_SAMPLE_RATE {
        resample(&samples, native_rate, TARGET_SAMPLE_RATE)
    } else {
        samples
    };
    let wav = encode_wav(&mono_16k, TARGET_SAMPLE_RATE).ok()?;
    Some(ConversationChunk { channel, started_at_ms, wav })
}

/// Shared state for an in-progress conversation capture. Managed as Tauri
/// state, mirroring `RecordingState`'s shape and locking conventions.
pub struct ConversationState {
    is_active: Arc<AtomicBool>,
    mic_thread: Mutex<Option<JoinHandle<()>>>,
    loopback_thread: Mutex<Option<JoinHandle<()>>>,
    mic_level: Arc<Mutex<f32>>,
    them_level: Arc<Mutex<f32>>,
    error: Arc<Mutex<Option<String>>>,
    chunk_rx: Mutex<Option<Receiver<ConversationChunk>>>,
}

impl Default for ConversationState {
    fn default() -> Self {
        Self {
            is_active: Arc::new(AtomicBool::new(false)),
            mic_thread: Mutex::new(None),
            loopback_thread: Mutex::new(None),
            mic_level: Arc::new(Mutex::new(0.0)),
            them_level: Arc::new(Mutex::new(0.0)),
            error: Arc::new(Mutex::new(None)),
            chunk_rx: Mutex::new(None),
        }
    }
}

impl ConversationState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_active(&self) -> bool {
        self.is_active.load(Ordering::SeqCst)
    }

    pub fn mic_level(&self) -> f32 {
        *self.mic_level.lock().unwrap()
    }

    pub fn them_level(&self) -> f32 {
        *self.them_level.lock().unwrap()
    }

    pub fn get_error(&self) -> Option<String> {
        self.error.lock().unwrap().clone()
    }

    /// Takes the chunk receiver so the command layer can drain it on a
    /// blocking task for the lifetime of the conversation. Single-consumer:
    /// call this once, right after `ConversationCapture::start`.
    pub fn take_chunk_receiver(&self) -> Option<Receiver<ConversationChunk>> {
        self.chunk_rx.lock().unwrap().take()
    }
}

pub struct ConversationCapture;

impl ConversationCapture {
    /// Start both capture threads. `mic_device_id` selects the microphone
    /// the same way `AudioRecorder::start` does; the loopback side always
    /// uses the system default output device (whatever the call app plays
    /// through).
    pub fn start(state: &ConversationState, mic_device_id: Option<String>) -> Result<(), AudioError> {
        if state.is_active.swap(true, Ordering::SeqCst) {
            return Err(AudioError::AlreadyRecording);
        }
        *state.error.lock().unwrap() = None;

        let (tx, rx) = channel::<ConversationChunk>();
        *state.chunk_rx.lock().unwrap() = Some(rx);

        let is_active = Arc::clone(&state.is_active);
        let mic_level = Arc::clone(&state.mic_level);
        let mic_error = Arc::clone(&state.error);
        let mic_tx = tx.clone();
        let mic_is_active = Arc::clone(&is_active);
        let mic_handle = std::thread::Builder::new()
            .name("whisperi-conv-mic".to_string())
            .spawn(move || {
                let result = panic::catch_unwind(panic::AssertUnwindSafe(|| {
                    run_capture_thread(
                        Channel::Me,
                        CaptureSource::Microphone(mic_device_id),
                        mic_tx,
                        mic_level,
                        mic_is_active,
                    );
                }));
                if result.is_err() {
                    log::error!("[Conversation] mic capture thread panicked");
                    set_error(&mic_error, "Microphone capture thread panicked".to_string());
                }
            })
            .map_err(|e| AudioError::StreamError(format!("Failed to spawn mic thread: {}", e)))?;

        let them_level = Arc::clone(&state.them_level);
        let loop_error = Arc::clone(&state.error);
        let loop_tx = tx;
        let loop_is_active = Arc::clone(&is_active);
        let loopback_handle = std::thread::Builder::new()
            .name("whisperi-conv-loopback".to_string())
            .spawn(move || {
                let result = panic::catch_unwind(panic::AssertUnwindSafe(|| {
                    run_capture_thread(Channel::Them, CaptureSource::Loopback, loop_tx, them_level, loop_is_active);
                }));
                if result.is_err() {
                    log::error!("[Conversation] loopback capture thread panicked");
                    set_error(&loop_error, "System-audio capture thread panicked".to_string());
                }
            })
            .map_err(|e| AudioError::StreamError(format!("Failed to spawn loopback thread: {}", e)))?;

        *state.mic_thread.lock().unwrap() = Some(mic_handle);
        *state.loopback_thread.lock().unwrap() = Some(loopback_handle);
        Ok(())
    }

    /// Stop both capture threads. Each thread flushes its trailing buffered
    /// utterance (if long enough to count as speech) as one last chunk
    /// before exiting, so the tail of the conversation isn't lost.
    pub fn stop(state: &ConversationState) -> Result<(), AudioError> {
        if !state.is_active.swap(false, Ordering::SeqCst) {
            return Err(AudioError::NotRecording);
        }
        if let Some(h) = state.mic_thread.lock().unwrap().take() {
            let _ = h.join();
        }
        if let Some(h) = state.loopback_thread.lock().unwrap().take() {
            let _ = h.join();
        }
        // Both thread-owned Sender clones are dropped by now (threads have
        // joined), so the consumer's rx.recv() will see the channel close
        // and exit on its own — no explicit signal needed here.
        Ok(())
    }
}

enum CaptureSource {
    Microphone(Option<String>),
    Loopback,
}

/// Negotiate the mix format for a render (output) device via
/// `default_output_config()` — the only config query that works for a
/// device we intend to loop back from. See the module doc for why
/// `default_input_config()`/`supported_input_configs()` don't apply here.
fn negotiate_loopback_config(device: &cpal::Device) -> Result<(StreamConfig, SampleFormat), AudioError> {
    let default = device
        .default_output_config()
        .map_err(|e| AudioError::ConfigError(e.to_string()))?;
    let sample_format = default.sample_format();
    let config: StreamConfig = default.into();
    Ok((config, sample_format))
}

type OpenedDevice = (cpal::Device, StreamConfig, SampleFormat);

fn open_mic_device(host: &cpal::Host, device_id: &Option<String>) -> Result<OpenedDevice, AudioError> {
    let device = match device_id {
        Some(id) => host
            .input_devices()
            .map_err(|e| AudioError::DeviceNotFound(e.to_string()))?
            .find(|d| d.name().map(|n| n == *id).unwrap_or(false))
            .ok_or_else(|| AudioError::DeviceNotFound(id.clone()))?,
        None => host.default_input_device().ok_or(AudioError::NoDevice)?,
    };
    let (config, format) = negotiate_config(&device)?;
    Ok((device, config, format))
}

fn open_loopback_device(host: &cpal::Host) -> Result<OpenedDevice, AudioError> {
    let device = host.default_output_device().ok_or(AudioError::NoDevice)?;
    let (config, format) = negotiate_loopback_config(&device)?;
    Ok((device, config, format))
}

fn run_capture_thread(
    channel_tag: Channel,
    source: CaptureSource,
    tx: Sender<ConversationChunk>,
    level: Arc<Mutex<f32>>,
    is_active: Arc<AtomicBool>,
) {
    let host = cpal::default_host();

    let device_and_config = match &source {
        CaptureSource::Microphone(device_id) => open_mic_device(&host, device_id),
        CaptureSource::Loopback => open_loopback_device(&host),
    };

    let (device, config, sample_format) = match device_and_config {
        Ok(v) => v,
        Err(e) => {
            log::error!("[Conversation] {:?} capture setup failed: {}", channel_tag, e);
            return;
        }
    };

    let channels = config.channels as usize;
    let sample_rate = config.sample_rate.0;
    let accumulator = Arc::new(Mutex::new(VadAccumulator::new(sample_rate)));

    let stream_result = build_capture_stream(
        &device,
        &config,
        sample_format,
        channels,
        sample_rate,
        Arc::clone(&accumulator),
        Arc::clone(&level),
        tx.clone(),
        channel_tag,
    );

    let stream = match stream_result {
        Ok(s) => s,
        Err(e) => {
            log::error!("[Conversation] {:?} failed to build stream: {}", channel_tag, e);
            return;
        }
    };

    if let Err(e) = stream.play() {
        log::error!("[Conversation] {:?} failed to play stream: {}", channel_tag, e);
        return;
    }

    while is_active.load(Ordering::SeqCst) {
        std::thread::sleep(std::time::Duration::from_millis(10));
    }

    drop(stream);

    // Flush whatever's still buffered as a final chunk.
    if let Ok(mut acc) = accumulator.lock() {
        if let Some((started_at_ms, samples)) = acc.finalize() {
            if let Some(chunk) = to_chunk(channel_tag, started_at_ms, samples, sample_rate) {
                let _ = tx.send(chunk);
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn build_capture_stream(
    device: &cpal::Device,
    config: &StreamConfig,
    sample_format: SampleFormat,
    channels: usize,
    sample_rate: u32,
    accumulator: Arc<Mutex<VadAccumulator>>,
    level: Arc<Mutex<f32>>,
    tx: Sender<ConversationChunk>,
    channel_tag: Channel,
) -> Result<cpal::Stream, AudioError> {
    macro_rules! build {
        ($t:ty) => {
            build_capture_stream_typed::<$t>(device, config, channels, sample_rate, accumulator, level, tx, channel_tag)
        };
    }
    match sample_format {
        SampleFormat::F32 => build!(f32),
        SampleFormat::I16 => build!(i16),
        SampleFormat::U16 => build!(u16),
        SampleFormat::I8 => build!(i8),
        SampleFormat::U8 => build!(u8),
        SampleFormat::I32 => build!(i32),
        SampleFormat::U32 => build!(u32),
        SampleFormat::I64 => build!(i64),
        SampleFormat::U64 => build!(u64),
        SampleFormat::F64 => build!(f64),
        _ => Err(AudioError::ConfigError(format!("Unsupported sample format: {:?}", sample_format))),
    }
}

#[allow(clippy::too_many_arguments)]
fn build_capture_stream_typed<T: cpal::Sample + cpal::SizedSample + Send + 'static>(
    device: &cpal::Device,
    config: &StreamConfig,
    channels: usize,
    sample_rate: u32,
    accumulator: Arc<Mutex<VadAccumulator>>,
    level: Arc<Mutex<f32>>,
    tx: Sender<ConversationChunk>,
    channel_tag: Channel,
) -> Result<cpal::Stream, AudioError>
where
    f32: cpal::FromSample<T>,
{
    let err_callback = move |err: cpal::StreamError| {
        log::error!("[Conversation] {:?} stream error: {}", channel_tag, err);
    };

    let stream = device
        .build_input_stream(
            config,
            move |data: &[T], _: &cpal::InputCallbackInfo| {
                let mut mono = Vec::with_capacity(data.len() / channels.max(1));
                let mut peak: f32 = 0.0;
                for frame in data.chunks(channels.max(1)) {
                    let sample: f32 =
                        frame.iter().map(|s| <f32 as cpal::Sample>::from_sample(*s)).sum::<f32>() / channels.max(1) as f32;
                    mono.push(sample);
                    peak = peak.max(sample.abs());
                }

                if let Ok(mut l) = level.lock() {
                    *l = peak;
                }

                let finished = accumulator.lock().ok().and_then(|mut acc| acc.push(&mono));
                if let Some((started_at_ms, samples)) = finished {
                    if let Some(chunk) = to_chunk(channel_tag, started_at_ms, samples, sample_rate) {
                        let _ = tx.send(chunk);
                    }
                }
            },
            err_callback,
            None,
        )
        .map_err(|e| AudioError::StreamError(e.to_string()))?;

    Ok(stream)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(n: usize, amplitude: f32) -> Vec<f32> {
        (0..n).map(|i| amplitude * (i as f32 * 0.3).sin()).collect()
    }

    fn silence(n: usize) -> Vec<f32> {
        vec![0.0; n]
    }

    #[test]
    fn vad_does_not_finalize_while_speech_continues() {
        let mut acc = VadAccumulator::new(16_000);
        // 16 frames of 100 loud samples = 1600 samples ≈ 100ms of continuous speech.
        for _ in 0..16 {
            assert!(acc.push(&sine(100, 0.5)).is_none());
        }
    }

    #[test]
    fn vad_finalizes_after_silence_hangover() {
        let mut acc = VadAccumulator::new(16_000);
        // ~300ms of speech first, well over MIN_CHUNK_MS.
        for _ in 0..30 {
            acc.push(&sine(160, 0.5));
        }
        // Push silence until the hangover threshold trips.
        let mut result = None;
        for _ in 0..200 {
            if let Some(r) = acc.push(&silence(160)) {
                result = Some(r);
                break;
            }
        }
        let (started_at_ms, samples) = result.expect("should finalize after hangover");
        assert!(started_at_ms > 0);
        assert!(!samples.is_empty());
    }

    #[test]
    fn vad_drops_chunks_shorter_than_min_duration() {
        let mut acc = VadAccumulator::new(16_000);
        // A single very short blip of speech (well under MIN_CHUNK_MS), then
        // immediate long silence — should be discarded, not emitted.
        acc.push(&sine(50, 0.5));
        let mut result = None;
        for _ in 0..200 {
            if let Some(r) = acc.push(&silence(160)) {
                result = Some(r);
                break;
            }
        }
        assert!(result.is_none(), "sub-minimum blip should be dropped, not finalized");
    }

    #[test]
    fn vad_finalizes_on_max_duration_even_without_silence() {
        let mut acc = VadAccumulator::new(16_000);
        // Each iteration pushes 160 samples = 10ms at 16kHz; MAX_CHUNK_MS is
        // 20_000ms, so this needs ~2000 iterations to reach it — push a
        // comfortable margin past that.
        let mut result = None;
        for _ in 0..2200 {
            if let Some(r) = acc.push(&sine(160, 0.5)) {
                result = Some(r);
                break;
            }
        }
        let (_, samples) = result.expect("should force-finalize a long monologue");
        assert!(samples.len() as u64 * 1000 / 16_000 <= MAX_CHUNK_MS + 200);
    }

    #[test]
    fn vad_finalize_on_empty_buffer_returns_none() {
        let mut acc = VadAccumulator::new(16_000);
        assert!(acc.finalize().is_none());
    }

    #[test]
    fn to_chunk_rejects_near_silent_buffer() {
        // Long enough to pass duration, but flat silence — is_speechless
        // should reject it at the finalize boundary regardless of the VAD.
        let samples = silence(16_000); // 1s of silence at 16kHz
        assert!(to_chunk(Channel::Me, now_ms(), samples, 16_000).is_none());
    }

    #[test]
    fn to_chunk_accepts_real_speech_like_signal() {
        let samples = sine(16_000, 0.3); // 1s, well above the RMS floor
        let chunk = to_chunk(Channel::Them, now_ms(), samples, 16_000);
        assert!(chunk.is_some());
        assert_eq!(chunk.unwrap().channel, Channel::Them);
    }

    #[test]
    fn channel_as_str_matches_serde_rename() {
        assert_eq!(Channel::Me.as_str(), "me");
        assert_eq!(Channel::Them.as_str(), "them");
    }
}
