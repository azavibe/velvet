use super::{ResultExt, recordings_dir};
use crate::audio::archive::{self, ArchiveFailure};
use crate::database::{Database, StatsPayload, StatsPeriod, Transcription};
use tauri::{AppHandle, Manager, State};

#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn save_transcription(
    app: AppHandle,
    db: State<'_, Database>,
    original_text: String,
    processed_text: Option<String>,
    processing_method: String,
    agent_name: Option<String>,
    error: Option<String>,
    duration_ms: Option<i64>,
    // Raw WAV bytes for the recording, when the caller wants it archived in
    // History & Analytics. The command completes only after the asset is
    // ready or durably marked failed.
    audio_data: Option<Vec<u8>>,
    reconciled_text: Option<String>,
    reconciliation_status: String,
    reconciliation_confidence: Option<f64>,
    reconciliation_evidence: Option<String>,
) -> Result<i64, String> {
    let id = db
        .save_transcription_with_reconciliation(
            &original_text,
            processed_text.as_deref(),
            &processing_method,
            agent_name.as_deref(),
            error.as_deref(),
            duration_ms,
            reconciled_text.as_deref(),
            &reconciliation_status,
            reconciliation_confidence,
            reconciliation_evidence.as_deref(),
        )
        .str_err()?;

    if let Some(audio) = audio_data {
        let dir = recordings_dir(&app)?;
        let path = dir.join(format!("dictation-{id}.wav"));
        let (asset_id, _) = db.begin_audio_asset("dictation", id, "main").str_err()?;
        if let Err(error) = db.set_audio_asset_path(asset_id, &path) {
            let message = error.to_string();
            let _ = db.mark_audio_asset_failed(asset_id, ArchiveFailure::Open.code());
            return Err(message);
        }

        let path_for_write = path.clone();
        let write_result = tauri::async_runtime::spawn_blocking(move || {
            archive::write_wav_atomically(&path_for_write, &audio)
        })
        .await;
        let db2 = app.state::<Database>();
        match write_result {
            Ok(Ok(metadata)) => {
                if let Err(error) =
                    db2.mark_audio_asset_ready(asset_id, metadata.byte_length, metadata.duration_ms)
                {
                    let _ = db2.mark_audio_asset_failed(asset_id, ArchiveFailure::Metadata.code());
                    let _ = archive::remove_recording_if_safe(&dir, &path.to_string_lossy());
                    let temp = archive::temp_path(&path);
                    let _ = archive::remove_recording_if_safe(&dir, &temp.to_string_lossy());
                    return Err(error.to_string());
                }
            }
            Ok(Err(error)) => {
                db2.mark_audio_asset_failed(asset_id, error.code())
                    .str_err()?;
                log::warn!(
                    "[Whisperi] Dictation audio archive failed: {}",
                    error.code()
                );
            }
            Err(_) => {
                db2.mark_audio_asset_failed(asset_id, ArchiveFailure::Interrupted.code())
                    .str_err()?;
                log::warn!(
                    "[Whisperi] Dictation audio archive was interrupted: {}",
                    ArchiveFailure::Interrupted.code()
                );
            }
        }
    }

    Ok(id)
}

#[tauri::command]
pub fn get_transcriptions(
    _app: AppHandle,
    db: State<'_, Database>,
    limit: u32,
    offset: u32,
) -> Result<Vec<Transcription>, String> {
    db.get_transcriptions(limit, offset).str_err()
}

#[tauri::command]
pub fn delete_transcription(
    app: AppHandle,
    db: State<'_, Database>,
    id: i64,
) -> Result<(), String> {
    db.delete_transcription(id, &recordings_dir(&app).str_err()?)
        .str_err()
}

#[tauri::command]
pub fn clear_transcriptions(app: AppHandle, db: State<'_, Database>) -> Result<(), String> {
    db.clear_transcriptions(&recordings_dir(&app).str_err()?)
        .str_err()
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
