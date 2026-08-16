//! Tauri commands for the Conversations feature: start/stop dual-channel
//! capture, drain transcribed chunks into the DB + frontend events, and
//! generate on-demand suggestions from a persona prompt + the transcript
//! so far.

use std::fs::File;
use std::io::BufWriter;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Emitter, Manager, State};

use super::{ResultExt, recordings_dir};
use crate::audio::conversation::{Channel, ConversationCapture, ConversationChunk, ConversationState};
use crate::audio::recorder::TARGET_SAMPLE_RATE;
use crate::database::{ConversationDetail, ConversationSummary, Database};
use crate::reasoning::{self, ReasoningRequest};

/// Streams each channel's transcribed chunks to its own WAV file on disk as
/// the conversation runs, rather than buffering the whole session in
/// memory — chunks arrive already resampled to `TARGET_SAMPLE_RATE`/mono
/// (see `audio::conversation::to_chunk`), which is exactly the format
/// written here, so no re-encoding is needed beyond decode-then-append.
/// Writers open lazily on each channel's first chunk, so a channel that
/// never captures anything (e.g. no one on the other end talks) leaves no
/// file rather than an empty one.
#[derive(Default)]
pub struct ConversationAudioArchive {
    me: Mutex<Option<hound::WavWriter<BufWriter<File>>>>,
    them: Mutex<Option<hound::WavWriter<BufWriter<File>>>>,
}

fn wav_spec() -> hound::WavSpec {
    hound::WavSpec {
        channels: 1,
        sample_rate: TARGET_SAMPLE_RATE,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    }
}

impl ConversationAudioArchive {
    /// Appends one chunk's decoded samples to the given channel's file,
    /// opening (and recording the path via `on_open`) on first use.
    /// Best-effort throughout: a failure here should never take down
    /// transcription, which is why every error just logs and returns.
    fn append(&self, channel: Channel, wav_bytes: &[u8], path_for_new_writer: impl FnOnce() -> std::path::PathBuf, on_open: impl FnOnce(&std::path::Path)) {
        let mutex = match channel {
            Channel::Me => &self.me,
            Channel::Them => &self.them,
        };
        let mut guard = match mutex.lock() {
            Ok(g) => g,
            Err(_) => return,
        };
        if guard.is_none() {
            let path = path_for_new_writer();
            match hound::WavWriter::create(&path, wav_spec()) {
                Ok(w) => {
                    on_open(&path);
                    *guard = Some(w);
                }
                Err(e) => {
                    log::error!("[Conversation] failed to open audio archive {:?}: {}", path, e);
                    return;
                }
            }
        }
        let Some(writer) = guard.as_mut() else { return };
        let mut reader = match hound::WavReader::new(std::io::Cursor::new(wav_bytes)) {
            Ok(r) => r,
            Err(e) => {
                log::error!("[Conversation] failed to decode chunk for archiving: {}", e);
                return;
            }
        };
        for sample in reader.samples::<i16>() {
            match sample {
                Ok(s) => {
                    let _ = writer.write_sample(s);
                }
                Err(_) => break,
            }
        }
        let _ = writer.flush();
    }

