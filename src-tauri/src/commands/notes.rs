//! Tauri commands for Notes: mic-only capture with pause/resume, inline
//! editing, and an opt-in AI Markdown cleanup pass. Mirrors the
//! Conversations command layer's shape (broadcast events so any window
//! stays in sync, chunk consumer on a blocking task) but single-channel and
//! without personas/suggestions.

use std::fs::File;
use std::io::BufWriter;
use std::path::{Path, PathBuf};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
    mpsc,
};
use tauri::{AppHandle, Emitter, Manager, State};

use super::{ResultExt, recordings_dir};
use crate::audio::archive::{self, ArchiveFailure};
use crate::audio::note_capture::{NoteCapture, NoteCaptureState, NoteChunk};
use crate::audio::recorder::TARGET_SAMPLE_RATE;
use crate::database::{Database, Note};
use crate::tray::{self, RecordingSource};

pub struct NoteAudioArchive {
    writer: Mutex<Option<ActiveWav>>,
    completion: Mutex<Option<mpsc::Receiver<()>>>,
    aborted: AtomicBool,
}

impl Default for NoteAudioArchive {
    fn default() -> Self {
        Self {
            writer: Mutex::new(None),
            completion: Mutex::new(None),
            aborted: AtomicBool::new(false),
        }
    }
}

struct ActiveWav {
    note_id: i64,
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

impl NoteAudioArchive {
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

