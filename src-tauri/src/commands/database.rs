use super::{ResultExt, recordings_dir};
use crate::database::{Database, StatsPayload, StatsPeriod, Transcription};
use tauri::{AppHandle, Manager, State};

#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub fn save_transcription(
    app: AppHandle,
    db: State<'_, Database>,
    original_text: String,
    processed_text: Option<String>,
    processing_method: String,
    agent_name: Option<String>,
    error: Option<String>,
    duration_ms: Option<i64>,
    // Raw WAV bytes for the recording, when the caller wants it archived
    // to disk (History & Analytics). Written in the background — this
    // command returns as soon as the row is saved, before the file
    // write/`audio_path` update finish.
    audio_data: Option<Vec<u8>>,
) -> Result<i64, String> {
    let id = db
        .save_transcription(
            &original_text,
            processed_text.as_deref(),
            &processing_method,
            agent_name.as_deref(),
            error.as_deref(),
            duration_ms,
        )
        .str_err()?;

    if let Some(audio) = audio_data {
        let app2 = app.clone();
        tauri::async_runtime::spawn_blocking(move || {
            let dir = match recordings_dir(&app2) {
                Ok(d) => d,
                Err(e) => {
                    log::error!("[Whisperi] Failed to resolve recordings dir: {}", e);
                    return;
                }
            };
            let path = dir.join(format!("dictation-{id}.wav"));
            if let Err(e) = std::fs::write(&path, &audio) {
                log::error!("[Whisperi] Failed to write dictation audio: {}", e);
                return;
            }
            let db2 = app2.state::<Database>();
            if let Err(e) = db2.update_transcription_audio_path(id, &path.to_string_lossy()) {
                log::error!("[Whisperi] Failed to record dictation audio_path: {}", e);
            }
        });
    }

    Ok(id)
}

#[tauri::command]
pub fn get_transcriptions(
    db: State<'_, Database>,
    limit: u32,
    offset: u32,
) -> Result<Vec<Transcription>, String> {
    db.get_transcriptions(limit, offset).str_err()
}

#[tauri::command]
pub fn delete_transcription(db: State<'_, Database>, id: i64) -> Result<(), String> {
    db.delete_transcription(id).str_err()
}

#[tauri::command]
pub fn clear_transcriptions(db: State<'_, Database>) -> Result<(), String> {
    db.clear_transcriptions().str_err()
}

#[tauri::command]
pub fn get_stats(db: State<'_, Database>, period: String) -> Result<StatsPayload, String> {
    let p = match period.as_str() {
        "today" => StatsPeriod::Today,
        "week" => StatsPeriod::Week,
        "all" => StatsPeriod::All,
        other => return Err(format!("unknown stats period: {other}")),
    };
    db.get_stats(p).str_err()
}