    /// Finalizes (patches the WAV header, flushes) and drops both writers.
    /// Safe to call even if a writer was never opened, or was already
    /// finalized — a late trailing chunk after stop() just has nothing to
    /// append to and is dropped from the archive (still transcribed fine).
    pub(crate) fn finalize(&self) {
        if let Ok(mut g) = self.me.lock() {
            if let Some(w) = g.take() {
                let _ = w.finalize();
            }
        }
        if let Ok(mut g) = self.them.lock() {
            if let Some(w) = g.take() {
                let _ = w.finalize();
            }
        }
    }
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Window to look back for a possible echo, and the word-overlap ratio
/// above which a "me" utterance is treated as speaker output bleeding into
/// the microphone rather than the user actually speaking.
///
/// Known limitation: this drops the *whole* matching "me" utterance, not
/// just the echoed portion. If the leaked audio runs directly into the
/// user's real reply with no pause between them (so the VAD accumulator
/// buffers them as one chunk), that reply is lost along with the echo —
/// trimming just the overlapping prefix would need word-level alignment
/// between the two transcriptions, which this deliberately simple
/// same-source-audio check doesn't attempt. Headphones avoid the whole
/// class of problem; this only helps the on-speakers case.
const ECHO_WINDOW_MS: i64 = 3000;
const ECHO_OVERLAP_THRESHOLD: f64 = 0.5;

fn normalized_words(text: &str) -> std::collections::HashSet<String> {
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(|w| w.to_string())
        .collect()
}

/// Jaccard similarity (intersection / union) of the two texts' word sets.
/// Cheap and dependency-free — exact wording rarely matches between two
/// independent Whisper transcriptions of the *same* audio (different
/// channel gain, VAD boundaries), so this is deliberately loose rather
/// than requiring a near-exact match.
fn word_overlap_ratio(a: &str, b: &str) -> f64 {
    let wa = normalized_words(a);
    let wb = normalized_words(b);
    if wa.is_empty() || wb.is_empty() {
        return 0.0;
    }
    let intersection = wa.intersection(&wb).count();
    let union = wa.union(&wb).count();
    intersection as f64 / union as f64
}

#[derive(Clone, serde::Serialize)]
struct ConversationUtterancePayload {
    id: i64,
    conversation_id: i64,
    channel: String,
    started_at_ms: i64,
    text: String,
}

#[derive(Clone, serde::Serialize)]
struct ConversationErrorPayload {
    conversation_id: i64,
    message: String,
}

#[tauri::command]
pub async fn start_conversation(
    app: AppHandle,
    conv_state: State<'_, Arc<ConversationState>>,
    db: State<'_, Database>,
    mic_device_id: Option<String>,
    groq_api_key: String,
    persona_name: Option<String>,
) -> Result<i64, String> {
    log::info!("[Conversation] starting, persona={:?}", persona_name);

    let conversation_id = db.create_conversation(persona_name.as_deref()).str_err()?;

    if let Err(e) = ConversationCapture::start(&**conv_state, mic_device_id) {
        // Roll back the DB row we just created rather than leaving an
        // empty, never-ended conversation behind.
        let _ = db.delete_conversation(conversation_id);
        return Err(e.to_string());
    }

    // Broadcast to every window (not just the one that called this command)
    // so any window's UI can adopt this conversation and the hotkey stays
    // live regardless of which window is focused or visible.
    #[derive(Clone, serde::Serialize)]
    struct ConversationStartedPayload {
        conversation_id: i64,
        persona_name: Option<String>,
    }
    let _ = app.emit(
        "conversation-started",
        ConversationStartedPayload { conversation_id, persona_name: persona_name.clone() },
    );

    let rx = match conv_state.take_chunk_receiver() {
        Some(rx) => rx,
        None => {
            let _ = ConversationCapture::stop(&**conv_state);
            let _ = db.delete_conversation(conversation_id);
            return Err("Conversation capture started without a chunk receiver".to_string());
        }
    };

    let app_for_consumer = app.clone();
    let api_key = groq_api_key;

    // Drain finished chunks on a blocking thread (std::mpsc::Receiver::recv
    // blocks); spawn each chunk's transcription as its own async task so a
    // slow "them" chunk never delays "me" chunks queued behind it.
    tauri::async_runtime::spawn_blocking(move || {
        while let Ok(chunk) = rx.recv() {
            let app2 = app_for_consumer.clone();
            let api_key2 = api_key.clone();
            tauri::async_runtime::spawn(async move {
                handle_conversation_chunk(app2, conversation_id, chunk, api_key2).await;
            });
        }
        log::info!("[Conversation] chunk consumer ended for conversation {}", conversation_id);
    });

    Ok(conversation_id)
}

async fn handle_conversation_chunk(app: AppHandle, conversation_id: i64, chunk: ConversationChunk, api_key: String) {
    let channel_str = chunk.channel.as_str();
    let started_at_ms = chunk.started_at_ms;

    // Archive regardless of transcription outcome — it's real audio that
    // happened, even if this particular chunk fails to transcribe or comes
    // back empty. Runs on a blocking thread since WAV decode + file I/O
    // shouldn't happen on the async runtime.
    {
        let app_for_archive = app.clone();
        let wav_for_archive = chunk.wav.clone();
        let channel = chunk.channel;
        tauri::async_runtime::spawn_blocking(move || {
            let archive = app_for_archive.state::<ConversationAudioArchive>();
            archive.append(
                channel,
                &wav_for_archive,
                || {
                    recordings_dir(&app_for_archive)
                        .unwrap_or_else(|_| std::env::temp_dir())
                        .join(format!("conversation-{conversation_id}-{}.wav", channel.as_str()))
                },
                |path| {
                    let db = app_for_archive.state::<Database>();
                    let path_str = path.to_string_lossy().to_string();
                    let (me, them) = match channel {
                        Channel::Me => (Some(path_str.as_str()), None),
                        Channel::Them => (None, Some(path_str.as_str())),
                    };
                    let _ = db.update_conversation_audio_paths(conversation_id, me, them);
                },
            );
        });
    }

    let result = crate::transcription::cloud::transcribe_groq(
        chunk.wav,
        &api_key,
        "whisper-large-v3-turbo",
        None,
        None,
    )
    .await;

    let text = match result {
        Ok(t) => t.text.trim().to_string(),
        Err(e) => {
            log::error!("[Conversation] {} chunk transcription failed: {}", channel_str, e);
            let _ = app.emit(
                "conversation-error",
                ConversationErrorPayload { conversation_id, message: e.to_string() },
            );
            return;
        }
    };

    if text.is_empty() {
        return;
    }

    let db = app.state::<Database>();

    // Echo check, "me" side only: if the loopback ("them") channel said
    // something near-identical in the last few seconds, this "me" chunk is
    // almost certainly speaker output bleeding into the microphone, not
    // the user actually speaking. Deliberately asymmetric — loopback is
    // the authoritative capture of whatever's playing through the
    // speakers, so a "them" chunk is never dropped just because the mic
    // happened to pick up something similar; only the mic side can be the
    // artifact here.
    if channel_str == "me" {
        if let Ok(recent) = db.get_recent_utterances(conversation_id, started_at_ms - ECHO_WINDOW_MS) {
            let is_echo = recent
                .iter()
                .filter(|u| u.channel == "them")
                .any(|u| word_overlap_ratio(&u.text, &text) >= ECHO_OVERLAP_THRESHOLD);
            if is_echo {
                log::info!("[Conversation] dropped likely-echo me chunk: {:?}", text);
                return;
            }
        }
    }

    let utterance_id = match db.insert_conversation_utterance(conversation_id, channel_str, started_at_ms, &text) {
        Ok(id) => id,
        Err(e) => {
            log::error!("[Conversation] failed to persist utterance: {}", e);
            return;
        }
    };

    let _ = app.emit(
        "conversation-utterance",
        ConversationUtterancePayload {
            id: utterance_id,
            conversation_id,
            channel: channel_str.to_string(),
            started_at_ms,
            text,
        },
    );
}

#[tauri::command]
pub fn stop_conversation(
    app: AppHandle,
    conv_state: State<'_, Arc<ConversationState>>,
    db: State<'_, Database>,
    conversation_id: i64,
    title: Option<String>,
) -> Result<(), String> {
    // Stopping joins both capture threads; each flushes its trailing
    // buffered utterance as one last chunk before exiting. That final
    // chunk is still transcribed asynchronously by the consumer task
    // spawned in start_conversation, so a `conversation-utterance` event
    // for it can arrive slightly after this command returns.
    ConversationCapture::stop(&**conv_state).str_err()?;
    db.end_conversation(conversation_id, title.as_deref()).str_err()?;
    let _ = app.emit("conversation-stopped", conversation_id);

    let app_for_archive = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        app_for_archive.state::<ConversationAudioArchive>().finalize();
    });

    Ok(())
}