    fn append(
        &self,
        db: &Database,
        recordings_root: &Path,
        note_id: i64,
        wav_bytes: &[u8],
    ) -> Result<(), ArchiveFailure> {
        if self.aborted.load(Ordering::SeqCst) {
            return Err(ArchiveFailure::Interrupted);
        }
        let mut guard = self
            .writer
            .lock()
            .map_err(|_| ArchiveFailure::Interrupted)?;

        if guard.as_ref().map(|active| active.note_id) != Some(note_id) {
            if let Some(active) = guard.take() {
                drop(guard);
                Self::finalize_active(db, recordings_root, active);
                guard = self
                    .writer
                    .lock()
                    .map_err(|_| ArchiveFailure::Interrupted)?;
            }

            let (asset_id, _) = db
                .begin_audio_asset("note", note_id, "main")
                .map_err(|_| ArchiveFailure::Open)?;
            let final_path = recordings_root.join(format!("note-{note_id}-segment-{asset_id}.wav"));
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
                note_id,
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

    fn finalize_active(db: &Database, recordings_root: &Path, mut active: ActiveWav) {
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
                    log::warn!("[Note] audio state update failed: {error}");
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
        let Some(active) = self.writer.lock().ok().and_then(|mut guard| guard.take()) else {
            return;
        };
        Self::finalize_active(db, recordings_root, active);
    }

    pub(crate) fn abort(&self, db: &Database, recordings_root: &Path) {
        self.aborted.store(true, Ordering::SeqCst);
        let Some(active) = self.writer.lock().ok().and_then(|mut guard| guard.take()) else {
            return;
        };
        let _ = db.mark_audio_asset_failed(active.asset_id, ArchiveFailure::Interrupted.code());
        let _ =
            archive::remove_recording_if_safe(recordings_root, &active.temp_path.to_string_lossy());
    }
}

#[derive(Clone, serde::Serialize)]
struct NoteUtterancePayload {
    note_id: i64,
    started_at_ms: i64,
    text: String,
}

#[derive(Clone, serde::Serialize)]
struct NoteErrorPayload {
    note_id: i64,
    message: String,
}

/// Starts capture for a note. `note_id: None` creates a fresh note; `Some`
/// resumes capture appending onto an existing one ("Append Dictation" from
/// the History card). Either way returns the note id capture is targeting.
#[tauri::command]
pub async fn start_note_capture(
    app: AppHandle,
    note_state: State<'_, Arc<NoteCaptureState>>,
    db: State<'_, Database>,
    mic_device_id: Option<String>,
    groq_api_key: String,
    note_id: Option<i64>,
) -> Result<i64, String> {
    let created_fresh = note_id.is_none();
    let note_id = match note_id {
        Some(id) => id,
        None => db.create_note().str_err()?,
    };
    let recordings_root = recordings_dir(&app).ok();

    if let Err(e) = NoteCapture::start(&**note_state, mic_device_id) {
        // Only roll back a note we just created — an "append" target that
        // failed to start should stay as it was.
        if created_fresh {
            let _ = db.delete_note(
                note_id,
                recordings_root.as_deref().unwrap_or_else(|| Path::new("")),
            );
        }
        return Err(e.to_string());
    }

    let _ = app.emit("note-started", note_id);

    let rx = match note_state.take_chunk_receiver() {
        Some(rx) => rx,
        None => {
            let _ = NoteCapture::stop(&**note_state);
            return Err("Note capture started without a chunk receiver".to_string());
        }
    };

    tray::start_recording(&app, RecordingSource::Note);

    let app_for_consumer = app.clone();
    let api_key = groq_api_key;
    let (archive_done_tx, archive_done_rx) = mpsc::channel();
    app.state::<NoteAudioArchive>()
        .set_consumer_completion(archive_done_rx);

    tauri::async_runtime::spawn_blocking(move || {
        while let Ok(chunk) = rx.recv() {
            {
                let db = app_for_consumer.state::<Database>();
                let archive = app_for_consumer.state::<NoteAudioArchive>();
                match recordings_dir(&app_for_consumer) {
                    Ok(recordings_root) => {
                        if let Err(error) =
                            archive.append(&db, &recordings_root, note_id, &chunk.wav)
                        {
                            log::warn!("[Note] audio archive failed: {}", error.code());
                        }
                    }
                    Err(_) => log::warn!(
                        "[Note] audio archive failed: {}",
                        ArchiveFailure::Open.code()
                    ),
                }
            }
            let app2 = app_for_consumer.clone();
            let api_key2 = api_key.clone();
            tauri::async_runtime::spawn(async move {
                handle_note_chunk(app2, note_id, chunk, api_key2).await;
            });
        }
        let _ = archive_done_tx.send(());
        log::info!("[Note] chunk consumer ended for note {}", note_id);
    });

    Ok(note_id)
}

async fn handle_note_chunk(app: AppHandle, note_id: i64, chunk: NoteChunk, api_key: String) {
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
            log::error!("[Note] chunk transcription failed: {}", e);
            let _ = app.emit(
                "note-error",
                NoteErrorPayload {
                    note_id,
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
    if let Err(e) = db.append_note_transcript(note_id, &text) {
        log::error!("[Note] failed to persist transcript: {}", e);
        return;
    }

    let _ = app.emit(
        "note-utterance",
        NoteUtterancePayload {
            note_id,
            started_at_ms,
            text,
        },
    );
}

#[tauri::command]
pub fn pause_note_capture(
    app: AppHandle,
    note_state: State<'_, Arc<NoteCaptureState>>,
) -> Result<(), String> {
    note_state.pause();
    tray::stop_recording(&app, RecordingSource::Note, None);
    Ok(())
}

#[tauri::command]
pub fn resume_note_capture(
    app: AppHandle,
    note_state: State<'_, Arc<NoteCaptureState>>,
) -> Result<(), String> {
    note_state.resume();
    tray::start_recording(&app, RecordingSource::Note);
    Ok(())
}

#[tauri::command]
pub fn stop_note_capture(
    app: AppHandle,
    note_state: State<'_, Arc<NoteCaptureState>>,
) -> Result<(), String> {
    NoteCapture::stop(&**note_state).str_err()?;
    tray::stop_recording(&app, RecordingSource::Note, None);
    let recordings_root = recordings_dir(&app).str_err()?;
    let db = app.state::<Database>();
    let archive = app.state::<NoteAudioArchive>();
    if archive.wait_for_consumer().is_err() {
        archive.abort(&db, &recordings_root);
        return Err(ArchiveFailure::Interrupted.code().to_string());
    }
    let _ = app.emit("note-stopped", ());
    archive.finalize(&db, &recordings_root);

    Ok(())
}

#[tauri::command]
pub fn is_note_capture_active(
    note_state: State<'_, Arc<NoteCaptureState>>,
) -> Result<bool, String> {
    Ok(note_state.is_active())
}

#[tauri::command]
pub fn is_note_capture_paused(
    note_state: State<'_, Arc<NoteCaptureState>>,
) -> Result<bool, String> {
    Ok(note_state.is_paused())
}

#[tauri::command]
pub fn get_note_capture_error(
    note_state: State<'_, Arc<NoteCaptureState>>,
) -> Result<Option<String>, String> {
    Ok(note_state.get_error())
}

#[tauri::command]
pub fn list_notes(
    _app: AppHandle,
    db: State<'_, Database>,
    limit: u32,
    offset: u32,
) -> Result<Vec<Note>, String> {
    db.list_notes(limit, offset).str_err()
}

#[tauri::command]
pub fn get_note(_app: AppHandle, db: State<'_, Database>, note_id: i64) -> Result<Note, String> {
    db.get_note(note_id).str_err()
}

/// Title-only rename — used by the live capture window (see
/// `Database::set_note_title`).
#[tauri::command]
pub fn set_note_title(db: State<'_, Database>, note_id: i64, title: String) -> Result<(), String> {
    db.set_note_title(note_id, &title).str_err()
}

/// Inline edit save from the History card's Edit toggle.
#[tauri::command]
pub fn update_note(
    db: State<'_, Database>,
    note_id: i64,
    title: Option<String>,
    raw_transcript: String,
) -> Result<(), String> {
    db.update_note(note_id, title.as_deref(), &raw_transcript)
        .str_err()
}

#[tauri::command]
pub fn delete_note(app: AppHandle, db: State<'_, Database>, note_id: i64) -> Result<(), String> {
    db.delete_note(note_id, &recordings_dir(&app).str_err()?)
        .str_err()
}

const CLEANUP_SYSTEM_PROMPT: &str = "You turn a raw speech-to-text transcript of a note into clean, well-organized Markdown. \
Use bullet points for lists, **bold** key terms and action items, and keep the person's original wording and meaning — \
don't add information that isn't in the transcript. Start your response with a single '# Title' line summarizing the note \
in a few words, then a blank line, then the formatted body. Output only the Markdown, nothing else.";

/// Runs the opt-in Markdown cleanup pass over a note's raw transcript and
/// saves the result. Not run automatically — the user triggers this
/// per-note from the History card.
#[tauri::command]
pub async fn cleanup_note(
    db: State<'_, Database>,
    note_id: i64,
    model: String,
    provider: String,
    api_key: String,
) -> Result<String, String> {
    let note = db.get_note(note_id).str_err()?;
    if note.raw_transcript.trim().is_empty() {
        return Err("No transcript yet".to_string());
    }

    let req = crate::reasoning::ReasoningRequest {
        text: note.raw_transcript.clone(),
        model,
        provider,
        system_prompt: CLEANUP_SYSTEM_PROMPT.to_string(),
        api_key,
        max_tokens: Some(1500),
        temperature: Some(0.3),
    };

    let response = crate::reasoning::process(&req)
        .await
        .map_err(|e| e.to_string())?;
    db.set_note_markdown(note_id, &response.text).str_err()?;
    Ok(response.text)
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
        let root = std::env::temp_dir().join(format!("agenda-note-audio-{label}-{suffix}"));
        fs::create_dir_all(&root).unwrap();
        root
    }

    fn test_wav() -> Vec<u8> {
        crate::audio::recorder::encode_wav(&[0.1, -0.1, 0.2, -0.2], 16_000).unwrap()
    }

    #[test]
    fn appended_note_sessions_publish_ordered_segments() {
        let db = Database::new_in_memory().unwrap();
        let archive = NoteAudioArchive::default();
        let root = test_root("segments");
        let note_id = db.create_note().unwrap();

        archive.append(&db, &root, note_id, &test_wav()).unwrap();
        archive.finalize(&db, &root);
        archive.append(&db, &root, note_id, &test_wav()).unwrap();
        let saving = db.get_note(note_id).unwrap();
        assert_eq!(saving.audio_segments.len(), 2);
        assert_eq!(saving.audio_segments[0].status, AudioAssetStatus::Ready);
        assert_eq!(saving.audio_segments[1].status, AudioAssetStatus::Saving);
        assert_eq!(saving.audio_segments[1].path, None);

        archive.finalize(&db, &root);
        let ready = db.get_note(note_id).unwrap();
        assert_eq!(ready.audio_segments.len(), 2);
        assert_eq!(ready.audio_segments[0].sequence, 0);
        assert_eq!(ready.audio_segments[1].sequence, 1);
        assert!(
            ready
                .audio_segments
                .iter()
                .all(|segment| segment.status == AudioAssetStatus::Ready)
        );
        assert_eq!(
            ready.audio_path.as_deref(),
            ready.audio_segments[1].path.as_deref()
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn interrupted_note_segment_stays_failed_and_unpublished() {
        let db = Database::new_in_memory().unwrap();
        let archive = NoteAudioArchive::default();
        let root = test_root("abort");
        let note_id = db.create_note().unwrap();

        archive.append(&db, &root, note_id, &test_wav()).unwrap();
        let saving = db.get_note(note_id).unwrap();
        let asset = saving.audio_segments[0].clone();
        let final_path = root.join(format!("note-{note_id}-segment-{}.wav", asset.id));
        archive.abort(&db, &root);

        let failed = db.get_note(note_id).unwrap();
        assert_eq!(failed.audio_segments[0].status, AudioAssetStatus::Failed);
        assert_eq!(
            failed.audio_segments[0].error.as_deref(),
            Some("interrupted")
        );
        assert_eq!(failed.audio_segments[0].path, None);
        assert!(!final_path.exists());
        assert!(!crate::audio::archive::temp_path(&final_path).exists());

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn deleting_note_during_finalization_does_not_leave_an_orphan() {
        let db = Database::new_in_memory().unwrap();
        let archive = NoteAudioArchive::default();
        let root = test_root("delete-race");
        let note_id = db.create_note().unwrap();

        archive.append(&db, &root, note_id, &test_wav()).unwrap();
        let asset = db.get_note(note_id).unwrap().audio_segments[0].clone();
        let final_path = root.join(format!("note-{note_id}-segment-{}.wav", asset.id));

        db.delete_note(note_id, &root).unwrap();
        archive.finalize(&db, &root);

        assert!(!final_path.exists());
        assert!(!crate::audio::archive::temp_path(&final_path).exists());
        let _ = fs::remove_dir_all(root);
    }
}
