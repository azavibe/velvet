//! Tauri commands for the Conversations feature: start/stop dual-channel
//! capture, drain transcribed chunks into the DB + frontend events, and
//! generate on-demand suggestions from a persona prompt + the transcript
//! so far.

use std::fs::File;
use std::io::BufWriter;
use std::path::{Path, PathBuf};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
    mpsc,
};
use std::time::{SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Emitter, Manager, State};

use super::{ResultExt, recordings_dir};
use crate::audio::archive::{self, ArchiveFailure};
use crate::audio::conversation::{
    Channel, ConversationCapture, ConversationChunk, ConversationState,
};
use crate::audio::recorder::TARGET_SAMPLE_RATE;
use crate::database::{ConversationDetail, ConversationSummary, Database};
use crate::reasoning::{self, ReasoningRequest};
use crate::tray::{self, RecordingSource};

/// Streams each channel's transcribed chunks to its own WAV file on disk as
/// the conversation runs, rather than buffering the whole session in
/// memory — chunks arrive already resampled to `TARGET_SAMPLE_RATE`/mono
/// (see `audio::conversation::to_chunk`), which is exactly the format
/// written here, so no re-encoding is needed beyond decode-then-append.
/// Writers open lazily on each channel's first chunk, so a channel that
/// never captures anything (e.g. no one on the other end talks) leaves no
/// file rather than an empty one.
pub struct ConversationAudioArchive {
    me: Mutex<Option<ActiveWav>>,
    them: Mutex<Option<ActiveWav>>,
    completion: Mutex<Option<mpsc::Receiver<()>>>,
    aborted: AtomicBool,
}

impl Default for ConversationAudioArchive {
    fn default() -> Self {
        Self {
            me: Mutex::new(None),
            them: Mutex::new(None),
            completion: Mutex::new(None),
            aborted: AtomicBool::new(false),
        }
    }
}

struct ActiveWav {
    asset_id: i64,
    final_path: PathBuf,
    temp_path: PathBuf,
    writer: Option<hound::WavWriter<BufWriter<File>>>,
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
    pub(crate) fn set_consumer_completion(&self, receiver: mpsc::Receiver<()>) {
        self.aborted.store(false, Ordering::SeqCst);
        if let Ok(mut guard) = self.completion.lock() {
            *guard = Some(receiver);
        }
    }

    pub(crate) fn wait_for_consumer(&self) -> Result<(), ArchiveFailure> {
        let receiver = self
            .completion
            .lock()
            .ok()
            .and_then(|mut guard| guard.take());
        let Some(receiver) = receiver else {
            return Ok(());
        };
        receiver
            .recv_timeout(std::time::Duration::from_secs(10))
            .map_err(|_| ArchiveFailure::Interrupted)
    }

    /// Appends one chunk on the single receiver-drain thread. Keeping archive
    /// work here, instead of spawning one task per chunk, preserves capture
    /// order and lets stop wait for every write before finalization.
    fn append(
        &self,
        db: &Database,
        recordings_root: &Path,
        conversation_id: i64,
        channel: Channel,
        wav_bytes: &[u8],
    ) -> Result<(), ArchiveFailure> {
        if self.aborted.load(Ordering::SeqCst) {
            return Err(ArchiveFailure::Interrupted);
        }
        let mutex = match channel {
            Channel::Me => &self.me,
            Channel::Them => &self.them,
        };
        let mut guard = mutex.lock().map_err(|_| ArchiveFailure::Interrupted)?;
        if guard.is_none() {
            let channel_name = channel.as_str();
            let (asset_id, _) = db
                .begin_audio_asset("conversation", conversation_id, channel_name)
                .map_err(|_| ArchiveFailure::Open)?;
            let final_path = recordings_root.join(format!(
                "conversation-{conversation_id}-{channel_name}-asset-{asset_id}.wav"
            ));
            if db.set_audio_asset_path(asset_id, &final_path).is_err() {
                let _ = db.mark_audio_asset_failed(asset_id, ArchiveFailure::Open.code());
                return Err(ArchiveFailure::Open);
            }
            let temp_path = archive::temp_path(&final_path);
            let writer = match hound::WavWriter::create(&temp_path, wav_spec()) {
                Ok(writer) => writer,
                Err(_) => {
                    let _ = db.mark_audio_asset_failed(asset_id, ArchiveFailure::Open.code());
                    let _ = archive::remove_recording_if_safe(
                        recordings_root,
                        &temp_path.to_string_lossy(),
                    );
                    return Err(ArchiveFailure::Open);
                }
            };
            *guard = Some(ActiveWav {
                asset_id,
                final_path,
                temp_path,
                writer: Some(writer),
            });
        }

        let result = (|| -> Result<(), ArchiveFailure> {
            let active = guard.as_mut().ok_or(ArchiveFailure::Interrupted)?;
            let writer = active.writer.as_mut().ok_or(ArchiveFailure::Interrupted)?;
            let mut reader = hound::WavReader::new(std::io::Cursor::new(wav_bytes))
                .map_err(|_| ArchiveFailure::InvalidWav)?;
            for sample in reader.samples::<i16>() {
                let sample = sample.map_err(|_| ArchiveFailure::InvalidWav)?;
                writer
                    .write_sample(sample)
                    .map_err(|_| ArchiveFailure::Write)?;
            }
            writer.flush().map_err(|_| ArchiveFailure::Flush)
        })();

        if let Err(error) = result {
            let active = guard.take();
            drop(guard);
            if let Some(active) = active {
                let _ = db.mark_audio_asset_failed(active.asset_id, error.code());
                let _ = archive::remove_recording_if_safe(
                    recordings_root,
                    &active.temp_path.to_string_lossy(),
                );
            }
            return Err(error);
        }
        Ok(())
    }