#[tauri::command]
pub fn get_conversation_audio_levels(conv_state: State<'_, Arc<ConversationState>>) -> Result<(f32, f32), String> {
    Ok((conv_state.mic_level(), conv_state.them_level()))
}

#[tauri::command]
pub fn is_conversation_active(conv_state: State<'_, Arc<ConversationState>>) -> Result<bool, String> {
    Ok(conv_state.is_active())
}

/// Surfaces a capture-thread setup/stream error (e.g. no loopback-capable
/// output device) so the frontend can show it instead of silently sitting
/// on an empty transcript.
#[tauri::command]
pub fn get_conversation_error(conv_state: State<'_, Arc<ConversationState>>) -> Result<Option<String>, String> {
    Ok(conv_state.get_error())
}

#[tauri::command]
pub fn list_conversations(db: State<'_, Database>, limit: u32, offset: u32) -> Result<Vec<ConversationSummary>, String> {
    db.list_conversations(limit, offset).str_err()
}

#[tauri::command]
pub fn get_conversation(db: State<'_, Database>, conversation_id: i64) -> Result<ConversationDetail, String> {
    db.get_conversation(conversation_id).str_err()
}

#[tauri::command]
pub fn delete_conversation(db: State<'_, Database>, conversation_id: i64) -> Result<(), String> {
    db.delete_conversation(conversation_id).str_err()
}

