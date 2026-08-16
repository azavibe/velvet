//! Single-channel (microphone only) audio capture for Notes.
//!
//! Deliberately separate from `conversation.rs` rather than reusing it with
//! the loopback side turned off: Notes wants an explicit pause/resume the
//! user drives (e.g. "pause the video, jot a note, unpause"), not just a
//! silence gap the VAD skips over on its own. Pausing here stops audio from
//! reaching the VAD accumulator at all — including background noise or
//! stray words the user doesn't want transcribed while paused — and flushes
//! whatever was buffered so a pause mid-sentence doesn't lose it.
//!
//! The VAD chunking (silence-hangover, max-duration, min-duration) is the
//! same shape as `conversation.rs`'s mic side; kept as a separate copy
//! rather than factored out because the two are likely to diverge (Notes
//! may eventually want longer silence tolerance for dictating slowly) and
//! sharing now would just add an indirection for no current benefit.

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

const SILENCE_HANGOVER_MS: u64 = 700;
const MAX_CHUNK_MS: u64 = 20_000;
const MIN_CHUNK_MS: u64 = 300;
const FRAME_SILENCE_RMS_FLOOR: f32 = 0.0015;

#[derive(Debug, Clone, Serialize)]
pub struct NoteChunk {
    pub started_at_ms: i64,
    pub wav: Vec<u8>,
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Same shape as `conversation::VadAccumulator` — see that module for the
/// field-by-field rationale (speech_samples vs buf.len(), etc.).
struct VadAccumulator {
    sample_rate: u32,
    buf: Vec<f32>,
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

fn to_chunk(started_at_ms: i64, samples: Vec<f32>, native_rate: u32) -> Option<NoteChunk> {
    if is_speechless(&samples, native_rate) {
        return None;
    }
    let mono_16k = if native_rate != TARGET_SAMPLE_RATE {
        resample(&samples, native_rate, TARGET_SAMPLE_RATE)
    } else {
        samples
    };
    let wav = encode_wav(&mono_16k, TARGET_SAMPLE_RATE).ok()?;
    Some(NoteChunk { started_at_ms, wav })
}

/// Shared state for an in-progress note capture. Mirrors `ConversationState`
/// but single-channel and with an explicit pause flag.
pub struct NoteCaptureState {
    is_active: Arc<AtomicBool>,
    is_paused: Arc<AtomicBool>,
    thread: Mutex<Option<JoinHandle<()>>>,
    level: Arc<Mutex<f32>>,
    error: Arc<Mutex<Option<String>>>,
    chunk_rx: Mutex<Option<Receiver<NoteChunk>>>,
}

impl Default for NoteCaptureState {
    fn default() -> Self {
        Self {
            is_active: Arc::new(AtomicBool::new(false)),
            is_paused: Arc::new(AtomicBool::new(false)),
            thread: Mutex::new(None),
            level: Arc::new(Mutex::new(0.0)),
            error: Arc::new(Mutex::new(None)),
            chunk_rx: Mutex::new(None),
        }
    }
}

impl NoteCaptureState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_active(&self) -> bool {
        self.is_active.load(Ordering::SeqCst)
    }

    pub fn is_paused(&self) -> bool {
        self.is_paused.load(Ordering::SeqCst)
    }

    pub fn level(&self) -> f32 {
        *self.level.lock().unwrap()
    }

    pub fn get_error(&self) -> Option<String> {
        self.error.lock().unwrap().clone()
    }

    pub fn take_chunk_receiver(&self) -> Option<Receiver<NoteChunk>> {
        self.chunk_rx.lock().unwrap().take()
    }

    pub fn pause(&self) {
        self.is_paused.store(true, Ordering::SeqCst);
    }

    pub fn resume(&self) {
        self.is_paused.store(false, Ordering::SeqCst);
    }
}

pub struct NoteCapture;

impl NoteCapture {
    pub fn start(state: &NoteCaptureState, mic_device_id: Option<String>) -> Result<(), AudioError> {
        if state.is_active.swap(true, Ordering::SeqCst) {
            return Err(AudioError::AlreadyRecording);
        }
        state.is_paused.store(false, Ordering::SeqCst);
        *state.error.lock().unwrap() = None;

        let (tx, rx) = channel::<NoteChunk>();
        *state.chunk_rx.lock().unwrap() = Some(rx);

        let is_active = Arc::clone(&state.is_active);
        let is_paused = Arc::clone(&state.is_paused);
        let level = Arc::clone(&state.level);
        let error = Arc::clone(&state.error);
        let handle = std::thread::Builder::new()
            .name("aral-note-mic".to_string())
            .spawn(move || {
                let result = panic::catch_unwind(panic::AssertUnwindSafe(|| {
                    run_capture_thread(mic_device_id, tx, level, is_active, is_paused);
                }));
                if result.is_err() {
                    log::error!("[Note] mic capture thread panicked");
                    set_error(&error, "Microphone capture thread panicked".to_string());
                }
            })
            .map_err(|e| AudioError::StreamError(format!("Failed to spawn mic thread: {}", e)))?;

        *state.thread.lock().unwrap() = Some(handle);
        Ok(())
    }