    fn finalize_channel(
        &self,
        db: &Database,
        recordings_root: &Path,
        mutex: &Mutex<Option<ActiveWav>>,
    ) {
        let Some(mut active) = mutex.lock().ok().and_then(|mut guard| guard.take()) else {
            return;
        };
        let Some(writer) = active.writer.take() else {
            let _ = db.mark_audio_asset_failed(active.asset_id, ArchiveFailure::Interrupted.code());
            let _ = archive::remove_recording_if_safe(
                recordings_root,
                &active.temp_path.to_string_lossy(),
            );
            return;
        };
        let result = writer
            .finalize()
            .map_err(|_| ArchiveFailure::Flush)
            .and_then(|_| archive::finalize_wav_file(&active.temp_path, &active.final_path));
        match result {
            Ok(metadata) => {
                if let Err(error) = db.mark_audio_asset_ready(
                    active.asset_id,
                    metadata.byte_length,
                    metadata.duration_ms,
                ) {
                    let _ = db
                        .mark_audio_asset_failed(active.asset_id, ArchiveFailure::Metadata.code());
                    let _ = archive::remove_recording_if_safe(
                        recordings_root,
                        &active.final_path.to_string_lossy(),
                    );
                    let _ = archive::remove_recording_if_safe(
                        recordings_root,
                        &active.temp_path.to_string_lossy(),
                    );
                    log::warn!("[Conversation] audio state update failed: {error}");
                }
            }
            Err(error) => {
                let _ = db.mark_audio_asset_failed(active.asset_id, error.code());
                let _ = archive::remove_recording_if_safe(
                    recordings_root,
                    &active.temp_path.to_string_lossy(),
                );
            }
        }
    }

    pub(crate) fn finalize(&self, db: &Database, recordings_root: &Path) {
        self.finalize_channel(db, recordings_root, &self.me);
        self.finalize_channel(db, recordings_root, &self.them);
    }