/// Build a suggested reply from a persona's system prompt plus the full
/// transcript so far, using the same reasoning pipeline enhancement uses.
/// Fired either automatically on turn-end (loopback VAD hangover, driven
/// from the frontend today) or on demand via the always-live hotkey.
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn generate_suggestion(
    app: AppHandle,
    db: State<'_, Database>,
    conversation_id: i64,
    persona_system_prompt: String,
    persona_name: Option<String>,
    model: String,
    provider: String,
    api_key: String,
) -> Result<String, String> {
    let detail = db.get_conversation(conversation_id).str_err()?;

    if detail.utterances.is_empty() {
        return Err("No transcript yet".to_string());
    }

    let transcript = detail
        .utterances
        .iter()
        .map(|u| format!("{}: {}", if u.channel == "me" { "Me" } else { "Them" }, u.text))
        .collect::<Vec<_>>()
        .join("\n");

    let req = ReasoningRequest {
        text: format!("[CONVERSATION_TRANSCRIPT]\n{}", transcript),
        model,
        provider,
        system_prompt: persona_system_prompt,
        api_key,
        max_tokens: Some(300),
        temperature: Some(0.6),
    };

    let response = reasoning::process(&req).await.map_err(|e| e.to_string())?;
    let created_at_ms = now_ms();

    let suggestion_id = db
        .insert_conversation_suggestion(conversation_id, created_at_ms, persona_name.as_deref(), &response.text)
        .str_err()?;

    #[derive(Clone, serde::Serialize)]
    struct SuggestionPayload {
        id: i64,
        conversation_id: i64,
        created_at_ms: i64,
        persona_name: Option<String>,
        text: String,
    }
    let _ = app.emit(
        "conversation-suggestion",
        SuggestionPayload {
            id: suggestion_id,
            conversation_id,
            created_at_ms,
            persona_name,
            text: response.text.clone(),
        },
    );

    Ok(response.text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn word_overlap_detects_near_identical_echo() {
        let a = "Good luck getting that power before 2031, that's the situation that we have today.";
        let b = "a request for all hundreds of megawatts of power to build the data center today, good luck getting that power before 2031, that's the situation that we have today.";
        assert!(word_overlap_ratio(a, b) >= ECHO_OVERLAP_THRESHOLD);
    }

    #[test]
    fn word_overlap_low_for_unrelated_text() {
        let a = "can you tell me more about your data center clients";
        let b = "no this is not true I am going to fix this and change it";
        assert!(word_overlap_ratio(a, b) < ECHO_OVERLAP_THRESHOLD);
    }

    #[test]
    fn word_overlap_empty_text_is_zero() {
        assert_eq!(word_overlap_ratio("", "hello"), 0.0);
        assert_eq!(word_overlap_ratio("hello", ""), 0.0);
    }

    #[test]
    fn word_overlap_identical_text_is_one() {
        assert_eq!(word_overlap_ratio("hello world", "Hello, World!"), 1.0);
    }
}
