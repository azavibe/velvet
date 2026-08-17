//! Tauri commands for Notes: mic-only capture with pause/resume, inline
//! editing, and an opt-in AI Markdown cleanup pass. Mirrors the
//! Conversations command layer's shape (broadcast events so any window
//! stays in sync, chunk consumer on a blocking task) but single-channel and
//! without personas/suggestions.

use std::fs::File;
use std::io::BufWriter;
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Emitter, Manager, State};

use super::{ResultExt, recordings_dir};
use crate::audio::note_capture::{NoteCapture, NoteCaptureState, NoteChunk};
use crate::audio::recorder::TARGET_SAMPLE_RATE;
use crate::database::{Database, Note};

/// Streams a note's transcribed chunks to a WAV file as capture runs, same
/// streaming-write approach as `ConversationAudioArchive`. Keyed by note id
/// so a stray chunk from a just-stopped session can't be appended to the
/// next note that starts capturing.
///
/// Known limitation: "Append Dictation" on a note that was already stopped
/// once starts a *new* audio file (hound can't append to an existing WAV
/// without rewriting its header), so `notes.audio_path` ends up pointing at
/// only the most recent capture session's audio — earlier sessions'
/// segments stay on disk but stop being linked from the note. Fine for the
/// common case (one continuous session, paused/resumed internally); a
/// segment-list would be needed to fix the rarer multi-session case.
#[derive(Default)]
pub struct NoteAudioArchive {
    writer: Mutex<Option<(i64, hound::WavWriter<BufWriter<File>>)>>,
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
    fn append(&self, note_id: i64, wav_bytes: &[u8], path_for_new_writer: impl FnOnce() -> std::path::PathBuf, on_open: impl FnOnce(&std::path::Path)) {
        let Ok(mut guard) = self.writer.lock() else { return };

        if guard.as_ref().map(|(id, _)| *id) != Some(note_id) {
            // A different (or no) note is currently open — finalize it and
            // open a fresh writer for this one.
            if let Some((_, w)) = guard.take() {
                let _ = w.finalize();
            }
            let path = path_for_new_writer();
            match hound::WavWriter::create(&path, wav_spec()) {
                Ok(w) => {
                    on_open(&path);
                    *guard = Some((note_id, w));
                }
                Err(e) => {
                    log::error!("[Note] failed to open audio archive {:?}: {}", path, e);
                    return;
                }
            }
        }

        let Some((_, writer)) = guard.as_mut() else { return };
        let mut reader = match hound::WavReader::new(std::io::Cursor::new(wav_bytes)) {
            Ok(r) => r,
            Err(e) => {
                log::error!("[Note] failed to decode chunk for archiving: {}", e);
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

    pub(crate) fn finalize(&self) {
        if let Ok(mut guard) = self.writer.lock() {
            if let Some((_, w)) = guard.take() {
                let _ = w.finalize();
            }
        }
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

    if let Err(e) = NoteCapture::start(&**note_state, mic_device_id) {
        // Only roll back a note we just created — an "append" target that
        // failed to start should stay as it was.
        if created_fresh {
            let _ = db.delete_note(note_id);
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

    let app_for_consumer = app.clone();
    let api_key = groq_api_key;

    tauri::async_runtime::spawn_blocking(move || {
        while let Ok(chunk) = rx.recv() {
            let app2 = app_for_consumer.clone();
            let api_key2 = api_key.clone();
            tauri::async_runtime::spawn(async move {
                handle_note_chunk(app2, note_id, chunk, api_key2).await;
            });
        }
        log::info!("[Note] chunk consumer ended for note {}", note_id);
    });

    Ok(note_id)
}

async fn handle_note_chunk(app: AppHandle, note_id: i64, chunk: NoteChunk, api_key: String) {
    let started_at_ms = chunk.started_at_ms;

    {
        let app_for_archive = app.clone();
        let wav_for_archive = chunk.wav.clone();
        tauri::async_runtime::spawn_blocking(move || {
            let archive = app_for_archive.state::<NoteAudioArchive>();
            archive.append(
                note_id,
                &wav_for_archive,
                || {
                    recordings_dir(&app_for_archive)
                        .unwrap_or_else(|_| std::env::temp_dir())
                        .join(format!("note-{note_id}-{}.wav", started_at_ms))
                },
                |path| {
                    let db = app_for_archive.state::<Database>();
                    let _ = db.update_note_audio_path(note_id, &path.to_string_lossy());
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
            log::error!("[Note] chunk transcription failed: {}", e);
            let _ = app.emit("note-error", NoteErrorPayload { note_id, message: e.to_string() });
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

    let _ = app.emit("note-utterance", NoteUtterancePayload { note_id, started_at_ms, text });
}

#[tauri::command]
pub fn pause_note_capture(note_state: State<'_, Arc<NoteCaptureState>>) -> Result<(), String> {
    note_state.pause();
    Ok(())
}

#[tauri::command]
pub fn resume_note_capture(note_state: State<'_, Arc<NoteCaptureState>>) -> Result<(), String> {
    note_state.resume();
    Ok(())
}

#[tauri::command]
pub fn stop_note_capture(app: AppHandle, note_state: State<'_, Arc<NoteCaptureState>>) -> Result<(), String> {
    NoteCapture::stop(&**note_state).str_err()?;
    let _ = app.emit("note-stopped", ());

    let app_for_archive = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        app_for_archive.state::<NoteAudioArchive>().finalize();
    });

    Ok(())
}

#[tauri::command]
pub fn is_note_capture_active(note_state: State<'_, Arc<NoteCaptureState>>) -> Result<bool, String> {
    Ok(note_state.is_active())
}

#[tauri::command]
pub fn is_note_capture_paused(note_state: State<'_, Arc<NoteCaptureState>>) -> Result<bool, String> {
    Ok(note_state.is_paused())
}

#[tauri::command]
pub fn get_note_capture_error(note_state: State<'_, Arc<NoteCaptureState>>) -> Result<Option<String>, String> {
    Ok(note_state.get_error())
}

#[tauri::command]
pub fn list_notes(db: State<'_, Database>, limit: u32, offset: u32) -> Result<Vec<Note>, String> {
    db.list_notes(limit, offset).str_err()
}

#[tauri::command]
pub fn get_note(db: State<'_, Database>, note_id: i64) -> Result<Note, String> {
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
pub fn update_note(db: State<'_, Database>, note_id: i64, title: Option<String>, raw_transcript: String) -> Result<(), String> {
    db.update_note(note_id, title.as_deref(), &raw_transcript).str_err()
}

#[tauri::command]
pub fn delete_note(db: State<'_, Database>, note_id: i64) -> Result<(), String> {
    db.delete_note(note_id).str_err()
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

    let response = crate::reasoning::process(&req).await.map_err(|e| e.to_string())?;
    db.set_note_markdown(note_id, &response.text).str_err()?;
    Ok(response.text)
}