    pub(crate) fn abort(&self, db: &Database, recordings_root: &Path) {
        self.aborted.store(true, Ordering::SeqCst);
        for mutex in [&self.me, &self.them] {
            let Some(active) = mutex.lock().ok().and_then(|mut guard| guard.take()) else {
                continue;
            };
            let _ = db.mark_audio_asset_failed(active.asset_id, ArchiveFailure::Interrupted.code());
            let _ = archive::remove_recording_if_safe(
                recordings_root,
                &active.temp_path.to_string_lossy(),
            );
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
    let recordings_root = recordings_dir(&app).ok();

    if let Err(e) = ConversationCapture::start(&**conv_state, mic_device_id) {
        // Roll back the DB row we just created rather than leaving an
        // empty, never-ended conversation behind.
        let _ = db.delete_conversation(
            conversation_id,
            recordings_root.as_deref().unwrap_or_else(|| Path::new("")),
        );
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
        ConversationStartedPayload {
            conversation_id,
            persona_name: persona_name.clone(),
        },
    );

    let rx = match conv_state.take_chunk_receiver() {
        Some(rx) => rx,
        None => {
            let _ = ConversationCapture::stop(&**conv_state);
            let _ = db.delete_conversation(
                conversation_id,
                recordings_root.as_deref().unwrap_or_else(|| Path::new("")),
            );
            return Err("Conversation capture started without a chunk receiver".to_string());
        }
    };

    tray::start_recording(&app, RecordingSource::Conversation);

    let app_for_consumer = app.clone();
    let api_key = groq_api_key;
    let (archive_done_tx, archive_done_rx) = mpsc::channel();
    app.state::<ConversationAudioArchive>()
        .set_consumer_completion(archive_done_rx);

    // Drain finished chunks on a blocking thread. Archive writes happen on
    // this receiver thread in capture order; transcription remains concurrent
    // so a slow "them" request never delays later "me" requests.
    tauri::async_runtime::spawn_blocking(move || {
        while let Ok(chunk) = rx.recv() {
            {
                let db = app_for_consumer.state::<Database>();
                let archive = app_for_consumer.state::<ConversationAudioArchive>();
                match recordings_dir(&app_for_consumer) {
                    Ok(recordings_root) => {
                        if let Err(error) = archive.append(
                            &db,
                            &recordings_root,
                            conversation_id,
                            chunk.channel,
                            &chunk.wav,
                        ) {
                            log::warn!("[Conversation] audio archive failed: {}", error.code());
                        }
                    }
                    Err(_) => log::warn!(
                        "[Conversation] audio archive failed: {}",
                        ArchiveFailure::Open.code()
                    ),
                }
            }
            let app2 = app_for_consumer.clone();
            let api_key2 = api_key.clone();
            tauri::async_runtime::spawn(async move {
                handle_conversation_chunk(app2, conversation_id, chunk, api_key2).await;
            });
        }
        let _ = archive_done_tx.send(());
        log::info!(
            "[Conversation] chunk consumer ended for conversation {}",
            conversation_id
        );
    });

    Ok(conversation_id)
}

async fn handle_conversation_chunk(
    app: AppHandle,
    conversation_id: i64,
    chunk: ConversationChunk,
    api_key: String,
) {
    let channel_str = chunk.channel.as_str();
    let started_at_ms = chunk.started_at_ms;

    let result = crate::transcription::cloud::transcribe_groq(
        chunk.wav,
        &api_key,
        "whisper-large-v3-turbo",
        None,
        None,
        None,
    )
    .await;

    let text = match result {
        Ok(t) => t.text.trim().to_string(),
        Err(e) => {
            log::error!(
                "[Conversation] {} chunk transcription failed: {}",
                channel_str,
                e
            );
            let _ = app.emit(
                "conversation-error",
                ConversationErrorPayload {
                    conversation_id,
                    message: e.to_string(),
                },
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
        if let Ok(recent) =
            db.get_recent_utterances(conversation_id, started_at_ms - ECHO_WINDOW_MS)
        {
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

    let utterance_id = match db.insert_conversation_utterance(
        conversation_id,
        channel_str,
        started_at_ms,
        &text,
    ) {
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
    tray::stop_recording(&app, RecordingSource::Conversation, None);
    let recordings_root = recordings_dir(&app).str_err()?;
    let archive = app.state::<ConversationAudioArchive>();
    if archive.wait_for_consumer().is_err() {
        archive.abort(&db, &recordings_root);
        return Err(ArchiveFailure::Interrupted.code().to_string());
    }
    db.end_conversation(conversation_id, title.as_deref())
        .str_err()?;
    let _ = app.emit("conversation-stopped", conversation_id);
    archive.finalize(&db, &recordings_root);

    Ok(())
}

#[tauri::command]
pub fn get_conversation_audio_levels(
    conv_state: State<'_, Arc<ConversationState>>,
) -> Result<(f32, f32), String> {
    Ok((conv_state.mic_level(), conv_state.them_level()))
}

#[tauri::command]
pub fn is_conversation_active(
    conv_state: State<'_, Arc<ConversationState>>,
) -> Result<bool, String> {
    Ok(conv_state.is_active())
}

/// Surfaces a capture-thread setup/stream error (e.g. no loopback-capable
/// output device) so the frontend can show it instead of silently sitting
/// on an empty transcript.
#[tauri::command]
pub fn get_conversation_error(
    conv_state: State<'_, Arc<ConversationState>>,
) -> Result<Option<String>, String> {
    Ok(conv_state.get_error())
}

#[tauri::command]
pub fn list_conversations(
    _app: AppHandle,
    db: State<'_, Database>,
    limit: u32,
    offset: u32,
) -> Result<Vec<ConversationSummary>, String> {
    db.list_conversations(limit, offset).str_err()
}

#[tauri::command]
pub fn get_conversation(
    _app: AppHandle,
    db: State<'_, Database>,
    conversation_id: i64,
) -> Result<ConversationDetail, String> {
    db.get_conversation(conversation_id).str_err()
}

#[tauri::command]
pub fn delete_conversation(
    app: AppHandle,
    db: State<'_, Database>,
    conversation_id: i64,
) -> Result<(), String> {
    db.delete_conversation(conversation_id, &recordings_dir(&app).str_err()?)
        .str_err()
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
        .map(|u| {
            format!(
                "{}: {}",
                if u.channel == "me" { "Me" } else { "Them" },
                u.text
            )
        })
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
        .insert_conversation_suggestion(
            conversation_id,
            created_at_ms,
            persona_name.as_deref(),
            &response.text,
        )
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
        let a =
            "Good luck getting that power before 2031, that's the situation that we have today.";
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

#[cfg(test)]
mod archive_tests {
    use super::*;
    use crate::database::{AudioAssetStatus, Database};
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn test_root(label: &str) -> PathBuf {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("agenda-conversation-audio-{label}-{suffix}"));
        fs::create_dir_all(&root).unwrap();
        root
    }

    fn test_wav() -> Vec<u8> {
        crate::audio::recorder::encode_wav(&[0.1, -0.1, 0.2, -0.2], 16_000).unwrap()
    }

    #[test]
    fn serialized_chunks_publish_one_complete_ready_wav() {
        let db = Database::new_in_memory().unwrap();
        let archive = ConversationAudioArchive::default();
        let root = test_root("success");
        let conversation_id = db.create_conversation(None).unwrap();
        let wav = test_wav();

        archive
            .append(&db, &root, conversation_id, Channel::Me, &wav)
            .unwrap();
        archive
            .append(&db, &root, conversation_id, Channel::Me, &wav)
            .unwrap();

        let saving = db.list_conversations(10, 0).unwrap().remove(0);
        let saving_asset = saving.audio_asset_me.unwrap();
        assert_eq!(saving_asset.status, AudioAssetStatus::Saving);
        assert_eq!(saving_asset.path, None);
        let final_path = root.join(format!(
            "conversation-{conversation_id}-me-asset-{}.wav",
            saving_asset.id
        ));
        assert!(!final_path.exists());

        archive.finalize(&db, &root);

        let ready = db.get_conversation(conversation_id).unwrap().conversation;
        let ready_asset = ready.audio_asset_me.unwrap();
        assert_eq!(ready_asset.status, AudioAssetStatus::Ready);
        assert_eq!(
            ready_asset.path.as_deref(),
            Some(final_path.to_str().unwrap())
        );
        assert!(!crate::audio::archive::temp_path(&final_path).exists());
        let mut reader = hound::WavReader::open(&final_path).unwrap();
        assert_eq!(reader.samples::<i16>().count(), 8);

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn abort_marks_conversation_audio_failed_without_publishing() {
        let db = Database::new_in_memory().unwrap();
        let archive = ConversationAudioArchive::default();
        let root = test_root("abort");
        let conversation_id = db.create_conversation(None).unwrap();

        archive
            .append(&db, &root, conversation_id, Channel::Them, &test_wav())
            .unwrap();
        let saving = db.list_conversations(10, 0).unwrap().remove(0);
        let asset = saving.audio_asset_them.unwrap();
        let final_path = root.join(format!(
            "conversation-{conversation_id}-them-asset-{}.wav",
            asset.id
        ));
        archive.abort(&db, &root);

        let failed = db.get_conversation(conversation_id).unwrap().conversation;
        let failed_asset = failed.audio_asset_them.unwrap();
        assert_eq!(failed_asset.status, AudioAssetStatus::Failed);
        assert_eq!(failed_asset.error.as_deref(), Some("interrupted"));
        assert_eq!(failed_asset.path, None);
        assert!(!final_path.exists());
        assert!(!crate::audio::archive::temp_path(&final_path).exists());

        let _ = fs::remove_dir_all(root);
    }
}