    pub fn stop(state: &NoteCaptureState) -> Result<(), AudioError> {
        if !state.is_active.swap(false, Ordering::SeqCst) {
            return Err(AudioError::NotRecording);
        }
        state.is_paused.store(false, Ordering::SeqCst);
        if let Some(h) = state.thread.lock().unwrap().take() {
            let _ = h.join();
        }
        Ok(())
    }
}

fn run_capture_thread(
    mic_device_id: Option<String>,
    tx: Sender<NoteChunk>,
    level: Arc<Mutex<f32>>,
    is_active: Arc<AtomicBool>,
    is_paused: Arc<AtomicBool>,
) {
    let host = cpal::default_host();

    let device = match &mic_device_id {
        Some(id) => host
            .input_devices()
            .ok()
            .and_then(|mut it| it.find(|d| d.name().map(|n| n == *id).unwrap_or(false))),
        None => host.default_input_device(),
    };
    let Some(device) = device else {
        log::error!("[Note] no microphone device available");
        return;
    };
    let (config, sample_format) = match negotiate_config(&device) {
        Ok(v) => v,
        Err(e) => {
            log::error!("[Note] failed to negotiate mic config: {}", e);
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
        Arc::clone(&is_paused),
    );

    let stream = match stream_result {
        Ok(s) => s,
        Err(e) => {
            log::error!("[Note] failed to build stream: {}", e);
            return;
        }
    };

    if let Err(e) = stream.play() {
        log::error!("[Note] failed to play stream: {}", e);
        return;
    }

    let mut was_paused = false;
    while is_active.load(Ordering::SeqCst) {
        let now_paused = is_paused.load(Ordering::SeqCst);
        // Flush the buffered utterance the moment pause is requested, so a
        // mid-sentence pause is transcribed instead of silently dropped
        // (and doesn't bleed into whatever's said after resuming).
        if now_paused && !was_paused {
            if let Ok(mut acc) = accumulator.lock() {
                if let Some((started_at_ms, samples)) = acc.finalize() {
                    if let Some(chunk) = to_chunk(started_at_ms, samples, sample_rate) {
                        let _ = tx.send(chunk);
                    }
                }
            }
        }
        was_paused = now_paused;
        std::thread::sleep(std::time::Duration::from_millis(10));
    }

    drop(stream);

    if let Ok(mut acc) = accumulator.lock() {
        if let Some((started_at_ms, samples)) = acc.finalize() {
            if let Some(chunk) = to_chunk(started_at_ms, samples, sample_rate) {
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
    tx: Sender<NoteChunk>,
    is_paused: Arc<AtomicBool>,
) -> Result<cpal::Stream, AudioError> {
    macro_rules! build {
        ($t:ty) => {
            build_capture_stream_typed::<$t>(device, config, channels, sample_rate, accumulator, level, tx, is_paused)
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
    tx: Sender<NoteChunk>,
    is_paused: Arc<AtomicBool>,
) -> Result<cpal::Stream, AudioError>
where
    f32: cpal::FromSample<T>,
{
    let err_callback = move |err: cpal::StreamError| {
        log::error!("[Note] stream error: {}", err);
    };

    let stream = device
        .build_input_stream(
            config,
            move |data: &[T], _: &cpal::InputCallbackInfo| {
                if is_paused.load(Ordering::SeqCst) {
                    if let Ok(mut l) = level.lock() {
                        *l = 0.0;
                    }
                    return;
                }
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
                    if let Some(chunk) = to_chunk(started_at_ms, samples, sample_rate) {
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
        for _ in 0..16 {
            assert!(acc.push(&sine(100, 0.5)).is_none());
        }
    }

    #[test]
    fn vad_finalizes_after_silence_hangover() {
        let mut acc = VadAccumulator::new(16_000);
        for _ in 0..30 {
            acc.push(&sine(160, 0.5));
        }
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
    fn to_chunk_rejects_near_silent_buffer() {
        let samples = silence(16_000);
        assert!(to_chunk(now_ms(), samples, 16_000).is_none());
    }

    #[test]
    fn to_chunk_accepts_real_speech_like_signal() {
        let samples = sine(16_000, 0.3);
        let chunk = to_chunk(now_ms(), samples, 16_000);
        assert!(chunk.is_some());
    }

    #[test]
    fn note_capture_state_pause_resume_toggles() {
        let state = NoteCaptureState::new();
        assert!(!state.is_paused());
        state.pause();
        assert!(state.is_paused());
        state.resume();
        assert!(!state.is_paused());
    }
}
